//! ファイルのアップロード（`multipart/form-data`）を、アプリを組み立てて通しで確かめる。
//!
//! 置き場所は一時ディレクトリにします（`config/filesystems.rs` に当たる設定を `Hooks` で入れる）。

use bengara::session::{StartSession, VerifyCsrfToken};
use bengara::testing::{TestClient, TestResponse};
use bengara::{
    DiskConfig, Hooks, Registry, Request, Response, Result, Route, Storage, StorageConfig,
};

const BOUNDARY: &str = "----bengaraTestBoundary7MA4YWxk";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";

/// このテストの置き場所の根。プロセスごとに分けます。
fn storage_root() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("bengara-upload-test-{}", std::process::id()))
}

fn configs(registry: &mut Registry) {
    registry.set(StorageConfig {
        default: "local".into(),
        disks: vec![
            DiskConfig::new("local", storage_root().join("app").to_string_lossy()),
            DiskConfig::new("public", storage_root().join("public").to_string_lossy()),
        ],
    });
}

fn hooks() -> Hooks {
    Hooks {
        configs,
        ..Hooks::default()
    }
}

/// 欄 1 つ。`filename` があればファイルの欄です。
struct Part<'a> {
    name: &'a str,
    filename: Option<&'a str>,
    content: &'a [u8],
}

fn text<'a>(name: &'a str, value: &'a str) -> Part<'a> {
    Part {
        name,
        filename: None,
        content: value.as_bytes(),
    }
}

fn file<'a>(name: &'a str, filename: &'a str, content: &'a [u8]) -> Part<'a> {
    Part {
        name,
        filename: Some(filename),
        content,
    }
}

fn multipart(parts: &[Part<'_>]) -> Vec<u8> {
    let mut body = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        match part.filename {
            Some(filename) => body.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"{}\"; filename=\"{filename}\"\r\n\
                     Content-Type: image/png\r\n\r\n",
                    part.name
                )
                .as_bytes(),
            ),
            None => body.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"{}\"\r\n\r\n",
                    part.name
                )
                .as_bytes(),
            ),
        }
        body.extend_from_slice(part.content);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    body
}

fn content_type() -> String {
    format!("multipart/form-data; boundary={BOUNDARY}")
}

async fn send(
    client: &TestClient,
    uri: &str,
    parts: &[Part<'_>],
    extra: &[(&str, &str)],
) -> TestResponse {
    let content_type = content_type();
    let mut headers = vec![("content-type", content_type.as_str())];
    headers.extend_from_slice(extra);
    client.send("POST", uri, multipart(parts), &headers).await
}

// ---- ハンドラ ----

/// 検査してから、既定の置き場所と `public` に保存する。
async fn store(req: Request) -> Result<Response> {
    let input = req.validate(&[
        ("title", "required"),
        ("avatar", "required|image|mimes:png|max:1"),
    ])?;
    let Some(avatar) = req.file("avatar") else {
        return Ok(Response::new(500));
    };
    let local = avatar.store("avatars").await?;
    let named = avatar.store_as("avatars", "me.png").await?;
    let public = Storage::disk("public").put_file("avatars", avatar).await?;
    bengara::json(&bengara::serde_json::json!({
        "title": input.get("title"),
        "local": local,
        "named": named,
        "public": public,
        "client_name": avatar.client_original_name(),
        "extension": avatar.extension(),
        "size": avatar.size(),
    }))
}

/// 受け取ったものを見せる（保存しない）。
async fn inspect(req: Request) -> Result<Response> {
    let photos: Vec<String> = req
        .files("photos")
        .iter()
        .map(|f| f.client_original_name().to_string())
        .collect();
    bengara::json(&bengara::serde_json::json!({
        "inputs": req.input_all(),
        "has_avatar": req.has_file("avatar"),
        "photos": photos,
        "body_len": req.body().len(),
    }))
}

/// `.stream_body()` のルート。本文は読まずに、少しずつ読んだ長さだけを返す。
async fn raw(req: Request) -> Result<Response> {
    let has_file = req.has_file("avatar");
    let inputs = req.input_all().len();
    let mut stream = req.body_stream()?;
    let mut total = 0;
    while let Some(chunk) = stream.chunk().await? {
        total += chunk.len();
    }
    bengara::json(&bengara::serde_json::json!({
        "has_file": has_file,
        "inputs": inputs,
        "streamed": total,
    }))
}

async fn token(req: Request) -> Result<Response> {
    bengara::json(&bengara::serde_json::json!({ "token": req.csrf_token() }))
}

/// 前の画面。覚えた入力（`old`）を見せる。
async fn edit(req: Request) -> Result<Response> {
    let session = req.session();
    let old_count = ["title", "avatar", "_token"]
        .iter()
        .filter(|key| session.old(key).is_some())
        .count();
    bengara::json(&bengara::serde_json::json!({
        "title": session.old("title"),
        "avatar": session.old("avatar"),
        "all": session.all(),
        "old_count": old_count,
    }))
}

/// セッションと CSRF の確認を入れたアプリ。
fn web_app() -> bengara::Application {
    bengara::Application::configure()
        .with_routing(|r| {
            r.web(|| {
                Route::get("/token", token);
                Route::get("/profile/edit", edit);
                Route::post("/profile", store);
            })
        })
        .with_middleware(|m| {
            m.append(StartSession::memory())
                .append(VerifyCsrfToken::new())
        })
        .create()
}

/// ミドルウェアの無いアプリ。
fn plain_app() -> bengara::Application {
    bengara::Application::configure()
        .with_routing(|r| {
            r.web(|| {
                Route::post("/store", store);
                Route::post("/inspect", inspect);
                Route::post("/raw", raw).stream_body();
            })
        })
        .create()
}

fn test<F, Fut>(factory: fn() -> bengara::Application, body: F)
where
    F: FnOnce(TestClient) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    std::fs::create_dir_all(storage_root()).unwrap();
    bengara::testing::run(hooks(), factory, body);
}

#[test]
fn 送ったファイルを置き場所に保存する() {
    test(plain_app, |client| async move {
        let response = send(
            &client,
            "/store",
            &[text("title", "題"), file("avatar", "../../evil.html", PNG)],
            &[],
        )
        .await;
        let json = response.assert_ok().json();
        assert_eq!(json["title"], "題");
        // 送られてきた名前はパスを落として見せるが、保存には使わない。
        assert_eq!(json["client_name"], "evil.html");
        assert_eq!(json["extension"], "png");
        assert_eq!(json["size"], PNG.len());

        let local = json["local"].as_str().unwrap();
        assert!(
            local.starts_with("avatars/") && local.ends_with(".png"),
            "{local}"
        );
        assert_eq!(local.len(), "avatars/".len() + 44, "{local}");
        let saved = std::fs::read(storage_root().join("app").join(local)).unwrap();
        assert_eq!(saved, PNG);

        assert_eq!(json["named"], "avatars/me.png");
        assert!(storage_root().join("app/avatars/me.png").is_file());

        let public = json["public"].as_str().unwrap();
        assert!(
            storage_root().join("public").join(public).is_file(),
            "{public}"
        );
    });
}

#[test]
fn 文字の欄は入力に入りファイルは入らない() {
    test(plain_app, |client| async move {
        let response = send(
            &client,
            "/inspect?q=1",
            &[
                text("b", "2"),
                file("avatar", "a.png", PNG),
                text("a", "1"),
                file("photos[]", "1.png", PNG),
                file("photos[]", "2.png", PNG),
                // ファイルを選ばずに送った欄。
                file("empty", "", b""),
            ],
            &[],
        )
        .await;
        let json = response.assert_ok().json();
        // 本文の欄が送った順に並び、クエリが後ろに付く。ファイルの欄は入らない。
        assert_eq!(
            json["inputs"],
            bengara::serde_json::json!([["b", "2"], ["a", "1"], ["q", "1"]])
        );
        assert_eq!(json["has_avatar"], true);
        assert_eq!(
            json["photos"],
            bengara::serde_json::json!(["1.png", "2.png"])
        );
        // 本文そのものは今までどおり読める。
        assert!(json["body_len"].as_u64().unwrap() > 0);
    });
}

#[test]
fn 検査に落ちたら422() {
    test(plain_app, |client| async move {
        let accept = [("accept", "application/json")];
        // 画像でない（名前と MIME は png と偽っている）。
        let response = send(
            &client,
            "/store",
            &[
                text("title", "題"),
                file("avatar", "a.png", b"<svg onload=alert(1)>"),
            ],
            &accept,
        )
        .await;
        let json = response.assert_status(422).json();
        let errors = json["errors"]["avatar"].as_array().unwrap();
        assert!(
            errors.iter().any(|e| e.as_str().unwrap().contains("画像")),
            "{errors:?}"
        );

        // 大きすぎる（max:1 は 1 KB）。
        let mut big = PNG.to_vec();
        big.resize(1025, 0);
        let response = send(
            &client,
            "/store",
            &[text("title", "題"), file("avatar", "a.png", &big)],
            &accept,
        )
        .await;
        let json = response.assert_status(422).json();
        assert_eq!(
            json["errors"]["avatar"],
            bengara::serde_json::json!(["avatar は 1 KB 以下のファイルにしてください。"])
        );

        // 送らなかった。
        let response = send(&client, "/store", &[text("title", "題")], &accept).await;
        assert!(response.assert_status(422).json()["errors"]["avatar"].is_array());
    });
}

#[test]
fn multipartの中の_tokenでcsrfが通る() {
    test(web_app, |client| async move {
        let token = client.csrf_token("/token").await;
        let response = send(
            &client,
            "/profile",
            &[
                text("_token", &token),
                text("title", "題"),
                file("avatar", "a.png", PNG),
            ],
            &[],
        )
        .await;
        response.assert_ok();

        // `_token` が無ければ 419。
        let response = send(
            &client,
            "/profile",
            &[text("title", "題"), file("avatar", "a.png", PNG)],
            &[],
        )
        .await;
        response.assert_status(419);
    });
}

#[test]
fn 前の画面に戻すときoldにファイルの欄を入れない() {
    test(web_app, |client| async move {
        let token = client.csrf_token("/token").await;
        let response = send(
            &client,
            "/profile",
            &[
                text("_token", &token),
                text("title", "残す題"),
                file("avatar", "a.png", b"SECRET-FILE-CONTENT"),
            ],
            &[
                ("host", "localhost"),
                ("referer", "http://localhost/profile/edit"),
            ],
        )
        .await;
        response.assert_redirect("/profile/edit");

        let json = client.get("/profile/edit").await.assert_ok().json();
        assert_eq!(json["title"], "残す題");
        assert_eq!(json["avatar"], bengara::serde_json::Value::Null);
        assert!(!json.to_string().contains("SECRET-FILE-CONTENT"), "{json}");
        // `_token` も覚えない（`redact`）。覚えたのは題だけ。
        assert_eq!(json["old_count"], 1, "{json}");
    });
}

#[test]
fn stream_bodyのルートでは解析しない() {
    test(plain_app, |client| async move {
        let body = multipart(&[text("title", "題"), file("avatar", "a.png", PNG)]);
        let expected = body.len();
        let content_type = content_type();
        let response = client
            .send("POST", "/raw", body, &[("content-type", &content_type)])
            .await;
        let json = response.assert_ok().json();
        assert_eq!(json["has_file"], false);
        assert_eq!(json["inputs"], 0);
        assert_eq!(json["streamed"], expected);

        // 壊れた本文でも 400 にしない（読まないので）。
        let response = client
            .send(
                "POST",
                "/raw",
                b"not multipart".to_vec(),
                &[("content-type", &content_type)],
            )
            .await;
        response.assert_ok();
    });
}

#[test]
fn 壊れた本文は400_上限を超えたら413() {
    test(plain_app, |client| async move {
        let content_type = content_type();
        // 終わりの区切りが無い。
        let mut cut = multipart(&[text("title", "題")]);
        cut.truncate(cut.len() - 10);
        client
            .send("POST", "/inspect", cut, &[("content-type", &content_type)])
            .await
            .assert_status(400);
        // 境界が無い。
        client
            .send(
                "POST",
                "/inspect",
                multipart(&[text("title", "題")]),
                &[("content-type", "multipart/form-data")],
            )
            .await
            .assert_status(400);

        // ファイルは 20 個まで。
        let names: Vec<String> = (0..21).map(|i| format!("{i}.png")).collect();
        let parts: Vec<Part<'_>> = names
            .iter()
            .map(|name| file("photos[]", name, PNG))
            .collect();
        send(&client, "/inspect", &parts[..20], &[])
            .await
            .assert_ok();
        send(&client, "/inspect", &parts, &[])
            .await
            .assert_status(413);
    });
}
