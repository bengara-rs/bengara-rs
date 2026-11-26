# ログ

bengara が残すログと、その読み方です。本番で何が起きたかを、後から追えるようにします。

- アプリからは、これまでどおり `tracing::info!` などで書きます（`bengara::tracing`）。
- Laravel の `Log` ファサードはありません（[laravel-differences.md](laravel-differences.md)）。

```rust
bengara::tracing::warn!(user_id = user.id, "メールを送れませんでした");
```

## 何が残るか

ログには 3 つの種類があります。

| 種類       | 中身                                       | 見分け方（tracing の target） |
|------------|--------------------------------------------|-------------------------------|
| アクセス   | 1 リクエストに 1 件                        | `bengara::access`             |
| メトリクス | 60 秒に 1 件（[下](#メトリクス)）          | `bengara::metrics`            |
| アプリ     | 上の 2 つ以外の全部（フレームワークとアプリ） | それ以外                   |

- アクセスとメトリクスを出すのは `serve` だけです。テストの道具（`TestClient`）から送ったものは出ません。
- health（`.health("/up")`）が 200 を返したものは、アクセスに出しません。ロードバランサーの確認で埋まるためです。200 以外は出します。

## 出し先と LOG_CHANNEL

出し先は `LOG_CHANNEL` で選びます。3 つの種類は、いつも同じ出し先に出ます。

| `LOG_CHANNEL`    | 標準出力 | 日ごとのテキスト | SQLite                              |
|------------------|----------|------------------|-------------------------------------|
| `daily`（既定）  | 出す     | 出す             | 出す（機能フラグ `sqlite` があるとき） |
| `stdout`         | 出す     | 出さない         | 出さない                            |

- 読めない値（Laravel の `stack` など）は、警告を出して `daily` にします。
- `LOG_CHANNEL` を書いていないアプリも `daily` です。`storage/logs/` に書き始めます。
- 場面ごとの違いは次のとおりです。

| 場面                                     | 標準出力           | テキストと SQLite                  |
|------------------------------------------|--------------------|------------------------------------|
| `serve`・`queue:work`                    | 裏のスレッドで書く | `LOG_CHANNEL` のとおり             |
| ほかのコマンド（`migrate`・自作など）    | その場で書く       | `LOG_CHANNEL` のとおり             |
| `init`・`version`・`help`                | 出さない           | 出さない（`.env` もログも始めない） |
| `#[bengara::test]`                       | その場で書く       | 出さない（`LOG_CHANNEL` を見ない） |

- テキストと SQLite は、`storage/` の場所が決まった後に始めます。それより前のログ（`.env` の警告など）は標準出力にだけ出ます。
- `storage/logs/` が無いときは作りません。警告を 1 回出し、標準出力だけで続けます（`storage:init` で作れます）。
- 標準出力に色が付くのは、端末に出していて、`NO_COLOR` が無いか空のときだけです。ファイルへ流すと色は付きません。

## ファイルの場所と名前

| ファイル                                | 中身                                         |
|-----------------------------------------|----------------------------------------------|
| `storage/logs/access-YYYY-MM-DD.log`    | アクセス                                     |
| `storage/logs/app-YYYY-MM-DD.log`       | アプリ                                       |
| `storage/logs/metrics-YYYY-MM-DD.log`   | メトリクス                                   |
| `storage/logs/logs-YYYY-MM-DD.sqlite`   | 3 種類とも（表 `access`・`app`・`metrics`）。`-wal` と `-shm` も同じ場所にできる |

- `storage/` は `APP_STORAGE_PATH` で変えられます（[configuration.md](configuration.md)）。
- 日付は記録の時刻（**UTC**）の日付です。日をまたいだら次の日のファイルに書きます。名前の付け替え（ローテート）はしません。
- 文字は UTF-8、改行は LF です。1 件が 1 行です。
- メールのログ（`mail.log`）はこれとは別です（[mail.md](mail.md)）。

## 行の形

時刻はどれも UTC で、ミリ秒まで書きます（`2026-11-26T02:51:00.123Z`）。

### 標準出力

```text
2026-11-26T02:51:00.123Z  WARN メールを送れませんでした error="connection refused" req=4f1c0d2e9a7b4c55a1e3f0b2c4d6e8f0
2026-11-26T02:51:00.135Z  INFO GET /reset/*** -> 200 (12 ms) req=4f1c0d2e9a7b4c55a1e3f0b2c4d6e8f0
2026-11-26T02:52:00.000Z  INFO metrics pid=4123 port=8001 uptime_seconds=60 requests=5320 …
```

| 種類       | 形                                                                      |
|------------|-------------------------------------------------------------------------|
| アプリ     | `{時刻} {水準} {メッセージ}{ key=value …}{ req=ID}`                     |
| アクセス   | `{時刻}  INFO {メソッド} {行き先} -> {ステータス} ({ミリ秒} ms) req={ID}` |
| メトリクス | `{時刻}  INFO metrics {下の logfmt から time= を除いたもの}`            |

- 水準は `ERROR` `WARN` `INFO` `DEBUG` `TRACE` を右寄せ 5 文字で書きます。
- メッセージの中の改行と制御文字は `\n` `\t` `\u0001` の形にします。`"` と `\` はそのままです。
- `key=value` はイベントの欄を書いた順に並べます。値に空白・`"`・`=`・制御文字があるときは `"…"`（JSON の文字列の形）で囲みます。
- `req=ID` は、リクエストの中で出たイベントにだけ付きます。

### アクセス（テキスト）

nginx の combined 形式と先頭をそろえています。今ある解析の道具で読めます。

```text
203.0.113.9 - - [26/Nov/2026:02:51:00 +0000] "GET /reset/*** HTTP/1.1" 200 1532 "-" "Mozilla/5.0 (X11; Linux x86_64)" rt=0.012 req=4f1c0d2e9a7b4c55a1e3f0b2c4d6e8f0 route=password.reset
```

| 欄         | 値                                                                          |
|------------|-----------------------------------------------------------------------------|
| IP         | 元の相手のアドレス（`TRUSTED_PROXIES` で信頼した前段のときは転送元）。分からなければ `-` |
| 時刻       | UTC。時差は常に `+0000`                                                     |
| 行き先     | 秘密を伏せた後のパスとクエリ（下の「伏せるもの」）                          |
| プロトコル | `HTTP/1.0` か `HTTP/1.1`                                                    |
| バイト数   | 送った本文のバイト数。HEAD と本文の無い応答は 0                             |
| Referer    | いつも `-`。ほかのサイトの URL に秘密が入りうるので残さない                 |
| User-Agent | 先頭 512 バイトまで。無ければ `-`                                           |
| `rt`       | 秒（小数 3 桁）                                                             |
| `req`      | リクエスト ID                                                               |
| `route`    | ルート名。無ければ `-`                                                      |

- `"…"` の中の `"` と `\` と制御文字は `\xHH` にします。1 件が必ず 1 行に収まります。

### アプリ（テキスト）

Laravel のログと同じ形です。

```text
[2026-11-26T02:51:00.123Z] production.WARNING: メールを送れませんでした {"target":"bengara::mail","request_id":"4f1c0d2e9a7b4c55a1e3f0b2c4d6e8f0","error":"connection refused"}
```

- `production` の所は、ログを始めたときの環境変数 `APP_ENV` です（無ければ `production`）。
- 水準は `ERROR` `WARNING` `INFO` `DEBUG` `TRACE`。WARN は Laravel に合わせて `WARNING` です。
- メッセージの中の改行などは、JSON の文字列と同じ規則で逃がします。
- 最後は文脈の JSON です。鍵の順は `target` → `request_id`（リクエストの中だけ）→ イベントの欄（書いた順）。数と真偽はそのまま、ほかは文字列です。

### メトリクス（テキスト）

logfmt（`key=value` を空白で並べた形）です。

```text
time=2026-11-26T02:52:00.000Z pid=4123 port=8001 uptime_seconds=60 requests=5320 status_1xx=0 status_2xx=5200 status_3xx=80 status_4xx=40 status_5xx=0 status_408=0 status_413=0 status_419=1 status_429=3 p50_ms=1 p95_ms=5 p99_ms=10 max_ms=31.204 inflight=2 panics=0 websockets=14 dropped=0 rss_bytes=10289152 cpu_percent=3.2 disk_free_bytes=44567883776 disk_total_bytes=105088212992
```

- 鍵の名前と順は、SQLite の `metrics` 表の列と同じです。
- 値が無いものは、鍵ごと書きません（機能フラグが無い・OS が対応していない・リクエストが 0 件のときの分位）。

### 伏せるもの

- 秘密になりうる名前のクエリの値は `***` に伏せます（`?signature=***`・`?token=***` など）。
- 同じ名前のパス引数も伏せます。`/reset/{token}` のルートは `/reset/***` と出ます。
- 伏せる名前は、`old()` が覚えない項目と同じです（[validation.md](validation.md)）。
- ルートに当たらなかったパス（404・405）は、届いたまま出ます（クエリは伏せます）。

## SQLite

機能フラグ `sqlite` があるとき、テキストと同じ記録を 1 日 1 つのファイルにも書きます。

| 表        | 列                                                                                         |
|-----------|--------------------------------------------------------------------------------------------|
| `access`  | `id` `time` `request_id` `ip` `method` `path` `protocol` `status` `bytes` `duration_us` `route` `user_agent` `pid` |
| `app`     | `id` `time` `level` `target` `message` `context`（テキストと同じ JSON） `request_id` `pid` |
| `metrics` | `id` `time` と、メトリクスの鍵と同じ名前の列（値が無いものは NULL）                        |

- `time` は `YYYY-MM-DDTHH:MM:SS.mmmZ` の文字列です。`pid` は書いたプロセスの番号です。
- 分からない値（`ip`・`route`・`user_agent`・リクエストの外の `request_id`）は NULL です。
- 表の版は `PRAGMA user_version`（いまは 1）です。新しい版の bengara が作ったファイル（版が 2 以上）には書きません。

問い合わせの例です。

```sql
-- エラーを新しい順に
SELECT time, message, context FROM app WHERE level = 'ERROR' ORDER BY id DESC LIMIT 50;

-- 500 を返したリクエストと、そのときのアプリのログ
SELECT a.time, a.method, a.path, p.level, p.message
FROM access AS a JOIN app AS p ON p.request_id = a.request_id
WHERE a.status >= 500;

-- 遅いリクエスト
SELECT time, method, path, duration_us / 1000.0 AS ms FROM access ORDER BY duration_us DESC LIMIT 20;
```

## 残す日数

`LOG_DAILY_DAYS`（既定 14）で決めます。

- 今日（UTC）を含めて、その日数分を残します。`0` は消しません。
- 消すのは、名前が正確に合うものだけです（`access-` `app-` `metrics-` の `.log` と、`logs-` の `.sqlite`・`-wal`・`-shm`）。
  `mail.log` や、桁の違う日付のファイルには触りません。
- 消すのは、書き始めたときと、日をまたいだときです。
- 読めない値は、警告を出して 14 にします。

## 絞り込み

細かさは `RUST_LOG` で決めます。全部の出し先に同じように効きます。

| `RUST_LOG`                            | 動き                                        |
|---------------------------------------|---------------------------------------------|
| 無い                                  | `APP_DEBUG` が真なら `debug`、偽なら `info` |
| `trace` `debug` `info` `warn` `error` | その細かさで出す                            |
| `off`                                 | 何も出さない                                |
| `info,sqlx=debug`                     | 流した SQL も出す（既定では出さない）       |
| `myapp=debug`                         | 自分のアプリだけ細かく出す                  |
| `info,bengara::access=off`            | アクセスのログを止める                      |
| `info,bengara::metrics=off`           | メトリクスのログを止める                    |

- 下の 4 つの書き方には、機能フラグ `log-filter` が要ります（既定で入っています）。
- 何も書かないときは `info,hyper=warn,tower=warn,sqlx=warn`（`APP_DEBUG` が真なら `debug,…`）です。
- ログの細かさは、**環境変数**の `APP_DEBUG` で決まります。`config/app.rs` の `debug` を変えても変わりません。
  ログの準備を、`config/` を読む前に済ませるためです。

## リクエスト ID

リクエストごとに ID を決め、応答とログに付けます。

| 項目       | 中身                                                                                 |
|------------|--------------------------------------------------------------------------------------|
| 応答       | すべての応答に `X-Request-Id` を付けます（404・413・419・500・静的ファイルも）       |
| 作り方     | 32 文字の小文字の 16 進。同じプロセスの中で重なりません                              |
| 受け取る   | 前段のプロキシ（`TRUSTED_PROXIES` で信頼したもの）が付けた `X-Request-Id` で、`[0-9A-Za-z._-]` の 1〜64 文字のときだけ。何本も来たら最初の 1 本 |
| ハンドラから | `req.request_id()`（[requests-and-responses.md](requests-and-responses.md)）        |
| ログ       | アクセスの `req=`、アプリの `req=` と `request_id`                                   |

- ハンドラとミドルウェアの中で出したログには、自動で ID が付きます。
- 信頼していない相手が送った `X-Request-Id` は使いません。相手が自由に決めた値をログに混ぜないためです。

## メトリクス

`serve` が 60 秒ごとに 1 件出します。最初の 1 件は始めてから 60 秒後です。間隔は変えられません。

| 鍵                                              | 意味                                                          |
|-------------------------------------------------|---------------------------------------------------------------|
| `pid` `port` `uptime_seconds`                   | プロセスの番号・待ち受けのポート・`serve` を始めてからの秒   |
| `requests`                                      | 区間（前の 1 件から）の応答の数。health も数える              |
| `status_1xx`〜`status_5xx`                      | 区間の、状態の種類ごとの数                                    |
| `status_408` `status_413` `status_419` `status_429` | 区間の、目立たせたい状態ごとの数（待ちきれない・大きすぎる・CSRF・回数の制限） |
| `p50_ms` `p95_ms` `p99_ms`                      | 応答時間の分位（ミリ秒）。区間に 0 件なら無し                 |
| `max_ms`                                        | 区間の最大の応答時間（ミリ秒）                                |
| `inflight`                                      | いま処理中の数                                                |
| `panics`                                        | 区間にパニックしたハンドラの数                                |
| `websockets`                                    | いまつながっている WebSocket の数（機能フラグ `websocket` のときだけ） |
| `dropped`                                       | 区間に、溢れて捨てたログと書けなかったログの数               |
| `rss_bytes`                                     | プロセスが使っているメモリー（機能フラグ `metrics-system`）   |
| `cpu_percent`                                   | 区間の CPU の使用率（%）。コア数で割らないので 100 を超えることがある（`metrics-system`） |
| `disk_free_bytes` `disk_total_bytes`            | `storage/` のあるディスクの空きと全体（`metrics-system`）     |

- 分位は、応答時間を 19 の枠（0.1 ms〜60 秒と、それより上）に分けて数え、その枠の上限を出します。いちばん上の枠なら最大値です。
- 数えるのは本物の HTTP だけです。
- `metrics-system` は Linux・macOS・Windows で使えます。ほかの OS では、その鍵を書きません。

```toml
[dependencies]
bengara = { version = "0.1", features = ["sqlite", "metrics-system"] }
```

## 2 つのプロセスで動かすとき

[同じ `storage/` を指す 2 つのプロセス](deployment.md#プロセスを-2-つ動かす)が、同じファイルに書いても壊れません。

- テキストは追記で開き、1 回の書き出しを 1 回の書き込みにまとめます。行が混ざりません。
- SQLite は WAL で開き、5 秒まで順番を待ちます。
- どちらのプロセスの行かは、SQLite の `pid` と、メトリクスの `pid`・`port` で見分けます。
- 古いファイルを消す処理は、どのプロセスが何度流しても同じ結果になります。

## 制限

| 制限                                                         | 理由                                       |
|--------------------------------------------------------------|--------------------------------------------|
| 日付と時刻は UTC。日本時間では 9 時にファイルが変わる         | bengara は時差を持たない（`Carbon` と同じ） |
| 溢れたら捨てる（数はメトリクスの `dropped` に出る）           | 本体を待たせないため                       |
| 強制終了（2 回目の合図）では、書き切るのを 1 秒までしか待たない。溜まっていた分が残らないことがある | 待たずに終わるための合図なので |
| 時計が先へ飛ぶと、飛んだ先の日付を今日と見て古いファイルを消す | 記録の時刻で日付を決めているため |
| health の 200 はアクセスに出ない                              | ロードバランサーの確認で埋まるため         |
| アプリが自分で spawn したタスクと `spawn_blocking` の中には、リクエスト ID が伝わらない | ID はタスクごとに持つため |
| Windows では、別のプロセスが開いている古いファイルは次の機会まで消えない | Windows は開いているファイルを消せない |
| SQLite には機能フラグ `sqlite` が要る                          | sqlx の SQLite のドライバを使うため        |
| ルートに当たらなかったパスは、伏せずに出る                    | 伏せる名前（パス引数）が分からないため     |

- 正常に終わるときは、全部のログを書き切ってから終わります（上限 5 秒）。

## 関連

- [configuration.md](configuration.md) — `LOG_CHANNEL`・`LOG_DAILY_DAYS`・`RUST_LOG`
- [deployment.md](deployment.md) — 本番での置き方
- [requests-and-responses.md](requests-and-responses.md) — `request_id()`
- [decisions.md](decisions.md) — 判断 270〜277
