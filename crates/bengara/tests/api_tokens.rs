//! API 用のトークン（Sanctum の個人用アクセストークン）を、アプリを組み立てて通しで確かめる。
//!
//! `StartSession`（ファイルに置く）と `VerifyCsrfToken` も入れて、トークンの要求が
//! セッションを作らないこと、CSRF の確認から外せることを見ます。
//! DB はメモリ上の SQLite です（`DB_TEST_DATABASE` の既定）。

#![cfg(feature = "sqlite")]

use std::path::PathBuf;

use bengara::auth::{
    CheckAbilities, CheckForAnyAbility, HasApiTokens, NewAccessToken, PersonalAccessToken,
};
use bengara::database::{Migration, Schema, DB};
use bengara::session::{FileStore, SessionConfig, StartSession, VerifyCsrfToken};
use bengara::testing::{refresh_database, TestClient, TestResponse};
use bengara::{
    Authenticatable, Authenticate, Carbon, Hooks, Model, Request, Response, Result, Route, Value,
};

/// 指示書の値。PHP で Sanctum 4 と同じ組み立てで作ったもの。
const LARAVEL_TOKEN: &str = "1|Kq3vX9mLr2TzP8wN5bYc1HdF7gJs4AeU6oRiVxQt20e242d5";
const LARAVEL_HASH: &str = "f68f7bda9d73a8af744b98f43bf30e6ee41bcd2fd7c256e1c35a6b253fccb345";

#[derive(Model)]
#[model(table = "users")]
struct User {
    id: i64,
    name: String,
    password: String,
}

impl Authenticatable for User {
    fn password_hash(&self) -> &str {
        &self.password
    }
}

/// 別の種類の持ち主。`App\Models\Admin` になる。
#[derive(Model)]
#[model(table = "users")]
struct Admin {
    id: i64,
    name: String,
    password: String,
}

impl Authenticatable for Admin {
    fn password_hash(&self) -> &str {
        &self.password
    }
}

/// セッションのファイルを置く場所。テストのバイナリごとに分ける。
fn session_dir() -> PathBuf {
    std::env::temp_dir().join(format!("bengara-api-tokens-{}", std::process::id()))
}

fn session_files() -> usize {
    std::fs::read_dir(session_dir())
        .map(|entries| entries.count())
        .unwrap_or(0)
}

async fn me(req: Request) -> Result<Response> {
    let auth = req.auth();
    let name = match auth.user::<User>().await? {
        Some(user) => user.name,
        None => "（なし）".to_string(),
    };
    Ok(Response::text(format!(
        "{}:{}:{}",
        auth.check(),
        auth.id().unwrap_or_default(),
        name
    )))
}

async fn can(req: Request) -> Result<Response> {
    let auth = req.auth();
    let token = auth.current_access_token().expect("トークンで認証している");
    Ok(Response::text(format!(
        "{}:{}:{}",
        token.name,
        auth.token_can("posts:write"),
        auth.token_cant("posts:delete")
    )))
}

/// セッションに書く。数え方が合っているかを確かめるための、トークンを使わない画面。
async fn remember(req: Request) -> Result<Response> {
    req.session().put("seen", "1");
    Ok(Response::text("ok"))
}

async fn ok(_req: Request) -> Result<Response> {
    Ok(Response::text("ok"))
}

/// いまのトークンだけを消す（その端末だけログアウト）。
async fn logout(req: Request) -> Result<Response> {
    if let Some(token) = req.auth().current_access_token() {
        token.delete().await?;
    }
    Ok(Response::text("bye"))
}

fn app() -> bengara::Application {
    bengara::Application::configure()
        .with_routing(|r| {
            r.web(|| {
                Route::get("/api/me", me).middleware("auth:sanctum");
                Route::get("/web/remember", remember);
                Route::get("/api/can", can).middleware("auth:sanctum");
                Route::post("/api/posts", ok)
                    .name("api.posts.store")
                    .middleware("auth:sanctum")
                    .middleware("abilities:posts:write,posts:publish");
                Route::get("/api/any", ok)
                    .middleware("auth:sanctum")
                    .middleware("ability:posts:read,posts:write");
                // トークンの確認を付け忘れた形。能力の確認だけでは通さない。
                Route::get("/api/forgot", ok).middleware("abilities:posts:write,posts:publish");
                Route::post("/api/logout", logout)
                    .name("api.logout")
                    .middleware("auth:sanctum");
            })
        })
        .with_middleware(|m| {
            m.append(StartSession::new(
                FileStore::new(session_dir(), 7200),
                SessionConfig::default(),
            ))
            .append(VerifyCsrfToken::new().except_route("api.*"))
            .alias("auth:sanctum", Authenticate::token())
            .alias(
                "abilities:posts:write,posts:publish",
                CheckAbilities::new(["posts:write", "posts:publish"]),
            )
            .alias(
                "ability:posts:read,posts:write",
                CheckForAnyAbility::new(["posts:read", "posts:write"]),
            )
        })
        .create()
}

fn test<F, Fut>(body: F)
where
    F: FnOnce(TestClient) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let hooks = Hooks {
        migrations: || MIGRATIONS,
        ..Hooks::default()
    };
    bengara::testing::run(hooks, app, body);
}

/// 利用者の表とトークンの表。トークンの表は利用者と同じく `define` で作る。
static MIGRATIONS: &[Migration] = &[Migration {
    name: "2026_01_01_000000_create_tables",
    up: create_tables,
    down: drop_tables,
}];

fn create_tables(schema: &mut Schema) {
    schema.create("users", |t| {
        t.id();
        t.string("name");
        t.string("password");
    });
    PersonalAccessToken::define(schema);
}

fn drop_tables(schema: &mut Schema) {
    schema.drop_if_exists(PersonalAccessToken::TABLE);
    schema.drop_if_exists("users");
}

/// 表を作り、利用者を 1 人入れる。札は受け取って持っておく。
async fn setup() -> (bengara::testing::DatabaseGuard, User) {
    let guard = refresh_database().await;
    // 置き場所が既にあれば、storage/ が無くても書ける（`FileStore` は無いときだけ作る）。
    std::fs::create_dir_all(session_dir()).expect("セッションの置き場所を作れる");
    let mut user = User {
        id: 0,
        name: "花子".into(),
        password: String::new(),
    };
    user.save().await.expect("保存できる");
    (guard, user)
}

async fn get_with(client: &TestClient, uri: &str, token: &str) -> TestResponse {
    let header = format!("Bearer {token}");
    client
        .send("GET", uri, Vec::new(), &[("authorization", &header)])
        .await
}

async fn post_with(client: &TestClient, uri: &str, token: &str) -> TestResponse {
    let header = format!("Bearer {token}");
    client
        .send("POST", uri, Vec::new(), &[("authorization", &header)])
        .await
}

async fn stored_hash(id: i64) -> Option<String> {
    DB::table(PersonalAccessToken::TABLE)
        .where_("id", id)
        .value::<String>("token")
        .await
        .unwrap()
}

async fn last_used_at(id: i64) -> Option<String> {
    DB::table(PersonalAccessToken::TABLE)
        .where_("id", id)
        .value::<Option<String>>("last_used_at")
        .await
        .unwrap()
        .flatten()
}

async fn create(user: &User, abilities: &[&str]) -> NewAccessToken {
    user.create_token("iPhone", abilities, None)
        .await
        .expect("トークンを作れる")
}

#[test]
fn 作ると平文はそのときだけ見え表にはハッシュだけが残る() {
    test(|_client| async move {
        let (_db, user) = setup().await;
        let new = create(&user, &["*"]).await;

        let (id, plain) = new.plain_text_token.split_once('|').expect("{id}|{平文}");
        assert_eq!(id, new.access_token.id.to_string());
        assert_eq!(plain.len(), 48, "乱数 40 文字＋crc32b 8 文字");

        let stored = stored_hash(new.access_token.id).await.expect("行がある");
        assert_eq!(stored.len(), 64);
        assert_ne!(stored, plain, "平文は残さない");
        let rows = DB::table(PersonalAccessToken::TABLE).get().await.unwrap();
        assert_eq!(rows.len(), 1);
        for column in PersonalAccessToken::COLUMNS {
            let value: Option<String> = rows[0].get(column).unwrap_or(None);
            assert!(
                !value.unwrap_or_default().contains(plain),
                "{column} に平文が入っている"
            );
        }

        // 持ち主と能力。
        let token = &new.access_token;
        assert_eq!(token.tokenable_type, "App\\Models\\User");
        assert_eq!(token.tokenable_id, user.id.to_string());
        assert_eq!(token.abilities, vec!["*".to_string()]);
        assert!(
            !format!("{new:?}").contains(plain),
            "Debug に平文を出さない"
        );

        // 保存していない利用者には作れない。
        let unsaved = User {
            id: 0,
            name: String::new(),
            password: String::new(),
        };
        assert!(unsaved.create_token("x", &["*"], None).await.is_err());
    });
}

#[test]
fn bearerで通りそのユーザーで認証済みになる() {
    test(|client| async move {
        let (_db, user) = setup().await;
        let new = create(&user, &["posts:write"]).await;
        assert_eq!(last_used_at(new.access_token.id).await, None);

        get_with(&client, "/api/me", &new.plain_text_token)
            .await
            .assert_ok()
            .assert_see(&format!("true:{}:花子", user.id));

        // last_used_at を書く。
        assert!(last_used_at(new.access_token.id).await.is_some());

        get_with(&client, "/api/can", &new.plain_text_token)
            .await
            .assert_ok()
            .assert_see("iPhone:true:true");
    });
}

#[test]
fn 通らないときはリダイレクトせず401() {
    test(|client| async move {
        let (_db, user) = setup().await;
        let new = create(&user, &["*"]).await;
        let (id, _) = new.plain_text_token.split_once('|').unwrap();

        // ヘッダーが無い。画面からの要求でも 401（転送しない）。
        client.get("/api/me").await.assert_status(401);
        // JSON が欲しい相手には JSON。
        let response = client
            .send(
                "GET",
                "/api/me",
                Vec::new(),
                &[("accept", "application/json")],
            )
            .await
            .assert_status(401);
        assert!(response.json().is_object(), "{}", response.body());

        // 形が違う。
        let basic = format!("Basic {}", new.plain_text_token);
        client
            .send("GET", "/api/me", Vec::new(), &[("authorization", &basic)])
            .await
            .assert_status(401);
        get_with(&client, "/api/me", "").await.assert_status(401);
        // 別の値・id を変えた値。
        get_with(&client, "/api/me", &format!("{id}|ちがう"))
            .await
            .assert_status(401);
        get_with(&client, "/api/me", "999|abc")
            .await
            .assert_status(401);
        get_with(&client, "/api/me", "abc|abc")
            .await
            .assert_status(401);
        get_with(&client, "/api/me", "ちがう")
            .await
            .assert_status(401);

        // 期限切れ。
        let expired = user
            .create_token("old", &["*"], Some(Carbon::now().sub_minutes(1)))
            .await
            .unwrap();
        get_with(&client, "/api/me", &expired.plain_text_token)
            .await
            .assert_status(401);
        // 期限の前なら通る。
        let later = user
            .create_token("later", &["*"], Some(Carbon::now().add_minutes(10)))
            .await
            .unwrap();
        get_with(&client, "/api/me", &later.plain_text_token)
            .await
            .assert_ok();

        // 消したトークン。
        get_with(&client, "/api/me", &new.plain_text_token)
            .await
            .assert_ok();
        assert_eq!(user.tokens().delete().await.unwrap(), 3);
        get_with(&client, "/api/me", &new.plain_text_token)
            .await
            .assert_status(401);
        get_with(&client, "/api/me", &later.plain_text_token)
            .await
            .assert_status(401);
    });
}

#[test]
fn いまのトークンだけを消せる() {
    test(|client| async move {
        let (_db, user) = setup().await;
        let phone = create(&user, &["*"]).await;
        let tablet = create(&user, &["*"]).await;

        // CSRF の確認から外しているので、トークンだけで POST できる。
        post_with(&client, "/api/logout", &phone.plain_text_token)
            .await
            .assert_ok();
        get_with(&client, "/api/me", &phone.plain_text_token)
            .await
            .assert_status(401);
        get_with(&client, "/api/me", &tablet.plain_text_token)
            .await
            .assert_ok();
        assert_eq!(user.tokens().count().await.unwrap(), 1);
    });
}

#[test]
fn 能力を確かめる() {
    test(|client| async move {
        let (_db, user) = setup().await;
        let both = create(&user, &["posts:write", "posts:publish"]).await;
        let write = create(&user, &["posts:write"]).await;
        let read = create(&user, &["posts:read"]).await;
        let all = create(&user, &["*"]).await;
        let none = create(&user, &[]).await;

        // CheckAbilities はすべて要る。
        post_with(&client, "/api/posts", &both.plain_text_token)
            .await
            .assert_ok();
        post_with(&client, "/api/posts", &write.plain_text_token)
            .await
            .assert_status(403);
        post_with(&client, "/api/posts", &all.plain_text_token)
            .await
            .assert_ok();

        // CheckForAnyAbility はどれか 1 つ。
        get_with(&client, "/api/any", &read.plain_text_token)
            .await
            .assert_ok();
        get_with(&client, "/api/any", &write.plain_text_token)
            .await
            .assert_ok();
        get_with(&client, "/api/any", &all.plain_text_token)
            .await
            .assert_ok();
        get_with(&client, "/api/any", &none.plain_text_token)
            .await
            .assert_status(403);

        // トークンで認証していなければ 401。
        get_with(&client, "/api/forgot", &both.plain_text_token)
            .await
            .assert_status(401);
        client.post("/api/posts", "").await.assert_status(401);
    });
}

#[test]
fn セッションのcookieもファイルも作らない() {
    test(|client| async move {
        let (_db, user) = setup().await;
        let new = create(&user, &["*"]).await;
        let before = session_files();

        let response = get_with(&client, "/api/me", &new.plain_text_token)
            .await
            .assert_ok();
        assert_eq!(response.header("set-cookie"), None);
        let response = post_with(&client, "/api/posts", &new.plain_text_token)
            .await
            .assert_ok();
        assert_eq!(response.header("set-cookie"), None);
        let response = get_with(&client, "/api/me", "ちがう")
            .await
            .assert_status(401);
        assert_eq!(response.header("set-cookie"), None);

        assert_eq!(
            session_files(),
            before,
            "セッションのファイルが増えていない"
        );

        // 数え方の確かめ。セッションに書く画面なら、Cookie もファイルもできる。
        let response = client.get("/web/remember").await.assert_ok();
        assert!(response.header("set-cookie").is_some());
        assert_eq!(session_files(), before + 1, "書けば増える");
    });
}

#[test]
fn laravelで発行したトークンをそのまま読める() {
    test(|client| async move {
        let (_db, user) = setup().await;
        let now = Carbon::now().to_date_time_string();
        DB::table(PersonalAccessToken::TABLE)
            .insert(&[
                ("id", Value::Int(1)),
                ("tokenable_type", Value::Text("App\\Models\\User".into())),
                ("tokenable_id", Value::Int(user.id)),
                ("name", Value::Text("laravel".into())),
                ("token", Value::Text(LARAVEL_HASH.into())),
                ("abilities", Value::Text(r#"["*"]"#.into())),
                ("created_at", Value::Text(now.clone())),
                ("updated_at", Value::Text(now)),
            ])
            .await
            .unwrap();

        get_with(&client, "/api/me", LARAVEL_TOKEN)
            .await
            .assert_ok()
            .assert_see("花子");

        // `|` の無い値（平文だけ）でも引ける。
        let (_, plain) = LARAVEL_TOKEN.split_once('|').unwrap();
        get_with(&client, "/api/me", plain)
            .await
            .assert_ok()
            .assert_see("花子");
        let found = PersonalAccessToken::find_token(plain)
            .await
            .unwrap()
            .expect("見つかる");
        assert_eq!(found.id, 1);
        assert_eq!(found.name, "laravel");
    });
}

#[test]
fn 持ち主の種類が違えばuserはnone() {
    test(|client| async move {
        let (_db, user) = setup().await;
        let admin = Admin::find(1)
            .await
            .unwrap()
            .expect("同じ行を Admin として読む");
        let new = admin.create_token("admin", &["*"], None).await.unwrap();
        assert_eq!(new.access_token.tokenable_type, "App\\Models\\Admin");

        // 認証は通るが、`User` としては読まない。
        get_with(&client, "/api/me", &new.plain_text_token)
            .await
            .assert_ok()
            .assert_see("true:1:（なし）");
        // `tokens()` も種類で分かれる。
        assert_eq!(user.tokens().count().await.unwrap(), 0);
        assert_eq!(admin.tokens().count().await.unwrap(), 1);
    });
}

#[test]
fn last_used_atは60秒以内なら書き直さない() {
    test(|client| async move {
        let (_db, user) = setup().await;
        let new = create(&user, &["*"]).await;
        let id = new.access_token.id;

        let set = |at: Carbon| async move {
            DB::table(PersonalAccessToken::TABLE)
                .where_("id", id)
                .update(&[("last_used_at", at.into())])
                .await
                .unwrap();
        };

        // 10 秒前に使ったばかり → 書かない。
        let recent = Carbon::now().sub_seconds(10);
        set(recent).await;
        get_with(&client, "/api/me", &new.plain_text_token)
            .await
            .assert_ok();
        assert_eq!(
            last_used_at(id).await.as_deref(),
            Some(recent.to_date_time_string().as_str())
        );

        // 2 分前 → 書く。
        let old = Carbon::now().sub_minutes(2);
        set(old).await;
        get_with(&client, "/api/me", &new.plain_text_token)
            .await
            .assert_ok();
        let written = last_used_at(id).await.expect("書いた");
        assert_ne!(written, old.to_date_time_string());
    });
}
