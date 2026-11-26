//! ログの行の形と、逃がし方（`docs/logging.md`）。
//!
//! どの行も「1 件 = 1 行」です。行の終わりの LF のほかに、CR と LF を入れません。
//! ここの関数は共有の状態に触らない、ただの関数です（テストで形を確かめるため）。

use std::fmt::Write as _;

use crate::support::time;

/// イベントの欄の値。tracing から受け取った型のまま持ちます。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FieldValue {
    I64(i64),
    U64(u64),
    F64(f64),
    Bool(bool),
    Str(String),
}

impl FieldValue {
    /// 整数として読む（負の数と小数は `None`）。
    pub(crate) fn as_u64(&self) -> Option<u64> {
        match self {
            FieldValue::U64(v) => Some(*v),
            FieldValue::I64(v) => u64::try_from(*v).ok(),
            FieldValue::Str(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// 小数として読む。
    pub(crate) fn as_f64(&self) -> Option<f64> {
        match self {
            FieldValue::F64(v) => Some(*v),
            FieldValue::U64(v) => Some(*v as f64),
            FieldValue::I64(v) => Some(*v as f64),
            _ => None,
        }
    }

    /// 文字として読む。数と真偽は文字にします。
    pub(crate) fn to_text(&self) -> String {
        match self {
            FieldValue::Str(s) => s.clone(),
            other => plain(other),
        }
    }
}

/// 数と真偽をそのまま書く。小数は余計な 0 を付けない（`1`・`2.5`）。
fn plain(value: &FieldValue) -> String {
    match value {
        FieldValue::I64(v) => v.to_string(),
        FieldValue::U64(v) => v.to_string(),
        FieldValue::F64(v) => v.to_string(),
        FieldValue::Bool(v) => v.to_string(),
        FieldValue::Str(s) => s.clone(),
    }
}

/// ログの水準。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Level {
    pub(crate) fn from_tracing(level: tracing::Level) -> Self {
        match level {
            tracing::Level::ERROR => Level::Error,
            tracing::Level::WARN => Level::Warn,
            tracing::Level::INFO => Level::Info,
            tracing::Level::DEBUG => Level::Debug,
            _ => Level::Trace,
        }
    }

    /// 標準出力の水準（右寄せ 5 文字）。
    fn padded(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => " WARN",
            Level::Info => " INFO",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
        }
    }

    /// テキストのファイルと SQLite の水準。WARN は Laravel に合わせて `WARNING`。
    pub(crate) fn laravel(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARNING",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
        }
    }

    /// 色の制御文字（ERROR 赤・WARN 黄・INFO 緑・DEBUG 青・TRACE 紫）。
    fn color(self) -> &'static str {
        match self {
            Level::Error => "\x1b[31m",
            Level::Warn => "\x1b[33m",
            Level::Info => "\x1b[32m",
            Level::Debug => "\x1b[34m",
            Level::Trace => "\x1b[35m",
        }
    }
}

/// 標準出力の行の頭（`{時刻} {水準} `）。色を付けるときは時刻を薄く、水準に色を付けます。
pub(crate) fn stdout_head(time: &str, level: Level, color: bool) -> String {
    if color {
        format!(
            "\x1b[2m{time}\x1b[0m {}{}\x1b[0m ",
            level.color(),
            level.padded()
        )
    } else {
        format!("{time} {} ", level.padded())
    }
}

/// 標準出力のアプリの行。`{時刻} {水準} {メッセージ}{ key=value …}{ req=ID}` と LF。
pub(crate) fn stdout_app(
    head: &str,
    message: &str,
    fields: &[(&str, FieldValue)],
    request_id: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(fields.len() + 2);
    if !message.is_empty() {
        parts.push(escape_stdout_message(message));
    }
    for (name, value) in fields {
        parts.push(format!("{name}={}", kv_value(value)));
    }
    if let Some(id) = request_id {
        parts.push(format!("req={}", escape_controls(id)));
    }
    let mut line = String::with_capacity(head.len() + 64);
    line.push_str(head);
    line.push_str(&parts.join(" "));
    line.push('\n');
    line
}

/// アクセスの記録。標準出力・テキスト・SQLite がここから作ります。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Access {
    pub(crate) method: String,
    /// 伏せた後の行き先。
    pub(crate) path: String,
    pub(crate) protocol: String,
    pub(crate) status: u64,
    pub(crate) bytes: u64,
    pub(crate) duration_us: u64,
    pub(crate) ip: Option<String>,
    pub(crate) user_agent: Option<String>,
    pub(crate) route: Option<String>,
    pub(crate) request_id: String,
}

/// 標準出力のアクセスの行。`{頭}{メソッド} {行き先} -> {ステータス} ({ミリ秒} ms) req={ID}` と LF。
pub(crate) fn stdout_access(head: &str, access: &Access) -> String {
    format!(
        "{head}{} {} -> {} ({} ms) req={}\n",
        escape_controls(&access.method),
        escape_controls(&access.path),
        access.status,
        access.duration_us / 1_000,
        escape_controls(&access.request_id),
    )
}

/// テキストのファイルのアクセスの行（nginx の combined 形式と先頭をそろえる）。
///
/// `{IP} - - [{時刻}] "{メソッド} {行き先} {プロトコル}" {ステータス} {バイト数} "-" "{User-Agent}"
/// rt={秒} req={ID} route={ルート名}` と LF。Referer はいつも `-` です。
pub(crate) fn text_access(millis: i64, access: &Access) -> String {
    let duration_ms = access.duration_us / 1_000;
    format!(
        "{} - - [{}] \"{} {} {}\" {} {} \"-\" \"{}\" rt={}.{:03} req={} route={}\n",
        access
            .ip
            .as_deref()
            .map(escape_controls)
            .unwrap_or_else(|| "-".into()),
        time::format_clf(millis),
        escape_quoted(&access.method),
        escape_quoted(&access.path),
        escape_quoted(&access.protocol),
        access.status,
        access.bytes,
        access
            .user_agent
            .as_deref()
            .map(escape_quoted)
            .unwrap_or_else(|| "-".into()),
        duration_ms / 1_000,
        duration_ms % 1_000,
        escape_controls(&access.request_id),
        access
            .route
            .as_deref()
            .map(escape_controls)
            .unwrap_or_else(|| "-".into()),
    )
}

/// テキストのファイルのアプリの行（Laravel の形）。
///
/// `[{時刻}] {APP_ENV}.{水準}: {メッセージ} {文脈の JSON}` と LF。
pub(crate) fn text_app(
    time: &str,
    app_env: &str,
    level: Level,
    message: &str,
    context: &str,
) -> String {
    format!(
        "[{time}] {}.{}: {} {context}\n",
        escape_message(app_env),
        level.laravel(),
        escape_message(message)
    )
}

/// アプリの行の文脈（JSON のオブジェクト）。
///
/// 鍵の順は `target` → `request_id`（あるときだけ）→ イベントの欄（書いた順）。
/// 整数・小数・真偽はそのまま、ほかは文字列です。`serde_json` の `Map` は鍵を並べ替えるので、
/// 順を保つために自分で組み立てます。
pub(crate) fn context_json(
    target: &str,
    request_id: Option<&str>,
    fields: &[(&str, FieldValue)],
) -> String {
    let mut out = String::with_capacity(64);
    out.push('{');
    out.push_str("\"target\":");
    out.push_str(&json_string(target));
    if let Some(id) = request_id {
        out.push_str(",\"request_id\":");
        out.push_str(&json_string(id));
    }
    for (name, value) in fields {
        out.push(',');
        out.push_str(&json_string(name));
        out.push(':');
        match value {
            FieldValue::I64(_) | FieldValue::U64(_) | FieldValue::Bool(_) => {
                out.push_str(&plain(value))
            }
            // JSON は NaN と無限大を書けないので、文字列にする。
            FieldValue::F64(v) if v.is_finite() => out.push_str(&plain(value)),
            FieldValue::F64(_) => out.push_str(&json_string(&plain(value))),
            FieldValue::Str(s) => out.push_str(&json_string(s)),
        }
    }
    out.push('}');
    out
}

/// メトリクスの logfmt。`time=` を先頭に置き、欄を書いた順に並べます。LF は付けません。
pub(crate) fn logfmt(time: Option<&str>, fields: &[(&str, FieldValue)]) -> String {
    let mut out = String::with_capacity(512);
    if let Some(time) = time {
        let _ = write!(out, "time={time}");
    }
    for (name, value) in fields {
        if !out.is_empty() {
            out.push(' ');
        }
        let _ = write!(out, "{name}={}", kv_value(value));
    }
    out
}

/// `key=value` の値。空白・`"`・`=`・制御文字があれば JSON の文字列の形（`"…"`）にします。
pub(crate) fn kv_value(value: &FieldValue) -> String {
    match value {
        FieldValue::Str(s) => {
            if s.chars()
                .any(|c| c == ' ' || c == '"' || c == '=' || c.is_control())
            {
                json_string(s)
            } else {
                s.clone()
            }
        }
        other => plain(other),
    }
}

/// JSON の文字列（前後の `"` を含む）。
fn json_string(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string())
}

/// JSON の文字列と同じ規則で逃がす。前後の `"` は付けません（アプリのメッセージ）。
pub(crate) fn escape_message(text: &str) -> String {
    if !text
        .chars()
        .any(|c| c == '"' || c == '\\' || c.is_control())
    {
        return text.to_string();
    }
    let quoted = json_string(text);
    quoted
        .get(1..quoted.len().saturating_sub(1))
        .unwrap_or("")
        .to_string()
}

/// 標準出力のメッセージ。制御文字だけを JSON と同じ形（`\n`・`\t`・`\u0001`）にします。
///
/// `"` と `\` はそのまま残します。Windows のパス（`C:\Users`）を読みやすく保つためです。
/// 標準出力で守るのは「1 件 = 1 行」だけです（テキストのファイルは [`escape_message`]）。
pub(crate) fn escape_stdout_message(text: &str) -> String {
    if !text.chars().any(char::is_control) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// アクセスの `"…"` の中。`"` と `\` と 0x00〜0x1F・0x7F を `\xHH`（大文字の 16 進）にします。
pub(crate) fn escape_quoted(text: &str) -> String {
    escape_bytes(text, |c| c == '"' || c == '\\' || is_ascii_control(c))
}

/// 制御文字（0x00〜0x1F・0x7F）だけを `\xHH` にします。`"…"` の外の欄に使います。
pub(crate) fn escape_controls(text: &str) -> String {
    escape_bytes(text, is_ascii_control)
}

fn is_ascii_control(c: char) -> bool {
    c.is_ascii_control()
}

fn escape_bytes(text: &str, escape: impl Fn(char) -> bool) -> String {
    if !text.chars().any(&escape) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if escape(c) {
            let _ = write!(out, "\\x{:02X}", c as u32);
        } else {
            out.push(c);
        }
    }
    out
}

/// 先頭 `max` バイトまでにする。UTF-8 の文字の切れ目で切ります。
pub(crate) fn truncate_utf8(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn access() -> Access {
        Access {
            method: "GET".into(),
            path: "/reset/***".into(),
            protocol: "HTTP/1.1".into(),
            status: 200,
            bytes: 1532,
            duration_us: 12_345,
            ip: Some("203.0.113.9".into()),
            user_agent: Some("Mozilla/5.0 (X11; Linux x86_64)".into()),
            route: Some("password.reset".into()),
            request_id: "4f1c0d2e9a7b4c55a1e3f0b2c4d6e8f0".into(),
        }
    }

    const AT: &str = "2026-11-26T02:51:00.123Z";

    fn at_millis() -> i64 {
        crate::support::time::parse_timestamp("2026-11-26 02:51:00").unwrap() * 1_000 + 123
    }

    #[test]
    fn 標準出力の頭は水準を右寄せ5文字にする() {
        assert_eq!(stdout_head(AT, Level::Info, false), format!("{AT}  INFO "));
        assert_eq!(stdout_head(AT, Level::Warn, false), format!("{AT}  WARN "));
        assert_eq!(stdout_head(AT, Level::Error, false), format!("{AT} ERROR "));
        assert_eq!(stdout_head(AT, Level::Trace, false), format!("{AT} TRACE "));
    }

    #[test]
    fn 色を付けるときは時刻を薄く水準に色() {
        assert_eq!(
            stdout_head(AT, Level::Warn, true),
            format!("\x1b[2m{AT}\x1b[0m \x1b[33m WARN\x1b[0m ")
        );
        assert!(stdout_head(AT, Level::Error, true).contains("\x1b[31mERROR"));
        assert!(stdout_head(AT, Level::Info, true).contains("\x1b[32m INFO"));
        assert!(stdout_head(AT, Level::Debug, true).contains("\x1b[34mDEBUG"));
        assert!(stdout_head(AT, Level::Trace, true).contains("\x1b[35mTRACE"));
    }

    #[test]
    fn 標準出力のアプリの行() {
        let head = stdout_head(AT, Level::Warn, false);
        let line = stdout_app(
            &head,
            "メールを送れませんでした",
            &[("error", FieldValue::Str("connection refused".into()))],
            Some("4f1c0d2e9a7b4c55a1e3f0b2c4d6e8f0"),
        );
        assert_eq!(
            line,
            "2026-11-26T02:51:00.123Z  WARN メールを送れませんでした error=\"connection refused\" req=4f1c0d2e9a7b4c55a1e3f0b2c4d6e8f0\n"
        );
        // リクエストの外では req= を付けない。欄の値は要るときだけ囲む。
        let line = stdout_app(
            &head,
            "x",
            &[
                ("n", FieldValue::U64(3)),
                ("ok", FieldValue::Bool(true)),
                ("k", FieldValue::Str("v".into())),
            ],
            None,
        );
        assert_eq!(line, format!("{head}x n=3 ok=true k=v\n"));
    }

    #[test]
    fn 標準出力のアクセスの行() {
        let head = stdout_head(AT, Level::Info, false);
        assert_eq!(
            stdout_access(&head, &access()),
            "2026-11-26T02:51:00.123Z  INFO GET /reset/*** -> 200 (12 ms) req=4f1c0d2e9a7b4c55a1e3f0b2c4d6e8f0\n"
        );
    }

    #[test]
    fn テキストのアクセスの行はcombinedの形() {
        assert_eq!(
            text_access(at_millis(), &access()),
            "203.0.113.9 - - [26/Nov/2026:02:51:00 +0000] \"GET /reset/*** HTTP/1.1\" 200 1532 \"-\" \"Mozilla/5.0 (X11; Linux x86_64)\" rt=0.012 req=4f1c0d2e9a7b4c55a1e3f0b2c4d6e8f0 route=password.reset\n"
        );
        // 分からない欄は `-`。
        let mut bare = access();
        bare.ip = None;
        bare.user_agent = None;
        bare.route = None;
        bare.duration_us = 2_500_400;
        let line = text_access(at_millis(), &bare);
        assert!(line.starts_with("- - - ["), "{line}");
        assert!(line.contains("\"-\" \"-\" rt=2.500 "), "{line}");
        assert!(line.ends_with(" route=-\n"), "{line}");
    }

    #[test]
    fn テキストのアプリの行はlaravelの形() {
        let context = context_json(
            "bengara::mail",
            Some("4f1c0d2e9a7b4c55a1e3f0b2c4d6e8f0"),
            &[("error", FieldValue::Str("connection refused".into()))],
        );
        assert_eq!(
            text_app(AT, "production", Level::Warn, "メールを送れませんでした", &context),
            "[2026-11-26T02:51:00.123Z] production.WARNING: メールを送れませんでした {\"target\":\"bengara::mail\",\"request_id\":\"4f1c0d2e9a7b4c55a1e3f0b2c4d6e8f0\",\"error\":\"connection refused\"}\n"
        );
    }

    #[test]
    fn 文脈は書いた順で数はそのまま() {
        let context = context_json(
            "app",
            None,
            &[
                ("z", FieldValue::U64(1)),
                ("a", FieldValue::I64(-2)),
                ("f", FieldValue::F64(2.5)),
                ("b", FieldValue::Bool(false)),
                ("nan", FieldValue::F64(f64::NAN)),
                ("s", FieldValue::Str("\"x\"\n".into())),
            ],
        );
        assert_eq!(
            context,
            r#"{"target":"app","z":1,"a":-2,"f":2.5,"b":false,"nan":"NaN","s":"\"x\"\n"}"#
        );
        let parsed: serde_json::Value = serde_json::from_str(&context).unwrap();
        assert_eq!(parsed["s"], "\"x\"\n");
    }

    #[test]
    fn どの行にも途中の改行を入れない() {
        let nasty = "a\r\nb\"c\\d\u{7f}e\u{1}";
        let head = stdout_head(AT, Level::Info, false);
        let mut evil = access();
        evil.method = nasty.into();
        evil.path = nasty.into();
        evil.user_agent = Some(nasty.into());
        evil.route = Some(nasty.into());
        evil.ip = Some(nasty.into());
        let lines = [
            stdout_app(&head, nasty, &[("k", FieldValue::Str(nasty.into()))], None),
            stdout_access(&head, &evil),
            text_access(at_millis(), &evil),
            text_app(
                AT,
                nasty,
                Level::Info,
                nasty,
                &context_json(nasty, None, &[(nasty, FieldValue::Str(nasty.into()))]),
            ),
            format!(
                "{}\n",
                logfmt(Some(AT), &[("k", FieldValue::Str(nasty.into()))])
            ),
        ];
        for line in lines {
            let body = line.strip_suffix('\n').expect("LF で終わる");
            assert!(!body.contains('\n') && !body.contains('\r'), "{line:?}");
        }
    }

    #[test]
    fn アクセスの囲みの中は16進で逃がす() {
        assert_eq!(
            escape_quoted("a\"b\\c\u{7f}\n日本"),
            "a\\x22b\\x5Cc\\x7F\\x0A日本"
        );
        assert_eq!(escape_controls("a\"b\tc"), "a\"b\\x09c");
    }

    #[test]
    fn 標準出力のメッセージは制御文字だけを逃がす() {
        assert_eq!(escape_stdout_message(r"C:\Users\mao"), r"C:\Users\mao");
        assert_eq!(escape_stdout_message("a\"b"), "a\"b");
        assert_eq!(
            escape_stdout_message("a\r\nb\tc\u{1}"),
            "a\\r\\nb\\tc\\u0001"
        );
    }

    #[test]
    fn メッセージはjsonの規則で前後の引用符なし() {
        assert_eq!(escape_message("ふつう"), "ふつう");
        assert_eq!(escape_message("a\"b\\c\nd\te"), "a\\\"b\\\\c\\nd\\te");
        assert_eq!(escape_message("\u{1}"), "\\u0001");
    }

    #[test]
    fn 標準出力のアプリの行はidに改行があっても1行() {
        // ファジング（log_line）で見つけた入力。実際の ID は形を確かめてから使うので
        // 改行は入らないが、ほかの行と同じく ID も逃がす。
        let head = stdout_head(AT, Level::Info, false);
        let line = stdout_app(&head, "m", &[], Some("a\r\nb"));
        assert_eq!(line, format!("{head}m req=a\\x0D\\x0Ab\n"));
    }

    #[test]
    fn 値は要るときだけ囲む() {
        assert_eq!(kv_value(&FieldValue::Str("abc".into())), "abc");
        assert_eq!(kv_value(&FieldValue::Str("a b".into())), "\"a b\"");
        assert_eq!(kv_value(&FieldValue::Str("a=b".into())), "\"a=b\"");
        assert_eq!(kv_value(&FieldValue::Str("a\"b".into())), "\"a\\\"b\"");
        assert_eq!(kv_value(&FieldValue::Str("a\nb".into())), "\"a\\nb\"");
        assert_eq!(kv_value(&FieldValue::F64(0.25)), "0.25");
        assert_eq!(kv_value(&FieldValue::F64(1.0)), "1");
    }

    #[test]
    fn 文字の切れ目で切る() {
        assert_eq!(truncate_utf8("abc", 512), "abc");
        assert_eq!(truncate_utf8("あいう", 4), "あ");
        assert_eq!(truncate_utf8("あいう", 6), "あい");
        let long = "x".repeat(600);
        assert_eq!(truncate_utf8(&long, 512).len(), 512);
    }

    #[test]
    fn logfmtは時刻を先頭に置く() {
        let fields = [
            ("pid", FieldValue::U64(4123)),
            ("p50_ms", FieldValue::F64(0.1)),
        ];
        assert_eq!(
            logfmt(Some(AT), &fields),
            format!("time={AT} pid=4123 p50_ms=0.1")
        );
        assert_eq!(logfmt(None, &fields), "pid=4123 p50_ms=0.1");
    }
}
