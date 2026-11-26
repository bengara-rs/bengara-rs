# 変更の履歴

bengara の、利用者に見える変更を書きます。

- 形は [Keep a Changelog 1.1.0](https://keepachangelog.com/ja/1.1.0/) です。見出しは日本語にしています（追加・変更・非推奨・削除・修正・セキュリティ）。
- 版の付け方は [docs/releases.md](docs/releases.md) にあります。
- 書くのは利用者に見える変更だけです。内部の作り直し・テストだけ・CI だけの変更は書きません。

## [Unreleased]

公開した版はまだありません。最初の版（0.1.0）には、主に次のものが入ります。

- Laravel と同じディレクトリ構成。`build.rs` がファイルを見つけて取り込むので、`mod` を書き並べない（[directory-structure.md](docs/directory-structure.md)）
- `cargo build --release` で実行ファイル 1 つ。`public/`・テンプレート・言語ファイルも中に入る（[deployment.md](docs/deployment.md)）
- `cargo artisan` のコマンド（`serve`・`migrate`・`queue:work`・`schedule:run` など）と自分のコマンド（[artisan.md](docs/artisan.md)）
- ルート・ミドルウェア・回数の制限・CORS・署名付き URL（[routing.md](docs/routing.md)・[middleware.md](docs/middleware.md)）
- リクエストとレスポンス、ファイルのアップロード、少しずつ読む本文（[requests-and-responses.md](docs/requests-and-responses.md)）
- 入力の検査（Laravel と同じ規則の文字列、`rules!`、`FormRequest`）（[validation.md](docs/validation.md)）
- セッション・CSRF（[session.md](docs/session.md)）
- 認証・認可・パスワードの再設定・API のトークン（Sanctum と同じ形）（[authentication.md](docs/authentication.md)・[authorization.md](docs/authorization.md)）
- SQLite / MySQL / MariaDB / PostgreSQL、クエリビルダ、モデル、マイグレーション、シーダー（[database.md](docs/database.md)・[models.md](docs/models.md)・[migrations.md](docs/migrations.md)）
- キャッシュ・ファイルの置き場所・キュー・定期処理・イベント・メール・外部コマンド・多言語（[docs/README.md](docs/README.md) の「周辺の機能」）
- WebSocket とチャンネルへの配信（機能フラグ `websocket`）（[websocket.md](docs/websocket.md)）
- テンプレート（機能フラグ `view`。Jinja 系）（[views.md](docs/views.md)）
- テストの道具（`#[bengara::test]`・テスト用のクライアント・データベースの作り直し）（[testing.md](docs/testing.md)）

### 追加

- 脆弱性の報告のしかたを [SECURITY.md](SECURITY.md) に書いた。
- 版の付け方と、壊さない約束の範囲を [docs/releases.md](docs/releases.md) に書いた。
- ログを 3 つの種類（アクセス・アプリ・メトリクス）に分け、`storage/logs/` に日ごとのテキスト（`access-`・`app-`・`metrics-YYYY-MM-DD.log`）を書くようにした（[logging.md](docs/logging.md)）。
- 機能フラグ `sqlite` があるとき、同じ記録を `storage/logs/logs-YYYY-MM-DD.sqlite`（表 `access`・`app`・`metrics`）にも書くようにした。
- 環境変数 `LOG_CHANNEL`（`daily` / `stdout`、既定 `daily`）と `LOG_DAILY_DAYS`（既定 14）を足した。`init` が作る `.env` にも入る。
- リクエスト ID を足した。すべての応答に `X-Request-Id` を付け、ログの `req=` / `request_id` にも書く。ハンドラからは `Request::request_id()` で読める。
  前段のプロキシ（`TRUSTED_PROXIES` で信頼したもの）が付けた `X-Request-Id` は、形が正しければ使う。
- `serve` が 60 秒ごとにメトリクス（応答の数・状態ごとの数・応答時間の分位・処理中の数・パニック・捨てたログの数など）をログに出すようにした。
- 機能フラグ `metrics-system` を足した。メトリクスにメモリー・CPU の使用率・`storage/` のディスクの空きを足す（Linux・macOS・Windows）。
- `about` に「ログ」の行を足した。

### 変更

- `.env` に `LOG_CHANNEL` が無いアプリは `daily` になり、`storage/logs/` に書き始める。標準出力だけにしたいときは `LOG_CHANNEL=stdout` と書く。
- 標準出力のログの形を変えた。時刻はミリ秒まで（`2026-11-26T02:51:00.123Z`）、欄は `key=value`、リクエストの中では末尾に `req=ID` が付く。
  アクセスの行の末尾にも `req=ID` が付く。
- 標準出力が端末でないとき（ファイルやパイプへ流すとき）と `NO_COLOR` があるときは、色を付けないようにした。
- `serve` の標準出力のログを、裏のスレッドで書くようにした。
- health（`.health("/up")`）が 200 を返したものは、アクセスのログに出さないようにした。
