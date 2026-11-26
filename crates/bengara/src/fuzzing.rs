//! ファジングのための口。`cfg(fuzzing)` のときだけあります（`cargo fuzz` がこの cfg を付けます）。
//!
//! **公開 API ではありません**（`docs/releases.md`）。内部の関数を、文字列とバイト列を受けて
//! 単純な値を返す薄い包みで出します。内部の型はそのまま出しません。

use std::sync::Arc;

/// `multipart/form-data` の本文を読む。読めたら文字の欄とファイルの数を返す。
pub fn multipart(body: &[u8], content_type: &str) -> Option<(usize, usize)> {
    crate::http::multipart::parse(&Arc::new(body.to_vec()), content_type)
        .ok()
        .map(|parsed| (parsed.fields.len(), parsed.files.len()))
}

/// `Cookie` ヘッダーを読む。名前と値の組を返す。
pub fn cookie_header(raw: &str) -> Vec<(String, String)> {
    crate::http::cookie::parse_cookie_header(raw)
}

/// パーセントの復号（`+` を空白にする形）。
pub fn percent_decode(input: &str) -> String {
    crate::http::percent::decode(input)
}

/// パーセントの復号（`+` を空白にしない形）。
pub fn percent_decode_strict(input: &str) -> String {
    crate::http::percent::decode_strict(input)
}

/// パーセントの符号化（予約されていない文字だけを残す）。
pub fn percent_encode(input: &str) -> String {
    crate::http::percent::encode(input, crate::http::percent::unreserved)
}

/// `.env` の中身を読む（本物の環境変数は見ない）。読めた組と警告の数を返す。
pub fn dotenv(text: &str) -> (Vec<(String, String)>, usize) {
    let (pairs, warnings) = crate::env_vars::parse_dotenv(text, &|_| None);
    (pairs, warnings.len())
}

/// `.env` の 1 つの値を読む（`key:generate` が使う形）。
pub fn dotenv_value(raw: &str) -> String {
    crate::env_vars::dotenv_value(raw)
}

/// `Cargo.toml` を読み、読み出しの関数を一通り呼ぶ。
pub fn cargo_toml(text: &str) -> usize {
    let manifest = crate::cli::cargo_toml::CargoToml::from_text(text);
    let mut seen = 0;
    seen += usize::from(manifest.package_name().is_some());
    seen += usize::from(manifest.value_in("package", "name").is_some());
    seen += usize::from(manifest.has_section("dependencies"));
    seen += usize::from(manifest.has_key("dependencies", "bengara"));
    seen += usize::from(manifest.has_path("main.rs"));
    seen += usize::from(manifest.bin_name_for_path("main.rs").is_some());
    seen += usize::from(manifest.has_bin_name("artisan"));
    seen += manifest.root_keys().len();
    seen
}

/// SHA-256。
pub fn sha256(input: &[u8]) -> [u8; 32] {
    crate::support::crypto::sha256(input)
}

/// HMAC-SHA256。
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    crate::support::crypto::hmac_sha256(key, message)
}

/// PBKDF2-HMAC-SHA256（32 バイト）。
pub fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    crate::support::crypto::pbkdf2_sha256(password, salt, iterations)
}

/// 定数時間の比較。
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    crate::support::crypto::constant_time_eq(a, b)
}

/// 16 進にする。
pub fn to_hex(bytes: &[u8]) -> String {
    crate::support::crypto::to_hex(bytes)
}

/// 16 進から戻す。
pub fn from_hex(input: &str) -> Option<Vec<u8>> {
    crate::support::crypto::from_hex(input)
}

/// ログの行を、`input` を全部の欄に入れて組み立てる（標準出力・テキスト・logfmt）。
pub fn log_lines(input: &str) -> Vec<String> {
    use crate::logging::format::{self, Access, FieldValue, Level};

    let time = "2026-11-26T02:51:00.123Z";
    let fields = [
        ("key", FieldValue::Str(input.to_string())),
        ("n", FieldValue::U64(input.len() as u64)),
    ];
    let access = Access {
        method: input.to_string(),
        path: input.to_string(),
        protocol: input.to_string(),
        status: 200,
        bytes: input.len() as u64,
        duration_us: 1_234,
        ip: Some(input.to_string()),
        user_agent: Some(format::truncate_utf8(input, 512).to_string()),
        route: Some(input.to_string()),
        request_id: input.to_string(),
    };
    let mut lines = Vec::new();
    for color in [false, true] {
        let head = format::stdout_head(time, Level::Warn, color);
        lines.push(format::stdout_app(&head, input, &fields, Some(input)));
        lines.push(format::stdout_access(&head, &access));
    }
    lines.push(format::text_access(0, &access));
    let context = format::context_json(input, Some(input), &fields);
    lines.push(format::text_app(time, input, Level::Error, input, &context));
    lines.push(format!("{}\n", format::logfmt(Some(time), &fields)));
    lines
}

/// リクエスト ID として受け取ってよい形か。
pub fn request_id_is_valid(value: &str) -> bool {
    crate::logging::request_id::is_valid(value)
}
