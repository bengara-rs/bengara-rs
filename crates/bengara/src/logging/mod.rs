//! ログ（`docs/logging.md`）。
//!
//! - 種類は 3 つ：アクセス（`bengara::access`）・メトリクス（`bengara::metrics`）・アプリ（ほか全部）。
//! - 出し先は標準出力・日ごとのテキスト・SQLite。`LOG_CHANNEL` で選びます。
//! - 書き出しで本体を待たせません。溢れたら捨てて数えます（メトリクスの `dropped`）。
//!
//! 始める順番（`kernel_impl::main`）：
//!
//! 1. [`init`]：`.env` を読んだ後。標準出力だけで始めます（その場で書く）。
//! 2. [`start_outputs`]：`storage/` を確かめた後。テキストと SQLite を始めます。
//! 3. [`start_stdout_background`]：`serve` だけ。標準出力を裏のスレッドで書きます。
//! 4. [`shutdown`]：終わるとき。全部のキューを書き切ります（上限 5 秒）。

pub(crate) mod file;
pub(crate) mod format;
pub(crate) mod layer;
pub(crate) mod queue;
pub(crate) mod request_id;
pub(crate) mod sqlite;
pub(crate) mod stdout;

use std::io::IsTerminal as _;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tracing_subscriber::prelude::*;

use self::queue::Queue;
use self::stdout::Stdout;

/// 終わるときに書き切るのを待つ上限。
const SHUTDOWN_LIMIT: Duration = Duration::from_secs(5);

/// `LOG_DAILY_DAYS` の既定。
const DEFAULT_DAYS: u32 = 14;

/// 出し先の選び方（`LOG_CHANNEL`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Channel {
    /// 標準出力に加えて、日ごとのテキストと SQLite に書く（既定）。
    Daily,
    /// 標準出力だけ。
    Stdout,
}

/// ログの設定。始めるときに環境変数から 1 回だけ読みます。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Settings {
    pub(crate) channel: Channel,
    pub(crate) days: u32,
}

/// `LOG_CHANNEL` と `LOG_DAILY_DAYS` を読む。読めない値は既定にして、警告の文を返します。
///
/// 値を関数で受けるのは、テストで環境変数を触らないためです。
pub(crate) fn read_settings(get: impl Fn(&str) -> Option<String>) -> (Settings, Vec<String>) {
    let mut warnings = Vec::new();
    let value = |name: &str| {
        get(name)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };

    let channel = match value("LOG_CHANNEL") {
        None => Channel::Daily,
        Some(raw) => match raw.to_ascii_lowercase().as_str() {
            "daily" => Channel::Daily,
            "stdout" => Channel::Stdout,
            _ => {
                warnings.push(format!(
                    "LOG_CHANNEL の `{raw}` は使えないので daily にします（使えるのは daily と stdout です）"
                ));
                Channel::Daily
            }
        },
    };
    let days = match value("LOG_DAILY_DAYS") {
        None => DEFAULT_DAYS,
        Some(raw) => raw.parse::<u32>().unwrap_or_else(|_| {
            warnings.push(format!(
                "LOG_DAILY_DAYS の `{raw}` は 0 以上の整数として読めないので {DEFAULT_DAYS} にします"
            ));
            DEFAULT_DAYS
        }),
    };
    (Settings { channel, days }, warnings)
}

/// 出し先の一式。層が持ち、テキストと SQLite は後から足します。
pub(crate) struct Outputs {
    color: bool,
    /// アプリの行に書く `APP_ENV`。
    app_env: String,
    stdout: Stdout,
    files: OnceLock<Queue<file::Record>>,
    sqlite: OnceLock<Queue<sqlite::Record>>,
}

impl Outputs {
    pub(crate) fn new(stdout: Stdout, color: bool, app_env: String) -> Self {
        Self {
            color,
            app_env,
            stdout,
            files: OnceLock::new(),
            sqlite: OnceLock::new(),
        }
    }
}

static OUTPUTS: OnceLock<Arc<Outputs>> = OnceLock::new();
static SETTINGS: OnceLock<Settings> = OnceLock::new();

/// ログを始める（コマンドから）。`LOG_CHANNEL` と `LOG_DAILY_DAYS` を読み、警告の文を返します。
///
/// 警告はまだ出せないので（購読者を立てている途中のため）、呼ぶ側が後で出します。
pub(crate) fn init() -> Vec<String> {
    let (settings, warnings) = read_settings(|name| std::env::var(name).ok());
    let _ = SETTINGS.set(settings);
    install();
    warnings
}

/// ログを始める（`#[bengara::test]` から）。標準出力だけに書き、`LOG_CHANNEL` を見ません。
pub(crate) fn init_for_tests() {
    install();
}

/// 購読者を立てる。2 回目からは何もしません（テストから何度呼ばれても安全）。
fn install() {
    if OUTPUTS.get().is_some() {
        return;
    }
    let color = stdout::use_color(
        std::io::stdout().is_terminal(),
        std::env::var_os("NO_COLOR").as_deref(),
    );
    let app_env = std::env::var("APP_ENV")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "production".to_string());
    let outputs = Arc::new(Outputs::new(Stdout::real(), color, app_env));
    if OUTPUTS.set(outputs.clone()).is_err() {
        return;
    }
    let layer = layer::LogLayer::new(outputs);

    let default = if crate::env("APP_DEBUG", false) {
        "debug"
    } else {
        "info"
    };

    #[cfg(feature = "log-filter")]
    let filter = {
        use tracing_subscriber::EnvFilter;
        // sqlx は投げた SQL を 1 本ずつ出すので、既定では黙らせる。
        // 見たいときは `RUST_LOG=info,sqlx=debug` のように指定する。
        EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new(format!("{default},hyper=warn,tower=warn,sqlx=warn"))
        })
    };

    #[cfg(not(feature = "log-filter"))]
    let filter = crate::kernel_impl::max_level(std::env::var("RUST_LOG").ok().as_deref(), default);

    let subscriber = tracing_subscriber::registry().with(filter).with(layer);
    // 二重に初期化しても落ちないようにする。
    let _ = tracing::subscriber::set_global_default(subscriber);
}

/// 日ごとのテキストと SQLite を始める。`storage/` を確かめた後に 1 回呼びます。
///
/// `storage/logs/` は作りません（起動時にディレクトリを作らない決まり）。無ければ WARN を
/// 1 回出して、標準出力だけで続けます。
pub(crate) fn start_outputs() {
    let (Some(outputs), Some(settings)) = (OUTPUTS.get(), SETTINGS.get()) else {
        return;
    };
    if settings.channel != Channel::Daily {
        return;
    }
    let dir = logs_dir();
    if !dir.is_dir() {
        tracing::warn!(
            "storage/logs/（{}）が無いので、ログをファイルと SQLite に書きません。`storage:init` で作れます",
            dir.display()
        );
        return;
    }

    let (files, receiver, counters) = queue::channel(queue::CAPACITY);
    match file::spawn(dir.clone(), settings.days, receiver, counters) {
        Ok(()) => {
            let _ = outputs.files.set(files);
        }
        Err(error) => eprintln!("bengara: ログのテキストの書き手を立てられませんでした: {error}"),
    }

    #[cfg(feature = "sqlite")]
    {
        let (rows, receiver, counters) = queue::channel(queue::CAPACITY);
        match sqlite::spawn(dir, settings.days, receiver, counters) {
            Ok(()) => {
                let _ = outputs.sqlite.set(rows);
            }
            Err(error) => {
                eprintln!("bengara: ログの SQLite の書き手を立てられませんでした: {error}")
            }
        }
    }
}

/// `serve` の起動時に、SQLite に書けない理由を 1 回だけ知らせる（機能フラグ `sqlite` が無いとき）。
pub(crate) fn note_sqlite_missing() {
    if cfg!(feature = "sqlite") {
        return;
    }
    if SETTINGS.get().is_some_and(|s| s.channel == Channel::Daily) {
        tracing::info!("SQLite にもログを残すには、機能フラグ \"sqlite\" が要ります");
    }
}

/// 標準出力を裏のスレッドで書き始める（`serve` だけ）。
pub(crate) fn start_stdout_background() {
    if let Some(outputs) = OUTPUTS.get() {
        outputs.stdout.start_background(queue::CAPACITY);
    }
}

/// 裏で書いている標準出力を、ここまで書き切る。`serve` の「終了しました」の前に呼びます。
pub(crate) fn flush_stdout() {
    if let Some(outputs) = OUTPUTS.get() {
        outputs.stdout.flush(Instant::now() + SHUTDOWN_LIMIT);
    }
}

/// 終わるとき。全部のキューを書き切るのを待ちます（全体で上限 5 秒）。
pub(crate) fn shutdown() {
    let Some(outputs) = OUTPUTS.get() else {
        return;
    };
    let deadline = Instant::now() + SHUTDOWN_LIMIT;
    outputs.stdout.flush(deadline);
    if let Some(files) = outputs.files.get() {
        files.flush(deadline);
    }
    if let Some(rows) = outputs.sqlite.get() {
        rows.flush(deadline);
    }
}

/// 捨てた数と書けなかった数を取り出して 0 に戻す（メトリクスの `dropped`）。
pub(crate) fn take_dropped() -> u64 {
    let Some(outputs) = OUTPUTS.get() else {
        return 0;
    };
    let mut total = outputs.stdout.take_dropped();
    if let Some(files) = outputs.files.get() {
        total = total.saturating_add(files.counters().take_dropped());
    }
    if let Some(rows) = outputs.sqlite.get() {
        total = total.saturating_add(rows.counters().take_dropped());
    }
    total
}

/// `storage/logs/`。
fn logs_dir() -> std::path::PathBuf {
    crate::paths::storage_root().join("logs")
}

/// `about` に出す 1 行。
pub(crate) fn about_label() -> String {
    let Some(settings) = SETTINGS.get() else {
        return "stdout".to_string();
    };
    match settings.channel {
        Channel::Stdout => "stdout".to_string(),
        Channel::Daily => {
            let days = if settings.days == 0 {
                "消さない".to_string()
            } else {
                format!("{} 日", settings.days)
            };
            let sqlite = if cfg!(feature = "sqlite") {
                "SQLite あり"
            } else {
                "SQLite なし"
            };
            let missing = if logs_dir().is_dir() {
                ""
            } else {
                "  ← storage/logs がありません（storage:init）"
            };
            format!("daily（storage/logs、{days}、{sqlite}）{missing}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(pairs: &[(&str, &str)]) -> (Settings, Vec<String>) {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        read_settings(move |name| {
            pairs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        })
    }

    #[test]
    fn 既定はdailyで14日() {
        let (s, warnings) = settings(&[]);
        assert_eq!(
            s,
            Settings {
                channel: Channel::Daily,
                days: 14
            }
        );
        assert!(warnings.is_empty());
        // 空は書いていないのと同じ。
        let (s, warnings) = settings(&[("LOG_CHANNEL", " "), ("LOG_DAILY_DAYS", "")]);
        assert_eq!(
            s,
            Settings {
                channel: Channel::Daily,
                days: 14
            }
        );
        assert!(warnings.is_empty());
    }

    #[test]
    fn stdoutと日数を読む() {
        let (s, warnings) = settings(&[("LOG_CHANNEL", "stdout"), ("LOG_DAILY_DAYS", "0")]);
        assert_eq!(
            s,
            Settings {
                channel: Channel::Stdout,
                days: 0
            }
        );
        assert!(warnings.is_empty());
        let (s, _) = settings(&[("LOG_CHANNEL", " Daily "), ("LOG_DAILY_DAYS", " 3 ")]);
        assert_eq!(
            s,
            Settings {
                channel: Channel::Daily,
                days: 3
            }
        );
    }

    #[test]
    fn 読めない値は警告して既定にする() {
        let (s, warnings) = settings(&[("LOG_CHANNEL", "stack"), ("LOG_DAILY_DAYS", "-1")]);
        assert_eq!(
            s,
            Settings {
                channel: Channel::Daily,
                days: 14
            }
        );
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].contains("LOG_CHANNEL"), "{warnings:?}");
        assert!(warnings[1].contains("LOG_DAILY_DAYS"), "{warnings:?}");
        let (_, warnings) = settings(&[("LOG_DAILY_DAYS", "abc")]);
        assert_eq!(warnings.len(), 1);
    }
}
