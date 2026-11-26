//! いまの時刻を文字列にする。
//!
//! `created_at` などに入れる `YYYY-MM-DD HH:MM:SS`（UTC）の文字列と、
//! UNIX 秒との行き来を受け持ちます。日時の型 [`Carbon`](super::Carbon) もこの計算を使います。
//! `chrono` を入れないために自分で計算します。

use std::time::{SystemTime, UNIX_EPOCH};

/// いまの時刻（UTC）を `YYYY-MM-DD HH:MM:SS` で返す。
pub(crate) fn now_string() -> String {
    format_timestamp(now_seconds())
}

/// いまの UNIX 時刻（秒）。
pub(crate) fn now_seconds() -> i64 {
    now_seconds_u64() as i64
}

/// いまの UNIX 時刻（秒）を符号なしで返す。
///
/// 期限（`expires_at`）を `u64` で持つ所が使います（セッションと署名付き URL）。
/// 時計が 1970 年より前を指していれば 0 です。
pub(crate) fn now_seconds_u64() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// いまの UNIX 時刻（ミリ秒）。
///
/// 「どちらが先か」を比べる印に使う。秒だと同じ秒の中の前後が分からない。
pub(crate) fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// `YYYY-MM-DD HH:MM:SS` を UNIX 時刻（秒）に戻す。読めなければ `None`。
///
/// 日付だけ（`YYYY-MM-DD`）も受け付け、00:00:00 として扱います。
///
/// # 桁はちょうどにそろえます
///
/// - 日付は `YYYY-MM-DD`（4 桁・2 桁・2 桁）。`2026-1-5` や `+2026-01-01` は読みません。
/// - 時刻は `HH:MM` か `HH:MM:SS`（どれも 2 桁）。`10`（時だけ）や `10:00:00:00`
///   （4 つ目の区切り）は読みません。
///
/// 以前は数として読めれば何桁でも通していました（`+5` も `parse` が読みます）。
/// 入力の検査（`date` の規則）がこれを使うので、崩れた値を日付として通さないためです。
///
/// # 時差は UTC に直します
///
/// 時刻の後ろの時差（`Z`・`+09:00`・`+0900`・`+09`・`-05:00`）は、足し引きして
/// **UTC の秒に直します。** `2026-10-09 10:00:00+09:00` は `2026-10-09 01:00:00` です。
///
/// 以前は時差を読み捨てていたので、`+09:00` の値が 9 時間ずれて読めていました
/// （パスワード再設定の期限が 9 時間延びる）。断る形にしなかったのは、
/// 読めない値を期限切れとして扱う所（`PasswordReset::expired`）で、
/// 時差付きの値が入ると再設定が常に断られるためです。
///
/// - 秒より細かい桁（`.123`）は捨てます。
/// - 時差の形が崩れているもの（`+9`・`+25:00`）は読めない扱い（`None`）です。
/// - それ以外の後ろに付いたもの（` UTC` など）は、今までどおり見ません。
///
/// bengara が自分で書く値と DB から読む値は、どれも時差の無い UTC です
/// （PostgreSQL の `timestamptz` も読むときに UTC に直します。`database/temporal.rs`）。
pub(crate) fn parse_timestamp(text: &str) -> Option<i64> {
    let text = text.trim();
    let (date, time) = match text.split_once(['T', ' ']) {
        Some((date, time)) => (date, time),
        None => (text, "00:00:00"),
    };

    let mut date_parts = date.split('-');
    let year: i64 = fixed_digits(date_parts.next()?, 4)?.parse().ok()?;
    let month: u32 = fixed_digits(date_parts.next()?, 2)?.parse().ok()?;
    let day: u32 = fixed_digits(date_parts.next()?, 2)?.parse().ok()?;
    // 年に上限が無かったころ、`99999999999999-01-01` が
    // `days * 86_400` の桁あふれでデバッグビルドではパニックし、
    // リリースでは巻き戻って過去の秒になりました。`PasswordReset::expired`
    // が**期限切れにならない**判定を返しえたので、範囲外は読めない扱いにします。
    // 上限は `format_timestamp` が出す `{year:04}` に合わせます。
    if !(1..=9999).contains(&year) {
        return None;
    }
    // 日の上限は月（とうるう年）で変わる。31 で通すと、`2026-02-31` が
    // `2026-03-03` として読めてしまう。
    if date_parts.next().is_some() || !(1..=12).contains(&month) {
        return None;
    }
    if !(1..=days_in_month(year, month)).contains(&day) {
        return None;
    }

    // 先頭の `HH:MM:SS` だけを取る。数字と `:` 以外が出たところで切ります。
    let time = time.trim();
    let end = time
        .find(|c: char| !(c.is_ascii_digit() || c == ':'))
        .unwrap_or(time.len());
    let mut time_parts = time[..end].split(':');
    let hour: i64 = fixed_digits(time_parts.next()?, 2)?.parse().ok()?;
    let minute: i64 = fixed_digits(time_parts.next()?, 2)?.parse().ok()?;
    let second: i64 = match time_parts.next() {
        Some(second) => fixed_digits(second, 2)?.parse().ok()?,
        None => 0,
    };
    // 4 つ目の区切りは読まない（`10:00:00:00`）。
    if time_parts.next().is_some() {
        return None;
    }
    // うるう秒（60 秒）は受け付けません。保存する側が作らない値です。
    if !(0..24).contains(&hour) || !(0..60).contains(&minute) || !(0..60).contains(&second) {
        return None;
    }

    let offset = parse_offset(&time[end..])?;

    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second - offset)
}

/// 時刻の後ろに付いた時差を秒で返す（東が正）。時差が無ければ 0。
///
/// 先に小数の秒（`.123`）を飛ばします。`+`・`-` で始まるのに形が崩れていれば `None`。
/// 時差でないもの（` UTC` など）は 0 として見ません（`parse_timestamp` を参照）。
fn parse_offset(rest: &str) -> Option<i64> {
    let rest = match rest.strip_prefix('.') {
        Some(fraction) => fraction.trim_start_matches(|c: char| c.is_ascii_digit()),
        None => rest,
    };
    let rest = rest.trim_start();
    let (sign, digits) = match rest.as_bytes().first() {
        Some(b'+') => (1, &rest[1..]),
        Some(b'-') => (-1, &rest[1..]),
        // `Z` も、時差でないものも 0。
        _ => return Some(0),
    };
    let (hours, minutes) = match digits.split_once(':') {
        Some((hours, minutes)) => (hours, minutes),
        None if digits.len() == 4 => digits.split_at(2),
        None => (digits, "00"),
    };
    let hours: i64 = fixed_digits(hours, 2)?.parse().ok()?;
    let minutes: i64 = fixed_digits(minutes, 2)?.parse().ok()?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 3_600 + minutes * 60))
}

/// ちょうど `len` 桁の数字だけでできていれば、そのまま返す。
///
/// `str::parse` は `+5` や `0005` も読むので、桁と符号はここで確かめます。
fn fixed_digits(text: &str, len: usize) -> Option<&str> {
    (text.len() == len && text.bytes().all(|b| b.is_ascii_digit())).then_some(text)
}

/// その月の日数。
pub(crate) fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        // 月の範囲は呼ぶ側で確かめています。
        _ => 0,
    }
}

/// うるう年か（グレゴリオ暦）。
fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// 年・月・日を 1970-01-01 からの日数に直す。`civil_from_days` の逆です。
pub(crate) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let year_of_era = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 } as i64;
    let day_of_year = (153 * mp + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// UNIX 時刻（秒）を `YYYY-MM-DD HH:MM:SS` にする。
pub(crate) fn format_timestamp(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = rest / 3_600;
    let minute = (rest % 3_600) / 60;
    let second = rest % 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
}

/// UNIX 時刻（ミリ秒）を `YYYY-MM-DDTHH:MM:SS.mmmZ`（UTC）にする。ログの時刻に使います。
pub(crate) fn format_iso8601_millis(millis: i64) -> String {
    let seconds = millis.div_euclid(1_000);
    let fraction = millis.rem_euclid(1_000);
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{fraction:03}Z",
        rest / 3_600,
        (rest % 3_600) / 60,
        rest % 60
    )
}

/// UNIX 時刻（ミリ秒）を nginx のアクセスログの時刻（`26/Nov/2026:02:51:00 +0000`）にする。
///
/// 月は英語の 3 文字です。時差は常に `+0000`（UTC）です。
pub(crate) fn format_clf(millis: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let seconds = millis.div_euclid(1_000);
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let name = MONTHS
        .get(month.saturating_sub(1) as usize)
        .copied()
        .unwrap_or("Jan");
    format!(
        "{day:02}/{name}/{year:04}:{:02}:{:02}:{:02} +0000",
        rest / 3_600,
        (rest % 3_600) / 60,
        rest % 60
    )
}

/// 1970-01-01 からの日数を `YYYY-MM-DD` にする。ログのファイル名に使います。
pub(crate) fn format_date(days: i64) -> String {
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// `YYYY-MM-DD` を 1970-01-01 からの日数に戻す。桁がちょうどで、実在する日付だけを読みます。
///
/// 年は 1〜9999 です（`parse_timestamp` と同じ範囲）。`0000` は読みません。
pub(crate) fn parse_date(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year: i64 = fixed_digits(&text[..4], 4)?.parse().ok()?;
    let month: u32 = fixed_digits(&text[5..7], 2)?.parse().ok()?;
    let day: u32 = fixed_digits(&text[8..], 2)?.parse().ok()?;
    if !(1..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
    {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

/// 1970-01-01 からの日数を、年・月・日に直す。
///
/// Howard Hinnant の `civil_from_days` と同じ手順です。
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 基準の日を変換できる() {
        assert_eq!(format_timestamp(0), "1970-01-01 00:00:00");
        assert_eq!(format_timestamp(86_399), "1970-01-01 23:59:59");
        assert_eq!(format_timestamp(86_400), "1970-01-02 00:00:00");
    }

    #[test]
    fn うるう年と月末を跨げる() {
        // 2024-02-29 12:34:56 UTC
        assert_eq!(format_timestamp(1_709_210_096), "2024-02-29 12:34:56");
        // 2026-10-05 00:00:00 UTC
        assert_eq!(format_timestamp(1_791_158_400), "2026-10-05 00:00:00");
        // 2000-03-01 00:00:00 UTC
        assert_eq!(format_timestamp(951_868_800), "2000-03-01 00:00:00");
    }

    #[test]
    fn 文字列から秒に戻せる() {
        for seconds in [
            0i64,
            86_399,
            86_400,
            1_709_210_096,
            1_791_158_400,
            951_868_800,
        ] {
            let text = format_timestamp(seconds);
            assert_eq!(parse_timestamp(&text), Some(seconds), "{text}");
        }
    }

    #[test]
    fn 日付だけや区切り文字つきも読める() {
        assert_eq!(parse_timestamp("1970-01-02"), Some(86_400));
        assert_eq!(parse_timestamp("1970-01-02T00:00:00"), Some(86_400));
        assert_eq!(parse_timestamp("1970-01-02 00:00:00.123"), Some(86_400));
        assert_eq!(parse_timestamp("  1970-01-02 00:00:00  "), Some(86_400));
    }

    #[test]
    fn 読めない文字列はnoneになる() {
        for broken in [
            "",
            "こわれた値",
            "2026-13-01",
            "2026-01-32",
            "2026-01",
            "x-01-01",
            // 月末を超えた日。31 で通していたころは繰り上がって読めてしまった。
            "2026-02-29",
            "2026-02-31",
            "2026-04-31",
            "2026-06-31",
            "2026-09-31",
            "2026-11-31",
            "2026-01-00",
            // 時刻の範囲。
            "2026-01-01 24:00:00",
            "2026-01-01 00:60:00",
            "2026-01-01 00:00:60",
            "2026-01-01T",
        ] {
            assert_eq!(parse_timestamp(broken), None, "{broken}");
        }
    }

    #[test]
    fn 桁がちょうどでない値はnoneになる() {
        for broken in [
            // 日付は 4-2-2 桁。
            "2026-1-05",
            "2026-01-5",
            "26-01-05",
            "02026-01-05",
            "+2026-01-05",
            "2026-+1-05",
            "2026-01-+5",
            // 時刻は 2:2(:2) 桁。
            "2026-01-05 10",
            "2026-01-05 1:00:00",
            "2026-01-05 10:0:00",
            "2026-01-05 10:00:0",
            "2026-01-05 010:00:00",
            // 4 つ目以降の区切り。
            "2026-01-05 10:00:00:00",
            "2026-01-05 10:00:00:",
        ] {
            assert_eq!(parse_timestamp(broken), None, "{broken}");
        }
        // 秒は省ける。
        assert_eq!(
            parse_timestamp("1970-01-02 01:02"),
            Some(86_400 + 3_600 + 120)
        );
    }

    #[test]
    fn 年の範囲を外れた値はnoneになる() {
        // 上限が無かったころ、デバッグビルドでは掛け算の桁あふれでパニックし、
        // リリースでは巻き戻って `PasswordReset::expired` が期限切れを
        // 見落としえました。
        for broken in [
            "99999999999999-01-01",
            "9999999999999999999999-01-01",
            "10000-01-01",
            "0000-01-01",
        ] {
            assert_eq!(parse_timestamp(broken), None, "{broken}");
        }
        // 範囲の端は読める。
        assert!(parse_timestamp("0001-01-01").is_some());
        assert!(parse_timestamp("9999-12-31 23:59:59").is_some());
        // `format_timestamp` が出す形は必ず読み戻せる。
        for seconds in [-62_135_596_800_i64, 253_402_300_799] {
            let text = format_timestamp(seconds);
            assert_eq!(parse_timestamp(&text), Some(seconds), "{text}");
        }
    }

    #[test]
    fn うるう年の二月二十九日は読める() {
        assert_eq!(parse_timestamp("2024-02-29"), Some(1_709_164_800));
        assert!(parse_timestamp("2000-02-29").is_some(), "400 の倍数");
        assert_eq!(parse_timestamp("1900-02-29"), None, "100 の倍数は平年");
        assert!(parse_timestamp("2026-02-28").is_some());
        // 月末ちょうどは通る。
        for text in ["2026-01-31", "2026-04-30", "2026-12-31"] {
            assert!(parse_timestamp(text).is_some(), "{text}");
        }
    }

    #[test]
    fn タイムゾーンつきの時刻はutcに直して読む() {
        let base = parse_timestamp("2026-10-09 10:00:00").unwrap();
        // 時差が 0 のもの。
        for text in [
            "2026-10-09 10:00:00Z",
            "2026-10-09T10:00:00Z",
            "2026-10-09 10:00:00+00:00",
            "2026-10-09 10:00:00+00",
            "2026-10-09 10:00:00.5",
            "2026-10-09 10:00:00.5Z",
            // 時差でないものは今までどおり見ない。
            "2026-10-09 10:00:00 UTC",
        ] {
            assert_eq!(parse_timestamp(text), Some(base), "{text}");
        }
        // 以前は時差を読み捨て、`+09:00` が 9 時間ずれて読めていた。
        let hour = 3_600;
        for (text, expected) in [
            ("2026-10-09 10:00:00+09:00", base - 9 * hour),
            ("2026-10-09T10:00:00+0900", base - 9 * hour),
            ("2026-10-09 10:00:00+09", base - 9 * hour),
            ("2026-10-09 10:00:00+05:30", base - 5 * hour - 30 * 60),
            // `-` が区切りの一覧に無かったころ、ここは None になっていた。
            // `PasswordReset::expired` は None を期限切れにするので、
            // こうした値が入るとトークンが常に拒否された。
            ("2026-10-09 10:00:00-05:00", base + 5 * hour),
            ("2026-10-09 10:00:00-0500", base + 5 * hour),
            ("2026-10-09T10:00:00.123456-05:00", base + 5 * hour),
        ] {
            assert_eq!(parse_timestamp(text), Some(expected), "{text}");
        }
        // 日付をまたいでも合う。
        assert_eq!(
            parse_timestamp("2026-10-09 01:00:00+09:00"),
            parse_timestamp("2026-10-08 16:00:00")
        );
        // 崩れた時差は読めない。
        for broken in [
            "2026-10-09 10:00:00+9",
            "2026-10-09 10:00:00+25:00",
            "2026-10-09 10:00:00+09:60",
            "2026-10-09 10:00:00+",
            "2026-10-09 10:00:00+09:0",
        ] {
            assert_eq!(parse_timestamp(broken), None, "{broken}");
        }
    }

    #[test]
    fn いまの秒と文字列が一致する() {
        let seconds = now_seconds();
        let text = now_string();
        // 秒をまたぐことがあるので 2 秒まで許す。
        let parsed = parse_timestamp(&text).expect("自分が作った形は読める");
        assert!((parsed - seconds).abs() <= 2, "{text} と {seconds}");
    }

    #[test]
    fn 符号なしの秒は符号つきの秒と同じ時刻を指す() {
        let signed = now_seconds();
        let unsigned = now_seconds_u64();
        // 秒をまたぐことがあるので 2 秒まで許す。
        assert!(unsigned.abs_diff(signed as u64) <= 2);
        assert!(unsigned > 1_700_000_000, "1970 年ではない");
    }

    #[test]
    fn いまの時刻は決まった形になる() {
        let now = now_string();
        assert_eq!(now.len(), 19, "YYYY-MM-DD HH:MM:SS の 19 文字");
        assert_eq!(now.as_bytes()[4], b'-');
        assert_eq!(now.as_bytes()[10], b' ');
        assert!(now.starts_with("20"));
    }

    fn millis_of(year: i64, month: u32, day: u32, h: i64, m: i64, s: i64, ms: i64) -> i64 {
        (days_from_civil(year, month, day) * 86_400 + h * 3_600 + m * 60 + s) * 1_000 + ms
    }

    #[test]
    fn ログの時刻はミリ秒までのutc() {
        let at = millis_of(2026, 11, 26, 2, 51, 0, 123);
        assert_eq!(format_iso8601_millis(at), "2026-11-26T02:51:00.123Z");
        assert_eq!(format_iso8601_millis(0), "1970-01-01T00:00:00.000Z");
        // 1970 年より前も、端数を正しく繰り下げる。
        assert_eq!(format_iso8601_millis(-1), "1969-12-31T23:59:59.999Z");
    }

    #[test]
    fn nginxの時刻は英語の月と時差0() {
        let at = millis_of(2026, 11, 26, 2, 51, 0, 999);
        assert_eq!(format_clf(at), "26/Nov/2026:02:51:00 +0000");
        assert_eq!(
            format_clf(millis_of(2026, 1, 5, 23, 0, 9, 0)),
            "05/Jan/2026:23:00:09 +0000"
        );
    }

    #[test]
    fn 日付の文字列と日数を行き来できる() {
        let days = days_from_civil(2026, 11, 26);
        assert_eq!(format_date(days), "2026-11-26");
        assert_eq!(parse_date("2026-11-26"), Some(days));
        assert_eq!(parse_date("2024-02-29"), Some(days_from_civil(2024, 2, 29)));
    }

    #[test]
    fn 崩れた日付は読まない() {
        for text in [
            "2026-1-1",
            "2026-01-1",
            "26-01-01",
            "2026/01/01",
            "2026-13-01",
            "2026-02-29",
            "2026-04-31",
            "2026-00-10",
            "2026-01-00",
            "+026-01-01",
            "2026-01-01x",
            // 年 0 は `parse_timestamp` と同じく読まない。
            "0000-01-01",
        ] {
            assert_eq!(parse_date(text), None, "{text}");
        }
        // 範囲の端は読める。
        assert!(parse_date("0001-01-01").is_some());
        assert!(parse_date("9999-12-31").is_some());
    }
}
