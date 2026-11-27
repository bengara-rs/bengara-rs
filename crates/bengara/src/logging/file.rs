//! 日ごとのテキストの書き手と、古いファイルを消す処理（`docs/logging.md`）。
//!
//! - ファイルは `storage/logs/{access,app,metrics}-YYYY-MM-DD.log`。日付は記録の時刻（UTC）です。
//! - 追記で開き、1 回の書き出しは `write_all` 1 回にまとめます。
//!   同じ `storage/` を指す 2 つのプロセスが同じファイルに書いても、行が混ざらないためです。
//! - ファイルごとにまとめておき、1 秒ごとか 64 KiB を超えたら書きます。

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::queue::{self, Counters, Message};
use crate::support::time;

/// まとめて書く量の上限。超えたらすぐ書きます。
const FLUSH_BYTES: usize = 64 * 1024;

/// まとめて書く間隔。
const FLUSH_EVERY: Duration = Duration::from_secs(1);

/// 1 日のミリ秒。
const DAY_MILLIS: i64 = 86_400_000;

/// 記録の種類（ファイルの名前の頭）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Access,
    App,
    Metrics,
}

impl Kind {
    const ALL: [Kind; 3] = [Kind::Access, Kind::App, Kind::Metrics];

    fn prefix(self) -> &'static str {
        match self {
            Kind::Access => "access",
            Kind::App => "app",
            Kind::Metrics => "metrics",
        }
    }

    fn index(self) -> usize {
        match self {
            Kind::Access => 0,
            Kind::App => 1,
            Kind::Metrics => 2,
        }
    }
}

/// 書き手に渡す 1 件。`line` は LF で終わる 1 行です。
pub(crate) struct Record {
    pub(crate) kind: Kind,
    pub(crate) millis: i64,
    pub(crate) line: String,
}

/// 記録の時刻（ミリ秒）から、1970-01-01 からの日数を出す。
pub(crate) fn day_of(millis: i64) -> i64 {
    millis.div_euclid(DAY_MILLIS)
}

/// ファイルの名前。
fn file_name(kind: Kind, day: i64) -> String {
    format!("{}-{}.log", kind.prefix(), time::format_date(day))
}

/// 1 つのファイルの書きかけ。
struct Slot {
    day: Option<i64>,
    buffer: Vec<u8>,
    records: u64,
    file: Option<File>,
}

impl Slot {
    fn new() -> Self {
        Self {
            day: None,
            buffer: Vec::new(),
            records: 0,
            file: None,
        }
    }
}

/// 日ごとのテキストの書き手。スレッドから使います（テストでは直接呼びます）。
pub(crate) struct Writer {
    dir: PathBuf,
    days: u32,
    slots: [Slot; 3],
    /// 最後に消す処理を流した日。
    swept: Option<i64>,
    counters: Arc<Counters>,
    last_flush: Instant,
}

impl Writer {
    pub(crate) fn new(dir: PathBuf, days: u32, counters: Arc<Counters>) -> Self {
        Self {
            dir,
            days,
            slots: [Slot::new(), Slot::new(), Slot::new()],
            swept: None,
            counters,
            last_flush: Instant::now(),
        }
    }

    /// 1 件を受け取る。日が変わったら前の日の分を書き、消す処理を流します。
    pub(crate) fn push(&mut self, record: Record) {
        let day = day_of(record.millis);
        let index = record.kind.index();
        if self.slots[index].day != Some(day) {
            self.flush_slot(index);
            let slot = &mut self.slots[index];
            slot.day = Some(day);
            // 日が変わったので、次に書くときに次の日のファイルを開く。
            slot.file = None;
        }
        if self.swept.is_none_or(|swept| day > swept) {
            self.sweep(day);
        }
        let slot = &mut self.slots[index];
        slot.buffer.extend_from_slice(record.line.as_bytes());
        slot.records += 1;
        if slot.buffer.len() >= FLUSH_BYTES {
            self.flush_slot(index);
        }
    }

    /// 古いファイルを消す（今日を `today` として）。
    pub(crate) fn sweep(&mut self, today: i64) {
        self.swept = Some(today);
        if let Err(message) = sweep(&self.dir, today, self.days) {
            self.counters.notify("消す処理", &message);
        }
    }

    /// 間隔が過ぎていれば、全部を書く。
    fn tick(&mut self) {
        if self.last_flush.elapsed() >= FLUSH_EVERY {
            self.flush_all();
        }
    }

    /// 全部を書く。
    pub(crate) fn flush_all(&mut self) {
        for kind in Kind::ALL {
            self.flush_slot(kind.index());
        }
        self.last_flush = Instant::now();
    }

    fn flush_slot(&mut self, index: usize) {
        let slot = &mut self.slots[index];
        if slot.buffer.is_empty() {
            return;
        }
        let Some(day) = slot.day else {
            return;
        };
        let kind = Kind::ALL[index];
        let path = self.dir.join(file_name(kind, day));
        let result = match slot.file.as_mut() {
            Some(file) => file.write_all(&slot.buffer),
            None => match OpenOptions::new().create(true).append(true).open(&path) {
                Ok(mut file) => {
                    let written = file.write_all(&slot.buffer);
                    slot.file = Some(file);
                    written
                }
                Err(error) => Err(error),
            },
        };
        if let Err(error) = result {
            self.counters.add_dropped(slot.records);
            // 次は開き直す（消された・置き場所が戻ったなど）。
            slot.file = None;
            self.counters
                .notify("テキスト", &format!("{}: {error}", path.display()));
        }
        slot.buffer.clear();
        slot.records = 0;
    }
}

/// 書き手のスレッドを立てる。
pub(crate) fn spawn(
    dir: PathBuf,
    days: u32,
    receiver: Receiver<Message<Record>>,
    counters: Arc<Counters>,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("bengara-log-file".into())
        .spawn(move || run(dir, days, receiver, counters))
        .map(|_| ())
}

fn run(dir: PathBuf, days: u32, receiver: Receiver<Message<Record>>, counters: Arc<Counters>) {
    queue::mark_writer();
    let mut writer = Writer::new(dir, days, counters);
    // 始めたときに、古いファイルを消す。
    writer.sweep(day_of(time::now_millis()));
    loop {
        match receiver.recv_timeout(FLUSH_EVERY) {
            Ok(Message::Record(record)) => {
                writer.push(record);
                writer.tick();
            }
            Ok(Message::Flush(done)) => {
                writer.flush_all();
                let _ = done.send(());
            }
            Err(RecvTimeoutError::Timeout) => writer.flush_all(),
            Err(RecvTimeoutError::Disconnected) => {
                writer.flush_all();
                return;
            }
        }
    }
}

/// 消してよいファイルの名前なら、その日付（日数）を返す。
///
/// 名前が正確に合うものだけです。`mail.log` や桁の違う日付（`app-2026-1-1.log`）には触りません。
pub(crate) fn dated_log_file(name: &str) -> Option<i64> {
    const TEXT: [&str; 3] = ["access-", "app-", "metrics-"];
    if let Some(rest) = TEXT.iter().find_map(|prefix| name.strip_prefix(prefix)) {
        return time::parse_date(rest.strip_suffix(".log")?);
    }
    let rest = name.strip_prefix("logs-")?;
    let date = rest
        .strip_suffix(".sqlite")
        .or_else(|| rest.strip_suffix(".sqlite-wal"))
        .or_else(|| rest.strip_suffix(".sqlite-shm"))?;
    time::parse_date(date)
}

/// 古いログのファイルを消す。今日（`today`）を含めて `days` 日分を残します。`0` は消しません。
///
/// 消せなかったもの（Windows で別のプロセスが開いているなど）は、次の機会にまた試します。
/// どのプロセスが何度流しても同じ結果になります。
pub(crate) fn sweep(dir: &Path, today: i64, days: u32) -> Result<(), String> {
    if days == 0 {
        return Ok(());
    }
    let cutoff = today - (i64::from(days) - 1);
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => return Err(format!("{}: {error}", dir.display())),
    };
    let mut failed = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(day) = dated_log_file(name) else {
            continue;
        };
        if day >= cutoff {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => {}
            // 別のプロセスが先に消した。
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => failed.push(format!("{name}: {error}")),
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "消せませんでした（次の機会にまた試します）: {}",
            failed.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テストごとの一時ディレクトリ。
    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bengara-log-file-{label}-{}-{}",
            std::process::id(),
            crate::support::crypto::to_hex(&crate::support::crypto::random_bytes::<6>())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn day(text: &str) -> i64 {
        time::parse_date(text).unwrap()
    }

    fn millis(date: &str, clock: &str) -> i64 {
        time::parse_timestamp(&format!("{date} {clock}")).unwrap() * 1_000
    }

    fn writer(dir: &Path, days: u32) -> Writer {
        Writer::new(dir.to_path_buf(), days, Arc::new(Counters::default()))
    }

    fn record(kind: Kind, millis: i64, line: &str) -> Record {
        Record {
            kind,
            millis,
            line: format!("{line}\n"),
        }
    }

    #[test]
    fn ファイルの名前は種類と日付() {
        assert_eq!(
            file_name(Kind::Access, day("2026-11-26")),
            "access-2026-11-26.log"
        );
        assert_eq!(
            file_name(Kind::App, day("2026-01-05")),
            "app-2026-01-05.log"
        );
        assert_eq!(
            file_name(Kind::Metrics, day("2026-12-31")),
            "metrics-2026-12-31.log"
        );
    }

    #[test]
    fn 追記で開き直しても前の行が残る() {
        let dir = temp_dir("append");
        let at = millis("2026-11-26", "10:00:00");
        let mut first = writer(&dir, 0);
        first.push(record(Kind::App, at, "1 行目"));
        first.flush_all();
        drop(first);
        let mut second = writer(&dir, 0);
        second.push(record(Kind::App, at, "2 行目"));
        second.flush_all();
        let text = std::fs::read_to_string(dir.join("app-2026-11-26.log")).unwrap();
        assert_eq!(text, "1 行目\n2 行目\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn utcの0時をまたぐと次の日のファイルに分かれる() {
        let dir = temp_dir("midnight");
        let mut writer = writer(&dir, 0);
        writer.push(record(
            Kind::Access,
            millis("2026-11-26", "23:59:59") + 999,
            "前の日",
        ));
        writer.push(record(
            Kind::Access,
            millis("2026-11-27", "00:00:00"),
            "次の日",
        ));
        writer.flush_all();
        assert_eq!(
            std::fs::read_to_string(dir.join("access-2026-11-26.log")).unwrap(),
            "前の日\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("access-2026-11-27.log")).unwrap(),
            "次の日\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn 消してよい名前だけを見分ける() {
        assert_eq!(
            dated_log_file("access-2026-11-26.log"),
            Some(day("2026-11-26"))
        );
        assert_eq!(
            dated_log_file("metrics-2026-11-26.log"),
            Some(day("2026-11-26"))
        );
        assert_eq!(
            dated_log_file("logs-2026-11-26.sqlite"),
            Some(day("2026-11-26"))
        );
        assert_eq!(
            dated_log_file("logs-2026-11-26.sqlite-wal"),
            Some(day("2026-11-26"))
        );
        assert_eq!(
            dated_log_file("logs-2026-11-26.sqlite-shm"),
            Some(day("2026-11-26"))
        );
        for name in [
            "mail.log",
            "app-2026-1-1.log",
            "app-2026-11-26.log.bak",
            "app-2026-11-26.txt",
            "laravel-2026-11-26.log",
            "logs-2026-11-26.sqlite-journal",
            "app-2026-02-30.log",
            "app-.log",
        ] {
            assert_eq!(dated_log_file(name), None, "{name}");
        }
    }

    /// 2026-11-01〜2026-11-26 の日付のファイルと、触ってはいけないファイルを並べる。
    fn lay_out(dir: &Path) {
        for d in 1..=26 {
            let date = format!("2026-11-{d:02}");
            for name in [
                format!("access-{date}.log"),
                format!("app-{date}.log"),
                format!("metrics-{date}.log"),
                format!("logs-{date}.sqlite"),
                format!("logs-{date}.sqlite-wal"),
                format!("logs-{date}.sqlite-shm"),
            ] {
                std::fs::write(dir.join(name), "x").unwrap();
            }
        }
        std::fs::write(dir.join("mail.log"), "x").unwrap();
        std::fs::write(dir.join("app-2026-1-1.log"), "x").unwrap();
    }

    fn remaining_days(dir: &Path) -> Vec<u32> {
        let mut days: Vec<u32> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let date = name.strip_prefix("app-")?.strip_suffix(".log")?.to_string();
                time::parse_date(&date)?;
                date[8..].parse().ok()
            })
            .collect();
        days.sort_unstable();
        days
    }

    #[test]
    fn 残す日数のとおりに消す() {
        let today = day("2026-11-26");
        for (days, expected_first) in [
            (1u32, Some(26u32)),
            (3, Some(24)),
            (14, Some(13)),
            (0, Some(1)),
        ] {
            let dir = temp_dir(&format!("sweep{days}"));
            lay_out(&dir);
            sweep(&dir, today, days).unwrap();
            let left = remaining_days(&dir);
            assert_eq!(left.first().copied(), expected_first, "N={days}");
            assert_eq!(left.last().copied(), Some(26), "N={days}");
            let expected_count = if days == 0 { 26 } else { days as usize };
            assert_eq!(left.len(), expected_count, "N={days}");
            // SQLite の 3 つも同じ日数だけ残る。
            for suffix in ["sqlite", "sqlite-wal", "sqlite-shm"] {
                let count = std::fs::read_dir(&dir)
                    .unwrap()
                    .flatten()
                    .filter(|e| {
                        e.file_name()
                            .to_string_lossy()
                            .ends_with(&format!(".{suffix}"))
                    })
                    .count();
                assert_eq!(count, expected_count, "N={days} {suffix}");
            }
            // 触ってはいけないものは残る。
            assert!(dir.join("mail.log").exists());
            assert!(dir.join("app-2026-1-1.log").exists());
            // 何度流しても同じ。
            sweep(&dir, today, days).unwrap();
            assert_eq!(remaining_days(&dir), left);
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn 日が変わったら古いファイルを消す() {
        let dir = temp_dir("rollover");
        lay_out(&dir);
        let mut writer = writer(&dir, 3);
        writer.push(record(Kind::App, millis("2026-11-26", "12:00:00"), "今日"));
        assert_eq!(remaining_days(&dir), vec![24, 25, 26]);
        writer.push(record(Kind::App, millis("2026-11-27", "00:00:01"), "明日"));
        writer.flush_all();
        assert_eq!(remaining_days(&dir), vec![25, 26, 27]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ふたつの書き手が同じファイルに書いても行が混ざらない() {
        let dir = temp_dir("two");
        let at = millis("2026-11-26", "10:00:00");
        let threads: Vec<_> = ["A", "B"]
            .into_iter()
            .map(|who| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    let mut writer = writer(&dir, 0);
                    for n in 0..2_000 {
                        let line = format!("{who}-{n:05}-{}", "x".repeat(200));
                        writer.push(record(Kind::Access, at, &line));
                        if n % 97 == 0 {
                            writer.flush_all();
                        }
                    }
                    writer.flush_all();
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let text = std::fs::read_to_string(dir.join("access-2026-11-26.log")).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4_000);
        for line in lines {
            assert_eq!(line.len(), 2 + 5 + 1 + 200, "{line}");
            assert!(line.ends_with(&"x".repeat(200)), "{line}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn 書けなければ捨てて数える() {
        let dir = temp_dir("fail");
        let counters = Arc::new(Counters::default());
        // 置き場所をファイルにして、開けないようにする。
        let blocked = dir.join("not-a-dir");
        std::fs::write(&blocked, "x").unwrap();
        let mut writer = Writer::new(blocked, 0, counters.clone());
        writer.push(record(Kind::App, millis("2026-11-26", "10:00:00"), "a"));
        writer.push(record(Kind::App, millis("2026-11-26", "10:00:00"), "b"));
        writer.flush_all();
        assert_eq!(counters.take_dropped(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }
}
