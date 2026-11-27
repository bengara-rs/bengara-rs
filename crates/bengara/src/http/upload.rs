//! アップロードされたファイル。Laravel の `UploadedFile` に当たります。
//!
//! ```ignore
//! if let Some(avatar) = req.file("avatar") {
//!     let path = avatar.store("avatars").await?; // "avatars/3f2a…9c.png"
//! }
//! ```
//!
//! **送られてきた名前と MIME は信じません。** `store` の名前は乱数で作り、拡張子は
//! 中身から推したものだけを付けます。推すのは画像の 5 種類（JPEG・PNG・GIF・WebP・BMP）だけです。
//! HTML と SVG には推しません。`public` の置き場所に入れて配ると、ブラウザがそのまま開くためです。

use std::ops::Range;
use std::sync::Arc;

use crate::error::Result;
use crate::storage::Storage;

/// アップロードされたファイル 1 つ。`req.file("avatar")` で受け取ります。
///
/// 中身は要求の本文を共有しています（写していません）。`Clone` も安く済みます。
///
/// | Laravel | bengara |
/// |---|---|
/// | `getClientOriginalName()` | [`client_original_name`](Self::client_original_name) |
/// | `getClientOriginalExtension()` | [`client_original_extension`](Self::client_original_extension) |
/// | `getClientMimeType()` | [`client_mime_type`](Self::client_mime_type) |
/// | `getSize()` | [`size`](Self::size) |
/// | `extension()` | [`extension`](Self::extension)（中身から推す） |
/// | `getMimeType()` | [`mime_type`](Self::mime_type)（中身から推す） |
/// | `get()` | [`get`](Self::get) |
/// | `store('avatars')` | [`store`](Self::store) |
/// | `storeAs('avatars', 'a.jpg')` | [`store_as`](Self::store_as) |
/// | `store('avatars', 'public')` | `Storage::disk("public").put_file("avatars", file)` |
#[derive(Clone)]
pub struct UploadedFile {
    /// 送られてきた名前（パスは落としたもの）。
    name: String,
    /// 送られてきた種類（`Content-Type`）。無ければ `application/octet-stream`。
    mime: String,
    /// 要求の本文そのもの。中身はこの中の `range` です。
    body: Arc<Vec<u8>>,
    range: Range<usize>,
}

/// 中身は出しません。名前・送られてきた種類・大きさだけです。
impl std::fmt::Debug for UploadedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UploadedFile")
            .field("client_original_name", &self.name)
            .field("client_mime_type", &self.mime)
            .field("size", &self.size())
            .finish()
    }
}

impl UploadedFile {
    /// 作る。`multipart.rs` が本文を読んだときに使います。
    pub(crate) fn new(name: String, mime: String, body: Arc<Vec<u8>>, range: Range<usize>) -> Self {
        Self {
            name,
            mime,
            body,
            range,
        }
    }

    /// 送られてきたファイル名。**信じないでください。**
    ///
    /// 手元のパス（`C:\Users\...\a.jpg`）が付いてきたときは、名前だけにしてあります。
    /// それ以外は相手が送ったままです。保存の名前に使うときは、自分で確かめてください。
    pub fn client_original_name(&self) -> &str {
        &self.name
    }

    /// 送られてきたファイル名の拡張子（最後の `.` より後ろ）。無ければ空文字。
    ///
    /// **信じないでください。** 中身から推した拡張子は [`extension`](Self::extension) です。
    pub fn client_original_extension(&self) -> &str {
        self.name.rsplit_once('.').map_or("", |(_, ext)| ext)
    }

    /// 送られてきた種類（`Content-Type`）。無ければ `application/octet-stream`。
    ///
    /// **信じないでください。** 中身から推した種類は [`mime_type`](Self::mime_type) です。
    pub fn client_mime_type(&self) -> &str {
        &self.mime
    }

    /// 大きさ（バイト）。
    pub fn size(&self) -> u64 {
        self.range.len() as u64
    }

    /// 中身。
    pub fn get(&self) -> &[u8] {
        &self.body[self.range.clone()]
    }

    /// 中身から推した拡張子。推せなければ `None`。
    ///
    /// | 中身 | 返す値 |
    /// |---|---|
    /// | JPEG | `jpg` |
    /// | PNG | `png` |
    /// | GIF | `gif` |
    /// | WebP | `webp` |
    /// | BMP | `bmp` |
    /// | それ以外（HTML・SVG を含む） | `None` |
    pub fn extension(&self) -> Option<&'static str> {
        self.kind().map(ImageKind::extension)
    }

    /// 中身から推した種類（`image/png` など）。推せなければ `None`。
    ///
    /// 推す範囲は [`extension`](Self::extension) と同じです。
    pub fn mime_type(&self) -> Option<&'static str> {
        self.kind().map(ImageKind::mime)
    }

    /// 既定の置き場所の `path` の下に、乱数の名前で保存する。保存したパスを返します。
    ///
    /// ```ignore
    /// let path = req.file("avatar").unwrap().store("avatars").await?;
    /// // "avatars/3f2a…9c.png"
    /// ```
    ///
    /// 名前は 40 文字の乱数です。拡張子は中身から推したもの（[`extension`](Self::extension)）
    /// だけを付け、推せなければ付けません。置き場所を選ぶときは
    /// [`Storage::put_file`] を使ってください。
    pub async fn store(&self, path: &str) -> Result<String> {
        Storage::default_disk()?.put_file(path, self).await
    }

    /// 既定の置き場所の `path` の下に、`name` の名前で保存する。保存したパスを返します。
    ///
    /// `name` は 1 つの名前だけです（`/` と `\` は断ります）。ほかは `Storage` の
    /// パスの確かめを通します（`..`・装置の名前・制御文字などを断ります）。
    /// 同じ名前があれば上書きします。置き場所を選ぶときは [`Storage::put_file_as`] を使ってください。
    pub async fn store_as(&self, path: &str, name: &str) -> Result<String> {
        Storage::default_disk()?.put_file_as(path, self, name).await
    }

    /// 保存に使う乱数の名前（40 文字の 16 進と、推した拡張子）。
    pub(crate) fn hash_name(&self) -> String {
        let random = crate::support::crypto::to_hex(&crate::support::crypto::random_bytes::<20>());
        match self.extension() {
            Some(ext) => format!("{random}.{ext}"),
            None => random,
        }
    }

    /// 中身から推した種類。
    pub(crate) fn kind(&self) -> Option<ImageKind> {
        ImageKind::sniff(self.get())
    }
}

/// 中身から推せる種類。**ここに無いものは推しません。**
///
/// HTML と SVG を入れないのは、`public` の置き場所に `.html` や `.svg` で置くと、
/// ブラウザがそのページとして開いてしまうためです（他人のスクリプトが動く）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImageKind {
    Jpeg,
    Png,
    Gif,
    Webp,
    Bmp,
}

/// 検査の規則 `mimes` に書ける名前。`jpg` と `jpeg` はどちらも JPEG です。
pub(crate) const MIMES_NAMES: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "bmp"];

impl ImageKind {
    /// 先頭のバイトから推す。
    pub(crate) fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(Self::Jpeg)
        } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some(Self::Gif)
        } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
            Some(Self::Webp)
        } else if is_bmp(bytes) {
            Some(Self::Bmp)
        } else {
            None
        }
    }

    pub(crate) const fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Gif => "gif",
            Self::Webp => "webp",
            Self::Bmp => "bmp",
        }
    }

    pub(crate) const fn mime(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
            Self::Bmp => "image/bmp",
        }
    }

    /// `mimes` の名前（`jpg` など）に当たるか。
    pub(crate) fn matches(self, name: &str) -> bool {
        match self {
            Self::Jpeg => name == "jpg" || name == "jpeg",
            other => name == other.extension(),
        }
    }
}

/// BMP か。`BM` の 2 バイトだけでは文字のファイル（`BMW…`）にも当たるので、
/// 後ろの情報の見出しの長さ（14 バイト目からの 4 バイト）が既知の値かも見ます。
fn is_bmp(bytes: &[u8]) -> bool {
    if bytes.len() < 26 || !bytes.starts_with(b"BM") {
        return false;
    }
    let size = u32::from_le_bytes([bytes[14], bytes[15], bytes[16], bytes[17]]);
    matches!(size, 12 | 40 | 52 | 56 | 64 | 108 | 124)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, mime: &str, content: &[u8]) -> UploadedFile {
        let len = content.len();
        UploadedFile::new(name.into(), mime.into(), Arc::new(content.to_vec()), 0..len)
    }

    fn bmp() -> Vec<u8> {
        let mut bytes = b"BM".to_vec();
        bytes.resize(14, 0);
        bytes.extend_from_slice(&40u32.to_le_bytes());
        bytes.resize(40, 0);
        bytes
    }

    #[test]
    fn 先頭のバイトから推す() {
        let cases: &[(&[u8], Option<ImageKind>)] = &[
            (b"\xFF\xD8\xFF\xE0rest", Some(ImageKind::Jpeg)),
            (b"\x89PNG\r\n\x1a\nrest", Some(ImageKind::Png)),
            (b"GIF87a...", Some(ImageKind::Gif)),
            (b"GIF89a...", Some(ImageKind::Gif)),
            (b"RIFF\x00\x00\x00\x00WEBPVP8 ", Some(ImageKind::Webp)),
            (&bmp(), Some(ImageKind::Bmp)),
            // 推さないもの。
            (b"<!DOCTYPE html><script>alert(1)</script>", None),
            (b"<html><body></body></html>", None),
            (
                b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"/>",
                None,
            ),
            (b"<svg onload=\"alert(1)\"/>", None),
            (b"%PDF-1.7", None),
            (b"BMW is a car maker and this is plain text.", None),
            (b"RIFF\x00\x00\x00\x00WAVEfmt ", None),
            (b"\xFF\xD8", None),
            (b"", None),
        ];
        for (bytes, expected) in cases {
            assert_eq!(
                ImageKind::sniff(bytes),
                *expected,
                "{:?}",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn 送られてきた名前と種類はそのまま見せ_中身から推したものと分ける() {
        // 名前は `.png`、種類は `image/png` と偽った HTML。
        let fake = file("a.png", "image/png", b"<html><script>alert(1)</script>");
        assert_eq!(fake.client_original_name(), "a.png");
        assert_eq!(fake.client_original_extension(), "png");
        assert_eq!(fake.client_mime_type(), "image/png");
        assert_eq!(fake.extension(), None);
        assert_eq!(fake.mime_type(), None);
        assert_eq!(fake.size(), 31);

        // 名前は `.txt` だが中身は JPEG。
        let jpeg = file("photo.txt", "text/plain", b"\xFF\xD8\xFF\xE0");
        assert_eq!(jpeg.extension(), Some("jpg"));
        assert_eq!(jpeg.mime_type(), Some("image/jpeg"));

        assert_eq!(file("noext", "", b"").client_original_extension(), "");
        assert_eq!(file("a.tar.gz", "", b"").client_original_extension(), "gz");
    }

    #[test]
    fn 乱数の名前には推した拡張子だけを付ける() {
        let png = file("a.html", "text/html", b"\x89PNG\r\n\x1a\n");
        let name = png.hash_name();
        assert_eq!(name.len(), 44, "{name}");
        assert!(name.ends_with(".png"), "{name}");
        assert!(name[..40].bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(png.hash_name(), name, "毎回ちがう");

        // 推せなければ拡張子を付けない（送られてきた `.html` も使わない）。
        let html = file("a.html", "text/html", b"<html></html>");
        assert_eq!(html.hash_name().len(), 40);
        assert!(!html.hash_name().contains('.'));
    }

    #[test]
    fn mimesの名前に当たるか() {
        assert!(ImageKind::Jpeg.matches("jpg"));
        assert!(ImageKind::Jpeg.matches("jpeg"));
        assert!(ImageKind::Png.matches("png"));
        assert!(!ImageKind::Png.matches("jpg"));
        for name in MIMES_NAMES {
            let hits = [
                ImageKind::Jpeg,
                ImageKind::Png,
                ImageKind::Gif,
                ImageKind::Webp,
                ImageKind::Bmp,
            ]
            .iter()
            .filter(|kind| kind.matches(name))
            .count();
            assert_eq!(hits, 1, "{name}");
        }
    }

    #[test]
    fn デバッグ表示に中身を出さない() {
        let f = file("a.txt", "text/plain", b"secret-content");
        let shown = format!("{f:?}");
        assert!(!shown.contains("secret-content"), "{shown}");
        assert!(
            shown.contains("a.txt") && shown.contains("size: 14"),
            "{shown}"
        );
    }
}
