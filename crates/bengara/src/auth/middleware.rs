//! ログインの有無で止めるミドルウェア。
//!
//! | 型 | 止める相手 | Laravel |
//! |---|---|---|
//! | `Authenticate` | ログインしていない人 | `auth` |
//! | `Authenticate::token()` | API のトークンを持たない人 | `auth:sanctum` |
//! | `RedirectIfAuthenticated` | ログイン中の人 | `guest` |
//! | `CheckAbilities` | 能力を**すべて**は持たないトークン | `abilities:` |
//! | `CheckForAnyAbility` | 能力を**1 つも**持たないトークン | `ability:` |
//!
//! ```ignore
//! // bootstrap/app.rs
//! .alias("auth", Authenticate::new())
//! .alias("guest", RedirectIfAuthenticated::new())
//! .alias("auth:sanctum", Authenticate::token())
//! .alias("abilities:posts:write", CheckAbilities::new(["posts:write"]))
//! ```
//!
//! ```ignore
//! Route::get("/profile", UserController::show).middleware("auth");
//! Route::post("/login", AuthController::login).middleware("guest");
//! ```

use super::{Authenticatable, PersonalAccessToken};
use crate::error::Error;
use crate::http::handler::BoxFuture;
use crate::http::middleware::{Middleware, Next};
use crate::http::Request;

/// 戻り先をセッションに入れておく場所。
///
/// 読む側は `Session::intended()` です。
pub(crate) const INTENDED_KEY: &str = "bengara_auth_intended";

/// 自分のサイトの中を指すパスか。
///
/// ログイン後の戻り先として覚えてよいかを決めます。`//evil.example` は
/// 「プロトコル相対 URL」で、転送先に使うと**外のサイト**へ飛ばせます。
/// `Location: //evil.example` はブラウザが `https://evil.example` と読むためです。
///
/// `/` で始まることだけを見ていたころは `/\evil.example` を通していました。
/// ブラウザはパスの中の `\` を `/` に直すので、これは `//evil.example` と同じ意味です。
/// タブや改行も URL から取り除かれるため、`/<タブ>/evil.example` が同じ形になります。
/// どちらも断ります。
pub(crate) fn is_internal_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix('/') else {
        return false;
    };
    if path.contains('\\') || path.bytes().any(|b| b.is_ascii_control()) {
        return false;
    }
    !rest.starts_with('/')
}

/// 戻り先として覚えてよいメソッドか。
///
/// ログイン後の転送は `GET` です。`GET` で開けるパスだけを覚えます
/// （`HEAD` は `GET` のルートで受けるので、同じ扱いにします）。
fn remembers_intended(method: &str) -> bool {
    matches!(method, "GET" | "HEAD")
}

/// ログイン必須にするミドルウェア。
///
/// | 状況 | 返すもの |
/// |---|---|
/// | JSON を期待している | 401 |
/// | 転送先を指定していない | 401 |
/// | 転送先を指定している | 302 |
/// | セッションが無い（`StartSession` の登録し忘れ） | 500（直し方を添えたエラー） |
///
/// # ログイン後に元の画面へ返す
///
/// 転送するとき（上の表の 302 の行）だけ、**もとのパスをセッションに覚えます。**
/// ログインを処理するハンドラで `req.session().intended()` を呼ぶと取り出せます。
/// 覚えるのは自分のサイトの中を指すパスだけです。
///
/// ```ignore
/// // ログインできたあとで
/// let back = req.session().intended().unwrap_or_else(|| "/".to_string());
/// redirect().to(back)
/// ```
///
/// 401 を返すだけの経路（JSON を期待しているとき、転送先を決めていないとき）では
/// **覚えません。** 使う相手がいないのに、401 のたびにセッションを書くことになります。
///
/// **覚えるのは `GET` と `HEAD` のときだけです**（Laravel も `GET` のときだけ覚えます）。
/// ログイン後の転送は `GET` で飛ぶので、`POST` 専用のパスを覚えても 405 になります。
///
/// # 利用者がまだいるかは見ません（[`provider`](Self::provider) を付けたときだけ見ます）
///
/// 既定では、セッションの利用者 ID（トークンなら持ち主）があれば通します。**DB は引きません。**
/// 利用者の行を消しても、その人のセッションやトークンは期限まで通ります。
/// Laravel の `auth` は毎回利用者を読むので、ここは違います。
/// 利用者を消すときは、`logout_user` でセッションを切り、`tokens().delete()` で
/// トークンも消してください。毎回確かめたいときは [`provider`](Self::provider) を付けます。
pub struct Authenticate {
    redirect_to: Option<String>,
    /// セッションではなく、API のトークンで確かめるか（[`token`](Self::token)）。
    token: bool,
    /// 利用者の型（[`provider`](Self::provider)）。`None` なら利用者を引きません。
    provider: Option<Provider>,
}

/// [`Authenticate::provider`] で決めた、利用者の型。
#[derive(Clone)]
struct Provider {
    /// トークンの持ち主の種類として通す名前（[`Authenticatable::morph_class`]）。
    morph_class: String,
    /// その主キーの利用者がいるか。
    exists: fn(String) -> ExistsFuture,
}

/// [`Provider::exists`] が返す future。
type ExistsFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = crate::error::Result<bool>> + Send>>;

/// `U` の表に、その主キーの行があるか。
fn user_exists<U: Authenticatable>(id: String) -> ExistsFuture {
    Box::pin(async move { Ok(U::find(id).await?.is_some()) })
}

impl Authenticate {
    /// 401 を返す形で作る。
    pub fn new() -> Self {
        Self {
            redirect_to: None,
            token: false,
            provider: None,
        }
    }

    /// 画面からの場合に転送する先を決める。
    ///
    /// JSON を期待しているリクエストには、転送ではなく 401 を返します。
    pub fn redirect_to(path: impl Into<String>) -> Self {
        Self {
            redirect_to: Some(path.into()),
            token: false,
            provider: None,
        }
    }

    /// 利用者の型を決め、**その利用者がまだいるか**を毎回確かめる。
    /// Laravel の `config/auth.php` の `providers`（`'model' => User::class`）に当たります。
    ///
    /// ```ignore
    /// // bootstrap/app.rs
    /// .alias("auth", Authenticate::redirect_to("/login").provider::<User>())
    /// .alias("auth:sanctum", Authenticate::token().provider::<User>())
    /// ```
    ///
    /// | 形 | 通さないもの | 返すもの |
    /// |---|---|---|
    /// | セッション | 利用者 ID の行が `U` の表に無い | ログインしていないときと同じ（401 か 302） |
    /// | トークン | 持ち主の種類（`tokenable_type`）が `U::morph_class()` と違う | 401 |
    /// | トークン | 持ち主の行が `U` の表に無い | 401 |
    ///
    /// - **通るたびに主キーで 1 回 DB を引きます。** ハンドラで `user::<U>()` を
    ///   呼ぶと、もう 1 回引きます。
    /// - トークンでは、別の型（`Admin` など）のトークンがこのルートを通らなくなります。
    ///   付けないと、`req.auth().id()` は持ち主の種類を見ずに `tokenable_id` を返します。
    /// - セッションの中身は書き換えません（利用者がいないときも、ログインの情報は残ります）。
    /// - DB を引けなかったときはエラー（500）です。
    pub fn provider<U: Authenticatable>(mut self) -> Self {
        self.provider = Some(Provider {
            morph_class: U::morph_class(),
            exists: user_exists::<U>,
        });
        self
    }

    /// API のトークン（`Authorization: Bearer ..`）で確かめる形で作る。
    /// Laravel の `auth:sanctum` に当たります。
    ///
    /// ```ignore
    /// // bootstrap/app.rs
    /// .alias("auth:sanctum", Authenticate::token())
    /// ```
    ///
    /// | 状況 | 返すもの |
    /// |---|---|
    /// | ヘッダーが無い・形が違う | 401 |
    /// | 表に無い・消した・値が違う | 401 |
    /// | 期限切れ（`expires_at` が過ぎた） | 401 |
    /// | 通った | そのまま通す |
    ///
    /// - **転送しません。** 画面からの要求にも 401 を返します。
    /// - **セッションを使いません。** 読みも書きもしないので、Cookie の無い要求で
    ///   セッションのファイルが増えることもありません。`StartSession` が無くても動きます。
    /// - 通ったら、その要求の中だけ「トークンの持ち主で認証済み」になります。
    ///   `req.auth()` の `check()`・`id()`・`user::<U>()` が持ち主を返します。
    /// - `last_used_at` を書きます。前に書いてから 60 秒たっていなければ書きません。
    ///   書けなかったときは警告をログに出し、要求は通します。
    /// - セッションでログインしていても、トークンが無ければ 401 です
    ///   （SPA の Cookie 認証は作っていません）。
    /// - **持ち主の種類と、持ち主がまだいるかは見ません。** 見るときは
    ///   [`provider`](Self::provider) を付けてください（`Authenticate::token().provider::<User>()`）。
    pub fn token() -> Self {
        Self {
            redirect_to: None,
            token: true,
            provider: None,
        }
    }
}

/// トークンで確かめる（[`Authenticate::token`]）。
fn authenticate_token(req: Request, next: Next, provider: Option<Provider>) -> BoxFuture {
    let options = next.render_options();
    Box::pin(async move {
        let found = match req.bearer_token() {
            Some(token) => PersonalAccessToken::find_token(token).await?,
            None => None,
        };
        let found = found.filter(|token| !token.is_expired());
        // 利用者の型を決めてあれば、持ち主の種類と、持ち主がまだいるかも見る。
        let found = match (found, provider) {
            (Some(token), Some(provider)) => {
                if token.tokenable_type != provider.morph_class {
                    tracing::debug!("トークンの持ち主の種類が違います");
                    None
                } else if !(provider.exists)(token.tokenable_id.clone()).await? {
                    tracing::debug!("トークンの持ち主が見つかりません");
                    None
                } else {
                    Some(token)
                }
            }
            (found, _) => found,
        };
        let Some(token) = found else {
            let error = Error::http(401, "ログインしてください");
            tracing::debug!("{error}");
            return Ok(crate::http::response::error_response(&error, options));
        };
        if let Err(e) = token.touch_last_used().await {
            tracing::warn!("トークンの last_used_at を書けませんでした: {e}");
        }
        let mut req = req;
        req.set_access_token(std::sync::Arc::new(token));
        next.run(req).await
    })
}

impl Default for Authenticate {
    fn default() -> Self {
        Self::new()
    }
}

impl Middleware for Authenticate {
    fn handle(&self, req: Request, next: Next) -> BoxFuture {
        if self.token {
            return authenticate_token(req, next, self.provider.clone());
        }
        // セッションが無いと、ログインしていても理由なく 401 になる。登録し忘れを知らせる。
        if req.try_session().is_none() {
            return Box::pin(async {
                Err(Error::msg(
                    "ログインの確認にはセッションが要ります。bootstrap/app.rs で \
                     StartSession を Authenticate（auth）より前に登録してください",
                ))
            });
        }
        match (req.auth().id(), &self.provider) {
            (Some(_), None) => return next.run(req),
            // 利用者の型を決めてあれば、その利用者がまだいるかを見る。
            (Some(id), Some(provider)) => {
                let exists = provider.exists;
                let redirect_to = self.redirect_to.clone();
                return Box::pin(async move {
                    if exists(id).await? {
                        return next.run(req).await;
                    }
                    tracing::debug!("セッションの利用者が見つかりません");
                    unauthenticated(req, next, redirect_to).await
                });
            }
            (None, _) => {}
        }
        unauthenticated(req, next, self.redirect_to.clone())
    }
}

/// ログインしていない（または利用者が見つからない）ときの応答。転送か 401 を返す。
fn unauthenticated(req: Request, next: Next, redirect_to: Option<String>) -> BoxFuture {
    let options = next.render_options();
    Box::pin(async move {
        // 転送するかどうかを先に決める。転送しないなら戻り先も覚えない。
        let will_redirect = redirect_to.is_some() && !options.wants_json;

        // 戻り先を覚えておく（ログイン後に元の画面へ返せるように）。
        // **自分のサイトの中だけ**を覚えます。
        // 401 を返すだけの経路で覚えても使い道が無く、401 のたびに
        // セッションの書き込みが増えるだけなので、転送するときに限ります。
        //
        // **`GET` と `HEAD` のときだけ覚えます。** ログイン後は `GET` で飛ばすので、
        // `POST` 専用の口のパスを覚えると、ログインできた直後に 405 になります。
        if will_redirect && remembers_intended(req.method()) {
            if let Some(session) = req.try_session() {
                let intended = req.full_path();
                if is_internal_path(&intended) {
                    session.put(INTENDED_KEY, intended);
                }
            }
        }

        match redirect_to {
            // JSON が欲しいなら、転送ではなく 401 を返す。
            Some(path) if !options.wants_json => crate::http::redirect().to(path),
            _ => {
                let error = Error::http(401, "ログインしてください");
                tracing::debug!("{error}");
                Ok(crate::http::response::error_response(&error, options))
            }
        }
    })
}

/// ログイン中の人を止めるミドルウェア。Laravel の `guest` に当たります。
///
/// ログイン・登録・パスワード再設定の口に掛けます。
///
/// | 状況 | 返すもの |
/// |---|---|
/// | セッションが無い（`StartSession` の登録し忘れ） | 500（直し方を書いたエラー） |
/// | ログインしていない | そのまま通す |
/// | JSON を期待している | 403 |
/// | 転送先を指定していない | 403 |
/// | 転送先を指定している | 302 |
///
/// **回数の制限と組み合わせるときに要ります。** `Throttle` はログイン中の人を
/// `user:{id}` ごとに数えます。ログイン中でも登録の口を叩けると、登録し直すたびに
/// 新しい枠を得て、制限を回せてしまいます。`guest` で止めるか、
/// `Throttle::by_ip` で接続元ごとに数えてください。
///
/// Laravel は転送だけを返しますが、ここでは転送先を決めないと 403 を返します。
/// 行き先（ログイン後の画面）はアプリごとに違い、決め打ちにできないためです。
pub struct RedirectIfAuthenticated {
    redirect_to: Option<String>,
}

impl RedirectIfAuthenticated {
    /// 403 を返す形で作る。
    pub fn new() -> Self {
        Self { redirect_to: None }
    }

    /// 画面からの場合に転送する先を決める。
    ///
    /// JSON を期待しているリクエストには、転送ではなく 403 を返します。
    pub fn redirect_to(path: impl Into<String>) -> Self {
        Self {
            redirect_to: Some(path.into()),
        }
    }
}

impl Default for RedirectIfAuthenticated {
    fn default() -> Self {
        Self::new()
    }
}

impl Middleware for RedirectIfAuthenticated {
    fn handle(&self, req: Request, next: Next) -> BoxFuture {
        // セッションが無いと、ログイン中の人も「ログインしていない」に見えて黙って通る。
        // `Authenticate` と同じく、登録し忘れを知らせる。
        if req.try_session().is_none() {
            return Box::pin(async {
                Err(Error::msg(
                    "ログイン中かの確認にはセッションが要ります。bootstrap/app.rs で \
                     StartSession を RedirectIfAuthenticated（guest）より前に登録してください",
                ))
            });
        }
        if req.auth().guest() {
            return next.run(req);
        }

        let options = next.render_options();
        let redirect_to = self.redirect_to.clone();
        Box::pin(async move {
            match redirect_to {
                // JSON が欲しいなら、転送ではなく 403 を返す。
                Some(path) if !options.wants_json => crate::http::redirect().to(path),
                _ => {
                    let error = Error::http(403, "ログイン中は使えません");
                    tracing::debug!("{error}");
                    Ok(crate::http::response::error_response(&error, options))
                }
            }
        })
    }
}

/// トークンが能力を**すべて**持っていなければ止める。Laravel の `abilities:` に当たります。
///
/// ```ignore
/// // bootstrap/app.rs
/// .alias("abilities:posts:write", CheckAbilities::new(["posts:write"]))
/// ```
///
/// ```ignore
/// Route::post("/api/posts", PostController::store)
///     .middleware("auth:sanctum")
///     .middleware("abilities:posts:write");
/// ```
///
/// | 状況 | 返すもの |
/// |---|---|
/// | トークンで認証していない | 401 |
/// | 足りない能力がある | 403 |
/// | すべて持っている（`"*"` を含む） | そのまま通す |
///
/// - `Authenticate::token()` より**後ろ**に置いてください。前に置くと、
///   まだ認証していないので必ず 401 になります。
/// - セッションでログインしているだけの要求も 401 です（Laravel も同じ）。
/// - 能力を 1 つも書かなければ、トークンがあれば通します（組み立てるときに警告を出します）。
pub struct CheckAbilities {
    abilities: Vec<String>,
}

impl CheckAbilities {
    /// 要る能力を決めて作る。
    ///
    /// 能力を 1 つも渡さないと、トークンさえあれば通ります（Laravel と同じ）。
    /// 書き忘れに気づけるよう、**組み立てるときに警告を出します。**
    pub fn new<I, S>(abilities: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let abilities: Vec<String> = abilities.into_iter().map(Into::into).collect();
        if abilities.is_empty() {
            tracing::warn!(
                "CheckAbilities::new に能力が 1 つもありません。トークンさえあれば、\
                 どのトークンも通ります"
            );
        }
        Self { abilities }
    }
}

impl Middleware for CheckAbilities {
    fn handle(&self, req: Request, next: Next) -> BoxFuture {
        let allowed = req
            .auth()
            .current_access_token()
            .map(|token| self.abilities.iter().all(|a| token.can(a)));
        check_ability(allowed, req, next)
    }
}

/// トークンが能力を**1 つも**持っていなければ止める。Laravel の `ability:` に当たります。
///
/// ```ignore
/// // bootstrap/app.rs
/// .alias("ability:posts:read,posts:write", CheckForAnyAbility::new(["posts:read", "posts:write"]))
/// ```
///
/// 返すものは [`CheckAbilities`] と同じです（認証していない → 401、持っていない → 403）。
/// 能力を 1 つも書かなければ、いつも 403 です（Laravel も同じ）。
pub struct CheckForAnyAbility {
    abilities: Vec<String>,
}

impl CheckForAnyAbility {
    /// 候補の能力を決めて作る。
    pub fn new<I, S>(abilities: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            abilities: abilities.into_iter().map(Into::into).collect(),
        }
    }
}

impl Middleware for CheckForAnyAbility {
    fn handle(&self, req: Request, next: Next) -> BoxFuture {
        let allowed = req
            .auth()
            .current_access_token()
            .map(|token| self.abilities.iter().any(|a| token.can(a)));
        check_ability(allowed, req, next)
    }
}

/// 能力の判定の結果で通すか止める。`None` はトークンで認証していない。
fn check_ability(allowed: Option<bool>, req: Request, next: Next) -> BoxFuture {
    let error = match allowed {
        Some(true) => return next.run(req),
        Some(false) => Error::http(403, "このトークンには、この操作の権限がありません"),
        None => Error::http(401, "ログインしてください"),
    };
    let options = next.render_options();
    Box::pin(async move {
        tracing::debug!("{error}");
        Ok(crate::http::response::error_response(&error, options))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// リクエストを 1 本通し、状態を返す。
    async fn guest_status(mw: RedirectIfAuthenticated, logged_in: bool, wants_json: bool) -> u16 {
        use crate::http::handler::Erased;
        use std::sync::Arc;

        let mut req = Request::new("POST", "/login");
        req.set_session(crate::session::Session::empty());
        if logged_in {
            req.auth().login_using_id(5).expect("ログインできる");
        }

        let stack: Arc<[Arc<dyn Middleware>]> = Arc::from(Vec::new());
        let handler = Arc::new(Erased::<_, ()>::new(|| async {
            Ok(crate::http::Response::text("ok"))
        }));
        let options = crate::http::response::RenderOptions::new(false, wants_json);
        mw.handle(req, Next::new(stack, handler, options))
            .await
            .expect("応答が返る")
            .status()
    }

    #[tokio::test]
    async fn ログイン中の人を止める() {
        // ログインしていなければ通す。
        assert_eq!(
            guest_status(RedirectIfAuthenticated::new(), false, false).await,
            200
        );
        assert_eq!(
            guest_status(RedirectIfAuthenticated::redirect_to("/home"), false, true).await,
            200
        );

        // ログイン中なら止める。
        assert_eq!(
            guest_status(RedirectIfAuthenticated::new(), true, false).await,
            403
        );
        assert_eq!(
            guest_status(RedirectIfAuthenticated::redirect_to("/home"), true, false).await,
            302
        );
        // JSON が欲しい相手には、転送先があっても 403。
        assert_eq!(
            guest_status(RedirectIfAuthenticated::redirect_to("/home"), true, true).await,
            403
        );

        assert!(RedirectIfAuthenticated::default().redirect_to.is_none());
    }

    #[tokio::test]
    async fn セッションが無ければ登録し忘れを知らせる() {
        use crate::http::handler::Erased;
        use std::sync::Arc;

        // StartSession を付け忘れると、理由の分からない 401 になっていた。
        let req = Request::new("GET", "/profile");
        let stack: Arc<[Arc<dyn Middleware>]> = Arc::from(Vec::new());
        let handler = Arc::new(Erased::<_, ()>::new(|| async {
            Ok(crate::http::Response::text("ok"))
        }));
        let options = crate::http::response::RenderOptions::new(false, false);
        let error = Authenticate::new()
            .handle(req, Next::new(stack, handler, options))
            .await
            .unwrap_err();
        assert_eq!(error.status(), 500);
        assert!(error.to_string().contains("StartSession"), "{error}");
    }

    #[tokio::test]
    async fn guestもセッションが無ければ登録し忘れを知らせる() {
        use crate::http::handler::Erased;
        use std::sync::Arc;

        // StartSession を付け忘れると、ログイン中の人も黙って通っていた。
        let req = Request::new("POST", "/login");
        let stack: Arc<[Arc<dyn Middleware>]> = Arc::from(Vec::new());
        let handler = Arc::new(Erased::<_, ()>::new(|| async {
            Ok(crate::http::Response::text("ok"))
        }));
        let options = crate::http::response::RenderOptions::new(false, false);
        let error = RedirectIfAuthenticated::new()
            .handle(req, Next::new(stack, handler, options))
            .await
            .unwrap_err();
        assert_eq!(error.status(), 500);
        assert!(error.to_string().contains("StartSession"), "{error}");
    }

    #[test]
    fn 転送先は任意() {
        assert!(Authenticate::new().redirect_to.is_none());
        assert_eq!(
            Authenticate::redirect_to("/login").redirect_to.as_deref(),
            Some("/login")
        );
        assert!(Authenticate::default().redirect_to.is_none());
    }

    /// 未ログインのリクエストを 1 本通し、状態と、覚えた戻り先を返す。
    async fn stopped(mw: Authenticate, method: &str) -> (u16, Option<String>) {
        use crate::http::handler::Erased;
        use std::sync::Arc;

        let session = crate::session::Session::empty();
        let mut req = Request::new(method, "/profile").with_query("page=2");
        req.set_session(session.clone());

        let stack: Arc<[Arc<dyn Middleware>]> = Arc::from(Vec::new());
        let handler = Arc::new(Erased::<_, ()>::new(|| async {
            Ok(crate::http::Response::text("ok"))
        }));
        let options = crate::http::response::RenderOptions::new(false, false);
        let response = mw
            .handle(req, Next::new(stack, handler, options))
            .await
            .expect("応答が返る");
        (response.status(), session.get(INTENDED_KEY))
    }

    #[tokio::test]
    async fn 戻り先はgetとheadのときだけ覚える() {
        // ログイン後は GET で飛ばす。POST 専用の口を覚えると 405 になる。
        for method in ["GET", "HEAD"] {
            let (status, intended) = stopped(Authenticate::redirect_to("/login"), method).await;
            assert_eq!(status, 302, "{method}");
            assert_eq!(intended.as_deref(), Some("/profile?page=2"), "{method}");
        }
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            let (status, intended) = stopped(Authenticate::redirect_to("/login"), method).await;
            assert_eq!(status, 302, "{method}: 転送はする");
            assert_eq!(intended, None, "{method}: 戻り先は覚えない");
        }

        // 401 を返すだけの経路では、GET でも覚えない（今までどおり）。
        let (status, intended) = stopped(Authenticate::new(), "GET").await;
        assert_eq!(status, 401);
        assert_eq!(intended, None);

        assert!(remembers_intended("GET"));
        assert!(remembers_intended("HEAD"));
        // メソッドは大文字小文字を区別する（`get` はどのルートにも当たらない）。
        assert!(!remembers_intended("get"));
        assert!(!remembers_intended("OPTIONS"));
    }

    #[test]
    fn 戻り先は自分のサイトの中だけ覚える() {
        assert!(is_internal_path("/"));
        assert!(is_internal_path("/profile"));
        assert!(is_internal_path("/profile?page=2"));

        // プロトコル相対 URL は外のサイトを指せる。
        assert!(!is_internal_path("//evil.example"));
        assert!(!is_internal_path("//evil.example/login"));
        // `\` はブラウザが `/` に直すので、これも外のサイトを指せる。
        assert!(!is_internal_path("/\\evil.example"));
        assert!(!is_internal_path("/\\evil.example/login"));
        assert!(!is_internal_path("/\\/evil.example"));
        assert!(
            !is_internal_path("/path/\\evil.example"),
            "途中の `\\` も断る"
        );
        // タブや改行は URL から取り除かれるので、`//` の形になる。
        assert!(!is_internal_path("/\t/evil.example"));
        assert!(!is_internal_path("/\n/evil.example"));
        assert!(!is_internal_path("/\r/evil.example"));
        // 絶対 URL も断る。
        assert!(!is_internal_path("https://evil.example"));
        // `/` で始まらないものも断る。
        assert!(!is_internal_path(""));
        assert!(!is_internal_path("profile"));

        assert_eq!(INTENDED_KEY, "bengara_auth_intended");
    }

    /// 能力の確認に 1 本通し、状態を返す。`abilities` が `None` ならトークンを載せない。
    async fn ability_status(mw: impl Middleware, abilities: Option<&[&str]>) -> u16 {
        use crate::http::handler::Erased;
        use std::sync::Arc;

        let mut req = Request::new("GET", "/api/posts");
        if let Some(abilities) = abilities {
            req.set_access_token(Arc::new(PersonalAccessToken::fake(
                "App\\Models\\User",
                "7",
                abilities,
            )));
        }
        let stack: Arc<[Arc<dyn Middleware>]> = Arc::from(Vec::new());
        let handler = Arc::new(Erased::<_, ()>::new(|| async {
            Ok(crate::http::Response::text("ok"))
        }));
        let options = crate::http::response::RenderOptions::new(false, false);
        mw.handle(req, Next::new(stack, handler, options))
            .await
            .expect("応答が返る")
            .status()
    }

    #[tokio::test]
    async fn checkabilitiesはすべて要る() {
        let mw = || CheckAbilities::new(["posts:read", "posts:write"]);
        assert_eq!(
            ability_status(mw(), Some(&["posts:read", "posts:write"])).await,
            200
        );
        assert_eq!(ability_status(mw(), Some(&["posts:read"])).await, 403);
        assert_eq!(ability_status(mw(), Some(&["*"])).await, 200);
        assert_eq!(ability_status(mw(), Some(&[])).await, 403);
        assert_eq!(ability_status(mw(), None).await, 401, "トークンが無い");
        // 何も書かなければ、トークンがあれば通す（Laravel と同じ）。
        let empty = || CheckAbilities::new(Vec::<String>::new());
        assert_eq!(ability_status(empty(), Some(&[])).await, 200);
        assert_eq!(ability_status(empty(), None).await, 401);
    }

    #[tokio::test]
    async fn checkforanyabilityはどれか1つ() {
        let mw = || CheckForAnyAbility::new(["posts:read", "posts:write"]);
        assert_eq!(ability_status(mw(), Some(&["posts:write"])).await, 200);
        assert_eq!(ability_status(mw(), Some(&["posts:read"])).await, 200);
        assert_eq!(ability_status(mw(), Some(&["*"])).await, 200);
        assert_eq!(ability_status(mw(), Some(&["users:read"])).await, 403);
        assert_eq!(ability_status(mw(), None).await, 401, "トークンが無い");
        // 何も書かなければ、いつも 403（Laravel と同じ）。
        let empty = || CheckForAnyAbility::new(Vec::<String>::new());
        assert_eq!(ability_status(empty(), Some(&["*"])).await, 403);
    }

    #[tokio::test]
    async fn トークンの確認はセッションが無くても401を返す() {
        use crate::http::handler::Erased;
        use std::sync::Arc;

        // ヘッダーが無ければ DB に行かずに 401。転送もしない。
        for (headers, wants_json) in [
            (vec![], false),
            (vec![], true),
            (
                vec![("authorization".to_string(), "Basic abc".to_string())],
                false,
            ),
            (
                vec![("authorization".to_string(), "Bearer ".to_string())],
                false,
            ),
        ] {
            let req = Request::new("GET", "/api/me").with_headers(headers);
            assert!(req.try_session().is_none());
            let stack: Arc<[Arc<dyn Middleware>]> = Arc::from(Vec::new());
            let handler = Arc::new(Erased::<_, ()>::new(|| async {
                Ok(crate::http::Response::text("ok"))
            }));
            let options = crate::http::response::RenderOptions::new(false, wants_json);
            let status = Authenticate::token()
                .handle(req, Next::new(stack, handler, options))
                .await
                .expect("500 にしない")
                .status();
            assert_eq!(status, 401, "wants_json={wants_json}");
        }
        assert!(Authenticate::token().token);
        assert!(!Authenticate::new().token);
        assert!(!Authenticate::redirect_to("/login").token);
    }

    #[test]
    fn bearerの値を取り出す() {
        let token = |value: &str| {
            Request::new("GET", "/")
                .with_headers(vec![("authorization".to_string(), value.to_string())])
                .bearer_token()
                .map(str::to_string)
        };
        assert_eq!(token("Bearer 1|abc").as_deref(), Some("1|abc"));
        assert_eq!(token("bearer  1|abc ").as_deref(), Some("1|abc"));
        assert_eq!(token("BEARER x").as_deref(), Some("x"));
        assert_eq!(token("Basic abc"), None);
        assert_eq!(token("Bearer"), None);
        assert_eq!(token("Bearer   "), None);
        assert_eq!(token("Bearerabc"), None);
        assert_eq!(Request::new("GET", "/").bearer_token(), None);
    }
}
