# 変更の履歴

bengara の、利用者に見える変更を書きます。

- 形は [Keep a Changelog 1.1.0](https://keepachangelog.com/ja/1.1.0/) です。見出しは日本語にしています（追加・変更・非推奨・削除・修正・セキュリティ）。
- 版の付け方は [docs/releases.md](docs/releases.md) にあります。
- 書くのは利用者に見える変更だけです。内部の作り直し・テストだけ・CI だけの変更は書きません。

## [Unreleased]

公開した版はまだありません。最初の版（0.1.0-alpha.1。試験版）には、主に次のものが入ります。

- Laravel と同じディレクトリ構成。`build.rs` がファイルを見つけて取り込むので、`mod`
  を書き並べない（[directory-structure.md](docs/directory-structure.md)）
- `cargo build --release` で実行ファイル 1 つ。`public/`
  ・テンプレート・言語ファイルも中に入る（[deployment.md](docs/deployment.md)）
- `cargo artisan` のコマンド（`serve`・`migrate`・`queue:work`・`schedule:run`
  など）と自分のコマンド（[artisan.md](docs/artisan.md)）
- ルート・ミドルウェア・回数の制限・CORS・署名付き
  URL（[routing.md](docs/routing.md)・[middleware.md](docs/middleware.md)）
-

リクエストとレスポンス、ファイルのアップロード、少しずつ読む本文（[requests-and-responses.md](docs/requests-and-responses.md)）

- 入力の検査（Laravel と同じ規則の文字列、`rules!`、`FormRequest`）（[validation.md](docs/validation.md)）
- セッション・CSRF（[session.md](docs/session.md)）
- 認証・認可・パスワードの再設定・API のトークン（Sanctum
  と同じ形）（[authentication.md](docs/authentication.md)・[authorization.md](docs/authorization.md)）
- SQLite / MySQL / MariaDB /
  PostgreSQL、クエリビルダ、モデル、マイグレーション、シーダー（[database.md](docs/database.md)・[models.md](docs/models.md)・[migrations.md](docs/migrations.md)）
-

キャッシュ・ファイルの置き場所・キュー・定期処理・イベント・メール・外部コマンド・多言語（[docs/README.md](docs/README.md)
の「周辺の機能」）

- WebSocket とチャンネルへの配信（機能フラグ `websocket`）（[websocket.md](docs/websocket.md)）
- テンプレート（機能フラグ `view`。Jinja 系）（[views.md](docs/views.md)）
- テストの道具（`#[bengara::test]`・テスト用のクライアント・データベースの作り直し）（[testing.md](docs/testing.md)）

### 追加

- 脆弱性の報告のしかたを [SECURITY.md](SECURITY.md) に書いた。
- 版の付け方と、壊さない約束の範囲を [docs/releases.md](docs/releases.md) に書いた。
- ログを 3 つの種類（アクセス・アプリ・メトリクス）に分け、`storage/logs/` に日ごとのテキスト（`access-`・`app-`・
  `metrics-YYYY-MM-DD.log`）を書くようにした（[logging.md](docs/logging.md)）。
- 機能フラグ `sqlite` があるとき、同じ記録を `storage/logs/logs-YYYY-MM-DD.sqlite`（表 `access`・`app`・`metrics`
  ）にも書くようにした。
- 環境変数 `LOG_CHANNEL`（`daily` / `stdout`、既定 `daily`）と `LOG_DAILY_DAYS`（既定 14）を足した。`init` が作る `.env` にも入る。
- リクエスト ID を足した。すべての応答に `X-Request-Id` を付け、ログの `req=` / `request_id` にも書く。ハンドラからは
  `Request::request_id()` で読める。
  前段のプロキシ（`TRUSTED_PROXIES` で信頼したもの）が付けた `X-Request-Id` は、形が正しければ使う。
- `serve` が 60 秒ごとにメトリクス（応答の数・状態ごとの数・応答時間の分位・処理中の数・パニック・捨てたログの数など）をログに出すようにした。
- 機能フラグ `metrics-system` を足した。メトリクスにメモリー・CPU の使用率・`storage/` のディスクの空きを足す（Linux・macOS・Windows）。
- `about` に「ログ」の行を足した。
- 静的ファイルが範囲（`Range`、1 つだけ）に 206 で応えるようにした。範囲の外は 416。`Accept-Ranges: bytes` を付ける。iOS の
  Safari で `<video>` が再生できる。
- ディスクの静的ファイルに `Last-Modified` を付け、`If-Modified-Since` で 304 を返すようにした（`If-None-Match`
  があればそちらが先）。
- `Authenticate::provider::<U>()` を足した。通るたびに利用者がまだいるかを確かめ、トークンでは持ち主の種類（
  `tokenable_type`）も確かめる。
- `SessionConfig::from_env()` と、環境変数 `SESSION_SECURE_COOKIE`（Laravel と同じ名前）を足した。本番以外でもセッションの
  Cookie に `Secure` を付けられる。
- `SessionStore::read_and_touch_with_status` と `SessionStore::blocking` を足した。どちらも既定の実装があり、自作の置き場所はそのまま動く。
- `Broadcast::fake()` の間の `disconnect` を記録するようにした。`Broadcast::disconnected` と
  `Broadcast::assert_disconnected` で確かめる。
- 型付きの `f64` の列に、32 ビットまでの整数（リテラルも）を渡せるようにした。
- 移行の表名・列名・索引名を確かめ、使えない名前は SQL を流す前のエラーにした。

### 変更

- `.env` に `LOG_CHANNEL` が無いアプリは `daily` になり、`storage/logs/` に書き始める。標準出力だけにしたいときは
  `LOG_CHANNEL=stdout` と書く。
- 標準出力のログの形を変えた。時刻はミリ秒まで（`2026-11-26T02:51:00.123Z`）、欄は `key=value`、リクエストの中では末尾に
  `req=ID` が付く。
  アクセスの行の末尾にも `req=ID` が付く。
- 標準出力が端末でないとき（ファイルやパイプへ流すとき）と `NO_COLOR` があるときは、色を付けないようにした。
- `serve` の標準出力のログを、裏のスレッドで書くようにした。
- health（`.health("/up")`）が 200 を返したものは、アクセスのログに出さないようにした。
- 本文を受け取れなかった応答（413・400・408）も、共通のミドルウェアを通すようにした。CORS の見出しが付き、`with_exceptions`
  も効く。ルートのミドルウェアとハンドラは通らない。`VerifyCsrfToken` はこの要求を確かめずに通す。
- フォーム（urlencoded）の組と JSON の一番外の鍵が 1000 を超えたら 413 にした（multipart の欄の数と同じ上限）。
- ルートに当たらない要求では multipart を分けないようにした（壊れた multipart を無いパスへ送ると、400 ではなく 404）。
- `abort` / `abort_with` に 3xx を渡すと、警告を 1 行出すようにした（応答は変えない。転送は `redirect()` を使う）。
- `email` の規則を厳しくした。`a@b..cc`・`a@-x.com`・`a@x.com,b@y.com`・`a"b@x.com` などを落とす。日本語を含むアドレスは今までどおり通す。
- ルートに当たった読むだけの応答には、置き場所が期限を延ばしたときだけ `Set-Cookie` を付けるようにした（前は毎回）。
- `Session::put` / `flash` は、同じ鍵の持ち越しの flash を消すようにした。`all()` の勝ち方を `get()` にそろえた。
- `Hash::make_async` は、4096 バイトを超える平文に `Err` を返すようにした。
- 設定より重い保存値を照合したとき、`CheckAbilities::new([])` を作ったとき、同じ型の設定を 2 回登録したときに、警告を出すようにした。
- `queue:work` の予約の目印 `jobs.reserved_at` に、予約の期限を書くようにした。取り直す側の `--retry-after` を見ないので、値の違う
  worker が混ざっても動いているジョブを取り直さない。**前の版の worker を全部止めてから起動する**（前の版が処理中の行はすぐ取り直される）。
- `queue:work` と `schedule:run` の成功・失敗をログ（tracing）にも出すようにした（成功は INFO、失敗は ERROR）。
- `queue:work` も、標準出力のログを裏のスレッドで書くようにした。
- `queue:retry`（全部）は、始めたときにあった行まで 500 件ずつ戻すようにした。
- 停止のとき、クライアントが切断した後も走り続けるハンドラを待つようにした（`APP_SHUTDOWN_TIMEOUT` まで）。
- 2 回目の停止の合図で強制終了するとき、溜まったログを 1 秒まで書き切ってから終わるようにした。
- `where_raw` / `or_where_raw` / `having_raw` の SQL を括弧でくくって置くようにした（`or` が前後の条件の外へ漏れていた）。
- `where_in` / `where_not_in` の一覧の `None` を、`is null` / `is not null` に分けて書くようにした（前は必ず 0 件）。
- `where_between` / `where_not_between` の端の `None` と、`select` / `add_select` / `order_by_raw` の引用の外の `?`
  をエラーにした。
- SQLite の `like` / `not like` に `escape '\'` を足した。どのドライバでも `\` が逃がしの文字になる。
- SQLite の `DB_URL` の相対パスを、カレントディレクトリではなく基準ディレクトリから見るようにした。
- デバッグビルドでは `public/` と `resources/views/` を見張らないようにした。CSS や HTML を直しても、本体と artisan
  をコンパイルし直さない（リリースビルドは今までどおり）。
- 生成コードの一覧（マイグレーション・シーダー・ジョブ・コマンド・設定）を 1 件 1 行にし、行末に直し方のコメントを付けた。
- `config/app.rs` が無いときの `APP_URL` の既定を `http://localhost:8000` にした（`init` の雛形と同じ）。
- `cargo artisan list` は、本体をビルドする前と、ビルドできなかったときに 1 行ずつ知らせるようにした。artisan
  の一覧と重なるコマンドは、本体の一覧に出さない。
- `init`・`--version`・`--help` は、`.env`・ログ・設定を読まずに答えるようにした。

### 修正

- 2 つのプロセスが同じ日のログの SQLite を同時に開くと、片方が「database is locked」で開けず、その回のログを捨てていた。5
  秒まで待って開き直す。
- WebSocket の書き込みが詰まっている間に `close` / `Broadcast::disconnect` を呼ぶと、書きかけの 1
  通と列の残りを捨てていた。送ってから閉じる（列あふれのときは今までどおり捨てる）。
- Ping を送り続けて読まない WebSocket の相手に、Ping のたびに最大 5 秒止められていた。Pong を 5 秒送れなければ切る。
- WebSocket の停止の待ちが、閉じた後のクロージャの後始末を待たずに進んでいた。
- イベントの聞く側の、最初の `.await` より前のパニックが、`try_dispatch` の呼び出し元まで巻き戻っていた。
- 配信の中継（`database`）が、書いた側の時計のずれやコミットの遅れで抜けた id を飛ばし、届かない行が出ることがあった。
- `schedule:run` が、前回の時刻が少しだけ（間隔以内）未来のときにも動かしていた。時計のずれた 2 台で毎回二重に動いた。
- worker の結果の記録のやり直しが、停止の合図の後は待たずに続けて失敗し、成功したジョブがもう 1 回動くことがあった。
- 期限切れの予約を諦めた行が続くと、worker が `--sleep` だけ待っていた。
- 錠の置き場所 `+locks/` に、落ちたプロセスの一時ファイルが溜まっていた。`cache:prune` と `schedule:run` が 300
  秒より古いものを消す。
- 日付の読み取りが年 0000 を通していた。
- `session:gc` が、まだ効く「切った」印を先に消していた。印に期限を書き、その期限で消す。
- ほかの端末を切ったときにファイルを消せなかったセッションが、期限まで読めていた。
- 処理中に切られていたと分かったとき、ブラウザに Cookie が残っていた。消す Cookie を返す。
- `PasswordReset::consume_if_valid` が期限を文字で比べていたので、`T` 区切りや時差つきの `created_at` だと期限切れでも通った。
- 失敗した後の `Transaction::rollback()` がエラーを返し、元のエラーを隠していた。`Ok(())` を返す。
- `group_by` と `distinct()` を両方付けた `count()` / `paginate` の総数が、`get()` の行数と合わなかった。
- PostgreSQL の `insert_get_id_as("t.id")` が、表の名前付きの `returning` を出していた。
- SQLite の `do update` を含む文で、`Affected.last_insert_id` に古い番号が入ることがあった（`None` にする）。
- 演算子を照合した形と SQL に書く形が違い、全角の空白入りの演算子で壊れた SQL になっていた。
- 名前付きの接続の初回が同時に来ると、つなぐ処理が人数分走っていた。
- `migrate` 系がパニック・Ctrl+C で札を残していた。`migrate:unlock` の後、前のプロセスが新しい札を消すこともあった。
- 小文字の 16 進で書いたルート（`Route::get("/caf%c3%a9")`）が必ず 404 だった。
- 署名付き URL が、途中のプロキシによる `%xx` の大文字小文字の書き換えで 403 になっていた（前の小文字のリンクも通る）。
- `Registered::name()` が、呼んだ時点のグループの名前を頭に付けていた。
- Cookie の値を囲む `"` を外していなかった（RFC 6265）。
- `init` が `src/main.rs` を消してから `Cargo.toml` を書いていた。書けないと、入口の無いプロジェクトが残った。
- パッケージ名が `artisan` のとき、`init` が同じ名前の `[[bin]]` を 2 つ書いていた。何も書かずに止まる。
- 空の `CARGO_MANIFEST_DIR` を基準ディレクトリにしていた。
- `.env` を読む前に `storage/` の場所が決まったとき、`.env` の `APP_STORAGE_PATH` を黙って無視していた。警告を出す。
- `key:generate` が、`APP_KEY=${後の行の変数}` を「設定済み」と見ていた。
- README と getting-started の `[patch.crates-io]` のパスを `../bengara-rs/` に直した。

### セキュリティ

- Cookie の名前の `%xx` を戻して読んでいたので、`%5F%5FHost-sid` が `__Host-sid` として読めた（`__Host-`
  の決まりをすり抜けられた）。名前は届いたまま読む。書く側は逃がして警告を出す。
- multipart の見出しで、値の無い引数・閉じの `"` の後ろの文字・2 本目の `Content-Disposition`
  ・折り返した行を読んでいた。重複の確認をすり抜けて、ファイルが文字の欄に化けた。どれも 400 にする。`filename*` の `%+F`
  も通さない。
- `FileCache` を直接呼ぶと、鍵 `../x` で置き場所の外を読み書きできた。`normalize_key` の出した形の鍵だけを受け取る。
