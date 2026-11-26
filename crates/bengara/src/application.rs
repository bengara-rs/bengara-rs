//! アプリの組み立てと、1リクエストの処理。
//!
//! `bootstrap/app.rs` が返す `Application` が、起動時に固定される本体です。

use std::path::Path;
use std::sync::Arc;

use crate::config_registry::app_config;
use crate::error::Error;
use crate::http::body::BodyPolicy;
use crate::http::handler::{Erased, ErasedHandler};
use crate::http::middleware::{Middleware, Middlewares, Next};
use crate::http::response::{error_response, RenderOptions};
use crate::http::routing::{Matched, RouteDef, Routes};
use crate::http::{Request, Response};
use crate::paths;

/// 組み立てが終わったアプリ。
///
/// 中身は読むだけなので、クローンは安く、スレッド間で共有できます。
#[derive(Clone)]
pub struct Application {
    inner: Arc<Inner>,
}

struct Inner {
    routes: Routes,
    /// 共通ミドルウェアだけの並び。404 や静的ファイルにも通すので、ルートの外に持ちます。
    global: Arc<[Arc<dyn Middleware>]>,
    /// `public/` の場所。`Arc` なので、裏のスレッドへ渡すときに確保が起きません。
    public_dir: Arc<Path>,
    /// `public/` の**実体**。シンボリックリンクの確認に使います。
    ///
    /// 起動時に1回だけ解決します。以前はリクエストごとに `canonicalize` を叩いていました。
    ///
    /// 起動時に無かったときは `None` で、**ディスクの `public/` は探しません。**
    /// 以前は、当たらない GET のたびに裏のスレッドで `public/` を探し直していました。
    /// 起動後に `public/` を作ったときは、起動し直すと配信します。
    public_canonical: Option<Arc<Path>>,
    /// `/storage/...` で配信する置き場所（`storage/app/public/`）。
    storage_public_dir: Arc<Path>,
    /// その実体。`storage/` を作る前に起動したときは `None` になります。
    storage_public_canonical: Option<Arc<Path>>,
    debug: bool,
    /// 受け取る本文の上限の既定（`APP_MAX_BODY_BYTES`）。ルートの `.max_body(n)` が優先します。
    max_body: u64,
    exceptions: Option<Arc<Exceptions>>,
    /// ルートに当たらなかったときの最内側（静的ファイル → 404）。
    ///
    /// 起動時に 1 つ作って使い回します。以前は 404 のたびに `Arc` を作っていました。
    /// `Inner` を強く持つと自分を指す輪になって解放されないので、`Weak` で持ちます。
    not_found: Arc<dyn ErasedHandler>,
    /// 実際に登録した health のパス（`Routing::health`）。アクセスログで見分けます。
    health: Option<Arc<str>>,
}

/// アクセスログに出す材料。ルートを引いた後でないと作れないものを返します。
pub(crate) struct LogInfo {
    /// 伏せた後の行き先（`server::log_target`）。
    pub(crate) target: String,
    /// 当たったルートの名前。
    pub(crate) route: Option<Arc<str>>,
    /// health のパスに当たったか。200 ならアクセスとして出しません。
    pub(crate) health: bool,
}

/// エラーからレスポンスを作る関数。
type ExceptionHandler = Box<dyn Fn(&Error) -> Option<Response> + Send + Sync>;

/// エラーの見せ方の差し替え。
///
/// `with_exceptions` で登録した関数を順に試し、最初に `Some` を返したものを使います。
/// どれも返さなければ、bengara の既定の画面になります。
#[derive(Default)]
pub struct Exceptions {
    handlers: Vec<ExceptionHandler>,
}

impl Exceptions {
    /// エラーからレスポンスを作る関数を足す。
    ///
    /// ```ignore
    /// .with_exceptions(|e| e.render(|error| {
    ///     (error.status() == 404).then(|| Response::text("見つかりません").with_status(404))
    /// }))
    /// ```
    pub fn render(
        mut self,
        handler: impl Fn(&Error) -> Option<Response> + Send + Sync + 'static,
    ) -> Self {
        self.handlers.push(Box::new(handler));
        self
    }

    /// 登録した関数を順に試す。
    pub(crate) fn apply(&self, error: &Error) -> Option<Response> {
        self.handlers.iter().find_map(|h| h(error))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }
}

impl Application {
    /// 組み立てを始める。
    ///
    /// ```ignore
    /// Application::configure()
    ///     .with_routing(|r| r.web(crate::routes::web::routes).health("/up"))
    ///     .create()
    /// ```
    pub fn configure() -> ApplicationBuilder {
        ApplicationBuilder::default()
    }

    /// 名前付きルートを、どこからでも引けるようにする（起動時に1回）。
    pub(crate) fn install_names(&self) {
        crate::http::routing::install_names(self.inner.routes.names().clone());
    }

    /// 登録されているルートの一覧（`route:list` の表示用）。
    pub(crate) fn routes(&self) -> &[RouteDef] {
        self.inner.routes.all()
    }

    /// 1本のリクエストを処理する。ソケットを使わないので、テストからも呼べます。
    pub(crate) async fn handle(&self, req: Request) -> Response {
        self.handle_logged(req, false).await.0
    }

    /// 1本のリクエストを処理し、`log` が真ならアクセスログに出す材料も返す。
    ///
    /// 行き先はルートを引いた**直後**に作ります。パス引数の名前が分かってからでないと、
    /// `/reset/{token}` の値を伏せられないためです（`server::log_target`）。
    /// 共有の表には触らないので、ロックは増えません。
    ///
    /// **リクエスト ID はここで決めます**（本物の HTTP もテストも通る）。`server.rs` が受け取って
    /// よい値を載せていればそれを、無ければ作ります。処理全体を ID の範囲で包むので、
    /// 中で出たアプリのログに `req=` が付きます。応答には必ず `X-Request-Id` を付けます。
    pub(crate) async fn handle_logged(
        &self,
        mut req: Request,
        log: bool,
    ) -> (Response, Option<LogInfo>) {
        let id = match req.request_id_arc() {
            Some(id) => id,
            None => {
                let id = crate::logging::request_id::generate();
                req.set_request_id(id.clone());
                id
            }
        };
        let (response, info) =
            crate::logging::request_id::scope(id.clone(), self.handle_inner(req, log)).await;
        // ハンドラが付けていても上書きする（ログと同じ値にそろえる）。
        let response = response.with_header(crate::logging::request_id::HEADER, id.to_string());
        (response, info)
    }

    async fn handle_inner(&self, mut req: Request, log: bool) -> (Response, Option<LogInfo>) {
        let is_head = req.method() == "HEAD";
        let options = self.render_options(&req);

        // 共通ミドルウェアは、ルートに当たらなかったときも通します（Laravel と同じ）。
        // そのため、ここでは並びを1本だけ回し、分岐は最内側のハンドラに寄せます。
        let (inner, policy) = self.innermost(&mut req, &options);
        let info = log.then(|| LogInfo {
            target: crate::server::log_target(&req),
            route: req.route_name_arc(),
            health: req.route_matched()
                && self
                    .inner
                    .health
                    .as_deref()
                    .is_some_and(|health| req.route_path() == Some(health)),
        });

        // 本文はルートが決まってから受け取る。上限と読み方（全部・少しずつ）がルートごとだからです。
        // 受け取れなかったとき（413・400・408）は、ハンドラとルートのミドルウェアを通さず、
        // **共通のミドルウェアだけ**を通してそのエラーを返します（`rejected_body`）。
        // 以前は共通のミドルウェアも通さなかったので、CORS の見出しが付かず、
        // ブラウザの JavaScript から「大きすぎる」が読めませんでした。
        // 後ろの HEAD の扱いと本文を持てない状態の扱いは、ほかの応答と同じく通します。
        let response = match req.receive_body(policy).await {
            Err(error) => {
                req.mark_body_rejected();
                self.run_stack(rejected_body(error), req, options).await
            }
            Ok(()) => self.run_stack(inner, req, options).await,
        };

        let response = if is_head {
            // 本文を空にする**前**に、元の長さを Content-Length にしておく。
            // HEAD は「GET と同じヘッダーで、本文だけが無いもの」です。
            // 本文を持てない状態（1xx・204・304）は長さを付けてはいけないので、
            // そのときだけ付けません。
            //
            // すでに長さが付いているときは、そのまま残します。静的ファイルの配信は
            // HEAD のときファイルを読まずに長さだけを入れるので、ここで数え直すと
            // 0 で上書きしてしまいます。
            let response = if crate::http::response::body_forbidden(response.status())
                || response.header("content-length").is_some()
            {
                response
            } else {
                let length = response.body().len();
                response.with_header("content-length", length.to_string())
            };
            response.with_body(Vec::new())
        } else {
            response
        };

        // 本文を持てない状態（1xx・204・304）では、本文と種類・長さの申告を落とす。
        //
        // `error_response` は自分で落としますが、利用者が
        // `Response::new(204).with_body(..)` のように**自分で組んだ**応答はそのまま来ます。
        // 本文が付いた 204 は HTTP として正しくなく、枠組みの長さが相手と食い違います。
        // 全部の応答が通るここで落とします。
        (
            crate::http::response::without_forbidden_body(response),
            info,
        )
    }

    /// 共通の並び → ルートの並び → ハンドラを回して、応答にする。
    async fn run_stack(
        &self,
        inner: Arc<dyn ErasedHandler>,
        req: Request,
        options: RenderOptions,
    ) -> Response {
        let id = req.request_id_arc();
        let future = Next::new(self.inner.global.clone(), inner, options.clone()).run(req);
        // 停止のときに終わるのを待てるように数える（タスクが終わると数から引く）。
        let task = crate::server::track_request();
        // spawn すると task-local が切れるので、同じ ID で包み直す。
        let future = async move {
            let _task = task;
            match id {
                Some(id) => crate::logging::request_id::scope(id, future).await,
                None => future.await,
            }
        };

        // ミドルウェアとハンドラのパニックを1リクエストに閉じ込める。
        // 並びの一番外で包むので、共通ミドルウェアの中で起きたものも 500 になります。
        //
        // この形は意図したものです（決定記録 #008）。`catch_unwind` に替えると
        // `UnwindSafe` の扱いが広く波及するので、ここは変えません。
        //
        // クライアントが切断すると、axum 側の処理は落ちますが、ここで切り出したタスクは
        // 走り続けます（途中の `.await` で止めると、書きかけの処理が残るため）。
        // 停止のときは、このタスクも終わるまで待ちます（`APP_SHUTDOWN_TIMEOUT` まで。
        // `server::track_request`）。上限を過ぎたら、ランタイムごと打ち切られます。
        match tokio::spawn(future).await {
            // `Next::run` は必ず `Ok` を返します（エラーはその中でレスポンスに変わります）。
            // ここの `Err` には来ませんが、型を合わせるために同じ関数を通します。
            Ok(result) => crate::http::response::render(result, options),
            Err(join_error) => {
                if join_error.is_panic() {
                    crate::metrics::record_panic();
                }
                let error = Error::msg(panic_message(join_error, options.debug));
                tracing::error!("ハンドラがパニックしました: {error}");
                error_response(&error, options)
            }
        }
    }

    /// 最内側のハンドラと、本文の受け取り方を決める（ルート本体・405・静的ファイル・404）。
    ///
    /// パス引数・ルート名・登録パスは、ミドルウェアからも見えるように**ここで**載せます。
    /// ルートに当たらなかったときの本文は、既定の上限で全部読みます。
    fn innermost(
        &self,
        req: &mut Request,
        options: &RenderOptions,
    ) -> (Arc<dyn ErasedHandler>, BodyPolicy) {
        let default_policy = BodyPolicy {
            limit: self.inner.max_body,
            stream: false,
        };
        let handler = match self.inner.routes.find(req.method(), req.path()) {
            Matched::Found { def, path, params } => {
                let policy = BodyPolicy {
                    limit: def.max_body.unwrap_or(self.inner.max_body),
                    stream: def.stream_body,
                };
                let name = def.name.clone();
                req.set_params(params);
                req.set_route_name(name);
                // 登録パス（`/posts/{id}` の形）。回数の制限が鍵にします。
                // 届いたパスは `/login/` のように相手が形を変えられるので、鍵にできません。
                req.set_route_path(Some(path));
                req.mark_matched();

                // ルートごとの並びが無ければ、ハンドラをそのまま返す。
                // 包み直すと `Arc` を1つ作って捨てるだけなので、何もしない。
                let Some(stack) = def.stack.clone() else {
                    return (def.handler.clone(), policy);
                };

                let handler = def.handler.clone();
                let options = options.clone();
                let wrapped: Arc<dyn ErasedHandler> = Arc::new(Erased::new(move |req: Request| {
                    // ルートごとの並びは、共通の並びの内側で回す。
                    // `Fn` なので捕まえたものは動かせない。どれも `Arc` のクローンで済む。
                    let next = Next::new(stack.clone(), handler.clone(), options.clone());
                    async move { next.run(req).await }
                }));
                return (wrapped, policy);
            }
            Matched::MethodNotAllowed { mut allowed } => {
                // HEAD は GET で受けると決めているので、GET があれば HEAD も並べる。
                if allowed.contains(&"GET") && !allowed.contains(&"HEAD") {
                    allowed.push("HEAD");
                }
                allowed.sort_unstable();
                // パスが同じでもメソッドが違うときは、先に静的ファイルを探す。
                // `Route::post("/report", ...)` があると `public/report` が
                // `GET` で見えなくなるためです。`GET` のルートがあるときは触りません。
                let try_static =
                    matches!(req.method(), "GET" | "HEAD") && !allowed.contains(&"GET");
                let allow = allowed.join(", ");
                let app = self.clone();
                let options = options.clone();
                Arc::new(Erased::new(move |req: Request| {
                    let allow = allow.clone();
                    let app = app.clone();
                    let options = options.clone();
                    async move {
                        if try_static {
                            if let Some(response) = app.serve_static(&req).await {
                                return Ok(response);
                            }
                        }
                        let error = Error::Http {
                            status: 405,
                            message: String::new(),
                        };
                        Ok(error_response(&error, options).with_header("allow", allow))
                    }
                }))
            }
            // 起動時に作ったものをそのまま返す。描画の指定はリクエストから作り直せる。
            Matched::NotFound => self.inner.not_found.clone(),
        };
        (handler, default_policy)
    }

    /// 描画の指定（`APP_DEBUG` と、JSON で返すか）をリクエストから作る。
    fn render_options(&self, req: &Request) -> RenderOptions {
        RenderOptions::new(self.inner.debug, wants_json(req))
            .with_exceptions(self.inner.exceptions.clone())
    }

    async fn serve_static_or_404(&self, req: &Request) -> Response {
        if let Some(response) = self.serve_static(req).await {
            return response;
        }
        error_response(
            &Error::Http {
                status: 404,
                message: String::new(),
            },
            self.render_options(req),
        )
    }

    /// 静的ファイルを探して返す。無ければ `None`。
    ///
    /// `GET` と `HEAD` のときだけ探します。入口はパスで決まります
    /// （`/storage/...` なら置き場所だけ、それ以外は埋め込み → ディスク）。
    async fn serve_static(&self, req: &Request) -> Option<Response> {
        if matches!(req.method(), "GET" | "HEAD") {
            // 振り分けの前に1回だけ正規化する。続いた `/` をまとめ、`%xx` を戻します。
            // 以前は生のパスで `/storage/` を判定していたので、`//storage/x.css` と
            // `/%73torage/x.css` が `public/` 側の枝に入っていました。
            let path = crate::http::statics::normalize_path(req.path());
            // 条件つきの取得（`If-None-Match` など）と範囲（`Range`）の見出し。
            let conditions = crate::http::statics::Conditions::from_request(req);
            // HEAD は本文を返さないので、ファイルを読まずに長さだけ知らせる。
            let want_body = req.method() != "HEAD";

            // 1. `/storage/...` は storage/app/public/ から。
            if crate::http::statics::is_storage_path(&path) {
                let dir = self.inner.storage_public_dir.clone();
                let root = self.inner.storage_public_canonical.clone();
                let found = tokio::task::spawn_blocking(move || {
                    crate::http::statics::serve_storage(
                        &dir,
                        root.as_deref(),
                        &path,
                        &conditions,
                        want_body,
                    )
                })
                .await
                .unwrap_or(None);
                if found.is_some() {
                    return found;
                }
            } else {
                // 2. バイナリに埋め込んだ public/（リリースビルドのとき）。
                //    メモリから読むだけなので、裏のスレッドに回す必要はない。
                if let Some(response) = crate::http::statics::serve_embedded(
                    crate::http::statics::embedded(),
                    &path,
                    &conditions,
                ) {
                    return Some(response);
                }
                // 3. ディスクの public/。`public/storage/` の中は返さない（`serve_public`）。
                //    起動時に public/ が無かったなら探さない（裏のスレッドにも回さない）。
                let dir = self.inner.public_dir.clone();
                let root = self.inner.public_canonical.clone()?;
                let found = tokio::task::spawn_blocking(move || {
                    crate::http::statics::serve_public(
                        &dir,
                        Some(&*root),
                        &path,
                        &conditions,
                        want_body,
                    )
                })
                .await
                .unwrap_or(None);
                if found.is_some() {
                    return found;
                }
            }
        }
        None
    }
}

/// 本文を受け取れなかった要求の、最内側のハンドラ。受け取りのエラーをそのまま返します。
///
/// 共通のミドルウェアの内側に置き、ハンドラとルートのミドルウェアの代わりにします。
///
/// | 通るもの | 決めたこと |
/// |---|---|
/// | CORS などの共通のミドルウェア | 通す。断った応答にも見出しが付く |
/// | `VerifyCsrfToken` | 確かめずに通す（`Request::body_rejected`）。中で返るのはこのエラーだけ |
/// | ルートのミドルウェア・ハンドラ | 通さない。本文の無い要求を渡さない |
/// | `with_exceptions`・`APP_DEBUG` | ほかのエラーと同じく効く |
fn rejected_body(error: Error) -> Arc<dyn ErasedHandler> {
    // 受け取りのエラーは状態コードと短い文だけ（`Error::Http`）。写せる形にして持つ。
    let status = error.status();
    let message = match error {
        Error::Http { message, .. } => message,
        other => other.to_string(),
    };
    Arc::new(Erased::new(move |_req: Request| {
        let error = Error::Http {
            status,
            message: message.clone(),
        };
        async move { Err::<Response, Error>(error) }
    }))
}

/// クライアントが JSON を欲しがっているか。
///
/// `Accept` に `application/json` があるか、`X-Requested-With: XMLHttpRequest` が付いているか、
/// 自分が JSON を送ってきたか、のどれかで判断します。Laravel の `expectsJson()` と同じ考え方です。
fn wants_json(req: &Request) -> bool {
    wants_json_with(|name| req.header(name))
}

/// クライアントが JSON を欲しがっているか（ヘッダーを引く関数から判断する）。
///
/// `Request` を組み立てる**前**（本文が大きすぎる・読めないとき）からも同じ判断を
/// 使いたいので、ヘッダーの引き方だけを受け取る形にしてあります。
/// `header` に渡す名前は小文字です。
pub(crate) fn wants_json_with<'a>(header: impl Fn(&str) -> Option<&'a str>) -> bool {
    if let Some(accept) = header("accept") {
        if contains_ignore_case(accept, "application/json") || contains_ignore_case(accept, "+json")
        {
            return true;
        }
        // ブラウザは text/html を先に書く。それが無く */* だけなら API 呼び出しとみなす。
        if !contains_ignore_case(accept, "text/html") && accept.contains("*/*") {
            return true;
        }
    }
    if header("x-requested-with").is_some_and(|v| v.eq_ignore_ascii_case("XMLHttpRequest")) {
        return true;
    }
    // 自分が JSON を送ってきたか。判定は `Request` と同じ関数を使う（食い違いを作らない）。
    header("content-type").is_some_and(crate::http::request::json_content_type)
}

/// 大文字小文字を区別せずに部分一致を見る。
///
/// `Accept` を小文字に直してから探すと、1リクエストで `String` を作ってしまいます。
/// 探す語はどれも ASCII なので、バイト列のまま見れば確保が要りません。
fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    let needle = needle.as_bytes();
    if needle.is_empty() {
        return true;
    }
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

/// パニックしたハンドラの、見せる文を決める。
///
/// `panic!("...")` に書かれた文は `debug` のときだけ添えます。
/// 本番で出すと、内部の事情をそのまま外へ見せてしまいます。
fn panic_message(join_error: tokio::task::JoinError, debug: bool) -> String {
    if join_error.is_cancelled() {
        return "処理が打ち切られました".to_string();
    }
    let base = "ハンドラの中でパニックが起きました";
    if !debug || !join_error.is_panic() {
        return base.to_string();
    }
    let payload = join_error.into_panic();
    let detail = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned());
    match detail {
        Some(detail) => format!("{base}: {detail}"),
        None => base.to_string(),
    }
}

/// `Application` の組み立て。
#[derive(Default)]
pub struct ApplicationBuilder {
    routing: Routing,
    middleware: Middlewares,
    exceptions: Exceptions,
    events: crate::events::Events,
}

impl ApplicationBuilder {
    /// エラーの見せ方を差し替える。
    ///
    /// 登録した関数を上から順に試し、最初に応答を返したものを使います。
    /// どれも返さなければ bengara の既定の形になります。
    ///
    /// ```ignore
    /// .with_exceptions(|e| e.render(|error| {
    ///     (error.status() == 404).then(|| Response::text("ありません").with_status(404))
    /// }))
    /// ```
    pub fn with_exceptions(mut self, configure: impl FnOnce(Exceptions) -> Exceptions) -> Self {
        self.exceptions = configure(std::mem::take(&mut self.exceptions));
        self
    }

    /// ミドルウェアを登録する。
    pub fn with_middleware(mut self, configure: impl FnOnce(Middlewares) -> Middlewares) -> Self {
        self.middleware = configure(std::mem::take(&mut self.middleware));
        self
    }

    /// ルートを決める。
    pub fn with_routing(mut self, configure: impl FnOnce(Routing) -> Routing) -> Self {
        self.routing = configure(std::mem::take(&mut self.routing));
        self
    }

    /// 出来事（イベント）に対して、聞く側を登録する。
    ///
    /// ```ignore
    /// .with_events(|e| {
    ///     e.listen("user.registered", SendWelcome::handle)
    ///         .listen("user.registered", NotifyAdmin::handle)
    /// })
    /// ```
    ///
    /// 起動時に固定されます。実行中には増えません。
    pub fn with_events(
        mut self,
        configure: impl FnOnce(crate::events::Events) -> crate::events::Events,
    ) -> Self {
        self.events = configure(std::mem::take(&mut self.events));
        self
    }

    /// 組み立てを終えて `Application` にする。
    ///
    /// # パニック
    ///
    /// 次のときに、ここでパニックします。起動時に気づけるようにするためです。
    ///
    /// - 同じメソッドとパスのルートが2本ある
    /// - ルート名が重なっている
    /// - 登録していないミドルウェアの名前が使われている
    pub fn create(self) -> Application {
        let mut defs = self.routing.defs;
        let mut health_path: Option<Arc<str>> = None;
        if let Some(path) = self.routing.health {
            // `path` は `Routing::health` が登録と同じ形にそろえてある（`register_path`）。
            // そろえずに比べると重なりに気づけず、`Routes::build` でパニックしていた。
            let already = defs.iter().any(|d| d.path == path && d.method == "GET");
            if already {
                tracing::warn!("{path} は routes/*.rs にもあるため、health の登録を飛ばします");
            } else {
                defs.extend(crate::http::routing::collect(|| {
                    crate::http::Route::get(&path, health);
                }));
                health_path = Some(Arc::from(path.as_str()));
            }
        }
        // 名前を実体に置き換え、ルートごとに1本の並びにして固定する。
        // ここで済ませておけば、リクエスト処理中は読むだけで済む。
        // 共通のものは別に1本持つので、ルートの並びには入れない（二重に通さない）。
        //
        // **`with_middleware` を一度も書いていないときも必ず通します。**
        // 以前は「1つも登録されていなければ組み立てを省く」としていましたが、
        // 未登録の名前に気づく仕組み（`aliased_stack` のパニック）もそこにあるため、
        // `.middleware("admin")` が無言で素通しになっていました。
        for def in &mut defs {
            if def.middleware.is_empty() {
                continue;
            }
            let stack = self.middleware.aliased_stack(&def.middleware);
            if !stack.is_empty() {
                def.stack = Some(stack);
            }
        }
        let global = self.middleware.global_stack();

        // 聞く側を固定する。以後は読むだけ。
        crate::events::install(self.events);

        // 置き場所と、その実体を起動時に1回だけ決める。以後は読むだけ。
        let public_dir: Arc<Path> = Arc::from(paths::app_path("public"));
        let public_canonical = public_dir.canonicalize().ok().map(Arc::from);
        let storage_public_dir: Arc<Path> = Arc::from(crate::storage::Storage::public_root());
        let storage_public_canonical = storage_public_dir.canonicalize().ok().map(Arc::from);

        let exceptions = (!self.exceptions.is_empty()).then(|| Arc::new(self.exceptions));
        Application {
            inner: Arc::new_cyclic(|weak: &std::sync::Weak<Inner>| {
                let weak = weak.clone();
                let not_found: Arc<dyn ErasedHandler> =
                    Arc::new(Erased::new(move |req: Request| {
                        let weak = weak.clone();
                        async move {
                            match weak.upgrade() {
                                Some(inner) => {
                                    Ok(Application { inner }.serve_static_or_404(&req).await)
                                }
                                // アプリが先に落ちたあとに届いた分。実際には起きない。
                                None => Ok(Response::new(404)),
                            }
                        }
                    }));
                Inner {
                    routes: Routes::build(defs),
                    global,
                    public_dir,
                    public_canonical,
                    storage_public_dir,
                    storage_public_canonical,
                    debug: app_config().debug,
                    max_body: crate::http::body::max_body_from_env(),
                    exceptions,
                    not_found,
                    health: health_path,
                }
            }),
        }
    }
}

/// `health("/up")` が返す内容。
///
/// 出すのは `status` と `name` だけです。`env`（`production` など）は外から
/// 誰でも見られる場所に出す理由が無いので入れません。
async fn health() -> crate::error::Result<Response> {
    let config = app_config();
    Response::json(&serde_json::json!({
        "status": "ok",
        "name": config.name,
    }))
}

/// ルートの決め方。
#[derive(Default)]
pub struct Routing {
    defs: Vec<RouteDef>,
    health: Option<String>,
}

impl Routing {
    /// `routes/web.rs` のルート定義を取り込む。
    ///
    /// ```ignore
    /// r.web(crate::routes::web::routes)
    /// ```
    pub fn web(mut self, define: impl FnOnce()) -> Self {
        self.defs.extend(crate::http::routing::collect(define));
        self
    }

    /// 生存確認用のルートを足す（Laravel の `/up` に当たります）。
    ///
    /// ここで**登録と同じ形**にそろえて持ちます（`register_path`）。`create()` は
    /// この値を `routes/*.rs` のパスと突き合わせて、重なっていれば飛ばします。
    /// 以前は渡された生の文字列のまま持っていたので、`.health("up")` と
    /// `Route::get("/up", ..)` が重なっていることに気づけず、起動時にパニックしました。
    pub fn health(mut self, path: &str) -> Self {
        self.health = Some(crate::http::routing::register_path(path));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Result as AppResult;
    use crate::http::{Next, Route};

    async fn ok() -> AppResult<Response> {
        Ok(Response::text("ok"))
    }

    /// 共通ミドルウェアの見本（Laravel の AddPoweredBy に当たる形）。
    async fn powered_by(req: Request, next: Next) -> AppResult<Response> {
        let response = next.run(req).await?;
        Ok(response
            .with_header("x-powered-by", "bengara")
            .with_added_header("x-mark", "global"))
    }

    /// ルートに付けるミドルウェアの見本。
    async fn marker(req: Request, next: Next) -> AppResult<Response> {
        let response = next.run(req).await?;
        Ok(response.with_added_header("x-mark", "route"))
    }

    fn app() -> Application {
        Application::configure()
            .with_middleware(|m| m.append(powered_by).alias("marked", marker))
            .with_routing(|r| {
                r.web(|| {
                    Route::get("/", ok);
                    Route::post("/posts", ok);
                    Route::get("/admin", ok).middleware("marked");
                })
            })
            .create()
    }

    fn marks(response: &Response) -> Vec<&str> {
        response
            .headers()
            .iter()
            .filter(|(k, _)| k == "x-mark")
            .map(|(_, v)| v.as_str())
            .collect()
    }

    #[tokio::test]
    async fn 共通ミドルウェアは404にも通る() {
        let response = app().handle(Request::new("GET", "/missing")).await;
        assert_eq!(response.status(), 404);
        assert_eq!(response.header("x-powered-by"), Some("bengara"));
    }

    #[tokio::test]
    async fn 共通ミドルウェアは405にも通る() {
        let response = app().handle(Request::new("DELETE", "/")).await;
        assert_eq!(response.status(), 405);
        assert_eq!(response.header("x-powered-by"), Some("bengara"));
        // HEAD は GET で受けるので、GET があれば HEAD も並べる。
        assert_eq!(response.header("allow"), Some("GET, HEAD"));
    }

    #[tokio::test]
    async fn アクセスログの行き先はパス引数の秘密を伏せる() {
        let app = Application::configure()
            .with_routing(|r| {
                r.web(|| {
                    Route::get("/reset/{token}", ok);
                })
            })
            .create();

        let req = Request::new("GET", "/reset/abc123").with_query("email=a%40b&signature=x");
        let (response, info) = app.handle_logged(req, true).await;
        assert_eq!(response.status(), 200);
        assert_eq!(
            info.map(|info| info.target).as_deref(),
            Some("/reset/***?email=a%40b&signature=***")
        );

        // 当たらなかったパスは、届いたパスのまま（クエリだけ伏せる）。
        let req = Request::new("GET", "/missing/abc").with_query("token=x");
        let (_, info) = app.handle_logged(req, true).await;
        assert_eq!(
            info.map(|info| info.target).as_deref(),
            Some("/missing/abc?token=***")
        );

        // ログを出さないときは作らない。
        let (_, info) = app
            .handle_logged(Request::new("GET", "/reset/abc123"), false)
            .await;
        assert!(info.is_none());
    }

    #[tokio::test]
    async fn healthのパスとルート名をログの材料に返す() {
        let app = Application::configure()
            .with_routing(|r| {
                r.web(|| {
                    Route::get("/items", ok).name("items.index");
                })
                .health("/up")
            })
            .create();

        let (response, info) = app.handle_logged(Request::new("GET", "/up"), true).await;
        assert_eq!(response.status(), 200);
        let info = info.unwrap();
        assert!(info.health);
        assert_eq!(info.route, None);

        let (_, info) = app.handle_logged(Request::new("GET", "/items"), true).await;
        let info = info.unwrap();
        assert!(!info.health);
        assert_eq!(info.route.as_deref(), Some("items.index"));

        // 当たらなかったものは health ではない。
        let (_, info) = app.handle_logged(Request::new("GET", "/upx"), true).await;
        assert!(!info.unwrap().health);
    }

    #[tokio::test]
    async fn healthを飛ばしたときは利用者のルートをhealthと見ない() {
        let app = Application::configure()
            .with_routing(|r| {
                r.web(|| {
                    Route::get("/up", ok);
                })
                .health("/up")
            })
            .create();
        let (_, info) = app.handle_logged(Request::new("GET", "/up"), true).await;
        assert!(!info.unwrap().health);
    }

    #[tokio::test]
    async fn どの応答にもリクエストidが付く() {
        async fn boom() -> crate::error::Result<Response> {
            panic!("わざと");
        }
        async fn own_id() -> crate::error::Result<Response> {
            Ok(Response::text("x").with_header("x-request-id", "handler"))
        }
        let app = Application::configure()
            .with_middleware(|m| m.append(Csrf))
            .with_routing(|r| {
                r.web(|| {
                    Route::get("/", ok);
                    Route::get("/boom", boom);
                    Route::get("/own", own_id);
                    Route::post("/small", ok).max_body(4);
                    Route::post("/guarded", ok);
                })
            })
            .create();

        let cases = [
            Request::new("GET", "/"),
            Request::new("GET", "/missing"),
            Request::new("DELETE", "/"),
            Request::new("GET", "/boom"),
            Request::new("GET", "/own"),
            Request::new("POST", "/small").with_pending_body(
                crate::http::body::PendingBody::memory(b"too large body".to_vec()),
            ),
            Request::new("POST", "/guarded"),
        ];
        let mut seen = std::collections::HashSet::new();
        for req in cases {
            let label = format!("{} {}", req.method(), req.path());
            let response = app.handle(req).await;
            let id = response
                .header("x-request-id")
                .unwrap_or_else(|| panic!("{label}: {}", response.status()))
                .to_string();
            assert_eq!(id.len(), 32, "{label}: {id}");
            assert!(seen.insert(id), "{label}");
        }
    }

    /// 書き込みの要求を 419 で止める（テスト用の CSRF の代わり）。
    struct Csrf;

    impl Middleware for Csrf {
        fn handle(&self, req: Request, next: Next) -> crate::http::BoxFuture {
            Box::pin(async move {
                if req.method() == "POST" && req.path() == "/guarded" {
                    return Err(Error::http(419, ""));
                }
                next.run(req).await
            })
        }
    }

    #[tokio::test]
    async fn 受け取ったidはそのまま返す() {
        let app = Application::configure()
            .with_routing(|r| {
                r.web(|| {
                    Route::get("/", ok);
                })
            })
            .create();
        let mut req = Request::new("GET", "/");
        req.set_request_id(Arc::from("from-proxy.1"));
        let response = app.handle(req).await;
        assert_eq!(response.header("x-request-id"), Some("from-proxy.1"));
    }

    #[tokio::test]
    async fn ハンドラの中のログにidが付きspawnの後でも同じ() {
        use crate::logging::{layer::LogLayer, stdout::Stdout, Outputs};
        use tracing_subscriber::prelude::*;

        async fn warns(req: Request) -> crate::error::Result<Response> {
            tracing::warn!("ハンドラの中");
            Ok(Response::text(req.request_id().to_string()))
        }
        let app = Application::configure()
            .with_routing(|r| {
                r.web(|| {
                    Route::get("/", warns);
                })
            })
            .create();

        let buffer = Arc::new(std::sync::Mutex::new(Vec::new()));
        let outputs = Arc::new(Outputs::new(
            Stdout::capture(buffer.clone()),
            false,
            "testing".into(),
        ));
        let subscriber = tracing_subscriber::registry().with(LogLayer::new(outputs));
        // `#[tokio::test]` は 1 スレッドなので、spawn した先も同じ購読者を使う。
        let _guard = tracing::subscriber::set_default(subscriber);
        let response = app.handle(Request::new("GET", "/")).await;
        let id = response.header("x-request-id").unwrap().to_string();
        assert_eq!(std::str::from_utf8(response.body()).unwrap(), id);
        let text = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
        assert!(text.contains(&format!("ハンドラの中 req={id}")), "{text}");
    }

    #[tokio::test]
    async fn メソッドが違うだけなら静的ファイルを探す() {
        // `Route::post("/report", ...)` があると、以前は `public/report` が
        // `GET` で見えずに 405 だけを返していた。
        static FILES: &[(&str, &[u8])] = &[("report", b"report!")];
        crate::http::statics::install_embedded(FILES);

        let app = Application::configure()
            .with_routing(|r| {
                r.web(|| {
                    Route::post("/report", ok);
                })
            })
            .create();

        let response = app.handle(Request::new("GET", "/report")).await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.body(), b"report!");

        // 静的ファイルが無いパスは、これまでどおり 405。
        let response = app.handle(Request::new("GET", "/missing")).await;
        assert_eq!(response.status(), 404);
        let response = app.handle(Request::new("DELETE", "/report")).await;
        assert_eq!(response.status(), 405);
        assert_eq!(response.header("allow"), Some("POST"));
    }

    /// 接続元のアドレスを載せたリクエスト（回数の制限は、これが無いと数えない）。
    fn from_peer(method: &str, path: &str) -> Request {
        let addr: std::net::SocketAddr = "203.0.113.9:12345".parse().expect("アドレスの形");
        Request::new(method, path).with_remote_addr(addr)
    }

    /// 1 回だけ通す回数の制限を、名前の無いルートに付けたアプリ。
    fn throttled_app() -> Application {
        Application::configure()
            .with_middleware(|m| {
                m.alias(
                    "throttle",
                    crate::http::Throttle::new(1, std::time::Duration::from_secs(60)),
                )
            })
            .with_routing(|r| {
                r.web(|| {
                    Route::post("/login", ok).middleware("throttle");
                    Route::get("/items/{id}", ok).middleware("throttle");
                    Route::get("/あ", ok).middleware("throttle");
                    Route::get("/other", ok).middleware("throttle");
                })
            })
            .create()
    }

    #[tokio::test]
    async fn 名前の無いルートは末尾のスラッシュで回数の制限を回せない() {
        // 以前は届いたパスを鍵にしていたので、`/login` `/login/` `/login//` が
        // 別々の枠になり、`/` を足すだけで好きなだけ試せた（どれも同じハンドラに届く）。
        let app = throttled_app();
        let first = app.handle(from_peer("POST", "/login")).await;
        assert_eq!(first.status(), 200);
        for path in ["/login/", "/login//", "/login///", "/login"] {
            let response = app.handle(from_peer("POST", path)).await;
            assert_eq!(response.status(), 429, "{path} は同じ枠で数える");
        }
        // 別のルートは巻き込まない。
        let other = app.handle(from_peer("GET", "/other")).await;
        assert_eq!(other.status(), 200);
    }

    #[tokio::test]
    async fn 名前の無いルートはパス引数を変えても回数の制限を回せない() {
        // 以前は `/items/1` と `/items/2` が別々の枠だった（名前を付けたときだけ防げた）。
        let app = throttled_app();
        assert_eq!(app.handle(from_peer("GET", "/items/1")).await.status(), 200);
        for path in ["/items/2", "/items/20000", "/items/1/"] {
            let response = app.handle(from_peer("GET", path)).await;
            assert_eq!(response.status(), 429, "{path} は同じ枠で数える");
        }
    }

    #[tokio::test]
    async fn 名前の無いルートは十六進の大文字小文字で回数の制限を回せない() {
        // `%E3` と `%e3` は同じルートに当たる。鍵も同じでないと枠が 2 倍になる。
        let app = throttled_app();
        let first = app.handle(from_peer("GET", "/%E3%81%82")).await;
        assert_eq!(first.status(), 200);
        for path in ["/%e3%81%82", "/%E3%81%82/", "/%e3%81%82//"] {
            let response = app.handle(from_peer("GET", path)).await;
            assert_eq!(response.status(), 429, "{path} は同じ枠で数える");
        }
    }

    #[tokio::test]
    async fn 当たったルートの登録パスをミドルウェアから読める() {
        // 共通のミドルウェアからも見えること（照合は並びを回す前に済ませる）。
        async fn show_route_path(req: Request, next: Next) -> AppResult<Response> {
            let path = req.route_path().unwrap_or("(none)").to_string();
            let response = next.run(req).await?;
            Ok(response.with_header("x-route-path", path))
        }
        let app = Application::configure()
            .with_middleware(|m| m.append(show_route_path))
            .with_routing(|r| {
                r.web(|| {
                    Route::get("/posts/{id}", ok);
                })
            })
            .create();
        let response = app.handle(Request::new("GET", "/posts/12/")).await;
        assert_eq!(response.header("x-route-path"), Some("/posts/{id}"));
        // 当たらないとき（404・405）は載せない。
        let response = app.handle(Request::new("GET", "/missing")).await;
        assert_eq!(response.header("x-route-path"), Some("(none)"));
        let response = app.handle(Request::new("POST", "/posts/12")).await;
        assert_eq!(response.status(), 405);
        assert_eq!(response.header("x-route-path"), Some("(none)"));
    }

    #[tokio::test]
    async fn healthが重なっていても起動できる() {
        // `.health("up")` と `Route::get("/up", ..)` の両方があるとき、以前は
        // 生の文字列と登録後のパスを比べていたので重なりに気づけず、
        // `Routes::build` が「同じパスが2回」で**起動時にパニック**していた。
        for given in ["up", "/up", "/up/"] {
            let app = Application::configure()
                .with_routing(|r| {
                    r.web(|| {
                        Route::get("/up", ok);
                    })
                    .health(given)
                })
                .create();
            // 飛ばした結果、`routes/web.rs` のほうが残る。
            assert_eq!(app.routes().len(), 1, "{given}");
            let response = app.handle(Request::new("GET", "/up")).await;
            assert_eq!(response.status(), 200, "{given}");
            assert_eq!(response.body(), b"ok", "{given}");
        }
    }

    #[tokio::test]
    async fn 小文字のメソッドは405になる() {
        // HTTP のメソッドは大文字小文字を区別する。以前は大文字にそろえていたので、
        // `get /` がハンドラまで通っていた。
        let response = app().handle(Request::new("get", "/")).await;
        assert_eq!(response.status(), 405);
        assert_eq!(response.header("allow"), Some("GET, HEAD"));
    }

    #[test]
    fn 大文字のacceptでもjsonを欲しがっていると分かる() {
        // `Accept` を小文字に直さずに見るので、大文字でも効くことを確かめる。
        fn accept(value: &'static str) -> impl Fn(&str) -> Option<&'static str> {
            move |want: &str| (want == "accept").then_some(value)
        }
        assert!(wants_json_with(accept("APPLICATION/JSON")));
        assert!(wants_json_with(accept("application/VND.api+JSON")));
        // ブラウザの Accept は JSON ではない。
        assert!(!wants_json_with(accept(
            "TEXT/HTML,application/xhtml+xml,*/*;q=0.8"
        )));
        // text/html が無く */* だけなら API 呼び出しとみなす。
        assert!(wants_json_with(accept("*/*")));
    }

    #[tokio::test]
    async fn 共通が外側でルートのものが内側() {
        let response = app().handle(Request::new("GET", "/admin")).await;
        assert_eq!(response.status(), 200);
        // 戻りは内側から順に書き足されるので、ルートのものが先に並ぶ。
        assert_eq!(marks(&response), vec!["route", "global"]);
    }

    #[tokio::test]
    async fn 共通は二重に通らない() {
        let response = app().handle(Request::new("GET", "/")).await;
        assert_eq!(response.status(), 200);
        assert_eq!(marks(&response), vec!["global"]);
    }

    #[test]
    #[should_panic(expected = "ミドルウェア `missing` は登録されていません")]
    fn with_middlewareを書かなくても未登録の名前で止まる() {
        // 以前は「1つも登録されていなければ検査を省く」形だったので、
        // init が作る bootstrap/app.rs（with_middleware なし）のままだと
        // `.middleware("missing")` が無言で素通しになっていた。
        Application::configure()
            .with_routing(|r| {
                r.web(|| {
                    Route::get("/", ok).middleware("missing");
                })
            })
            .create();
    }

    #[tokio::test]
    async fn ミドルウェアを使わないルートはそのまま動く() {
        // 検査を必ず通す形にしても、名前を付けていないルートには何も掛からない。
        let app = Application::configure()
            .with_routing(|r| {
                r.web(|| {
                    Route::get("/", ok);
                })
            })
            .create();
        let response = app.handle(Request::new("GET", "/")).await;
        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn headは本文を外してもcontent_lengthを残す() {
        let response = app().handle(Request::new("HEAD", "/")).await;
        assert_eq!(response.status(), 200);
        assert!(response.body().is_empty(), "本文は返さない");
        assert_eq!(
            response.header("content-length"),
            Some("2"),
            "`ok` の 2 バイト"
        );
    }

    #[tokio::test]
    async fn 自分で組んだ204の本文も落とす() {
        // `error_response` を通らない道でも落とすこと。
        // 本文が付いた 204 は HTTP として正しくなく、枠組みの長さが食い違う。
        async fn no_content() -> AppResult<Response> {
            Ok(Response::new(204)
                .with_header("content-type", "text/plain")
                .with_body("消えるはず"))
        }

        let app = Application::configure()
            .with_routing(|r| {
                r.web(|| {
                    Route::get("/", no_content);
                })
            })
            .create();

        for method in ["GET", "HEAD"] {
            let response = app.handle(Request::new(method, "/")).await;
            assert_eq!(response.status(), 204, "{method}");
            assert!(response.body().is_empty(), "{method}: 本文は付けない");
            assert_eq!(response.header("content-type"), None, "{method}");
            assert_eq!(response.header("content-length"), None, "{method}");
        }
    }

    // ---- 本文の受け取り（上限・少しずつ読む） ----

    /// まだ読んでいない本文を載せた POST（サーバーとテストのクライアントと同じ形）。
    fn post_body(path: &str, body: &[u8]) -> Request {
        Request::new("POST", path)
            .with_pending_body(crate::http::body::PendingBody::memory(body.to_vec()))
    }

    /// 受け取った本文の長さを返す。
    async fn body_length(req: Request) -> AppResult<Response> {
        Ok(Response::text(req.body().len().to_string()))
    }

    /// 本文を少しずつ読み、合計と `body()` の長さを返す。
    async fn streamed_length(req: Request) -> AppResult<Response> {
        let buffered = req.body().len();
        let mut body = req.body_stream()?;
        let mut total = 0;
        while let Some(chunk) = body.chunk().await? {
            total += chunk.len();
        }
        Ok(Response::text(format!("{total}/{buffered}")))
    }

    fn body_app() -> Application {
        Application::configure()
            .with_routing(|r| {
                r.web(|| {
                    Route::post("/small", body_length).max_body(4);
                    Route::post("/default", body_length);
                    Route::post("/stream", streamed_length)
                        .stream_body()
                        .max_body(8);
                    Route::post("/twice", |req: Request| async move {
                        req.body_stream()?;
                        req.body_stream()?;
                        Ok(Response::text("届かない"))
                    })
                    .stream_body();
                })
            })
            .create()
    }

    #[tokio::test]
    async fn ルートの上限ちょうどは通り_超えたら413() {
        let app = body_app();
        let ok = app.handle(post_body("/small", b"1234")).await;
        assert_eq!(ok.status(), 200);
        assert_eq!(ok.body_text(), "4");

        let over = app.handle(post_body("/small", b"12345")).await;
        assert_eq!(over.status(), 413);
    }

    #[tokio::test]
    async fn 上限を決めないルートは既定の上限() {
        let app = body_app();
        let ok = app.handle(post_body("/default", &[b'a'; 1024])).await;
        assert_eq!(ok.body_text(), "1024");

        let limit = usize::try_from(crate::http::body::DEFAULT_MAX_BODY_BYTES).unwrap();
        let over = app
            .handle(post_body("/default", &vec![b'a'; limit + 1]))
            .await;
        assert_eq!(over.status(), 413);
    }

    #[tokio::test]
    async fn 申告が上限を超えていれば読まずに413() {
        // 中身は 1 バイトでも、申告が上限を超えていれば断る。
        let req = post_body("/small", b"1")
            .with_headers(vec![("content-length".to_string(), "100".to_string())]);
        assert_eq!(body_app().handle(req).await.status(), 413);
    }

    #[tokio::test]
    async fn 本文のエラーはjsonを欲しがる相手にjsonで返す() {
        let req = post_body("/small", b"12345")
            .with_headers(vec![("accept".to_string(), "application/json".to_string())]);
        let response = body_app().handle(req).await;
        assert_eq!(response.status(), 413);
        assert_eq!(response.header("content-type"), Some("application/json"));
    }

    #[tokio::test]
    async fn headで本文のエラーになっても本文は返さない() {
        // 申告が上限を超えた HEAD。413 でも、HEAD なので本文は付けない（長さは残す）。
        let req = Request::new("HEAD", "/small")
            .with_pending_body(crate::http::body::PendingBody::memory(Vec::new()))
            .with_headers(vec![("content-length".to_string(), "100".to_string())]);
        let app = Application::configure()
            .with_routing(|r| {
                r.web(|| {
                    Route::get("/small", body_length).max_body(4);
                })
            })
            .create();
        let response = app.handle(req).await;
        assert_eq!(response.status(), 413);
        assert!(response.body().is_empty(), "HEAD に本文は付けない");
        assert!(response.header("content-length").is_some());
    }

    #[tokio::test]
    async fn stream_bodyのルートは少しずつ読めて_bodyは空() {
        let response = body_app().handle(post_body("/stream", b"12345678")).await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.body_text(), "8/0");
    }

    #[tokio::test]
    async fn 少しずつ読んでも上限を超えたら413() {
        let response = body_app().handle(post_body("/stream", b"123456789")).await;
        assert_eq!(response.status(), 413);
    }

    #[tokio::test]
    async fn stream_bodyの無いルートでbody_streamを呼ぶと500() {
        async fn wrong(req: Request) -> AppResult<Response> {
            req.body_stream()?;
            Ok(Response::text("届かない"))
        }
        let app = Application::configure()
            .with_routing(|r| {
                r.web(|| {
                    Route::post("/wrong", wrong);
                })
            })
            .create();
        assert_eq!(app.handle(post_body("/wrong", b"x")).await.status(), 500);
    }

    #[tokio::test]
    async fn body_streamは1回しか取り出せない() {
        assert_eq!(
            body_app().handle(post_body("/twice", b"x")).await.status(),
            500
        );
    }

    #[tokio::test]
    async fn 本文のエラーは共通のミドルウェアだけを通る() {
        // 以前は共通のミドルウェアも通さなかったので、CORS の見出しが付かなかった。
        // ルートのミドルウェアとハンドラは、今も通さない。
        let app = Application::configure()
            .with_middleware(|m| m.append(powered_by).alias("marked", marker))
            .with_routing(|r| {
                r.web(|| {
                    Route::post("/small", body_length)
                        .max_body(4)
                        .middleware("marked");
                })
            })
            .create();
        let over = app.handle(post_body("/small", b"12345")).await;
        assert_eq!(over.status(), 413);
        assert_eq!(over.header("x-powered-by"), Some("bengara"));
        assert_eq!(marks(&over), ["global"]);

        let ok = app.handle(post_body("/small", b"1234")).await;
        assert_eq!(ok.header("x-powered-by"), Some("bengara"));
        assert_eq!(marks(&ok), ["route", "global"]);
    }

    #[tokio::test]
    async fn 本文のエラーにもwith_exceptionsが効く() {
        let app = Application::configure()
            .with_exceptions(|e| {
                e.render(|error| {
                    (error.status() == 413).then(|| Response::text("大きすぎます").with_status(413))
                })
            })
            .with_routing(|r| {
                r.web(|| {
                    Route::post("/small", body_length).max_body(4);
                })
            })
            .create();
        let over = app.handle(post_body("/small", b"12345")).await;
        assert_eq!(over.status(), 413);
        assert_eq!(over.body_text(), "大きすぎます");
    }

    #[tokio::test]
    async fn 無いパスへの壊れたmultipartは404() {
        // 以前はルートに当たらなくても multipart を分けていたので、400 になっていた。
        let req = post_body("/missing", b"broken").with_headers(vec![(
            "content-type".to_string(),
            "multipart/form-data; boundary=XyZ".to_string(),
        )]);
        assert_eq!(body_app().handle(req).await.status(), 404);
        // ルートがあれば今までどおり 400。
        let req = post_body("/default", b"broken").with_headers(vec![(
            "content-type".to_string(),
            "multipart/form-data; boundary=XyZ".to_string(),
        )]);
        assert_eq!(body_app().handle(req).await.status(), 400);
    }

    #[tokio::test]
    async fn フォームとjsonの項目は1000まで() {
        let limit = crate::http::multipart::MAX_PARTS;
        let send = |content_type: &str, body: String| {
            post_body("/default", body.as_bytes())
                .with_headers(vec![("content-type".to_string(), content_type.to_string())])
        };
        let form = |count: usize| vec!["a=1"; count].join("&");
        let json = |count: usize| {
            let keys: Vec<String> = (0..count).map(|i| format!("\"k{i}\":1")).collect();
            format!("{{{}}}", keys.join(","))
        };
        let app = body_app();
        let urlencoded = "application/x-www-form-urlencoded";
        // ちょうどは通る。空の組（`&&`）は数えない。
        let at_limit = format!("{}&&&", form(limit));
        assert_eq!(app.handle(send(urlencoded, at_limit)).await.status(), 200);
        assert_eq!(
            app.handle(send(urlencoded, form(limit + 1))).await.status(),
            413
        );
        assert_eq!(
            app.handle(send("application/json", json(limit)))
                .await
                .status(),
            200
        );
        assert_eq!(
            app.handle(send("application/json", json(limit + 1)))
                .await
                .status(),
            413
        );
        // 入れ子の鍵は数えない。
        let nested = format!("{{\"a\":{}}}", json(limit + 1));
        assert_eq!(
            app.handle(send("application/json", nested)).await.status(),
            200
        );
    }
}
