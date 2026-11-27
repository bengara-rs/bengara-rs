//! `multipart/form-data` の本文を読む（RFC 7578）。
//!
//! 本文を全部受けてから読みます（[`Request::receive_body`](crate::http::Request)）。
//! `.stream_body()` のルートでは読みません。区切りの規則は RFC 2046 の 5.1.1 に従います。
//!
//! | 中身 | 行き先 |
//! |---|---|
//! | `filename` の無い欄 | 入力（`input_pairs`）。送られてきた順 |
//! | `filename` のある欄 | ファイル（`req.file()`）。中身は本文を共有し、写しません |
//! | `filename=""` の欄（ファイルを選ばずに送った） | どちらにも入れない（PHP と同じ） |
//!
//! | 起きたこと | 状態コード |
//! |---|---|
//! | 形が壊れている（境界が無い・終わりの区切りが無い・`name` が無い など） | 400 |
//! | ファイルの数・欄の数・欄の見出しの長さが上限を超えた | 413 |
//!
//! 上限を超えたときは、本文の上限（`.max_body`）と同じ 413 にそろえます。
//! PHP は超えた分を黙って捨てますが、欄が抜けたまま処理が進むほうが危ないので断ります。

use std::sync::Arc;

use super::upload::UploadedFile;
use crate::error::{Error, Result};

/// 1 本の要求で受けるファイルの数。PHP の `max_file_uploads` の既定と同じです。
///
/// ファイルを選ばずに送った欄（`filename=""`）は数えません（PHP と同じ）。
pub(crate) const MAX_FILES: usize = 20;

/// 1 本の要求で受ける欄の数（文字の欄とファイルの欄の合計）。
///
/// PHP の `max_input_vars` の既定と同じです。
pub(crate) const MAX_PARTS: usize = 1000;

/// 欄 1 つの見出し（`Content-Disposition` などの行）の合計の長さ。
pub(crate) const MAX_PART_HEADER_BYTES: usize = 8 * 1024;

/// 読んだ結果。
#[derive(Debug, Clone, Default)]
pub(crate) struct Multipart {
    /// 文字の欄。送られてきた順です。
    pub(crate) fields: Vec<(String, String)>,
    /// ファイルの欄。送られてきた順です。名前は送られてきたまま（`photos[]` も）。
    pub(crate) files: Vec<(String, UploadedFile)>,
}

/// `Content-Type` が `multipart/form-data` か。
pub(crate) fn is_multipart(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("multipart/form-data")
}

/// 本文を読む。`body` は要求の本文そのもので、ファイルの中身はこれを共有します。
pub(crate) fn parse(body: &Arc<Vec<u8>>, content_type: &str) -> Result<Multipart> {
    let boundary = boundary(content_type)
        .ok_or_else(|| broken("境界（boundary）が無いか、形が正しくありません"))?;
    let mut delimiter = Vec::with_capacity(boundary.len() + 4);
    delimiter.extend_from_slice(b"\r\n--");
    delimiter.extend_from_slice(boundary.as_bytes());

    let bytes = body.as_slice();
    // 最初の区切りだけは、本文の先頭なら前の CRLF がありません。
    // 区切りより前（前置き）は読み捨てます。
    let first = if bytes.starts_with(&delimiter[2..]) {
        after_boundary(bytes, delimiter.len() - 2)
    } else {
        None
    };
    let mut after = match first {
        Some(after) => after,
        None => {
            next_delimiter(bytes, &delimiter, 0)
                .ok_or_else(|| broken("区切りが見つかりません"))?
                .1
        }
    };

    let mut out = Multipart::default();
    let mut parts = 0usize;
    // 終わりの区切り（`--boundary--`）の後ろ（後置き）も読み捨てます。
    while let After::Part(start) = after {
        let (end, next) = next_delimiter(bytes, &delimiter, start)
            .ok_or_else(|| broken("終わりの区切りがありません"))?;
        parts += 1;
        if parts > MAX_PARTS {
            return Err(too_many(format!("欄が {MAX_PARTS} を超えています")));
        }
        read_part(body, start, end, &mut out)?;
        after = next;
    }
    Ok(out)
}

/// 区切りの行の後ろ。
#[derive(Debug, PartialEq, Eq)]
enum After {
    /// 次の欄がここから始まる。
    Part(usize),
    /// 終わりの区切り（`--boundary--`）だった。
    Close,
}

/// `at`（`--boundary` の直後）から後ろが区切りの行なら、その読み方を返す。
///
/// RFC 2046 では、区切りの行は `--boundary` の後ろに `--`（終わり）か、
/// 空白とタブ（transport-padding）と CRLF が続く形だけです。
/// それ以外（`--boundaryX` など）は区切りではなく、中身の一部として扱います。
fn after_boundary(bytes: &[u8], at: usize) -> Option<After> {
    let rest = bytes.get(at..)?;
    if rest.starts_with(b"--") {
        return Some(After::Close);
    }
    let padding = rest
        .iter()
        .take_while(|b| matches!(b, b' ' | b'\t'))
        .count();
    rest[padding..]
        .starts_with(b"\r\n")
        .then_some(After::Part(at + padding + 2))
}

/// `from` から後ろで、区切りの行（CRLF `--boundary`）を探す。
///
/// 見つけた位置（CRLF の位置。前の欄の中身はここで終わる）と、後ろの読み方を返します。
fn next_delimiter(bytes: &[u8], delimiter: &[u8], from: usize) -> Option<(usize, After)> {
    let mut i = from;
    while let Some(offset) = find(&bytes[i..], delimiter) {
        let at = i + offset;
        if let Some(after) = after_boundary(bytes, at + delimiter.len()) {
            return Some((at, after));
        }
        i = at + 1;
    }
    None
}

/// `haystack` の中で最初の `needle` の位置。
///
/// 先頭の 1 バイト（`\r`）で候補を絞ってから突き合わせます。
/// `needle` は `\r\n--` で始まるので、候補がすぐ外れます。
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let first = *needle.first()?;
    let last_start = haystack.len().checked_sub(needle.len())?;
    let mut i = 0;
    while i <= last_start {
        let at = i + haystack[i..=last_start].iter().position(|&b| b == first)?;
        if haystack[at..].starts_with(needle) {
            return Some(at);
        }
        i = at + 1;
    }
    None
}

/// 欄 1 つ（`body[start..end]`）を読んで `out` に足す。
fn read_part(body: &Arc<Vec<u8>>, start: usize, end: usize, out: &mut Multipart) -> Result<()> {
    let part = &body[start..end];
    let (head, content) = split_head(part)?;
    let headers = parse_headers(head)?;
    let mut dispositions = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("content-disposition"))
        .map(|(_, value)| value.as_str());
    let disposition = dispositions
        .next()
        .ok_or_else(|| broken("欄に Content-Disposition がありません"))?;
    // 2 本あると、どちらを読むかで前段（WAF など）と食い違わせられる。
    if dispositions.next().is_some() {
        return Err(broken("欄に Content-Disposition が 2 本あります"));
    }
    let disposition = parse_disposition(disposition)?;

    let Some(filename) = disposition.filename else {
        let value = String::from_utf8_lossy(&part[content..]).into_owned();
        out.fields.push((disposition.name, value));
        return Ok(());
    };
    // 古いブラウザは手元のパスごと送ってきます。Laravel（Symfony）と同じく名前だけにします。
    let filename = base_name(&filename);
    if filename.is_empty() {
        // ファイルを選ばずに送った欄。PHP と同じく、無かったことにします。
        return Ok(());
    }
    if out.files.len() >= MAX_FILES {
        return Err(too_many(format!("ファイルが {MAX_FILES} を超えています")));
    }
    let mime = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| value.as_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("application/octet-stream");
    out.files.push((
        disposition.name,
        UploadedFile::new(
            filename.to_string(),
            mime.to_string(),
            Arc::clone(body),
            start + content..end,
        ),
    ));
    Ok(())
}

/// 欄を見出しと中身に分ける。中身の始まりの位置（欄の中での位置）を返します。
fn split_head(part: &[u8]) -> Result<(&[u8], usize)> {
    // 見出しが 1 行も無い欄（区切りの直後が空行）。
    if part.starts_with(b"\r\n") {
        return Ok((&[], 2));
    }
    // 見出しの終わり（空行）を、上限の長さの中だけで探す。
    let window = &part[..part.len().min(MAX_PART_HEADER_BYTES + 4)];
    match find(window, b"\r\n\r\n") {
        Some(at) if at <= MAX_PART_HEADER_BYTES => Ok((&part[..at], at + 4)),
        _ if part.len() > MAX_PART_HEADER_BYTES => Err(too_many(format!(
            "欄の見出しが {MAX_PART_HEADER_BYTES} バイトを超えています"
        ))),
        _ => Err(broken("欄の見出しの終わり（空行）がありません")),
    }
}

/// 見出しの行を名前と値の組にする。
///
/// 値は UTF-8 として読みます（ブラウザは日本語のファイル名をそのまま UTF-8 で送ります）。
///
/// 空白かタブで始まる行（古い折り返しの形。obs-fold）は 400 です。
/// RFC 7230 が廃止した形で、ブラウザは送りません。読むと、折り返しを読まない前段と
/// 見出しの中身が食い違います。
fn parse_headers(head: &[u8]) -> Result<Vec<(String, String)>> {
    let text = String::from_utf8_lossy(head);
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.split("\r\n") {
        if line.starts_with([' ', '\t']) {
            return Err(broken("見出しの折り返し（空白で始まる行）は受けません"));
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| broken("見出しの行に `:` がありません"))?;
        out.push((name.trim().to_string(), value.trim().to_string()));
    }
    Ok(out)
}

/// `Content-Disposition` から読んだもの。
#[derive(Debug, PartialEq, Eq)]
struct Disposition {
    name: String,
    /// `filename`（`filename*` があればそちら）。無ければ文字の欄です。
    filename: Option<String>,
}

/// `form-data; name="avatar"; filename="a.jpg"` を読む。
///
/// | 項目 | 決めたこと |
/// |---|---|
/// | 種類 | `form-data` だけ（RFC 7578 の 4.2） |
/// | `name` | 無ければ 400 |
/// | 同じ引数が 2 回 | 400。どちらを使うかで食い違わせる手口を断る |
/// | 値の無い引数（`; foo;`）・閉じの `"` の後ろに続く文字 | 400。上の確認をすり抜けるため |
/// | 引用符の中の `\"` と `\\` | 1 文字に戻す（RFC 2045 の quoted-string） |
/// | `%22` など | 戻さない。ブラウザが `"` を `%22` にして送りますが、PHP も戻しません |
/// | `filename*` | `filename` より優先する（RFC 6266 の 4.3）。UTF-8 と ISO-8859-1 だけ |
fn parse_disposition(value: &str) -> Result<Disposition> {
    let (kind, mut rest) = value.split_once(';').unwrap_or((value, ""));
    if !kind.trim().eq_ignore_ascii_case("form-data") {
        return Err(broken("Content-Disposition が form-data ではありません"));
    }
    let (mut name, mut filename, mut extended) = (None, None, None);
    loop {
        rest = rest.trim_start_matches([' ', '\t', ';']);
        if rest.is_empty() {
            break;
        }
        let (key, after) = rest
            .split_once('=')
            .ok_or_else(|| broken("Content-Disposition の引数の形が正しくありません"))?;
        // 引数の名前は token（RFC 9110）だけ。値の無い引数があると `foo; filename` が
        // 1 つの名前になり、`filename` を読み落としてファイルが文字の欄に化けていました。
        let key = key.trim();
        if key.is_empty() || !key.bytes().all(is_token_byte) {
            return Err(broken("Content-Disposition の引数の形が正しくありません"));
        }
        let after = after.trim_start_matches([' ', '\t']);
        let (parsed, remaining) = match after.strip_prefix('"') {
            Some(quoted_part) => {
                let (parsed, remaining) = quoted(quoted_part)?;
                // 閉じの `"` の後ろは、空白の後に `;` か終わりだけ。
                let remaining = remaining.trim_start_matches([' ', '\t']);
                if !(remaining.is_empty() || remaining.starts_with(';')) {
                    return Err(broken("Content-Disposition の引用符の後ろに文字があります"));
                }
                (parsed, remaining)
            }
            None => {
                let end = after.find(';').unwrap_or(after.len());
                (after[..end].trim_end().to_string(), &after[end..])
            }
        };
        rest = remaining;
        let slot = match key.to_ascii_lowercase().as_str() {
            "name" => &mut name,
            "filename" => &mut filename,
            "filename*" => &mut extended,
            _ => continue,
        };
        if slot.is_some() {
            return Err(broken("Content-Disposition に同じ引数が 2 回あります"));
        }
        *slot = Some(parsed);
    }
    let name = name.ok_or_else(|| broken("欄に name がありません"))?;
    let filename = match extended {
        Some(raw) => match (decode_extended(&raw), filename) {
            (Some(decoded), _) => Some(decoded),
            (None, Some(plain)) => Some(plain),
            (None, None) => return Err(broken("filename* を読めません")),
        },
        None => filename,
    };
    Ok(Disposition { name, filename })
}

/// 引用符で囲んだ値を読む。`s` は開きの `"` の後ろから。値と、閉じの `"` の後ろを返します。
fn quoted(s: &str) -> Result<(String, &str)> {
    let mut out = String::new();
    let mut chars = s.char_indices();
    while let Some((index, c)) = chars.next() {
        match c {
            '"' => return Ok((out, &s[index + 1..])),
            '\\' => match chars.clone().next() {
                Some((_, next @ ('"' | '\\'))) => {
                    out.push(next);
                    chars.next();
                }
                _ => out.push('\\'),
            },
            other => out.push(other),
        }
    }
    Err(broken("引用符が閉じていません"))
}

/// `filename*` の値（`UTF-8''%E7%94%BB.jpg`）を読む。読めなければ `None`。
///
/// RFC 7578 の 4.2 は、送る側が `filename*` を使わないように決めています。
/// ただ、送ってくる道具はあるので、受けるときは読みます（RFC 5987 / 8187 の形）。
fn decode_extended(raw: &str) -> Option<String> {
    let mut pieces = raw.splitn(3, '\'');
    let (charset, _language, encoded) = (pieces.next()?, pieces.next()?, pieces.next()?);
    let bytes = percent_decode_strict(encoded)?;
    if charset.eq_ignore_ascii_case("utf-8") {
        String::from_utf8(bytes).ok()
    } else if charset.eq_ignore_ascii_case("iso-8859-1") {
        Some(bytes.into_iter().map(char::from).collect())
    } else {
        None
    }
}

/// `%xx` を戻す。壊れた `%` があれば `None`（`+` は空白にしません）。
///
/// `xx` は 16 進の 2 桁だけです。`u8::from_str_radix` は先頭の `+` を受ける
/// （`%+F` が通る）ので使いません。
fn percent_decode_strict(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let high = hex_value(*bytes.get(i + 1)?)?;
            let low = hex_value(*bytes.get(i + 2)?)?;
            out.push(high << 4 | low);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

/// パスの最後の要素（`C:\Users\a\b.jpg` → `b.jpg`）。
fn base_name(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

/// 16 進の 1 桁の値。16 進でなければ `None`。
fn hex_value(byte: u8) -> Option<u8> {
    char::from(byte).to_digit(16).map(|digit| digit as u8)
}

/// token の文字か（RFC 9110 の tchar）。
fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

/// `Content-Type` の `boundary` を取り出す。形が正しくなければ `None`。
///
/// RFC 2046 の 5.1.1 の形だけを通します。
///
/// - 1〜70 文字
/// - 使える文字は英数字と `'()+_,-./:=?` と空白
/// - 末尾は空白にしない
///
/// 値は引用符で囲んでもかまいません。
fn boundary(content_type: &str) -> Option<String> {
    for param in content_type.split(';').skip(1) {
        let Some((key, value)) = param.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("boundary") {
            continue;
        }
        let value = value.trim();
        let value = match value.strip_prefix('"') {
            Some(inner) => inner.strip_suffix('"')?,
            None => value,
        };
        let usable = |b: u8| b.is_ascii_alphanumeric() || b"'()+_,-./:=? ".contains(&b);
        let ok =
            (1..=70).contains(&value.len()) && value.bytes().all(usable) && !value.ends_with(' ');
        return ok.then(|| value.to_string());
    }
    None
}

/// 形が壊れている（400）。
fn broken(reason: &str) -> Error {
    Error::http(400, format!("multipart の本文を読めません（{reason}）"))
}

/// 上限を超えた（413）。本文の上限と同じ状態コードです。
fn too_many(reason: String) -> Error {
    Error::http(413, format!("multipart の本文が大きすぎます（{reason}）"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CT: &str = "multipart/form-data; boundary=XyZ";

    fn run(body: &[u8]) -> Result<Multipart> {
        parse(&Arc::new(body.to_vec()), CT)
    }

    fn fields(m: &Multipart) -> Vec<(&str, &str)> {
        m.fields
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }

    #[test]
    fn 文字の欄を送られた順に読む() {
        let body = b"--XyZ\r\n\
            Content-Disposition: form-data; name=\"b\"\r\n\r\n\
            2\r\n\
            --XyZ\r\n\
            Content-Disposition: form-data; name=\"a\"\r\n\r\n\
            1\r\n\
            --XyZ\r\n\
            Content-Disposition: form-data; name=\"b\"\r\n\r\n\
            3\r\n\
            --XyZ--\r\n";
        let m = run(body).unwrap();
        assert_eq!(fields(&m), [("b", "2"), ("a", "1"), ("b", "3")]);
        assert!(m.files.is_empty());
    }

    #[test]
    fn ファイルの欄を読み_中身は本文を共有する() {
        let body = b"--XyZ\r\n\
            Content-Disposition: form-data; name=\"avatar\"; filename=\"a.png\"\r\n\
            Content-Type: image/png\r\n\r\n\
            \x89PNG\r\n\x1a\n\x00\r\n\
            --XyZ--";
        let shared = Arc::new(body.to_vec());
        let m = parse(&shared, CT).unwrap();
        let (name, file) = &m.files[0];
        assert_eq!(name, "avatar");
        assert_eq!(file.client_original_name(), "a.png");
        assert_eq!(file.client_mime_type(), "image/png");
        // 中身の中の CRLF はそのまま。最後の CRLF は区切りの一部なので入らない。
        assert_eq!(file.get(), b"\x89PNG\r\n\x1a\n\x00");
        // 写していない（本文と同じ置き場所を指す）。
        assert_eq!(Arc::strong_count(&shared), 2);
    }

    #[test]
    fn 前置きと後置きは読み捨てる() {
        let body = b"preamble\r\nmore\r\n--XyZ\r\n\
            Content-Disposition: form-data; name=\"a\"\r\n\r\n\
            1\r\n\
            --XyZ--\r\nepilogue --XyZ\r\n";
        assert_eq!(fields(&run(body).unwrap()), [("a", "1")]);
    }

    #[test]
    fn 境界の後ろの空白は読み飛ばす() {
        // RFC 2046 の transport-padding。
        let body = b"--XyZ \t\r\n\
            Content-Disposition: form-data; name=\"a\"\r\n\r\n\
            1\r\n\
            --XyZ--";
        assert_eq!(fields(&run(body).unwrap()), [("a", "1")]);
    }

    #[test]
    fn 境界に続く別の文字は区切りではない() {
        // `--XyZW` は区切りの行ではない。中身の一部として残す。
        let body = b"--XyZ\r\n\
            Content-Disposition: form-data; name=\"a\"\r\n\r\n\
            x\r\n--XyZW\r\ny\r\n\
            --XyZ--";
        assert_eq!(fields(&run(body).unwrap()), [("a", "x\r\n--XyZW\r\ny")]);
    }

    #[test]
    fn 中身が空の欄も読める() {
        let body = b"--XyZ\r\n\
            Content-Disposition: form-data; name=\"a\"\r\n\r\n\
            \r\n\
            --XyZ--";
        assert_eq!(fields(&run(body).unwrap()), [("a", "")]);
    }

    #[test]
    fn 日本語のファイル名を読む() {
        // ブラウザは UTF-8 のまま送る。
        let mut body = b"--XyZ\r\nContent-Disposition: form-data; name=\"f\"; filename=\"".to_vec();
        body.extend_from_slice("画像.jpg".as_bytes());
        body.extend_from_slice(b"\"\r\n\r\nabc\r\n--XyZ--");
        let m = run(&body).unwrap();
        assert_eq!(m.files[0].1.client_original_name(), "画像.jpg");

        // `filename*` があれば、そちらを使う（RFC 6266 の 4.3）。
        let body = b"--XyZ\r\n\
            Content-Disposition: form-data; name=\"f\"; filename=\"fallback.jpg\"; \
            filename*=UTF-8''%E7%94%BB%E5%83%8F.jpg\r\n\r\n\
            abc\r\n--XyZ--";
        let m = run(body).unwrap();
        assert_eq!(m.files[0].1.client_original_name(), "画像.jpg");

        // ISO-8859-1 も読む。
        let body = b"--XyZ\r\n\
            Content-Disposition: form-data; name=\"f\"; filename*=iso-8859-1'en'caf%E9.txt\r\n\r\n\
            abc\r\n--XyZ--";
        assert_eq!(
            run(body).unwrap().files[0].1.client_original_name(),
            "café.txt"
        );

        // 読めない `filename*` は `filename` に戻る。どちらも無ければ 400。
        let body = b"--XyZ\r\n\
            Content-Disposition: form-data; name=\"f\"; filename=\"plain.txt\"; filename*=UTF-8''%ZZ\r\n\r\n\
            abc\r\n--XyZ--";
        assert_eq!(
            run(body).unwrap().files[0].1.client_original_name(),
            "plain.txt"
        );
        let body = b"--XyZ\r\n\
            Content-Disposition: form-data; name=\"f\"; filename*=UTF-8''%FF\r\n\r\n\
            abc\r\n--XyZ--";
        assert_eq!(run(body).unwrap_err().status(), 400);
    }

    #[test]
    fn ファイル名はパスを落とし_引用符の中を戻す() {
        let body = b"--XyZ\r\n\
            Content-Disposition: form-data; name=\"f\"; filename=\"C:\\\\Users\\\\a\\\\b.jpg\"\r\n\r\n\
            abc\r\n--XyZ--";
        assert_eq!(
            run(body).unwrap().files[0].1.client_original_name(),
            "b.jpg"
        );
        let body = b"--XyZ\r\n\
            Content-Disposition: form-data; name=\"f\"; filename=\"../../etc/passwd\"\r\n\r\n\
            abc\r\n--XyZ--";
        assert_eq!(
            run(body).unwrap().files[0].1.client_original_name(),
            "passwd"
        );
        let body = b"--XyZ\r\n\
            Content-Disposition: form-data; name=\"a\\\"b\"; filename=\"x%22y.txt\"\r\n\r\n\
            abc\r\n--XyZ--";
        let m = run(body).unwrap();
        assert_eq!(m.files[0].0, "a\"b");
        // `%22` は戻さない（PHP と同じ）。
        assert_eq!(m.files[0].1.client_original_name(), "x%22y.txt");
    }

    #[test]
    fn ファイルを選ばずに送った欄は入れない() {
        let body = b"--XyZ\r\n\
            Content-Disposition: form-data; name=\"avatar\"; filename=\"\"\r\n\
            Content-Type: application/octet-stream\r\n\r\n\
            \r\n\
            --XyZ\r\n\
            Content-Disposition: form-data; name=\"title\"\r\n\r\n\
            t\r\n\
            --XyZ--";
        let m = run(body).unwrap();
        assert!(m.files.is_empty());
        assert_eq!(fields(&m), [("title", "t")]);
    }

    #[test]
    fn 種類の無いファイルはoctet_stream() {
        let body = b"--XyZ\r\n\
            Content-Disposition: form-data; name=\"f\"; filename=\"a.bin\"\r\n\r\n\
            abc\r\n--XyZ--";
        let m = run(body).unwrap();
        assert_eq!(m.files[0].1.client_mime_type(), "application/octet-stream");
    }

    #[test]
    fn 見出しの名前は大文字小文字を区別しない() {
        let body = b"--XyZ\r\n\
            content-disposition: FORM-DATA; name=\"a\"\r\n\
            X-Other: 1\r\n\r\n\
            1\r\n--XyZ--";
        assert_eq!(fields(&run(body).unwrap()), [("a", "1")]);
    }

    #[test]
    fn 見出しの折り返しは400() {
        // 以前は前の行の続きとして読んでいた。折り返しを読まない前段と食い違う。
        let body = b"--XyZ\r\n\
            content-disposition: form-data;\r\n name=\"a\"\r\n\r\n\
            1\r\n--XyZ--";
        assert_eq!(run(body).unwrap_err().status(), 400);
    }

    #[test]
    fn 引数の形の食い違いは400() {
        let cases: &[&[u8]] = &[
            // 閉じの `"` の後ろに文字。以前は 2 つ目の filename を読み飛ばし、重複の確認をすり抜けた。
            b"--XyZ\r\nContent-Disposition: form-data; name=\"a\"; filename=\"x\"junk; filename=\"y\"\r\n\r\n1\r\n--XyZ--",
            // 値の無い引数。以前は `foo; filename` を知らない引数として読み飛ばし、ファイルが文字の欄になった。
            b"--XyZ\r\nContent-Disposition: form-data; name=\"a\"; foo; filename=\"b\"\r\n\r\n1\r\n--XyZ--",
            // Content-Disposition が 2 本。
            b"--XyZ\r\nContent-Disposition: form-data; name=\"a\"\r\nContent-Disposition: form-data; name=\"b\"; filename=\"c\"\r\n\r\n1\r\n--XyZ--",
        ];
        for body in cases {
            let error = run(body)
                .err()
                .unwrap_or_else(|| panic!("{:?} が通った", String::from_utf8_lossy(body)));
            assert_eq!(error.status(), 400, "{:?}", String::from_utf8_lossy(body));
        }
        // 閉じの `"` の後ろの空白はかまわない。
        let body = b"--XyZ\r\nContent-Disposition: form-data; name=\"a\" ; filename=\"b.txt\" \r\n\r\n1\r\n--XyZ--";
        assert_eq!(
            run(body).unwrap().files[0].1.client_original_name(),
            "b.txt"
        );
    }

    #[test]
    fn filename星の16進は2桁だけ() {
        // `u8::from_str_radix` は `+F` を 15 と読んでいた。
        assert_eq!(percent_decode_strict("%+F"), None);
        assert_eq!(percent_decode_strict("%-1"), None);
        assert_eq!(percent_decode_strict("%E"), None);
        assert_eq!(percent_decode_strict("%e7%94"), Some(vec![0xe7, 0x94]));
    }

    #[test]
    fn 引用符の無い値も読む() {
        let body = b"--XyZ\r\n\
            Content-Disposition: form-data; name=a; filename=b.txt\r\n\r\n\
            1\r\n--XyZ--";
        let m = run(body).unwrap();
        assert_eq!(m.files[0].0, "a");
        assert_eq!(m.files[0].1.client_original_name(), "b.txt");
    }

    #[test]
    fn 壊れた本文は400() {
        let cases: &[&[u8]] = &[
            // 区切りが無い。
            b"name=a",
            // 終わりの区切りが無い（途中で切れた）。
            b"--XyZ\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\n1",
            b"--XyZ\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\n1\r\n--XyZ",
            // 見出しの終わりが無い。
            b"--XyZ\r\nContent-Disposition: form-data; name=\"a\"\r\n--XyZ--",
            // Content-Disposition が無い。
            b"--XyZ\r\nContent-Type: text/plain\r\n\r\n1\r\n--XyZ--",
            b"--XyZ\r\n\r\n1\r\n--XyZ--",
            // form-data でない。
            b"--XyZ\r\nContent-Disposition: attachment; name=\"a\"\r\n\r\n1\r\n--XyZ--",
            // name が無い。
            b"--XyZ\r\nContent-Disposition: form-data; filename=\"a\"\r\n\r\n1\r\n--XyZ--",
            // 同じ引数が 2 回。
            b"--XyZ\r\nContent-Disposition: form-data; name=\"a\"; name=\"b\"\r\n\r\n1\r\n--XyZ--",
            // 引用符が閉じていない。
            b"--XyZ\r\nContent-Disposition: form-data; name=\"a\r\n\r\n1\r\n--XyZ--",
            // 見出しの行に `:` が無い。
            b"--XyZ\r\nbroken\r\n\r\n1\r\n--XyZ--",
        ];
        for body in cases {
            let error = run(body)
                .err()
                .unwrap_or_else(|| panic!("{:?} が通った", String::from_utf8_lossy(body)));
            assert_eq!(error.status(), 400, "{:?}", String::from_utf8_lossy(body));
        }
    }

    #[test]
    fn 境界の形はrfc2046に従う() {
        assert_eq!(boundary(CT).as_deref(), Some("XyZ"));
        assert_eq!(
            boundary("multipart/form-data; charset=utf-8; BOUNDARY=\"a b:c?\"").as_deref(),
            Some("a b:c?")
        );
        let longest = "a".repeat(70);
        assert_eq!(
            boundary(&format!("multipart/form-data; boundary={longest}")),
            Some(longest)
        );
        for bad in [
            "multipart/form-data",
            "multipart/form-data; boundary=",
            "multipart/form-data; boundary=\"\"",
            "multipart/form-data; boundary=\"abc",
            "multipart/form-data; boundary=\"ab \"",
            "multipart/form-data; boundary=a<b",
            "multipart/form-data; boundary=あ",
        ] {
            assert_eq!(boundary(bad), None, "{bad}");
        }
        let too_long = format!("multipart/form-data; boundary={}", "a".repeat(71));
        assert_eq!(boundary(&too_long), None);
        let error = parse(&Arc::new(b"--XyZ--".to_vec()), "multipart/form-data").unwrap_err();
        assert_eq!(error.status(), 400);
    }

    #[test]
    fn multipartの判定() {
        assert!(is_multipart("multipart/form-data; boundary=a"));
        assert!(is_multipart("Multipart/Form-Data"));
        assert!(!is_multipart("multipart/mixed; boundary=a"));
        assert!(!is_multipart("application/x-www-form-urlencoded"));
    }

    fn file_part(name: &str) -> String {
        format!(
            "--XyZ\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"a.txt\"\r\n\r\nx\r\n"
        )
    }

    #[test]
    fn ファイルの数の上限() {
        let mut body: String = (0..MAX_FILES).map(|_| file_part("f[]")).collect();
        body.push_str("--XyZ--");
        assert_eq!(run(body.as_bytes()).unwrap().files.len(), MAX_FILES);

        let mut body: String = (0..=MAX_FILES).map(|_| file_part("f[]")).collect();
        body.push_str("--XyZ--");
        assert_eq!(run(body.as_bytes()).unwrap_err().status(), 413);

        // 選ばずに送った欄は数えない。
        let mut body: String = (0..MAX_FILES).map(|_| file_part("f[]")).collect();
        body.push_str(
            "--XyZ\r\nContent-Disposition: form-data; name=\"g\"; filename=\"\"\r\n\r\n\r\n",
        );
        body.push_str("--XyZ--");
        assert_eq!(run(body.as_bytes()).unwrap().files.len(), MAX_FILES);
    }

    #[test]
    fn 欄の数の上限() {
        let part = "--XyZ\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\n1\r\n";
        let mut body = part.repeat(MAX_PARTS);
        body.push_str("--XyZ--");
        assert_eq!(run(body.as_bytes()).unwrap().fields.len(), MAX_PARTS);

        let mut body = part.repeat(MAX_PARTS + 1);
        body.push_str("--XyZ--");
        assert_eq!(run(body.as_bytes()).unwrap_err().status(), 413);
    }

    #[test]
    fn 見出しの長さの上限() {
        let head = |len: usize| {
            let prefix = "Content-Disposition: form-data; name=\"a\"\r\nX-Pad: ";
            let pad = "p".repeat(len - prefix.len());
            format!("--XyZ\r\n{prefix}{pad}\r\n\r\n1\r\n--XyZ--")
        };
        assert!(run(head(MAX_PART_HEADER_BYTES).as_bytes()).is_ok());
        assert_eq!(
            run(head(MAX_PART_HEADER_BYTES + 1).as_bytes())
                .unwrap_err()
                .status(),
            413
        );
        // 終わりの空行が無いまま長いときも 413。
        let endless = format!(
            "--XyZ\r\nContent-Disposition: form-data; name=\"a\"\r\nX-Pad: {}\r\n--XyZ--",
            "p".repeat(MAX_PART_HEADER_BYTES)
        );
        assert_eq!(run(endless.as_bytes()).unwrap_err().status(), 413);
    }

    #[test]
    fn 探し方() {
        assert_eq!(find(b"abc\r\n--XyZ", b"\r\n--XyZ"), Some(3));
        assert_eq!(find(b"\r\r\n--XyZ", b"\r\n--XyZ"), Some(1));
        assert_eq!(find(b"\r\n--Xy", b"\r\n--XyZ"), None);
        assert_eq!(find(b"", b"\r\n"), None);
    }
}
