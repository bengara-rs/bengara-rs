//! tracing の層。イベントの種類を見分け、行を組み立ててキューへ入れます。
//!
//! 行の組み立ては、イベントを出したスレッドでします。キューへは組み立て済みのものを渡します。

use std::sync::Arc;

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

use super::format::{self, Access, FieldValue, Level};
use super::{file, queue, request_id, sqlite, Outputs};
use crate::support::time;

/// アクセスのイベントの target。
pub(crate) const ACCESS_TARGET: &str = "bengara::access";
/// メトリクスのイベントの target。
pub(crate) const METRICS_TARGET: &str = "bengara::metrics";

/// 記録の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Access,
    Metrics,
    App,
}

impl Kind {
    pub(crate) fn of(target: &str) -> Self {
        match target {
            ACCESS_TARGET => Kind::Access,
            METRICS_TARGET => Kind::Metrics,
            _ => Kind::App,
        }
    }
}

/// bengara のログの層。
pub(crate) struct LogLayer {
    outputs: Arc<Outputs>,
}

impl LogLayer {
    pub(crate) fn new(outputs: Arc<Outputs>) -> Self {
        Self { outputs }
    }
}

impl<S: Subscriber> Layer<S> for LogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        // 書き手のスレッドで出たイベントは捨てる（自分のログが自分に戻る輪を作らない）。
        if queue::in_writer() {
            return;
        }
        let metadata = event.metadata();
        let mut fields = Collect::default();
        event.record(&mut fields);
        self.outputs.emit(
            time::now_millis(),
            Kind::of(metadata.target()),
            metadata.target(),
            Level::from_tracing(*metadata.level()),
            fields.message,
            fields.fields,
        );
    }
}

/// イベントの欄を、書いた順に集める。
#[derive(Default)]
pub(crate) struct Collect {
    pub(crate) message: String,
    pub(crate) fields: Vec<(&'static str, FieldValue)>,
}

impl Visit for Collect {
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields.push((field.name(), FieldValue::I64(value)));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields.push((field.name(), FieldValue::U64(value)));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.fields.push((field.name(), FieldValue::F64(value)));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields.push((field.name(), FieldValue::Bool(value)));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields
                .push((field.name(), FieldValue::Str(value.to_string())));
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields
                .push((field.name(), FieldValue::Str(format!("{value:?}"))));
        }
    }
}

/// 欄を名前で引く。
fn field<'a>(fields: &'a [(&str, FieldValue)], name: &str) -> Option<&'a FieldValue> {
    fields
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value)
}

/// アクセスのイベントの欄から、記録を作る。
pub(crate) fn access_from_fields(fields: &[(&str, FieldValue)]) -> Access {
    let text = |name: &str| field(fields, name).map(FieldValue::to_text);
    let number = |name: &str| {
        field(fields, name)
            .and_then(FieldValue::as_u64)
            .unwrap_or(0)
    };
    Access {
        method: text("method").unwrap_or_default(),
        path: text("path").unwrap_or_default(),
        protocol: text("protocol").unwrap_or_default(),
        status: number("status"),
        bytes: number("bytes"),
        duration_us: number("duration_us"),
        ip: text("ip").filter(|v| !v.is_empty()),
        user_agent: text("user_agent").filter(|v| !v.is_empty()),
        route: text("route").filter(|v| !v.is_empty()),
        request_id: text("request_id").unwrap_or_default(),
    }
}

/// `u64` を SQLite の整数にする（溢れる値は上限にそろえる）。
fn as_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

impl Outputs {
    /// 1 件を、出し先ごとの形にして渡す。
    pub(crate) fn emit(
        &self,
        millis: i64,
        kind: Kind,
        target: &str,
        level: Level,
        message: String,
        fields: Vec<(&'static str, FieldValue)>,
    ) {
        let time = time::format_iso8601_millis(millis);
        let head = format::stdout_head(&time, level, self.color);
        let files = self.files.get();
        let sqlite = self.sqlite.get();
        match kind {
            Kind::Access => {
                let access = access_from_fields(&fields);
                self.stdout.write(format::stdout_access(&head, &access));
                if let Some(files) = files {
                    files.push(file::Record {
                        kind: file::Kind::Access,
                        millis,
                        line: format::text_access(millis, &access),
                    });
                }
                if let Some(sqlite) = sqlite {
                    sqlite.push(sqlite::Record {
                        millis,
                        row: sqlite::Row::Access(sqlite::AccessRow {
                            time,
                            request_id: access.request_id,
                            ip: access.ip,
                            method: access.method,
                            path: access.path,
                            protocol: access.protocol,
                            status: as_i64(access.status),
                            bytes: as_i64(access.bytes),
                            duration_us: as_i64(access.duration_us),
                            route: access.route,
                            user_agent: access.user_agent,
                            pid: i64::from(std::process::id()),
                        }),
                    });
                }
            }
            Kind::Metrics => {
                self.stdout
                    .write(format!("{head}metrics {}\n", format::logfmt(None, &fields)));
                if let Some(files) = files {
                    files.push(file::Record {
                        kind: file::Kind::Metrics,
                        millis,
                        line: format!("{}\n", format::logfmt(Some(&time), &fields)),
                    });
                }
                if let Some(sqlite) = sqlite {
                    sqlite.push(sqlite::Record {
                        millis,
                        row: sqlite::Row::Metrics(sqlite::MetricsRow::from_fields(time, &fields)),
                    });
                }
            }
            Kind::App => {
                let id = request_id::current();
                let id = id.as_deref();
                self.stdout
                    .write(format::stdout_app(&head, &message, &fields, id));
                if files.is_none() && sqlite.is_none() {
                    return;
                }
                let context = format::context_json(target, id, &fields);
                if let Some(files) = files {
                    files.push(file::Record {
                        kind: file::Kind::App,
                        millis,
                        line: format::text_app(&time, &self.app_env, level, &message, &context),
                    });
                }
                if let Some(sqlite) = sqlite {
                    sqlite.push(sqlite::Record {
                        millis,
                        row: sqlite::Row::App(sqlite::AppRow {
                            time,
                            level: level.laravel().to_string(),
                            target: target.to_string(),
                            message,
                            context,
                            request_id: id.map(str::to_string),
                            pid: i64::from(std::process::id()),
                        }),
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::queue::Message;
    use crate::logging::stdout::Stdout;
    use std::sync::mpsc::Receiver;
    use std::sync::Mutex;
    use tracing_subscriber::prelude::*;

    struct Harness {
        stdout: Arc<Mutex<Vec<u8>>>,
        files: Receiver<Message<file::Record>>,
        sqlite: Receiver<Message<sqlite::Record>>,
        outputs: Arc<Outputs>,
    }

    impl Harness {
        fn new() -> Self {
            let stdout = Arc::new(Mutex::new(Vec::new()));
            let outputs = Arc::new(Outputs::new(
                Stdout::capture(stdout.clone()),
                false,
                "production".into(),
            ));
            let (files_queue, files, _) = queue::channel(64);
            let (sqlite_queue, sqlite, _) = queue::channel(64);
            let _ = outputs.files.set(files_queue);
            let _ = outputs.sqlite.set(sqlite_queue);
            Self {
                stdout,
                files,
                sqlite,
                outputs,
            }
        }

        /// 層を付けた購読者の下で `body` を動かす（グローバルには設定しない）。
        fn run(&self, body: impl FnOnce()) {
            let subscriber =
                tracing_subscriber::registry().with(LogLayer::new(self.outputs.clone()));
            tracing::subscriber::with_default(subscriber, body);
        }

        fn stdout(&self) -> String {
            String::from_utf8(self.stdout.lock().unwrap().clone()).unwrap()
        }

        fn files(&self) -> Vec<(file::Kind, String)> {
            self.files
                .try_iter()
                .filter_map(|m| match m {
                    Message::Record(r) => Some((r.kind, r.line)),
                    Message::Flush(_) => None,
                })
                .collect()
        }

        fn rows(&self) -> Vec<sqlite::Row> {
            self.sqlite
                .try_iter()
                .filter_map(|m| match m {
                    Message::Record(r) => Some(r.row),
                    Message::Flush(_) => None,
                })
                .collect()
        }
    }

    /// 行の頭（時刻）を落とす。時刻は毎回変わるため。
    fn without_time(line: &str) -> &str {
        &line[24..]
    }

    #[test]
    fn アプリの行は3つの出し先に同じ記録を書く() {
        let harness = Harness::new();
        harness.run(|| {
            tracing::warn!(target: "bengara::mail", error = "connection refused", "メールを送れませんでした");
        });
        let stdout = harness.stdout();
        assert_eq!(
            without_time(&stdout),
            "  WARN メールを送れませんでした error=\"connection refused\"\n"
        );
        let files = harness.files();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, file::Kind::App);
        assert!(
            files[0].1.ends_with(
                "] production.WARNING: メールを送れませんでした {\"target\":\"bengara::mail\",\"error\":\"connection refused\"}\n"
            ),
            "{}",
            files[0].1
        );
        let rows = harness.rows();
        let [sqlite::Row::App(row)] = rows.as_slice() else {
            panic!("{rows:?}");
        };
        assert_eq!(row.level, "WARNING");
        assert_eq!(row.target, "bengara::mail");
        assert_eq!(row.message, "メールを送れませんでした");
        assert_eq!(row.request_id, None);
    }

    #[test]
    fn アクセスの行はアプリの側に混ざらない() {
        let harness = Harness::new();
        harness.run(|| {
            tracing::info!(
                target: ACCESS_TARGET,
                method = "GET",
                path = "/reset/***?token=***",
                protocol = "HTTP/1.1",
                status = 404u64,
                bytes = 10u64,
                duration_us = 12_345u64,
                ip = Some("203.0.113.9"),
                user_agent = Option::<&str>::None,
                route = Option::<&str>::None,
                request_id = "abc",
            );
        });
        assert_eq!(
            without_time(&harness.stdout()),
            "  INFO GET /reset/***?token=*** -> 404 (12 ms) req=abc\n"
        );
        let files = harness.files();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, file::Kind::Access);
        assert!(
            files[0].1.starts_with("203.0.113.9 - - ["),
            "{}",
            files[0].1
        );
        assert!(
            files[0]
                .1
                .ends_with("\"GET /reset/***?token=*** HTTP/1.1\" 404 10 \"-\" \"-\" rt=0.012 req=abc route=-\n"),
            "{}",
            files[0].1
        );
        let rows = harness.rows();
        let [sqlite::Row::Access(row)] = rows.as_slice() else {
            panic!("{rows:?}");
        };
        assert_eq!(row.status, 404);
        assert_eq!(row.user_agent, None);
        assert_eq!(row.route, None);
        assert_eq!(row.ip.as_deref(), Some("203.0.113.9"));
    }

    #[test]
    fn メトリクスの行() {
        let harness = Harness::new();
        harness.run(|| {
            tracing::info!(target: METRICS_TARGET, pid = 4123u64, port = 8001u64, p50_ms = Some(0.1f64), rss_bytes = Option::<u64>::None);
        });
        assert_eq!(
            without_time(&harness.stdout()),
            "  INFO metrics pid=4123 port=8001 p50_ms=0.1\n"
        );
        let files = harness.files();
        assert_eq!(files[0].0, file::Kind::Metrics);
        assert!(files[0].1.starts_with("time="), "{}", files[0].1);
        assert!(
            files[0].1.ends_with(" pid=4123 port=8001 p50_ms=0.1\n"),
            "{}",
            files[0].1
        );
        let rows = harness.rows();
        let [sqlite::Row::Metrics(row)] = rows.as_slice() else {
            panic!("{rows:?}");
        };
        assert_eq!(row.port, 8001);
        assert_eq!(row.rss_bytes, None);
    }

    #[test]
    fn リクエストの中のアプリの行にはidが付く() {
        let harness = Harness::new();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        harness.run(|| {
            runtime.block_on(request_id::scope(Arc::from("rid"), async {
                tracing::warn!("中で");
            }));
            tracing::warn!("外で");
        });
        let stdout = harness.stdout();
        let lines: Vec<&str> = stdout.lines().collect();
        assert!(lines[0].ends_with("中で req=rid"), "{stdout}");
        assert!(lines[1].ends_with("外で"), "{stdout}");
        let files = harness.files();
        assert!(
            files[0]
                .1
                .contains("{\"target\":\"bengara::logging::layer::tests\",\"request_id\":\"rid\"}"),
            "{}",
            files[0].1
        );
        assert!(!files[1].1.contains("request_id"), "{}", files[1].1);
    }

    #[test]
    fn 書き手のスレッドで出たイベントはログに戻らない() {
        let harness = Harness::new();
        let outputs = harness.outputs.clone();
        std::thread::spawn(move || {
            queue::mark_writer();
            let subscriber = tracing_subscriber::registry().with(LogLayer::new(outputs));
            tracing::subscriber::with_default(subscriber, || {
                tracing::error!("書き手の中");
            });
        })
        .join()
        .unwrap();
        assert!(harness.stdout().is_empty());
        assert!(harness.files().is_empty());
        assert!(harness.rows().is_empty());
    }

    #[test]
    fn ファイルとsqliteが無ければ標準出力だけ() {
        let stdout = Arc::new(Mutex::new(Vec::new()));
        let outputs = Arc::new(Outputs::new(
            Stdout::capture(stdout.clone()),
            false,
            "local".into(),
        ));
        let subscriber = tracing_subscriber::registry().with(LogLayer::new(outputs));
        tracing::subscriber::with_default(subscriber, || tracing::info!(n = 1, "だけ"));
        let text = String::from_utf8(stdout.lock().unwrap().clone()).unwrap();
        assert_eq!(without_time(&text), "  INFO だけ n=1\n");
    }
}
