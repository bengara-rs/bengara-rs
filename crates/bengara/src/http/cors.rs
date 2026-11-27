//! CORS（別のオリジンからの呼び出し）。Laravel の `HandleCors` に当たります。
//!
//! ```ignore
//! // bootstrap/app.rs
//! .with_middleware(|m| m.prepend(bengara::HandleCors::from_config()).append(StartSession::file()))
//!
//! // config/cors.rs
//! pub fn config() -> bengara::CorsConfig {
//!     bengara::CorsConfig {
//!         allowed_origins: vec!["https://app.example.com".into()],
//!         supports_credentials: true,
//!         ..Default::default()
//!     }
//! }
//! ```
//!
//! 動きは Laravel（fruitcake/php-cors）に合わせています。違う所は `HandleCors` の説明にあります。

use std::sync::Arc;

use crate::http::{BoxFuture, Middleware, Next, Request, Response};

/// 事前確認の応答に付ける `Vary`。どれが変わっても答えが変わるためです。
const PREFLIGHT_VARY: [&str; 3] = [
    "Origin",
    "Access-Control-Request-Method",
    "Access-Control-Request-Headers",
];

/// CORS の設定。`config/cors.rs` が返します。Laravel の `config/cors.php` に当たります。
///
/// ファイル名は自由です。見分けるのは名前ではなく、`config()` が返す型です。
///
/// | 欄 | 既定 | 意味 |
/// |---|---|---|
/// | `paths` | `["api/*"]` | 対象のパス。先頭の `/` は書かない。`*` は `/` も含めた何にでも当たる |
/// | `allowed_methods` | `["*"]` | 許すメソッド |
/// | `allowed_origins` | `["*"]` | 許すオリジン。`https://*.example.com` のように、ホストの先頭にだけ `*` を書ける |
/// | `allowed_headers` | `["*"]` | 許すヘッダー |
/// | `exposed_headers` | `[]` | ブラウザの JavaScript に見せるヘッダー |
/// | `max_age` | `0` | 事前確認の答えを覚えてよい秒数。0 は「覚えない」 |
/// | `supports_credentials` | `false` | Cookie 付きの呼び出しを許すか |
///
/// Laravel の `allowed_origins_patterns`（正規表現）はありません。依存を増やさないためです。
#[derive(Debug, Clone)]
pub struct CorsConfig {
    /// 対象のパス。先頭と末尾の `/` は無視します（`"/"` だけは根を指します）。
    pub paths: Vec<String>,
    /// 許すメソッド。`"*"` なら、事前確認で聞かれたものをそのまま返します。
    pub allowed_methods: Vec<String>,
    /// 許すオリジン。`"*"` なら全部です。
    pub allowed_origins: Vec<String>,
    /// 許すヘッダー。`"*"` なら、事前確認で聞かれたものをそのまま返します。
    pub allowed_headers: Vec<String>,
    /// ブラウザの JavaScript に見せるヘッダー（`Access-Control-Expose-Headers`）。
    pub exposed_headers: Vec<String>,
    /// 事前確認の答えを覚えてよい秒数（`Access-Control-Max-Age`）。
    pub max_age: u64,
    /// Cookie 付きの呼び出しを許すか（`Access-Control-Allow-Credentials`）。
    pub supports_credentials: bool,
}

impl Default for CorsConfig {
    /// `config/cors.rs` が無いときに使われる値。Laravel の既定から
    /// `sanctum/csrf-cookie` を除いたものです（その口が無いため）。
    fn default() -> Self {
        Self {
            paths: vec!["api/*".to_string()],
            allowed_methods: vec!["*".to_string()],
            allowed_origins: vec!["*".to_string()],
            allowed_headers: vec!["*".to_string()],
            exposed_headers: Vec::new(),
            max_age: 0,
            supports_credentials: false,
        }
    }
}

/// CORS のミドルウェア。Laravel の `HandleCors` に当たります。
///
/// **共通の並びの先頭に `prepend` で置きます。** `StartSession` より前に置くと、
/// 事前確認にセッションを配りません。`VerifyCsrfToken`・`Throttle` より前に置くと、
/// 419・429 の応答にも CORS のヘッダーが付きます。
///
/// ```ignore
/// .with_middleware(|m| m.prepend(bengara::HandleCors::from_config()))
/// ```
///
/// | 要求 | 動き |
/// |---|---|
/// | 対象外のパス | 何もしない |
/// | 事前確認（`OPTIONS` で `Access-Control-Request-Method` がある） | 先へ進めずに 204 を返す |
/// | それ以外 | 先へ進め、戻ってきた応答にヘッダーを足す（404・405・419・422・429・500 にも） |
///
/// 本文を受け取れなかったとき（413・400・408）の応答にも付きます。
/// オリジンが許されていないときは、CORS のヘッダーを付けません（ブラウザが止めます）。
///
/// # 付かない応答
///
/// ハンドラやミドルウェアがパニックしたときの 500 には付きません。
/// このミドルウェアの並びの外で作るためです。
///
/// # Laravel との違い
///
/// - `supports_credentials` と `allowed_origins` の `"*"` を同時に指定すると、**起動時にパニックします。**
///   Laravel は相手のオリジンをそのまま返すので、どのサイトにも Cookie 付きの呼び出しを許してしまいます。
/// - 許すオリジンが 1 つだけでも、相手が違えばオリジンを返しません
///   （Laravel は設定の 1 つをいつも返します。ブラウザが止める点は同じです）。
/// - `allowed_methods` が `"*"` のとき、聞かれたメソッドを大文字にせずに返します。
///   ブラウザは大文字小文字を区別して比べるためです。
pub struct HandleCors {
    inner: Arc<Compiled>,
}

/// 起動時に組み立てた、読むだけの設定。
struct Compiled {
    /// 前後の `/` を落とした対象のパス。
    paths: Vec<String>,
    /// 全部のオリジンを許すか（`supports_credentials` が偽のときだけ真になる）。
    any_origin: bool,
    /// そのまま比べるオリジン。
    exact_origins: Vec<String>,
    /// `https://*.example.com` の形。`("https://", ".example.com")` に分けて持つ。
    wildcard_origins: Vec<(String, String)>,
    /// メソッドは `"*"` か。
    any_method: bool,
    /// `Access-Control-Allow-Methods` に入れる値（`any_method` が偽のとき）。
    methods: String,
    /// ヘッダーは `"*"` か。
    any_header: bool,
    /// `Access-Control-Allow-Headers` に入れる値（`any_header` が偽のとき）。
    headers: String,
    /// `Access-Control-Expose-Headers` に入れる値。空なら付けない。
    exposed: String,
    /// `Access-Control-Max-Age` に入れる値。
    max_age: String,
    credentials: bool,
}

impl HandleCors {
    /// `config/cors.rs` の設定で作る。無ければ既定（[`CorsConfig::default`]）。
    ///
    /// # パニック
    ///
    /// 設定が矛盾しているとき（[`new`](Self::new) を参照）。
    pub fn from_config() -> Self {
        match crate::try_config::<CorsConfig>() {
            Some(config) => Self::new(config.clone()),
            None => Self::new(CorsConfig::default()),
        }
    }

    /// 設定を渡して作る。
    ///
    /// # パニック
    ///
    /// 次のときは起動時にパニックします。
    ///
    /// - `supports_credentials` が真で、`allowed_origins` に `"*"` がある
    /// - `supports_credentials` が真で、`exposed_headers` に `"*"` がある
    ///   （Cookie 付きの呼び出しでは、ブラウザが `*` を文字どおりの名前として扱うため）
    /// - `allowed_origins` に、ホストの先頭以外の `*` や、パス（末尾の `/` を含む）がある
    /// - `paths` に `https://...` のような URL がある（パスだけを比べるので当たらない）
    pub fn new(config: CorsConfig) -> Self {
        let has_star = |list: &[String]| list.iter().any(|value| value.trim() == "*");
        let any_origin = has_star(&config.allowed_origins);

        if config.supports_credentials && any_origin {
            panic!(
                "CorsConfig の supports_credentials が真のときは、allowed_origins に \"*\" を書けません。\
                 どのサイトにも Cookie 付きの呼び出しを許すことになります。\
                 許すオリジンを https://app.example.com のように並べてください"
            );
        }
        if config.supports_credentials && has_star(&config.exposed_headers) {
            panic!(
                "CorsConfig の supports_credentials が真のときは、exposed_headers に \"*\" を書けません。\
                 Cookie 付きの呼び出しでは、ブラウザが * を名前として扱うためです。\
                 見せるヘッダーの名前を並べてください"
            );
        }

        let mut exact_origins = Vec::new();
        let mut wildcard_origins = Vec::new();
        for origin in &config.allowed_origins {
            let origin = origin.trim();
            if origin == "*" {
                continue;
            }
            match parse_origin(origin) {
                Ok(Some(pair)) => wildcard_origins.push(pair),
                Ok(None) => exact_origins.push(origin.to_ascii_lowercase()),
                Err(reason) => {
                    panic!("CorsConfig の allowed_origins の \"{origin}\" は使えません。{reason}")
                }
            }
        }

        let paths = config
            .paths
            .iter()
            .map(|path| {
                let path = path.trim();
                if path.contains("://") {
                    panic!(
                        "CorsConfig の paths の \"{path}\" は使えません。\
                         URL ではなく、api/* のようにパスだけを書いてください"
                    );
                }
                trim_path(path).to_string()
            })
            .collect();

        let any_method = has_star(&config.allowed_methods);
        let any_header = has_star(&config.allowed_headers);
        let methods = config
            .allowed_methods
            .iter()
            .map(|method| method.trim().to_ascii_uppercase())
            .collect::<Vec<_>>()
            .join(", ");

        Self {
            inner: Arc::new(Compiled {
                paths,
                any_origin,
                exact_origins,
                wildcard_origins,
                any_method,
                methods,
                any_header,
                headers: join(&config.allowed_headers),
                exposed: join(&config.exposed_headers),
                max_age: config.max_age.to_string(),
                credentials: config.supports_credentials,
            }),
        }
    }
}

impl Middleware for HandleCors {
    fn handle(&self, req: Request, next: Next) -> BoxFuture {
        // 対象外のパスでは何も確保しない。
        if !self.inner.matches_path(req.path()) {
            return next.run(req);
        }

        let is_options = req.method() == "OPTIONS";
        if is_options {
            if let Some(method) = req.header("access-control-request-method") {
                // 事前確認。ルートの有無にかかわらず、先へ進めずに答える（405 にしない）。
                let response = self.inner.preflight(
                    req.header("origin"),
                    method,
                    req.header("access-control-request-headers"),
                );
                return Box::pin(std::future::ready(Ok(response)));
            }
        }

        let inner = self.inner.clone();
        let origin = req.header("origin").map(str::to_string);
        Box::pin(async move {
            // `next.run` はエラーも応答に変えて返すので、404・422・500 にも付く。
            let response = next.run(req).await?;
            Ok(inner.actual(response, origin.as_deref(), is_options))
        })
    }
}

impl Compiled {
    /// 対象のパスか。Laravel の `$request->is()` と同じく、前後の `/` を落として比べる。
    ///
    /// 届いたパスをそのまま比べます（`%xx` は戻しません）。ルートの照合と同じです。
    fn matches_path(&self, path: &str) -> bool {
        let path = trim_path(path);
        self.paths
            .iter()
            .any(|pattern| wildcard_match(pattern.as_bytes(), path.as_bytes()))
    }

    /// 許されたオリジンか。
    fn origin_allowed(&self, origin: &str) -> bool {
        self.exact_origins
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(origin))
            || self
                .wildcard_origins
                .iter()
                .any(|(scheme, suffix)| wildcard_origin_match(scheme, suffix, origin))
    }

    /// `Access-Control-Allow-Origin` に入れる値。付けないときは `None`。
    ///
    /// 全部を許すときは、`Origin` が無くても `*` を返します（Laravel と同じ）。
    /// こうしておくと、`Vary: Origin` を付けなくても、前段のキャッシュが
    /// 別のオリジンに古い答えを返しません。
    fn allow_origin(&self, origin: Option<&str>) -> Option<String> {
        if self.any_origin {
            return Some("*".to_string());
        }
        origin
            .filter(|origin| self.origin_allowed(origin))
            .map(str::to_string)
    }

    /// 事前確認の応答（204）。
    fn preflight(
        &self,
        origin: Option<&str>,
        method: &str,
        request_headers: Option<&str>,
    ) -> Response {
        let mut response = Response::no_content();
        if let Some(allow) = self.allow_origin(origin) {
            response = response.with_header("access-control-allow-origin", allow);
            if self.credentials {
                response = response.with_header("access-control-allow-credentials", "true");
            }
            // `*` のときは聞かれた値を返す。`*` をそのまま返すと、Cookie 付きの
            // 呼び出しでは名前として扱われ、`Authorization` はいつも `*` に含まれないため。
            let methods = if self.any_method {
                method.trim()
            } else {
                self.methods.as_str()
            };
            if !methods.is_empty() {
                response = response.with_header("access-control-allow-methods", methods);
            }
            let headers = if self.any_header {
                request_headers.unwrap_or("").trim()
            } else {
                self.headers.as_str()
            };
            if !headers.is_empty() {
                response = response.with_header("access-control-allow-headers", headers);
            }
            response = response.with_header("access-control-max-age", self.max_age.as_str());
        }
        PREFLIGHT_VARY
            .iter()
            .fold(response, |response, name| add_vary(response, name))
    }

    /// 普通の要求の応答にヘッダーを足す。
    fn actual(&self, response: Response, origin: Option<&str>, is_options: bool) -> Response {
        let mut response = response;
        if let Some(allow) = self.allow_origin(origin) {
            response = response.with_header("access-control-allow-origin", allow);
            if self.credentials {
                response = response.with_header("access-control-allow-credentials", "true");
            }
            if !self.exposed.is_empty() {
                response =
                    response.with_header("access-control-expose-headers", self.exposed.as_str());
            }
        }
        // 相手ごとに答えが変わるときは、許さなかった応答にも付ける。
        // 付けないと、前段のキャッシュが見出しの無い答えを別のオリジンに返します。
        if !self.any_origin {
            response = add_vary(response, "Origin");
        }
        // 事前確認でない `OPTIONS` も、事前確認と答えが分かれる（Laravel と同じ）。
        if is_options {
            response = add_vary(response, "Access-Control-Request-Method");
        }
        response
    }
}

/// 前後の `/` を落とす。`/` だけ（と空）は `/` のまま残す（Laravel の `path()` と同じ）。
fn trim_path(path: &str) -> &str {
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        "/"
    } else {
        trimmed
    }
}

/// 並びをカンマでつなぐ。前後の空白は落とす。
fn join(values: &[String]) -> String {
    values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// 設定のオリジンを調べる。
///
/// `*` が無ければ `Ok(None)`、`https://*.example.com` の形なら
/// `Ok(Some(("https://", ".example.com")))`。それ以外の `*` とパスは `Err`。
fn parse_origin(origin: &str) -> Result<Option<(String, String)>, &'static str> {
    let Some((scheme, rest)) = origin.split_once("://") else {
        // `null` のような、スキームの無い値はそのまま比べる。
        if origin.contains('*') {
            return Err("* は https://*.example.com のように、ホストの先頭にだけ書けます");
        }
        return Ok(None);
    };
    if rest.contains('/') {
        return Err("オリジンにはパスを付けません。末尾の / も外してください");
    }
    if !origin.contains('*') {
        return Ok(None);
    }
    match rest.strip_prefix('*') {
        Some(suffix) if suffix.starts_with('.') && suffix.len() > 1 && !suffix.contains('*') => {
            Ok(Some((
                format!("{}://", scheme.to_ascii_lowercase()),
                suffix.to_ascii_lowercase(),
            )))
        }
        _ => Err("* は https://*.example.com のように、ホストの先頭にだけ書けます"),
    }
}

/// `https://*.example.com` の形に当たるか。
///
/// `*` には 1 つ以上のラベル（英数字・`-`・`.`）が当たります。ポートは設定と同じときだけ当たります。
fn wildcard_origin_match(scheme: &str, suffix: &str, origin: &str) -> bool {
    let Some(rest) = strip_prefix_ignore_case(origin, scheme) else {
        return false;
    };
    if rest.len() <= suffix.len() || !rest.is_char_boundary(rest.len() - suffix.len()) {
        return false;
    }
    let (sub, tail) = rest.split_at(rest.len() - suffix.len());
    tail.eq_ignore_ascii_case(suffix)
        && !sub.starts_with('.')
        && sub
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
}

fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

/// `*` だけを特別に扱う照合。`*` は `/` を含めた 0 文字以上に当たる（Laravel の `Str::is`）。
///
/// 確保をしません。`*` の位置を覚えて戻るだけです。
fn wildcard_match(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    // 最後に見た `*` の位置と、そのとき `*` に当てた終わり。
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && pattern[p] == b'*' {
            star = Some((p, t));
            p += 1;
        } else if p < pattern.len() && pattern[p] == text[t] {
            p += 1;
            t += 1;
        } else if let Some((star_p, star_t)) = star {
            // `*` に 1 文字多く当てて、やり直す。
            p = star_p + 1;
            t = star_t + 1;
            star = Some((star_p, star_t + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&byte| byte == b'*')
}

/// `Vary` に名前を足す。すでにあれば足さない。`Vary: *` もそのまま。
///
/// 何本かに分かれていたら、1 本にまとめます。
fn add_vary(response: Response, name: &str) -> Response {
    let existing: Vec<&str> = response
        .headers()
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case("vary"))
        .map(|(_, value)| value.as_str())
        .collect();
    if existing.is_empty() {
        return response.with_header("vary", name);
    }
    let present = existing
        .iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .any(|token| token == "*" || token.eq_ignore_ascii_case(name));
    if present {
        return response;
    }
    let merged = format!("{}, {name}", existing.join(", "));
    response.with_header("vary", merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::handler::Erased;
    use crate::http::response::RenderOptions;

    fn compiled(config: CorsConfig) -> Arc<Compiled> {
        HandleCors::new(config).inner
    }

    /// 先に進んだら 200 と `x-reached` を返す並びで、ミドルウェアを 1 回通す。
    async fn run(mw: HandleCors, req: Request) -> Response {
        let handler = Arc::new(Erased::new(|| async {
            Ok(Response::text("ok").with_header("x-reached", "1"))
        }));
        let stack: Arc<[Arc<dyn Middleware>]> = vec![Arc::new(mw) as Arc<dyn Middleware>].into();
        Next::new(stack, handler, RenderOptions::new(false, false))
            .run(req)
            .await
            .unwrap()
    }

    #[test]
    fn パスの星は区切りも含めて当たる() {
        let cors = compiled(CorsConfig {
            paths: vec!["api/*".into(), "/exact/".into(), "a*b".into()],
            ..Default::default()
        });
        assert!(cors.matches_path("/api/posts"));
        assert!(cors.matches_path("/api/posts/1/comments"));
        assert!(cors.matches_path("/api/x/"));
        // Laravel と同じく、前後の `/` を落とすので `/api` と `/api/` は当たらない。
        assert!(!cors.matches_path("/api"));
        assert!(!cors.matches_path("/api/"));
        assert!(!cors.matches_path("/apix"));
        assert!(!cors.matches_path("/v1/api/posts"));
        assert!(cors.matches_path("/exact"));
        assert!(cors.matches_path("/exact/"));
        assert!(!cors.matches_path("/exact/more"));
        assert!(cors.matches_path("/ab"));
        assert!(cors.matches_path("/a/x/b"));
        assert!(!cors.matches_path("/a/x/bc"));
    }

    #[test]
    fn 根のパスも書ける() {
        let cors = compiled(CorsConfig {
            paths: vec!["/".into()],
            ..Default::default()
        });
        assert!(cors.matches_path("/"));
        assert!(!cors.matches_path("/x"));

        let all = compiled(CorsConfig {
            paths: vec!["*".into()],
            ..Default::default()
        });
        assert!(all.matches_path("/"));
        assert!(all.matches_path("/x/y"));
    }

    #[test]
    fn 星の照合() {
        assert!(wildcard_match(b"*", b""));
        assert!(wildcard_match(b"a*", b"a"));
        assert!(wildcard_match(b"*c", b"abc"));
        assert!(wildcard_match(b"a*b*c", b"aXbYbZc"));
        assert!(!wildcard_match(b"a*b*c", b"aXbYbZ"));
        assert!(!wildcard_match(b"abc", b"abcd"));
        assert!(!wildcard_match(b"", b"a"));
    }

    #[test]
    fn オリジンはそのままか先頭の星で比べる() {
        let cors = compiled(CorsConfig {
            allowed_origins: vec![
                "https://app.example.com".into(),
                "https://*.example.org".into(),
                "http://*.local.test:8080".into(),
            ],
            ..Default::default()
        });
        assert!(cors.origin_allowed("https://app.example.com"));
        // スキームとホストは大文字小文字を区別しない。
        assert!(cors.origin_allowed("HTTPS://APP.example.com"));
        assert!(!cors.origin_allowed("http://app.example.com"));
        assert!(!cors.origin_allowed("https://app.example.com:8443"));
        assert!(!cors.origin_allowed("https://app.example.com.evil.test"));

        assert!(cors.origin_allowed("https://a.example.org"));
        assert!(cors.origin_allowed("https://a.b.example.org"));
        // 星は 1 文字以上。親のドメインそのものには当たらない。
        assert!(!cors.origin_allowed("https://example.org"));
        assert!(!cors.origin_allowed("https://.example.org"));
        assert!(!cors.origin_allowed("https://evilexample.org"));
        assert!(!cors.origin_allowed("http://a.example.org"));
        assert!(!cors.origin_allowed("https://a.example.org:444"));
        assert!(!cors.origin_allowed("https://x/y.example.org"));

        assert!(cors.origin_allowed("http://dev.local.test:8080"));
        assert!(!cors.origin_allowed("http://dev.local.test"));
    }

    #[test]
    #[should_panic(expected = "allowed_origins に \"*\" を書けません")]
    fn 資格情報と全部のオリジンは同時に指定できない() {
        HandleCors::new(CorsConfig {
            supports_credentials: true,
            ..Default::default()
        });
    }

    #[test]
    #[should_panic(expected = "exposed_headers に \"*\" を書けません")]
    fn 資格情報と見せるヘッダーの星は同時に指定できない() {
        HandleCors::new(CorsConfig {
            supports_credentials: true,
            allowed_origins: vec!["https://app.example.com".into()],
            exposed_headers: vec!["*".into()],
            ..Default::default()
        });
    }

    #[test]
    fn 資格情報があってもメソッドとヘッダーの星は使える() {
        // 聞かれた値を返すので、矛盾にならない。
        HandleCors::new(CorsConfig {
            supports_credentials: true,
            allowed_origins: vec!["https://app.example.com".into()],
            ..Default::default()
        });
    }

    #[test]
    fn 使えないオリジンの書き方を断る() {
        for origin in [
            "https://app.*.com",
            "https://*example.com",
            "https://*.",
            "*.example.com",
            "https://*.*.example.com",
            "https://app.example.com/",
            "https://app.example.com/path",
        ] {
            let result = std::panic::catch_unwind(|| {
                HandleCors::new(CorsConfig {
                    allowed_origins: vec![origin.into()],
                    ..Default::default()
                })
            });
            assert!(result.is_err(), "{origin} は断るはず");
        }
    }

    #[test]
    #[should_panic(expected = "URL ではなく")]
    fn パスに_url_は書けない() {
        HandleCors::new(CorsConfig {
            paths: vec!["https://example.com/api/*".into()],
            ..Default::default()
        });
    }

    #[test]
    fn varyは足すだけで重ねない() {
        let response = add_vary(Response::text("x"), "Origin");
        assert_eq!(response.header("vary"), Some("Origin"));
        let response = add_vary(response, "origin");
        assert_eq!(response.header("vary"), Some("Origin"));
        let response = add_vary(
            Response::text("x")
                .with_added_header("vary", "Accept")
                .with_added_header("vary", "Cookie"),
            "Origin",
        );
        assert_eq!(response.header("vary"), Some("Accept, Cookie, Origin"));
        assert_eq!(
            response
                .headers()
                .iter()
                .filter(|(k, _)| k == "vary")
                .count(),
            1
        );
        let response = add_vary(Response::text("x").with_header("vary", "*"), "Origin");
        assert_eq!(response.header("vary"), Some("*"));
    }

    #[tokio::test]
    async fn 事前確認は先へ進めずに204() {
        let req = Request::new("OPTIONS", "/api/posts").with_headers(vec![
            ("origin".into(), "https://app.example.com".into()),
            ("access-control-request-method".into(), "PUT".into()),
            (
                "access-control-request-headers".into(),
                "authorization, content-type".into(),
            ),
        ]);
        let response = run(HandleCors::new(CorsConfig::default()), req).await;
        assert_eq!(response.status(), 204);
        assert_eq!(response.header("x-reached"), None);
        assert_eq!(response.header("access-control-allow-origin"), Some("*"));
        assert_eq!(response.header("access-control-allow-methods"), Some("PUT"));
        // `*` を返さず聞かれたものを返す。`Authorization` は `*` に含まれないため。
        assert_eq!(
            response.header("access-control-allow-headers"),
            Some("authorization, content-type")
        );
        // 0 も「覚えない」の意味で付ける（付けないとブラウザの既定の秒数になる）。
        assert_eq!(response.header("access-control-max-age"), Some("0"));
        assert_eq!(response.header("access-control-allow-credentials"), None);
        assert_eq!(
            response.header("vary"),
            Some("Origin, Access-Control-Request-Method, Access-Control-Request-Headers")
        );
    }

    #[tokio::test]
    async fn 並べたメソッドとヘッダーはそのまま返す() {
        let mw = HandleCors::new(CorsConfig {
            allowed_methods: vec!["get".into(), "POST".into()],
            allowed_headers: vec!["Content-Type".into(), "X-Requested-With".into()],
            max_age: 600,
            ..Default::default()
        });
        let req = Request::new("OPTIONS", "/api/posts").with_headers(vec![
            ("origin".into(), "https://app.example.com".into()),
            ("access-control-request-method".into(), "DELETE".into()),
        ]);
        let response = run(mw, req).await;
        assert_eq!(response.status(), 204);
        // 許すかどうかはブラウザが比べる（Laravel と同じ）。
        assert_eq!(
            response.header("access-control-allow-methods"),
            Some("GET, POST")
        );
        assert_eq!(
            response.header("access-control-allow-headers"),
            Some("Content-Type, X-Requested-With")
        );
        assert_eq!(response.header("access-control-max-age"), Some("600"));
    }

    #[tokio::test]
    async fn 事前確認でないoptionsは先へ進む() {
        let req = Request::new("OPTIONS", "/api/posts")
            .with_headers(vec![("origin".into(), "https://app.example.com".into())]);
        let response = run(HandleCors::new(CorsConfig::default()), req).await;
        assert_eq!(response.header("x-reached"), Some("1"));
        assert_eq!(response.header("access-control-allow-origin"), Some("*"));
        assert_eq!(
            response.header("vary"),
            Some("Access-Control-Request-Method")
        );
    }

    #[tokio::test]
    async fn 全部を許すときはオリジンが無くても星を返す() {
        let response = run(
            HandleCors::new(CorsConfig::default()),
            Request::new("GET", "/api/posts"),
        )
        .await;
        assert_eq!(response.header("access-control-allow-origin"), Some("*"));
        assert_eq!(response.header("vary"), None);
    }

    #[tokio::test]
    async fn 相手を選ぶときは許さない応答にもvaryを付ける() {
        let mw = HandleCors::new(CorsConfig {
            allowed_origins: vec!["https://app.example.com".into()],
            ..Default::default()
        });
        let req = Request::new("GET", "/api/posts")
            .with_headers(vec![("origin".into(), "https://evil.test".into())]);
        let response = run(mw, req).await;
        assert_eq!(response.header("x-reached"), Some("1"));
        assert_eq!(response.header("access-control-allow-origin"), None);
        assert_eq!(response.header("vary"), Some("Origin"));
    }
}
