//! 日ごとの SQLite の書き手（`storage/logs/logs-YYYY-MM-DD.sqlite`）。
//!
//! - 機能フラグ `sqlite` のときだけ書きます。sqlx のコードは `database/sqlite.rs` の
//!   `LogStore` に置き、ここには bengara の型（文字列と数）だけを置きます。
//! - 512 件か 1 秒でまとめ、`BEGIN IMMEDIATE` 〜 `COMMIT` の 1 回で書きます。
//! - 表と列は `docs/logging.md` の「SQLite」と同じです。

use super::format::FieldValue;

/// `access` 表の 1 行。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AccessRow {
    pub(crate) time: String,
    pub(crate) request_id: String,
    pub(crate) ip: Option<String>,
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) protocol: String,
    pub(crate) status: i64,
    pub(crate) bytes: i64,
    pub(crate) duration_us: i64,
    pub(crate) route: Option<String>,
    pub(crate) user_agent: Option<String>,
    pub(crate) pid: i64,
}

/// `app` 表の 1 行。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AppRow {
    pub(crate) time: String,
    pub(crate) level: String,
    pub(crate) target: String,
    pub(crate) message: String,
    pub(crate) context: String,
    pub(crate) request_id: Option<String>,
    pub(crate) pid: i64,
}

/// `metrics` 表の 1 行。列の順は logfmt の鍵の順と同じです。
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct MetricsRow {
    pub(crate) time: String,
    pub(crate) pid: i64,
    pub(crate) port: i64,
    pub(crate) uptime_seconds: i64,
    pub(crate) requests: i64,
    pub(crate) status_1xx: i64,
    pub(crate) status_2xx: i64,
    pub(crate) status_3xx: i64,
    pub(crate) status_4xx: i64,
    pub(crate) status_5xx: i64,
    pub(crate) status_408: i64,
    pub(crate) status_413: i64,
    pub(crate) status_419: i64,
    pub(crate) status_429: i64,
    pub(crate) p50_ms: Option<f64>,
    pub(crate) p95_ms: Option<f64>,
    pub(crate) p99_ms: Option<f64>,
    pub(crate) max_ms: Option<f64>,
    pub(crate) inflight: i64,
    pub(crate) panics: i64,
    pub(crate) websockets: Option<i64>,
    pub(crate) dropped: i64,
    pub(crate) rss_bytes: Option<i64>,
    pub(crate) cpu_percent: Option<f64>,
    pub(crate) disk_free_bytes: Option<i64>,
    pub(crate) disk_total_bytes: Option<i64>,
}

impl MetricsRow {
    /// メトリクスのイベントの欄から作る。無い欄は NULL（数の欄は 0）にします。
    pub(crate) fn from_fields(time: String, fields: &[(&str, FieldValue)]) -> Self {
        let int = |name: &str| -> Option<i64> {
            fields
                .iter()
                .find(|(key, _)| *key == name)
                .and_then(|(_, value)| value.as_u64())
                .map(|value| i64::try_from(value).unwrap_or(i64::MAX))
        };
        let real = |name: &str| -> Option<f64> {
            fields
                .iter()
                .find(|(key, _)| *key == name)
                .and_then(|(_, value)| value.as_f64())
        };
        Self {
            time,
            pid: int("pid").unwrap_or(0),
            port: int("port").unwrap_or(0),
            uptime_seconds: int("uptime_seconds").unwrap_or(0),
            requests: int("requests").unwrap_or(0),
            status_1xx: int("status_1xx").unwrap_or(0),
            status_2xx: int("status_2xx").unwrap_or(0),
            status_3xx: int("status_3xx").unwrap_or(0),
            status_4xx: int("status_4xx").unwrap_or(0),
            status_5xx: int("status_5xx").unwrap_or(0),
            status_408: int("status_408").unwrap_or(0),
            status_413: int("status_413").unwrap_or(0),
            status_419: int("status_419").unwrap_or(0),
            status_429: int("status_429").unwrap_or(0),
            p50_ms: real("p50_ms"),
            p95_ms: real("p95_ms"),
            p99_ms: real("p99_ms"),
            max_ms: real("max_ms"),
            inflight: int("inflight").unwrap_or(0),
            panics: int("panics").unwrap_or(0),
            websockets: int("websockets"),
            dropped: int("dropped").unwrap_or(0),
            rss_bytes: int("rss_bytes"),
            cpu_percent: real("cpu_percent"),
            disk_free_bytes: int("disk_free_bytes"),
            disk_total_bytes: int("disk_total_bytes"),
        }
    }
}

/// 3 つの表のどれかの 1 行。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Row {
    Access(AccessRow),
    App(AppRow),
    Metrics(MetricsRow),
}

/// 書き手に渡す 1 件。日付は `millis`（記録の時刻）で決めます。
///
/// 機能フラグ `sqlite` が無いときは書き手がいないので、作られることもありません。
#[cfg_attr(not(feature = "sqlite"), allow(dead_code))]
pub(crate) struct Record {
    pub(crate) millis: i64,
    pub(crate) row: Row,
}

/// ファイルの名前。
#[cfg_attr(not(feature = "sqlite"), allow(dead_code))]
pub(crate) fn file_name(day: i64) -> String {
    format!("logs-{}.sqlite", crate::support::time::format_date(day))
}

#[cfg(feature = "sqlite")]
pub(crate) use writer::spawn;

#[cfg(feature = "sqlite")]
mod writer {
    use std::path::PathBuf;
    use std::sync::mpsc::{Receiver, RecvTimeoutError};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::{file_name, Record, Row};
    use crate::database::LogStore;
    use crate::logging::file::{day_of, sweep};
    use crate::logging::queue::{self, Counters, Message};

    /// まとめる件数。
    const BATCH: usize = 512;
    /// まとめる時間。
    const BATCH_EVERY: Duration = Duration::from_secs(1);
    /// 開けなかったときに、次に開き直すまでの時間。
    const RETRY_EVERY: Duration = Duration::from_secs(60);

    /// 書き手のスレッドを立てる。
    pub(crate) fn spawn(
        dir: PathBuf,
        days: u32,
        receiver: Receiver<Message<Record>>,
        counters: Arc<Counters>,
    ) -> std::io::Result<()> {
        std::thread::Builder::new()
            .name("bengara-log-sqlite".into())
            .spawn(move || run(dir, days, receiver, counters))
            .map(|_| ())
    }

    /// 書き手の状態。テストでは直接使います。
    pub(crate) struct Writer {
        dir: PathBuf,
        days: u32,
        counters: Arc<Counters>,
        store: Option<(i64, LogStore)>,
        retry_at: Option<Instant>,
        swept: Option<i64>,
    }

    impl Writer {
        pub(crate) fn new(dir: PathBuf, days: u32, counters: Arc<Counters>) -> Self {
            Self {
                dir,
                days,
                counters,
                store: None,
                retry_at: None,
                swept: None,
            }
        }

        /// まとめた分を書く。日ごとに分けて、それぞれのファイルへ書きます。
        pub(crate) async fn write(&mut self, batch: Vec<Record>) {
            let mut start = 0;
            while start < batch.len() {
                let day = day_of(batch[start].millis);
                let end = batch[start..]
                    .iter()
                    .position(|record| day_of(record.millis) != day)
                    .map_or(batch.len(), |offset| start + offset);
                let rows: Vec<&Row> = batch[start..end].iter().map(|record| &record.row).collect();
                self.write_day(day, &rows).await;
                start = end;
            }
        }

        async fn write_day(&mut self, day: i64, rows: &[&Row]) {
            if self.store.as_ref().map(|(open, _)| *open) != Some(day) {
                self.store = None;
                if self.swept.is_none_or(|swept| day > swept) {
                    self.swept = Some(day);
                    if let Err(message) = sweep(&self.dir, day, self.days) {
                        self.counters.notify("消す処理", &message);
                    }
                }
                if self.retry_at.is_some_and(|at| Instant::now() < at) {
                    self.counters.add_dropped(rows.len() as u64);
                    return;
                }
                let path = self.dir.join(file_name(day));
                match LogStore::open(&path).await {
                    Ok(store) => {
                        self.retry_at = None;
                        self.store = Some((day, store));
                    }
                    Err(error) => {
                        self.retry_at = Some(Instant::now() + RETRY_EVERY);
                        self.counters.add_dropped(rows.len() as u64);
                        self.counters
                            .notify("SQLite", &format!("{}: {error}", path.display()));
                        return;
                    }
                }
            }
            let Some((_, store)) = self.store.as_mut() else {
                return;
            };
            if let Err(error) = store.write(rows).await {
                self.counters.add_dropped(rows.len() as u64);
                self.counters.notify("SQLite", &error.to_string());
                // 次は開き直す。
                self.store = None;
            }
        }
    }

    fn run(dir: PathBuf, days: u32, receiver: Receiver<Message<Record>>, counters: Arc<Counters>) {
        queue::mark_writer();
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                counters.notify("SQLite", &format!("書き手を始められませんでした: {error}"));
                // 受け取りだけは続け、捨てた数を数える（入れる側を詰まらせない）。
                for message in receiver {
                    match message {
                        Message::Record(_) => counters.add_dropped(1),
                        Message::Flush(done) => {
                            let _ = done.send(());
                        }
                    }
                }
                return;
            }
        };
        let mut writer = Writer::new(dir, days, counters);
        while let Ok(first) = receiver.recv() {
            let mut batch = Vec::new();
            let mut acks = Vec::new();
            let mut closed = false;
            let mut take = |message: Message<Record>, batch: &mut Vec<Record>| match message {
                Message::Record(record) => batch.push(record),
                Message::Flush(done) => acks.push(done),
            };
            take(first, &mut batch);
            let started = Instant::now();
            while batch.len() < BATCH {
                let left = BATCH_EVERY.saturating_sub(started.elapsed());
                if left.is_zero() {
                    break;
                }
                match receiver.recv_timeout(left) {
                    Ok(message) => {
                        let flushing = matches!(message, Message::Flush(_));
                        take(message, &mut batch);
                        if flushing {
                            break;
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => {
                        closed = true;
                        break;
                    }
                }
            }
            if !batch.is_empty() {
                runtime.block_on(writer.write(batch));
            }
            for done in acks {
                let _ = done.send(());
            }
            if closed {
                return;
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::super::{AccessRow, AppRow, MetricsRow};
        use super::*;
        use crate::support::time;

        fn temp_dir(label: &str) -> PathBuf {
            let dir = std::env::temp_dir().join(format!(
                "bengara-log-sqlite-{label}-{}-{}",
                std::process::id(),
                crate::support::crypto::to_hex(&crate::support::crypto::random_bytes::<6>())
            ));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn millis(date: &str) -> i64 {
            time::parse_timestamp(&format!("{date} 10:00:00")).unwrap() * 1_000
        }

        fn access(n: i64, millis: i64) -> Record {
            Record {
                millis,
                row: Row::Access(AccessRow {
                    time: time::format_iso8601_millis(millis),
                    request_id: format!("req{n}"),
                    ip: (n % 2 == 0).then(|| "203.0.113.9".to_string()),
                    method: "GET".into(),
                    path: format!("/p/{n}"),
                    protocol: "HTTP/1.1".into(),
                    status: 200,
                    bytes: n,
                    duration_us: 1_000 + n,
                    route: None,
                    user_agent: Some("ua".into()),
                    pid: 42,
                }),
            }
        }

        fn runtime() -> tokio::runtime::Runtime {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
        }

        #[test]
        fn 三つの表に書いて読み戻せる() {
            let dir = temp_dir("roundtrip");
            let at = millis("2026-11-26");
            let app = AppRow {
                time: time::format_iso8601_millis(at),
                level: "WARNING".into(),
                target: "bengara::mail".into(),
                message: "送れませんでした".into(),
                context: "{\"target\":\"bengara::mail\"}".into(),
                request_id: Some("req1".into()),
                pid: 42,
            };
            let metrics = MetricsRow {
                time: time::format_iso8601_millis(at),
                pid: 42,
                port: 8001,
                requests: 3,
                p50_ms: Some(0.25),
                websockets: None,
                rss_bytes: Some(1024),
                ..MetricsRow::default()
            };
            runtime().block_on(async {
                let mut writer = Writer::new(dir.clone(), 0, Arc::new(Counters::default()));
                writer
                    .write(vec![
                        access(1, at),
                        Record {
                            millis: at,
                            row: Row::App(app.clone()),
                        },
                        Record {
                            millis: at,
                            row: Row::Metrics(metrics.clone()),
                        },
                    ])
                    .await;
                let path = dir.join("logs-2026-11-26.sqlite");
                let (accesses, apps, metric_rows) = LogStore::read_all(&path).await.unwrap();
                assert_eq!(accesses.len(), 1);
                assert_eq!(
                    accesses[0],
                    match access(1, at).row {
                        Row::Access(row) => row,
                        _ => unreachable!(),
                    }
                );
                assert_eq!(apps, vec![app]);
                assert_eq!(metric_rows, vec![metrics]);
            });
            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn 新しい版が作ったファイルには書かない() {
            let dir = temp_dir("version");
            let path = dir.join("logs-2026-11-26.sqlite");
            let counters = Arc::new(Counters::default());
            runtime().block_on(async {
                LogStore::set_user_version(&path, 2).await.unwrap();
                let mut writer = Writer::new(dir.clone(), 0, counters.clone());
                writer.write(vec![access(1, millis("2026-11-26"))]).await;
            });
            assert_eq!(counters.take_dropped(), 1);
            runtime().block_on(async {
                assert_eq!(LogStore::user_version(&path).await.unwrap(), 2);
            });
            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn ふたつの書き手が同時に書いても全部の行が残る() {
            let dir = temp_dir("two");
            let at = millis("2026-11-26");
            let threads: Vec<_> = (0..2)
                .map(|who| {
                    let dir = dir.clone();
                    std::thread::spawn(move || {
                        runtime().block_on(async {
                            let counters = Arc::new(Counters::default());
                            let mut writer = Writer::new(dir, 0, counters.clone());
                            for chunk in 0..10 {
                                let batch = (0..100)
                                    .map(|n| access(who * 10_000 + chunk * 100 + n, at))
                                    .collect();
                                writer.write(batch).await;
                            }
                            counters.take_dropped()
                        })
                    })
                })
                .collect();
            for thread in threads {
                assert_eq!(thread.join().unwrap(), 0, "捨てた行がある");
            }
            runtime().block_on(async {
                let (accesses, _, _) = LogStore::read_all(&dir.join("logs-2026-11-26.sqlite"))
                    .await
                    .unwrap();
                assert_eq!(accesses.len(), 2_000);
            });
            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn 日をまたぐと次のファイルになり古いファイルを消す() {
            let dir = temp_dir("rollover");
            for suffix in ["sqlite", "sqlite-wal", "sqlite-shm"] {
                std::fs::write(dir.join(format!("logs-2026-11-20.{suffix}")), "x").unwrap();
            }
            runtime().block_on(async {
                let mut writer = Writer::new(dir.clone(), 3, Arc::new(Counters::default()));
                writer
                    .write(vec![
                        access(1, millis("2026-11-26")),
                        access(2, millis("2026-11-27")),
                    ])
                    .await;
                let (first, _, _) = LogStore::read_all(&dir.join("logs-2026-11-26.sqlite"))
                    .await
                    .unwrap();
                let (second, _, _) = LogStore::read_all(&dir.join("logs-2026-11-27.sqlite"))
                    .await
                    .unwrap();
                assert_eq!(first.len(), 1);
                assert_eq!(second.len(), 1);
            });
            for suffix in ["sqlite", "sqlite-wal", "sqlite-shm"] {
                assert!(
                    !dir.join(format!("logs-2026-11-20.{suffix}")).exists(),
                    "{suffix}"
                );
            }
            std::fs::remove_dir_all(&dir).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn メトリクスの欄から行を作る() {
        let fields = [
            ("pid", FieldValue::U64(4123)),
            ("port", FieldValue::U64(8001)),
            ("requests", FieldValue::U64(5)),
            ("p50_ms", FieldValue::F64(0.1)),
            ("cpu_percent", FieldValue::F64(3.2)),
        ];
        let row = MetricsRow::from_fields("t".into(), &fields);
        assert_eq!(row.pid, 4123);
        assert_eq!(row.port, 8001);
        assert_eq!(row.requests, 5);
        assert_eq!(row.p50_ms, Some(0.1));
        assert_eq!(row.p95_ms, None);
        assert_eq!(row.websockets, None);
        assert_eq!(row.rss_bytes, None);
        assert_eq!(row.cpu_percent, Some(3.2));
        assert_eq!(row.panics, 0);
    }

    #[test]
    fn ファイルの名前は日付() {
        let day = crate::support::time::parse_date("2026-11-26").unwrap();
        assert_eq!(file_name(day), "logs-2026-11-26.sqlite");
    }
}
