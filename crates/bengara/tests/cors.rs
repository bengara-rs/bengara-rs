//! CORS のミドルウェア（`HandleCors`）を、アプリを組み立てて通しで確かめる。
//!
//! `StartSession` と `VerifyCsrfToken` も入れて、`prepend` で先頭に置いたときの動きを見ます。
//! `#[bengara::test]` と同じ入口（`bengara::testing::run`）で、テスト用の `APP_KEY` を入れて動かします。

use bengara::session::{StartSession, VerifyCsrfToken};
use bengara::testing::{TestClient, TestResponse};
use bengara::{abort, CorsConfig, Error, HandleCors, Hooks, Request, Response, Result, Route};

const ORIGIN: &str = "https://app.example.com";

async fn index(_req: Request) -> Result<Response> {
    Ok(Response::text("一覧"))
}

/// セッションに書く。書くと `StartSession` が Cookie を返す（空のままだと返さない）。
async fn remember(req: Request) -> Result<Response> {
    req.session().put("seen", "1");
    Ok(Response::text("一覧"))
}

async fn invalid(_req: Request) -> Result<Response> {
    abort(422)
}

async fn broken(_req: Request) -> Result<Response> {
    Err(Error::msg("壊れた"))
}

/// 設定を変えてアプリを組む。`HandleCors` を先頭に、セッションと CSRF を後ろに置く。
fn build(config: CorsConfig) -> bengara::Application {
    bengara::Application::configure()
        .with_routing(|r| {
            r.web(|| {
                Route::get("/api/items", index);
                Route::post("/api/items", index);
                Route::get("/api/invalid", invalid);
                Route::get("/api/broken", broken);
                Route::get("/web", index);
                Route::post("/api/small", index).max_body(4);
            })
        })
        .with_middleware(move |m| {
            m.append(StartSession::memory())
                .append(VerifyCsrfToken::new())
                .prepend(HandleCors::new(config))
        })
        .create()
}

/// 既定の設定（`config/cors.rs` が無いとき）。全部のオリジンを許す。
fn default_app() -> bengara::Application {
    bengara::Application::configure()
        .with_routing(|r| {
            r.web(|| {
                Route::get("/api/items", remember);
            })
        })
        .with_middleware(|m| {
            m.append(StartSession::memory())
                .prepend(HandleCors::from_config())
        })
        .create()
}

/// 相手を 1 つに絞り、Cookie 付きの呼び出しを許す設定。
fn credentials_app() -> bengara::Application {
    build(CorsConfig {
        allowed_origins: vec![ORIGIN.into()],
        exposed_headers: vec!["X-Total".into()],
        max_age: 600,
        supports_credentials: true,
        ..Default::default()
    })
}

fn test<F, Fut>(factory: fn() -> bengara::Application, body: F)
where
    F: FnOnce(TestClient) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    bengara::testing::run(Hooks::default(), factory, body);
}

async fn preflight(client: &TestClient, uri: &str, origin: &str) -> TestResponse {
    client
        .send(
            "OPTIONS",
            uri,
            Vec::new(),
            &[
                ("origin", origin),
                ("access-control-request-method", "PUT"),
                ("access-control-request-headers", "authorization, x-custom"),
            ],
        )
        .await
}

async fn get_from(client: &TestClient, uri: &str, origin: &str) -> TestResponse {
    client
        .send("GET", uri, Vec::new(), &[("origin", origin)])
        .await
}

#[test]
fn 事前確認は204と見出しを返す() {
    test(default_app, |client| async move {
        let response = preflight(&client, "/api/items", ORIGIN).await;
        response
            .assert_status(204)
            .assert_header("access-control-allow-origin", "*")
            .assert_header("access-control-allow-methods", "PUT")
            .assert_header("access-control-allow-headers", "authorization, x-custom")
            .assert_header("access-control-max-age", "0");
    });
}

#[test]
fn ルートの無いパスへの事前確認も204() {
    test(default_app, |client| async move {
        // `PUT /api/items` も `/api/nothing` も無いが、405・404 にしない。
        preflight(&client, "/api/nothing", ORIGIN)
            .await
            .assert_status(204)
            .assert_header("access-control-allow-origin", "*");
    });
}

#[test]
fn 事前確認にはセッションのクッキーを返さない() {
    test(default_app, |client| async move {
        let response = preflight(&client, "/api/items", ORIGIN).await;
        assert_eq!(response.header("set-cookie"), None);
        assert_eq!(client.cookie("bengara_session"), None);

        // 比べるため。普通の要求は `StartSession` まで届き、Cookie を受け取る。
        get_from(&client, "/api/items", ORIGIN).await.assert_ok();
        assert!(client.cookie("bengara_session").is_some());
    });
}

#[test]
fn 許されないオリジンには見出しを付けない() {
    test(credentials_app, |client| async move {
        let response = preflight(&client, "/api/items", "https://evil.test").await;
        assert_eq!(response.status(), 204);
        assert_eq!(response.header("access-control-allow-origin"), None);
        assert_eq!(response.header("access-control-allow-methods"), None);
        assert_eq!(response.header("access-control-allow-credentials"), None);

        let response = get_from(&client, "/api/items", "https://evil.test").await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.header("access-control-allow-origin"), None);
        // 相手ごとに答えが変わるので、キャッシュ向けに付ける。
        assert!(response.header("vary").unwrap().contains("Origin"));
    });
}

#[test]
fn 対象外のパスは素通り() {
    test(default_app, |client| async move {
        let response = get_from(&client, "/web-nothing", ORIGIN).await;
        assert_eq!(response.status(), 404);
        assert_eq!(response.header("access-control-allow-origin"), None);

        // 対象外のパスの OPTIONS は、事前確認として扱わない（ルートが無いので 404）。
        let response = preflight(&client, "/web-nothing", ORIGIN).await;
        assert_eq!(response.status(), 404);
        assert_eq!(response.header("access-control-allow-origin"), None);
    });
}

#[test]
fn 資格情報を許すときは要求の値をそのまま返す() {
    test(credentials_app, |client| async move {
        preflight(&client, "/api/items", ORIGIN)
            .await
            .assert_status(204)
            .assert_header("access-control-allow-origin", ORIGIN)
            .assert_header("access-control-allow-credentials", "true")
            .assert_header("access-control-allow-methods", "PUT")
            .assert_header("access-control-allow-headers", "authorization, x-custom")
            .assert_header("access-control-max-age", "600");

        get_from(&client, "/api/items", ORIGIN)
            .await
            .assert_ok()
            .assert_header("access-control-allow-origin", ORIGIN)
            .assert_header("access-control-allow-credentials", "true")
            .assert_header("access-control-expose-headers", "X-Total");
    });
}

#[test]
fn 本文を受け取れなかった応答にも見出しが付く() {
    // 以前は本文のエラーがミドルウェアを通らず、ブラウザの JavaScript から 413 が読めなかった。
    // `VerifyCsrfToken` も通るが、トークンが無くても 419 にはしない（ハンドラは動かない）。
    test(credentials_app, |client| async move {
        let response = client
            .send(
                "POST",
                "/api/small",
                b"12345".to_vec(),
                &[("origin", ORIGIN)],
            )
            .await;
        assert_eq!(response.status(), 413);
        assert_eq!(response.header("access-control-allow-origin"), Some(ORIGIN));
        // 本文を受け取れたら、今までどおり CSRF の確認で 419。
        let response = client
            .send(
                "POST",
                "/api/small",
                b"1234".to_vec(),
                &[("origin", ORIGIN)],
            )
            .await;
        assert_eq!(response.status(), 419);
        assert_eq!(response.header("access-control-allow-origin"), Some(ORIGIN));
    });
}

#[test]
fn エラーの応答にも見出しが付く() {
    test(credentials_app, |client| async move {
        for (method, uri, status) in [
            ("GET", "/api/missing", 404),
            ("DELETE", "/api/items", 405),
            // `VerifyCsrfToken` より前にいるので、419 にも付く。
            ("POST", "/api/items", 419),
            ("GET", "/api/invalid", 422),
            ("GET", "/api/broken", 500),
        ] {
            let response = client
                .send(method, uri, Vec::new(), &[("origin", ORIGIN)])
                .await;
            assert_eq!(response.status(), status, "{method} {uri}");
            assert_eq!(
                response.header("access-control-allow-origin"),
                Some(ORIGIN),
                "{method} {uri}"
            );
            assert_eq!(
                response.header("access-control-allow-credentials"),
                Some("true"),
                "{method} {uri}"
            );
        }
    });
}
