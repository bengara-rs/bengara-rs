//! 静的ファイルの配信。
//!
//! 入口は2つあり、パスで**どちらか一方**に決まります。両方を続けて探すことはしません。
//!
//! | 入口                       | 探す先                                             |
//! |----------------------------|----------------------------------------------------|
//! | `/storage/...` で始まる    | `storage/app/public/` だけ                         |
//! | それ以外                   | 埋め込んだ `public/` → 無ければディスクの `public/` |
//!
//! どちらも `GET` と `HEAD` のときだけです。`/storage/x.css` が無くても
//! `public/storage/x.css` は探しません。
//!
//! 振り分けの**前に** `normalize_path` を1回だけ通します。以前は生のパスで
//! `/storage/` を判定していたので、`//storage/x.css` と `/%73torage/x.css` が
//! `public/` 側の枝に入り、`public/storage/x.css` を返していました
//! （上の「どちらか一方」が守れていませんでした）。
//!
//! `public/storage/` の中身は、**どんな書き方でも返しません**。守る場所は 2 つあります。
//!
//! | 場所 | 見るもの | 止めるもの |
//! |---|---|---|
//! | 入口（`is_storage_path`） | 最初の区間が `storage` か。**大文字小文字は区別しない** | `/STORAGE/x.css`。Windows では `/storage\x.css` も |
//! | `public/` の配信（`serve_public`） | 開く直前の**実体**が `public/storage/` の中か | 入口をすり抜けた書き方（Windows の `/storage./x.css` など） |
//!
//! 大文字小文字を区別しないファイルシステム（Windows、macOS の既定）では、
//! `public/STORAGE/x.css` と `public/storage/x.css` は同じファイルです。入口が
//! 小文字の `/storage/` だけを見ていたころは、`/STORAGE/x.css` が `public/` 側の枝に入り、
//! `public/storage/x.css` を返していました。
//!
//! 埋め込みはリリースビルドのときだけ入ります（`bengara-build` が `PROFILE` を見ます）。
//! デバッグビルドでは常にディスクを読むので、ファイルを直せばすぐ反映されます。
//!
//! ## 2回目以降の読み込み
//!
//! どちらの経路でも `ETag` と `Cache-Control` を付けます。ブラウザが
//! `If-None-Match` を送ってきて中身が変わっていなければ、本文を返さずに 304 を返します。
//! 画面1枚に20本のアセットがあると、リロードごとの全文送信がそのまま消えます。
//!
//! ETag は**弱い ETag**（`W/"..."`）です。ディスクのファイルは「長さと更新時刻」、
//! 埋め込みは「長さと中身のハッシュ」から作ります（起動時に 1 回だけ）。
//! ディスクのファイルは、開いて長さと更新時刻を採るだけで決まります。
//! 304 を返すときは本文を読みません。
//!
//! ディスクのファイルには `Last-Modified` も付けます。`If-None-Match` が無いときだけ
//! `If-Modified-Since` を見て、更新されていなければ 304 です（RFC 9110 の 13.1.3。
//! `If-None-Match` があればそちらが優先します）。埋め込みには更新時刻が無いので付けません。
//!
//! ## 一部だけの読み込み（`Range`）
//!
//! `Accept-Ranges: bytes` を付け、`Range: bytes=...` の**1 つだけの範囲**に 206 で応えます。
//! iOS の Safari は、範囲で読めないと `<video>` を再生しません。
//!
//! | 届いたもの | 応答 |
//! |---|---|
//! | 範囲が 1 つ（`bytes=0-99`・`bytes=100-`・`bytes=-100`） | 206 ＋ `Content-Range: bytes 0-99/長さ` |
//! | 範囲がファイルの外（`bytes=999-` で長さが 10 など） | 416 ＋ `Content-Range: bytes */長さ` |
//! | 範囲が 2 つ以上・形が読めない・`bytes` 以外の単位 | 範囲を無視して 200 で全部 |
//! | `If-Range` が合わない（弱い ETag はいつも合わない） | 範囲を無視して 200 で全部 |
//! | `HEAD` | 範囲を無視して 200（本文は読まない） |
//!
//! ディスクのファイルは、範囲の分だけを読みます。範囲の無い 200 は今までどおり全部を
//! メモリに読んでから返します。

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use crate::http::Response;

/// `/storage/...` で配信する頭のパス。
pub(crate) const STORAGE_PREFIX: &str = "/storage/";

/// `/storage/...` の応答に付ける `Content-Security-Policy`。
///
/// `sandbox` は、その文書をスクリプト実行もフォーム送信もできない別オリジンとして扱わせます。
/// `storage/app/public/` は利用者が上げたファイルの置き場所なので、`.svg` や `.html` を
/// 直接開かれても自分のオリジンでスクリプトが動かないようにします。
/// `public/` と埋め込みは自分で置いたファイルなので付けません。
pub(crate) const STORAGE_CSP: &str = "sandbox";

/// 振り分けの前に、リクエストのパスを1回だけ正規化する。
///
/// やることは2つです。
///
/// 1. 続いた `/` をまとめ、空の区切りを落とす（`//storage/x` → `/storage/x`）。
/// 2. `%xx` を元に戻す（`/%73torage/x` → `/storage/x`）。
///
/// 以前はここを通さず、`/storage/` の判定を**生のパス**で行い、その後の探索だけが
/// 復号していました。そのため2つの入口の振り分けと、実際に探す先が食い違いました。
/// 正規化した形で判定も探索も行うので、食い違いが起きません。
///
/// `+` は空白にしません。`+` が空白なのはフォームの規則で、URL のパスには
/// 当てはまりません。末尾の `/` は落ちます（`/storage/` は `/storage` になり、
/// どちらも 404 です）。
pub(crate) fn normalize_path(request_path: &str) -> String {
    let decoded = super::percent::decode_strict(request_path);
    let mut out = String::with_capacity(decoded.len() + 1);
    for part in decoded.split('/') {
        if part.is_empty() {
            continue;
        }
        out.push('/');
        out.push_str(part);
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}

/// `/storage/...` の入口か。**正規化したパス**で判定します。
///
/// `/storage` だけのときも storage 側に入れます。ここで `public/` 側へ落とすと、
/// `public/storage` を探してしまい、2つの入口が混ざります（storage 側で 404）。
///
/// 見るのは**最初の区間**です。次の 2 つも storage 側に入れます。
///
/// | 書き方 | なぜ storage 側か |
/// |---|---|
/// | `/STORAGE/x.css` `/Storage` | 大文字小文字を区別しないファイルシステムでは `public/storage/` と同じ場所を指す |
/// | `/storage\x.css`（Windows だけ） | Windows は `\` もパスの区切りとして読む |
///
/// storage 側に入っても、`serve_storage` は小文字の `/storage/` で始まるものしか
/// 配りません。上の 2 つは **404** になります（`public/` 側へは落ちません）。
///
/// 大文字小文字は **どの OS でも**区別しません。OS で動きを変えると、開発機
/// （Windows / macOS）と本番（Linux）で見えるファイルが変わるためです。
/// その引き換えに、Linux でも `public/Storage/` のような名前のディレクトリは配れません。
pub(crate) fn is_storage_path(path: &str) -> bool {
    is_storage_name(first_segment(path))
}

/// 正規化したパスの最初の区間（`/storage/x.css` なら `storage`）。
///
/// Windows では `\` も区切りです。`Path::components` が `\` で分けるので、
/// ここで分けないと `/storage\x.css` が `storage\x.css` という 1 区間に見えます。
/// Unix では `\` はファイル名に使える普通の文字なので、区切りにしません。
fn first_segment(path: &str) -> &str {
    let rest = path.trim_start_matches('/');
    let end = rest
        .find(|c: char| c == '/' || (cfg!(windows) && c == '\\'))
        .unwrap_or(rest.len());
    &rest[..end]
}

/// `/storage/` の入口の名前か。大文字小文字は区別しません（理由は `is_storage_path`）。
fn is_storage_name(name: &str) -> bool {
    name.eq_ignore_ascii_case(STORAGE_PREFIX.trim_matches('/'))
}

/// 静的ファイルに付ける `Cache-Control`。
///
/// 中身が変わったことに必ず気づけるよう、毎回確かめさせます。確かめた結果
/// 変わっていなければ 304 なので、本文は流れません。
/// 名前に中身の印が入ったファイル（`app.abc123.js` など）に長い期限を付けたいときは、
/// 前段のプロキシで上書きしてください。
const CACHE_CONTROL: &str = "public, max-age=0, must-revalidate";

/// 静的ファイルに付ける `X-Content-Type-Options`。
///
/// `storage/app/public/` はアプリが置いたファイルを配る入口です。種類の取り違えで
/// `.html` や `.svg` が同じオリジンのスクリプトとして動くのを止めます。
const NO_SNIFF: &str = "nosniff";

/// 静的ファイルの応答を変える、要求の見出し（条件つきの取得と範囲）。
///
/// `Application` が要求から写して作ります。ディスクを読むときは裏のスレッドへ
/// 渡すので、借りずに持ちます。
#[derive(Debug, Default, Clone)]
pub(crate) struct Conditions {
    /// `If-None-Match`。
    pub(crate) if_none_match: Option<String>,
    /// `If-Modified-Since`。`If-None-Match` があれば見ません。
    pub(crate) if_modified_since: Option<String>,
    /// `Range`。
    pub(crate) range: Option<String>,
    /// `If-Range`。合わなければ `Range` を無視します。
    pub(crate) if_range: Option<String>,
}

impl Conditions {
    /// 要求の見出しから写す。
    pub(crate) fn from_request(req: &crate::http::Request) -> Self {
        let header = |name: &str| req.header(name).map(str::to_string);
        Self {
            if_none_match: header("if-none-match"),
            if_modified_since: header("if-modified-since"),
            // `HEAD` では範囲を見ない（`Range` が決まっているのは GET だけ。RFC 9110 の 14.2）。
            range: header("range").filter(|_| req.method() != "HEAD"),
            if_range: header("if-range"),
        }
    }
}

/// 埋め込んだ1本ぶん。
pub(crate) struct EmbeddedFile {
    body: &'static [u8],
    /// 中身から作った弱い ETag。起動時に1回だけ作ります。
    etag: String,
}

/// 埋め込んだ `public/` の一覧。鍵は `public/` からの相対パス（区切りは常に `/`）。
pub(crate) type Embedded = HashMap<&'static str, EmbeddedFile>;

/// バイナリに埋め込んだ `public/`。起動時に1回だけ入れ、以後は読むだけです。
static EMBEDDED: OnceLock<Embedded> = OnceLock::new();

/// 埋め込んだ一覧を固定する。`bengara::app!()` が生成したものを渡します。
///
/// ここで一度だけ `HashMap` にします。1本ずつ探すと、見つからないとき（404 になる
/// すべてのリクエスト）に一覧を端まで見ることになります。
/// ETag も同時に作ります。中身は変わらないので、1回作れば使い回せます。
pub(crate) fn install_embedded(files: &'static [(&'static str, &'static [u8])]) {
    // 2回目は黙って無視する（テストから複数回呼ばれても落ちないように）。
    let _ = EMBEDDED.set(
        files
            .iter()
            .map(|(key, body)| {
                (
                    *key,
                    EmbeddedFile {
                        body,
                        etag: embedded_etag(body),
                    },
                )
            })
            .collect(),
    );
}

/// 埋め込んだ一覧。入っていなければ空。
pub(crate) fn embedded() -> &'static Embedded {
    static EMPTY: OnceLock<Embedded> = OnceLock::new();
    EMBEDDED
        .get()
        .unwrap_or_else(|| EMPTY.get_or_init(Embedded::new))
}

/// バイナリに埋め込んだ `public/` から探して返す。
pub(crate) fn serve_embedded(
    files: &'static Embedded,
    request_path: &str,
    conditions: &Conditions,
) -> Option<Response> {
    if files.is_empty() {
        return None;
    }
    let relative = safe_relative(request_path)?;
    let key = to_key(&relative)?;
    // `public/storage/` の中身は埋め込みからも返さない（入口をすり抜けたときの備え）。
    if is_storage_name(first_segment(&key)) {
        return None;
    }

    // そのままの鍵で探し、無ければディレクトリとして index.html を探す。
    if let Some(file) = files.get(key.as_str()) {
        return Some(embedded_response(&key, file, conditions));
    }
    let nested = format!("{key}/index.html");
    let file = files.get(nested.as_str())?;
    Some(embedded_response(&nested, file, conditions))
}

fn embedded_response(key: &str, file: &'static EmbeddedFile, conditions: &Conditions) -> Response {
    if etag_matches(conditions.if_none_match.as_deref(), &file.etag) {
        return not_modified(&file.etag, None);
    }
    let length = file.body.len() as u64;
    let body = match requested_range(conditions, &file.etag, None, length) {
        RangeAnswer::Full => file.body,
        RangeAnswer::Unsatisfiable => return range_not_satisfiable(length),
        RangeAnswer::Partial(start, end) => {
            // `static_bytes` は `&'static [u8]` をそのまま持つので、範囲もコピーしない。
            let slice = &file.body[start as usize..=end as usize];
            return Response::static_bytes(content_type(Path::new(key)), slice)
                .with_status(206)
                .with_header("content-range", format!("bytes {start}-{end}/{length}"))
                .with_header("etag", file.etag.clone())
                .with_header("cache-control", CACHE_CONTROL)
                .with_header("accept-ranges", "bytes")
                .with_header("x-content-type-options", NO_SNIFF);
        }
    };
    // `static_bytes` は `&'static [u8]` をそのまま持つので、本文をコピーしない。
    Response::static_bytes(content_type(Path::new(key)), body)
        .with_header("etag", file.etag.clone())
        .with_header("cache-control", CACHE_CONTROL)
        .with_header("accept-ranges", "bytes")
        .with_header("x-content-type-options", NO_SNIFF)
}

/// `storage/app/public/` から探して返す（`/storage/...`）。
///
/// 置き場所の外に出ようとしたときは、エラーにせず `None`（＝404）にします。
/// 攻撃の試みに「何が起きたか」を教えないためです。
pub(crate) fn serve_storage(
    public_disk: &Path,
    canonical_root: Option<&Path>,
    request_path: &str,
    conditions: &Conditions,
    want_body: bool,
) -> Option<Response> {
    let rest = request_path.strip_prefix(STORAGE_PREFIX)?;
    // `/storage/` だけで来たときは 404。`safe_relative("")` は `index.html` を返すので、
    // そのまま渡すと `storage/app/public/index.html` を配ってしまいます。
    // 「空なら index.html」は `public/`（`GET /`）のための規則です。
    //
    // `/storage//` のように `/` が続くと `rest` が `"/"` になります。先頭の `/` を
    // 落としてから空かどうかを見ます（落とさないと空判定をすり抜けます）。
    if rest.trim_start_matches('/').is_empty() {
        return None;
    }
    let response = serve(public_disk, canonical_root, rest, conditions, want_body)?;
    // 利用者が置いたファイルを配る入口なので、`.svg` や `.html` を直接開いても
    // 同じオリジンのスクリプトとして動かないようにする。304 には本文が無いので付けない。
    if response.status() == 304 {
        return Some(response);
    }
    Some(response.with_header("content-security-policy", STORAGE_CSP))
}

/// 配信するときの鍵（`/` 区切り）に直す。
fn to_key(relative: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?),
            _ => return None,
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// ディレクトリの中からファイルを探して返す。無ければ `None`。
///
/// `canonical_root` は置き場所の**実体**です。起動時に1回だけ解決したものを渡します
/// （以前はリクエストごとに `canonicalize` を2回叩いていました）。
/// まだ無かった置き場所（`storage/` を作る前に起動したとき）は `None` で渡され、
/// そのときだけここで解決します。
///
/// `want_body` が偽のときは**ファイルを読みません**。`HEAD` で 100 MB の
/// アセットを丸ごとメモリに読んでから捨てるのを避けるためです。長さは
/// `Content-Length` で知らせます。
///
/// 本文は**全部をメモリに読んでから**返します（流しながらは返しません）。
/// 大きなファイルを同時に取りに来られると、その数だけメモリを使います。
/// `Range` の 206 では、範囲の分だけを読みます。
pub(crate) fn serve(
    public_dir: &Path,
    canonical_root: Option<&Path>,
    request_path: &str,
    conditions: &Conditions,
    want_body: bool,
) -> Option<Response> {
    serve_from(
        public_dir,
        canonical_root,
        request_path,
        conditions,
        want_body,
        false,
    )
}

/// `public/` から探して返す。**`public/storage/` の中身は返しません。**
///
/// `/storage/...` は `storage/app/public/` だけを配る入口です。入口の判定
/// （`is_storage_path`）は文字列で見るので、ファイルシステムが同じ場所として読む
/// 別の書き方を数え切れません。Windows は区間の末尾の `.` と空白を落とし
/// （`/storage./x.css`）、短い名前（`/STORAG~1/x.css`）でも同じ場所を開きます。
///
/// そこで、こちらは**開く直前の実体**で確かめます。実体は OS が解決した本当の
/// 名前なので、どんな書き方で来ても `public/storage/` の中なら分かります。
pub(crate) fn serve_public(
    public_dir: &Path,
    canonical_root: Option<&Path>,
    request_path: &str,
    conditions: &Conditions,
    want_body: bool,
) -> Option<Response> {
    serve_from(
        public_dir,
        canonical_root,
        request_path,
        conditions,
        want_body,
        true,
    )
}

/// `serve` と `serve_public` の中身。`hide_storage` が真なら `storage/` の下を返さない。
fn serve_from(
    public_dir: &Path,
    canonical_root: Option<&Path>,
    request_path: &str,
    conditions: &Conditions,
    want_body: bool,
    hide_storage: bool,
) -> Option<Response> {
    let relative = safe_relative(request_path)?;
    let mut path = public_dir.join(&relative);
    if path.is_dir() {
        path.push("index.html");
    }
    // 1. 先に**実体**を求める。シンボリックリンクなどで置き場所の外に出ていないかを、
    //    開く前に確かめるためです。無いファイルはここで失敗するので 404 になります
    //    （以前の `File::open` が失敗していたのと同じ順です）。
    let canonical = path.canonicalize().ok()?;
    let root = match canonical_root {
        Some(root) => std::borrow::Cow::Borrowed(root),
        None => std::borrow::Cow::Owned(public_dir.canonicalize().ok()?),
    };
    // 置き場所の外なら `None`（＝404）。中なら、置き場所からの相対の形が採れる。
    let below_root = canonical.strip_prefix(&*root).ok()?;
    if hide_storage && starts_in_storage(below_root) {
        return None;
    }
    // 2. 確かめた**その実体**を開く。canonical なパスにはシンボリックリンクが無いので、
    //    開く直前に差し替えられても、置き場所の中のファイルにしかなりません。
    //    以前は「開く → canonicalize → 判定」の順だったので、その間に同じパスが
    //    差し替わると、外のファイルの中身を内側と判定して返せました。
    //
    //    ファイルを開くのは**1回だけ**。情報も中身も同じハンドルから採ります
    //    （開き直すと、古い ETag や本文と合わない Content-Length を返します。判断 #140）。
    let mut file = std::fs::File::open(&canonical).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }

    let etag = disk_etag(&metadata);
    let modified = modified_seconds(&metadata);
    if not_modified_since(conditions, &etag, modified) {
        return Some(not_modified(&etag, modified));
    }

    let length = metadata.len();
    let mut response = Response::new(200)
        .with_header("content-type", content_type(&canonical))
        .with_header("etag", etag.clone())
        .with_header("cache-control", CACHE_CONTROL)
        .with_header("accept-ranges", "bytes")
        .with_header("x-content-type-options", NO_SNIFF);
    if let Some(modified) = modified {
        response = response.with_header("last-modified", http_date(modified));
    }
    if !want_body {
        // 本文を読まないので、長さは自分で知らせる。範囲は見ない（HEAD）。
        return Some(response.with_header("content-length", length.to_string()));
    }
    match requested_range(conditions, &etag, modified, length) {
        RangeAnswer::Full => {}
        RangeAnswer::Unsatisfiable => return Some(range_not_satisfiable(length)),
        RangeAnswer::Partial(start, end) => {
            // 範囲の分だけを、開いたままのハンドルから読む。
            use std::io::{Read, Seek, SeekFrom};
            file.seek(SeekFrom::Start(start)).ok()?;
            let wanted = end - start + 1;
            let mut body = Vec::with_capacity(usize::try_from(wanted).unwrap_or(0));
            file.take(wanted).read_to_end(&mut body).ok()?;
            // 読む間に縮んだときは、長さの合わない 206 を返さない。
            if body.len() as u64 != wanted {
                return None;
            }
            return Some(
                response
                    .with_status(206)
                    .with_header("content-range", format!("bytes {start}-{end}/{length}"))
                    .with_body(body),
            );
        }
    }
    // 開いたままのハンドルから読む。ETag と同じ中身であることが保証されます。
    let mut body = Vec::with_capacity(usize::try_from(length).unwrap_or(0));
    std::io::Read::read_to_end(&mut file, &mut body).ok()?;
    Some(response.with_body(body))
}

/// `If-None-Match` か `If-Modified-Since` から見て、本文を返さずに 304 にしてよいか。
///
/// `If-None-Match` があれば、それだけで決めます（RFC 9110 の 13.1.3）。
fn not_modified_since(conditions: &Conditions, etag: &str, modified: Option<u64>) -> bool {
    if conditions.if_none_match.is_some() {
        return etag_matches(conditions.if_none_match.as_deref(), etag);
    }
    match (modified, conditions.if_modified_since.as_deref()) {
        (Some(modified), Some(since)) => {
            parse_http_date(since).is_some_and(|since| modified <= since)
        }
        _ => false,
    }
}

/// 範囲の求めにどう応えるか。
#[derive(Debug, PartialEq, Eq)]
enum RangeAnswer {
    /// 全部を 200 で返す。
    Full,
    /// `start..=end` を 206 で返す。
    Partial(u64, u64),
    /// 416 を返す。
    Unsatisfiable,
}

/// `Range` と `If-Range` を読む。
///
/// `If-Range` は、強い ETag か `Last-Modified` と同じ日時のときだけ合います。
/// 作る ETag はいつも弱いので、ETag で送られた `If-Range` は合わず、全部を返します
/// （RFC 9110 の 13.1.5。弱い ETag で範囲をつなぐと、違う版の中身が混ざりえます）。
fn requested_range(
    conditions: &Conditions,
    etag: &str,
    modified: Option<u64>,
    length: u64,
) -> RangeAnswer {
    let Some(range) = conditions.range.as_deref() else {
        return RangeAnswer::Full;
    };
    if let Some(if_range) = conditions.if_range.as_deref() {
        let if_range = if_range.trim();
        let fresh = if if_range.starts_with('"') {
            // 強い ETag どうしの比較。作る ETag は弱いので、ここは合いません。
            !etag.starts_with("W/") && if_range == etag
        } else if if_range.starts_with("W/") {
            false
        } else {
            modified.is_some() && parse_http_date(if_range) == modified
        };
        if !fresh {
            return RangeAnswer::Full;
        }
    }
    parse_range(range, length)
}

/// `bytes=0-99` の形を読む。1 つだけの範囲に応えます。
///
/// 読めない形・2 つ以上の範囲・`bytes` 以外の単位は `Full`（範囲を無視する。RFC 9110 の 14.2）。
fn parse_range(header: &str, length: u64) -> RangeAnswer {
    let header = header.trim();
    let Some((unit, spec)) = header.split_once('=') else {
        return RangeAnswer::Full;
    };
    if !unit.trim().eq_ignore_ascii_case("bytes") || spec.contains(',') {
        return RangeAnswer::Full;
    }
    let Some((first, last)) = spec.trim().split_once('-') else {
        return RangeAnswer::Full;
    };
    let digits = |text: &str| -> Option<Option<u64>> {
        if text.is_empty() {
            return Some(None);
        }
        if !text.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        // 桁が多すぎて読めない値は、ファイルの長さより大きいものとして扱う。
        Some(Some(text.parse::<u64>().unwrap_or(u64::MAX)))
    };
    let (Some(first), Some(last)) = (digits(first.trim()), digits(last.trim())) else {
        return RangeAnswer::Full;
    };
    match (first, last) {
        // `bytes=-100`：末尾の 100 バイト。
        (None, Some(suffix)) => {
            if suffix == 0 || length == 0 {
                return RangeAnswer::Unsatisfiable;
            }
            RangeAnswer::Partial(length.saturating_sub(suffix), length - 1)
        }
        // `bytes=100-`：100 バイト目から最後まで。
        (Some(start), None) => {
            if start >= length {
                return RangeAnswer::Unsatisfiable;
            }
            RangeAnswer::Partial(start, length - 1)
        }
        (Some(start), Some(end)) => {
            if start > end {
                return RangeAnswer::Full;
            }
            if start >= length {
                return RangeAnswer::Unsatisfiable;
            }
            RangeAnswer::Partial(start, end.min(length - 1))
        }
        (None, None) => RangeAnswer::Full,
    }
}

/// 範囲がファイルの外だった（416）。
fn range_not_satisfiable(length: u64) -> Response {
    Response::new(416)
        .with_header("content-range", format!("bytes */{length}"))
        .with_header("accept-ranges", "bytes")
}

/// 更新時刻（UNIX 時刻の秒）。採れなければ `None`。
fn modified_seconds(metadata: &std::fs::Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_secs())
}

/// UNIX 時刻（秒）を `Sun, 06 Nov 1994 08:49:37 GMT` の形（IMF-fixdate）にする。
fn http_date(seconds: u64) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let seconds = i64::try_from(seconds).unwrap_or(i64::MAX);
    let days = seconds.div_euclid(86_400);
    let in_day = seconds.rem_euclid(86_400);
    let (year, month, day) = crate::support::time::civil_from_days(days);
    // 1970-01-01 は木曜日。
    let weekday = DAYS[(days + 4).rem_euclid(7) as usize];
    format!(
        "{weekday}, {day:02} {} {year:04} {:02}:{:02}:{:02} GMT",
        MONTHS[(month - 1) as usize],
        in_day / 3600,
        in_day % 3600 / 60,
        in_day % 60
    )
}

/// `Sun, 06 Nov 1994 08:49:37 GMT` の形を UNIX 時刻（秒）にする。読めなければ `None`。
///
/// 読むのはこの形（IMF-fixdate）だけです。古い 2 つの形（RFC 850 と asctime）は
/// 読まず、条件が無いものとして扱います（全部を返す側に倒れます）。
fn parse_http_date(text: &str) -> Option<u64> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (_weekday, rest) = text.trim().split_once(", ")?;
    let mut parts = rest.split(' ');
    let day = parts.next()?;
    let month = parts.next()?;
    let year = parts.next()?;
    let time = parts.next()?;
    if parts.next() != Some("GMT") || parts.next().is_some() || day.len() != 2 {
        return None;
    }
    let month = MONTHS.iter().position(|m| *m == month)? + 1;
    let seconds =
        crate::support::time::parse_timestamp(&format!("{year}-{month:02}-{day} {time}"))?;
    u64::try_from(seconds).ok()
}

/// 置き場所からの相対パス（実体）が `storage/` の下か。
///
/// 大文字小文字は区別しません。区別しないファイルシステムでは、実体の名前が
/// `Storage` でも `/storage/` の入口と同じ場所だからです（`is_storage_path` と同じ規則）。
fn starts_in_storage(below_root: &Path) -> bool {
    matches!(
        below_root.components().next(),
        Some(Component::Normal(first)) if first.to_str().is_some_and(is_storage_name)
    )
}

/// 本文を返さない 304。`Last-Modified` は、ディスクのファイルのときだけ付けます。
fn not_modified(etag: &str, modified: Option<u64>) -> Response {
    let response = Response::new(304)
        .with_header("etag", etag.to_string())
        .with_header("cache-control", CACHE_CONTROL);
    match modified {
        Some(modified) => response.with_header("last-modified", http_date(modified)),
        None => response,
    }
}

/// ディスクのファイルの弱い ETag。長さと更新時刻から作ります。
///
/// 更新時刻は**ナノ秒まで**使います（ファイルシステムが持つ精度のまま）。
/// 秒で丸めていたころは、1 秒の中で同じ長さに書き換えると ETag が変わらず、
/// ブラウザに古い中身を使わせ続けていました。
fn disk_etag(metadata: &std::fs::Metadata) -> String {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    format!("W/\"{:x}-{:x}\"", metadata.len(), modified)
}

/// 埋め込んだファイルの弱い ETag。長さと中身から作ります。
///
/// 埋め込みに更新時刻はありません。中身のハッシュ（FNV-1a）を使うので、
/// 版が変わればかならず違う値になります。起動時に1回だけ計算します。
fn embedded_etag(body: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in body {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("W/\"{:x}-{hash:x}\"", body.len())
}

/// `If-None-Match` が合うか。
///
/// `*` と、カンマ区切りの一覧に対応します。弱い ETag しか作らないので、
/// `W/` の有無は無視して比べます。
fn etag_matches(header: Option<&str>, etag: &str) -> bool {
    let Some(header) = header else {
        return false;
    };
    let header = header.trim();
    if header == "*" {
        return true;
    }
    let want = etag.trim_start_matches("W/");
    header
        .split(',')
        .any(|candidate| candidate.trim().trim_start_matches("W/") == want)
}

/// リクエストのパスを、`public/` の下から出られない相対パスに直す。
///
/// 渡すのは `normalize_path` を通したパスです。**ここでは復号しません**。
/// 2回復号すると `%2520` が空白になり、二重の復号になります。
fn safe_relative(request_path: &str) -> Option<PathBuf> {
    let decoded = request_path.trim_start_matches('/');
    if decoded.contains('\0') {
        return None;
    }
    // `/` は `public/index.html` を指す。
    let decoded = if decoded.is_empty() {
        "index.html"
    } else {
        decoded
    };
    let mut out = PathBuf::new();
    for component in Path::new(decoded).components() {
        match component {
            Component::Normal(part) => out.push(part),
            // `..`、`/`、`C:` などは受け付けない。
            Component::CurDir => {}
            _ => return None,
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// 拡張子から Content-Type を決める。分からなければバイナリとして扱う。
fn content_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "map" => "application/json",
        "txt" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "pdf" => "application/pdf",
        "wasm" => "application/wasm",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `If-None-Match` だけを持つ条件。
    fn etag_only(etag: &str) -> Conditions {
        Conditions {
            if_none_match: Some(etag.to_string()),
            ..Conditions::default()
        }
    }

    #[test]
    fn 親ディレクトリへは出られない() {
        assert_eq!(safe_relative("/../secret"), None);
        assert_eq!(safe_relative("/a/../../b"), None);
    }

    #[test]
    fn ルートはindexを指す() {
        assert_eq!(safe_relative("/"), Some(PathBuf::from("index.html")));
    }

    #[test]
    fn 普通のパスは通る() {
        assert_eq!(
            safe_relative("/css/app.css"),
            Some(PathBuf::from("css").join("app.css"))
        );
        assert_eq!(
            safe_relative("/robots.txt"),
            Some(PathBuf::from("robots.txt"))
        );
    }

    #[test]
    fn 鍵はスラッシュ区切りになる() {
        assert_eq!(
            to_key(&PathBuf::from("css").join("app.css")).as_deref(),
            Some("css/app.css")
        );
        assert_eq!(
            to_key(&PathBuf::from("robots.txt")).as_deref(),
            Some("robots.txt")
        );
        assert_eq!(to_key(&PathBuf::new()), None);
    }

    const FILES: &[(&str, &[u8])] = &[
        ("index.html", b"<h1>top</h1>"),
        ("css/app.css", b"body{}"),
        ("docs/index.html", b"<h1>docs</h1>"),
    ];

    /// テスト用に、埋め込みの一覧を1つ作って借り続ける。
    fn files() -> &'static Embedded {
        static ONCE: OnceLock<Embedded> = OnceLock::new();
        ONCE.get_or_init(|| {
            FILES
                .iter()
                .map(|(key, body)| {
                    (
                        *key,
                        EmbeddedFile {
                            body,
                            etag: embedded_etag(body),
                        },
                    )
                })
                .collect()
        })
    }

    fn empty() -> &'static Embedded {
        static ONCE: OnceLock<Embedded> = OnceLock::new();
        ONCE.get_or_init(Embedded::new)
    }

    #[test]
    fn 埋め込みから配信できる() {
        let res =
            serve_embedded(files(), "/css/app.css", &Conditions::default()).expect("あるはず");
        assert_eq!(res.status(), 200);
        assert_eq!(res.body(), b"body{}");
        assert_eq!(
            res.header("content-type").unwrap(),
            "text/css; charset=utf-8"
        );
        assert!(res.header("etag").is_some());
        assert_eq!(res.header("cache-control"), Some(CACHE_CONTROL));
    }

    #[test]
    fn 埋め込みのルートはindexを指す() {
        let res = serve_embedded(files(), "/", &Conditions::default()).expect("あるはず");
        assert_eq!(res.body(), b"<h1>top</h1>");
    }

    #[test]
    fn 埋め込みのディレクトリはindexを探す() {
        let res = serve_embedded(files(), "/docs", &Conditions::default()).expect("あるはず");
        assert_eq!(res.body(), b"<h1>docs</h1>");
        assert_eq!(
            res.header("content-type").unwrap(),
            "text/html; charset=utf-8"
        );
    }

    #[test]
    fn 埋め込みに無ければnone() {
        assert!(serve_embedded(files(), "/none.txt", &Conditions::default()).is_none());
        assert!(serve_embedded(files(), "/../secret", &Conditions::default()).is_none());
    }

    #[test]
    fn 埋め込みが空ならnone() {
        assert!(serve_embedded(empty(), "/css/app.css", &Conditions::default()).is_none());
    }

    #[test]
    fn 埋め込みは同じetagなら304を返す() {
        let first =
            serve_embedded(files(), "/css/app.css", &Conditions::default()).expect("あるはず");
        let etag = first.header("etag").expect("etag が付く").to_string();

        let again = serve_embedded(files(), "/css/app.css", &etag_only(&etag)).expect("あるはず");
        assert_eq!(again.status(), 304);
        assert!(again.body().is_empty(), "304 に本文は付けない");
        assert_eq!(again.header("etag"), Some(etag.as_str()));

        // 別の値なら 200 で中身を返す。
        let other =
            serve_embedded(files(), "/css/app.css", &etag_only("W/\"0-0\"")).expect("あるはず");
        assert_eq!(other.status(), 200);
    }

    #[test]
    fn etagの比べ方() {
        assert!(etag_matches(Some("*"), "W/\"1-2\""));
        assert!(etag_matches(Some("W/\"1-2\""), "W/\"1-2\""));
        // 弱い印の有無は無視する。
        assert!(etag_matches(Some("\"1-2\""), "W/\"1-2\""));
        assert!(etag_matches(Some("W/\"9-9\", W/\"1-2\""), "W/\"1-2\""));
        assert!(!etag_matches(Some("W/\"9-9\""), "W/\"1-2\""));
        assert!(!etag_matches(None, "W/\"1-2\""));
    }

    #[test]
    fn 中身が違えばetagも違う() {
        assert_ne!(embedded_etag(b"a"), embedded_etag(b"b"));
        // 長さが同じで中身が違っても分かれる。
        assert_ne!(embedded_etag(b"ab"), embedded_etag(b"ba"));
        assert_eq!(embedded_etag(b"abc"), embedded_etag(b"abc"));
    }

    #[test]
    fn storageの頭が合わなければnone() {
        let dir = std::env::temp_dir();
        assert!(serve_storage(&dir, None, "/css/app.css", &Conditions::default(), true).is_none());
        assert!(serve_storage(&dir, None, "/storage", &Conditions::default(), true).is_none());
    }

    #[test]
    fn storageだけで来たらnone() {
        // `/storage/` は index.html を指さない。`public/` 用の規則を持ち込まない。
        let dir = std::env::temp_dir();
        assert!(serve_storage(&dir, None, STORAGE_PREFIX, &Conditions::default(), true).is_none());
        // `/` が続いても同じ。先頭の `/` を落としてから空かどうかを見る。
        assert!(serve_storage(&dir, None, "/storage//", &Conditions::default(), true).is_none());
        assert!(serve_storage(&dir, None, "/storage///", &Conditions::default(), true).is_none());
    }

    #[test]
    fn storageの配信にはcspが付く() {
        let dir = temp_dir("storage-csp");
        std::fs::write(dir.join("a.svg"), "<svg/>").unwrap();
        let res = serve_storage(&dir, None, "/storage/a.svg", &Conditions::default(), true)
            .expect("あるはず");
        assert_eq!(res.header("content-security-policy"), Some(STORAGE_CSP));
        // 304 には付けない（本文が無い）。
        let etag = res.header("etag").unwrap().to_string();
        let res = serve_storage(&dir, None, "/storage/a.svg", &etag_only(&etag), true).unwrap();
        assert_eq!(res.status(), 304);
        assert_eq!(res.header("content-security-policy"), None);
        // public/ 側には付けない。
        std::fs::write(dir.join("b.svg"), "<svg/>").unwrap();
        let res = serve(&dir, None, "/b.svg", &Conditions::default(), true).expect("あるはず");
        assert_eq!(res.header("content-security-policy"), None);
    }

    #[test]
    fn 静的ファイルにはnosniffが付く() {
        // 埋め込みから。
        let res =
            serve_embedded(files(), "/css/app.css", &Conditions::default()).expect("あるはず");
        assert_eq!(res.header("x-content-type-options"), Some(NO_SNIFF));

        // ディスクから。
        let dir = temp_dir("nosniff");
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        let root = dir.canonicalize().unwrap();
        let res =
            serve(&dir, Some(&root), "/a.txt", &Conditions::default(), true).expect("あるはず");
        assert_eq!(res.header("x-content-type-options"), Some(NO_SNIFF));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn パスのプラスは空白にしない() {
        assert_eq!(normalize_path("/a+b.css"), "/a+b.css");
        assert_eq!(
            safe_relative(&normalize_path("/a+b.css")),
            Some(PathBuf::from("a+b.css"))
        );
        // `%20` は空白に戻る（こちらはパスでも同じ）。
        assert_eq!(normalize_path("/a%20b.css"), "/a b.css");
        assert_eq!(
            safe_relative(&normalize_path("/a%20b.css")),
            Some(PathBuf::from("a b.css"))
        );
    }

    #[test]
    fn パスを1回だけ正規化する() {
        // 続いた `/` はまとまる。
        assert_eq!(normalize_path("//storage/x.css"), "/storage/x.css");
        assert_eq!(normalize_path("/a//b///c.css"), "/a/b/c.css");
        // `%xx` は元に戻る。
        assert_eq!(normalize_path("/%73torage/x.css"), "/storage/x.css");
        // 末尾の `/` は落ちる。
        assert_eq!(normalize_path("/storage/"), "/storage");
        assert_eq!(normalize_path("/storage///"), "/storage");
        // `/` はそのまま（`public/index.html` を指す）。
        assert_eq!(normalize_path("/"), "/");
        assert_eq!(normalize_path(""), "/");
    }

    #[test]
    fn 正規化すれば2つの入口は混ざらない() {
        // 以前は生のパスで判定していたので、`//storage/` と `/%73torage/` が
        // `public/` 側の枝に入り、`public/storage/x.css` を返していました。
        assert!(is_storage_path(&normalize_path("/storage/x.css")));
        assert!(is_storage_path(&normalize_path("//storage/x.css")));
        assert!(is_storage_path(&normalize_path("/%73torage/x.css")));
        // `/storage` だけのときも storage 側（そこで 404）。`public/storage` は探さない。
        assert!(is_storage_path(&normalize_path("/storage/")));
        assert!(is_storage_path(&normalize_path("/storage")));
        // 関係ないパスは `public/` 側。
        assert!(!is_storage_path(&normalize_path("/css/app.css")));
        assert!(!is_storage_path(&normalize_path("/storageroom/x.css")));
    }

    #[test]
    fn storageの入口は大文字小文字を区別しない() {
        // 大文字小文字を区別しないファイルシステム（Windows、macOS の既定）では、
        // `public/STORAGE/x.css` は `public/storage/x.css` と同じファイル。
        // 小文字だけを見ていたころは `public/` 側の枝に入り、それを返していた。
        for raw in [
            "/STORAGE/x.css",
            "/Storage/x.css",
            "/sToRaGe",
            "/STORAGE/",
            "//STORAGE//x.css",
            "/%53TORAGE/x.css",
        ] {
            assert!(is_storage_path(&normalize_path(raw)), "{raw} は storage 側");
        }
        // 最初の区間だけを見る。似た名前と、2 つ目以降の区間は `public/` 側。
        for raw in [
            "/STORAGEROOM/x.css",
            "/storage.txt",
            "/css/STORAGE/x.css",
            "/",
        ] {
            assert!(!is_storage_path(&normalize_path(raw)), "{raw} は public 側");
        }
        // storage 側に入っても配らない（小文字の `/storage/` で始まるものだけを配る）。
        let dir = temp_dir("storage-case");
        std::fs::write(dir.join("x.css"), "body{}").unwrap();
        for raw in ["/STORAGE/x.css", "/Storage/x.css"] {
            let path = normalize_path(raw);
            assert!(
                serve_storage(&dir, None, &path, &Conditions::default(), true).is_none(),
                "{raw} は 404"
            );
        }
        assert!(
            serve_storage(&dir, None, "/storage/x.css", &Conditions::default(), true).is_some()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn storageの入口はwindowsでは逆スラッシュも区切りとして見る() {
        // Windows は `\` もパスの区切りとして読むので、`/storage\x.css` は
        // `public/storage/x.css` を指す。Unix では `storage\x.css` という 1 つの名前。
        for raw in ["/storage\\x.css", "/storage%5Cx.css", "/STORAGE%5cx.css"] {
            assert_eq!(
                is_storage_path(&normalize_path(raw)),
                cfg!(windows),
                "{raw}"
            );
        }
        assert_eq!(first_segment("/storage/x.css"), "storage");
        assert_eq!(first_segment("/storage"), "storage");
        assert_eq!(first_segment("/"), "");
        assert_eq!(first_segment("/a/b"), "a");
    }

    #[test]
    fn publicの配信はstorageディレクトリの中身を返さない() {
        // 入口の判定は文字列で見るので、ファイルシステムが同じ場所として読む別の
        // 書き方（Windows の `/storage./x.css` など）を数え切れない。
        // `public/` の配信は、開く直前の**実体**で `public/storage/` の中かを見る。
        let dir = temp_dir("public-storage");
        std::fs::create_dir_all(dir.join("storage").join("sub")).unwrap();
        std::fs::write(dir.join("storage").join("x.css"), "secret{}").unwrap();
        std::fs::write(dir.join("storage").join("index.html"), "<p>x</p>").unwrap();
        std::fs::write(dir.join("storage").join("sub").join("y.css"), "y{}").unwrap();
        std::fs::write(dir.join("storageroom.css"), "ok{}").unwrap();
        std::fs::create_dir_all(dir.join("css").join("storage")).unwrap();
        std::fs::write(dir.join("css").join("storage").join("z.css"), "z{}").unwrap();
        let root = dir.canonicalize().unwrap();

        // どの書き方でも返さない。区別しないファイルシステムでは同じファイルに届き、
        // 区別するファイルシステムではそもそも無い。どちらも `None`。
        for path in [
            "/storage/x.css",
            "/STORAGE/x.css",
            "/Storage/x.css",
            "/storage./x.css",
            "/storage\\x.css",
            "/storage/sub/y.css",
            "/storage",
            "/css/../storage/x.css",
        ] {
            assert!(
                serve_public(&dir, Some(&root), path, &Conditions::default(), true).is_none(),
                "{path} は返さない"
            );
            assert!(
                serve_public(&dir, None, path, &Conditions::default(), true).is_none(),
                "{path} は返さない（実体をその場で解決する道）"
            );
        }
        // 似た名前と、2 つ目以降の区間が `storage` のものは今までどおり配る。
        for path in ["/storageroom.css", "/css/storage/z.css"] {
            assert!(
                serve_public(&dir, Some(&root), path, &Conditions::default(), true).is_some(),
                "{path} は配る"
            );
        }
        // `storage/app/public/` の配信（`serve`）は、この制限を持たない。
        // `/storage/storage/x.css`（置き場所の中の `storage/` ディレクトリ）は配れる。
        assert!(serve(
            &dir,
            Some(&root),
            "/storage/x.css",
            &Conditions::default(),
            true
        )
        .is_some());
        assert!(serve_storage(
            &dir,
            Some(&root),
            "/storage/storage/x.css",
            &Conditions::default(),
            true
        )
        .is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 埋め込みもstorageディレクトリの中身を返さない() {
        const LIST: &[(&str, &[u8])] = &[
            ("storage/x.css", b"secret{}"),
            ("storage/index.html", b"<p>x</p>"),
            ("Storage/y.css", b"y{}"),
            ("storageroom.css", b"ok{}"),
        ];
        static ONCE: OnceLock<Embedded> = OnceLock::new();
        let files = ONCE.get_or_init(|| {
            LIST.iter()
                .map(|(key, body)| {
                    (
                        *key,
                        EmbeddedFile {
                            body,
                            etag: embedded_etag(body),
                        },
                    )
                })
                .collect()
        });
        for path in ["/storage/x.css", "/storage", "/Storage/y.css"] {
            assert!(
                serve_embedded(files, path, &Conditions::default()).is_none(),
                "{path}"
            );
        }
        assert!(serve_embedded(files, "/storageroom.css", &Conditions::default()).is_some());
    }

    #[test]
    fn 正規化したstorageのパスは404のまま() {
        let dir = std::env::temp_dir();
        for raw in ["/storage/", "/storage//", "/storage///", "/storage"] {
            let path = normalize_path(raw);
            assert!(is_storage_path(&path), "{raw} は storage 側");
            assert!(
                serve_storage(&dir, None, &path, &Conditions::default(), true).is_none(),
                "{raw} は 404"
            );
        }
    }

    #[test]
    fn 正規化したパスを2回復号しない() {
        // `%2520` は `%20` に戻るだけ。空白にはしない（二重の復号を避ける）。
        let path = normalize_path("/a%2520b.css");
        assert_eq!(path, "/a%20b.css");
        assert_eq!(safe_relative(&path), Some(PathBuf::from("a%20b.css")));
    }

    #[test]
    fn 拡張子から種類を決める() {
        assert_eq!(content_type(Path::new("a.css")), "text/css; charset=utf-8");
        assert_eq!(content_type(Path::new("a.PNG")), "image/png");
        assert_eq!(
            content_type(Path::new("a.unknown")),
            "application/octet-stream"
        );
    }

    /// テスト用の空ディレクトリ。名前が重ならないように用途を付ける。
    fn temp_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("bengara-statics-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn ディスクから配信してetagを付ける() {
        let dir = temp_dir("disk");
        std::fs::write(dir.join("a.txt"), "やきそば").unwrap();
        let root = dir.canonicalize().unwrap();

        let res =
            serve(&dir, Some(&root), "/a.txt", &Conditions::default(), true).expect("あるはず");
        assert_eq!(res.status(), 200);
        assert_eq!(res.body_text(), "やきそば");
        let etag = res.header("etag").expect("etag が付く").to_string();

        // 同じ etag なら 304。本文もファイルも読まない。
        let again = serve(&dir, Some(&root), "/a.txt", &etag_only(&etag), true).expect("あるはず");
        assert_eq!(again.status(), 304);
        assert!(again.body().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 一秒の中の書き換えでもetagが変わる() {
        // 秒で丸めていたころは、同じ長さの書き換えを 1 秒の中ですると ETag が同じだった。
        let dir = temp_dir("subsec");
        let path = dir.join("a.txt");
        std::fs::write(&path, "0123456789").unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        let base = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);

        file.set_modified(base + std::time::Duration::from_millis(100))
            .unwrap();
        let first = disk_etag(&file.metadata().unwrap());
        file.set_modified(base + std::time::Duration::from_millis(600))
            .unwrap();
        let second = disk_etag(&file.metadata().unwrap());
        assert_ne!(first, second, "同じ秒・同じ長さでも違う ETag になる");

        drop(file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ディスクの配信はetagと本文を同じハンドルから採る() {
        // 以前は metadata と read でファイルを2回開いていたので、その間に
        // 書き換わると ETag と本文の長さが食い違いました。
        let dir = temp_dir("onefd");
        std::fs::write(dir.join("a.txt"), "0123456789").unwrap();
        let root = dir.canonicalize().unwrap();

        let res =
            serve(&dir, Some(&root), "/a.txt", &Conditions::default(), true).expect("あるはず");
        let etag = res.header("etag").expect("etag が付く").to_string();
        // ETag の頭は長さ（16進）。本文の長さと必ずそろう。
        let head = format!("W/\"{:x}-", res.body().len());
        assert!(etag.starts_with(&head), "{etag} が {head} で始まらない");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 本文が要らないときは読まずに長さだけ返す() {
        let dir = temp_dir("head");
        std::fs::write(dir.join("a.txt"), "0123456789").unwrap();
        let root = dir.canonicalize().unwrap();

        let res =
            serve(&dir, Some(&root), "/a.txt", &Conditions::default(), false).expect("あるはず");
        assert_eq!(res.status(), 200);
        assert!(res.body().is_empty(), "本文は読まない");
        assert_eq!(res.header("content-length"), Some("10"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 置き場所の実体を渡さなくても外には出られない() {
        let dir = temp_dir("root");
        std::fs::write(dir.join("a.txt"), "x").unwrap();

        // 実体を渡さない場合は、その場で解決する（起動時に無かった置き場所のため）。
        assert!(serve(&dir, None, "/a.txt", &Conditions::default(), true).is_some());
        assert!(serve(&dir, None, "/../a.txt", &Conditions::default(), true).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn 置き場所の外を指すリンクは404() {
        // 判定は**実体**で行い、その実体を開きます。開いてから判定する順だと、
        // その間に同じパスが差し替わったときに、外のファイルの中身を内側と
        // 判定して返せました。
        let dir = temp_dir("symlink");
        let outside = temp_dir("symlink-outside");
        std::fs::write(outside.join("secret.txt"), "ひみつ").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), dir.join("link.txt")).unwrap();
        let root = dir.canonicalize().unwrap();

        assert!(serve(&dir, Some(&root), "/link.txt", &Conditions::default(), true).is_none());
        assert!(serve(&dir, None, "/link.txt", &Conditions::default(), true).is_none());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn 無いファイルはnone() {
        let dir = temp_dir("missing");
        let root = dir.canonicalize().unwrap();
        assert!(serve(&dir, Some(&root), "/none.txt", &Conditions::default(), true).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `Range` だけを持つ条件。
    fn range(value: &str) -> Conditions {
        Conditions {
            range: Some(value.to_string()),
            ..Conditions::default()
        }
    }

    #[test]
    fn 範囲の読み方() {
        use RangeAnswer::*;
        assert_eq!(parse_range("bytes=0-3", 10), Partial(0, 3));
        assert_eq!(parse_range("bytes=4-", 10), Partial(4, 9));
        assert_eq!(parse_range("bytes=-3", 10), Partial(7, 9));
        // 末尾より大きい範囲は末尾までに縮める。
        assert_eq!(parse_range("bytes=5-999", 10), Partial(5, 9));
        assert_eq!(parse_range("bytes=-999", 10), Partial(0, 9));
        assert_eq!(parse_range("BYTES = 1-2", 10), Partial(1, 2));
        // ファイルの外は 416。
        assert_eq!(parse_range("bytes=10-", 10), Unsatisfiable);
        assert_eq!(
            parse_range("bytes=99999999999999999999999-", 10),
            Unsatisfiable
        );
        assert_eq!(parse_range("bytes=-0", 10), Unsatisfiable);
        assert_eq!(parse_range("bytes=0-", 0), Unsatisfiable);
        // 読めない形・2 つ以上・ほかの単位は無視して全部。
        for ignored in [
            "bytes=0-1,4-5",
            "bytes=5-1",
            "bytes=a-b",
            "bytes=-",
            "bytes=+1-2",
            "items=0-1",
            "0-1",
        ] {
            assert_eq!(parse_range(ignored, 10), Full, "{ignored}");
        }
    }

    #[test]
    fn ディスクのファイルを範囲で返す() {
        let dir = temp_dir("range");
        std::fs::write(dir.join("v.mp4"), "0123456789").unwrap();
        let root = dir.canonicalize().unwrap();

        let full = serve(&dir, Some(&root), "/v.mp4", &Conditions::default(), true).unwrap();
        assert_eq!(full.status(), 200);
        assert_eq!(full.header("accept-ranges"), Some("bytes"));

        let res = serve(&dir, Some(&root), "/v.mp4", &range("bytes=2-5"), true).unwrap();
        assert_eq!(res.status(), 206);
        assert_eq!(res.body(), b"2345");
        assert_eq!(res.header("content-range"), Some("bytes 2-5/10"));
        assert_eq!(res.header("content-type"), Some("video/mp4"));
        assert_eq!(res.header("etag"), full.header("etag"));

        let res = serve(&dir, Some(&root), "/v.mp4", &range("bytes=-2"), true).unwrap();
        assert_eq!(res.body(), b"89");

        let res = serve(&dir, Some(&root), "/v.mp4", &range("bytes=10-"), true).unwrap();
        assert_eq!(res.status(), 416);
        assert_eq!(res.header("content-range"), Some("bytes */10"));
        assert!(res.body().is_empty());

        // 2 つ以上の範囲は全部を 200 で返す。
        let res = serve(&dir, Some(&root), "/v.mp4", &range("bytes=0-1,3-4"), true).unwrap();
        assert_eq!(res.status(), 200);
        assert_eq!(res.body(), b"0123456789");

        // storage の入口でも同じ。CSP も付く。
        let res = serve_storage(
            &dir,
            Some(&root),
            "/storage/v.mp4",
            &range("bytes=0-0"),
            true,
        )
        .unwrap();
        assert_eq!(res.status(), 206);
        assert_eq!(res.body(), b"0");
        assert_eq!(res.header("content-security-policy"), Some(STORAGE_CSP));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn if_rangeが合わなければ全部を返す() {
        let dir = temp_dir("if-range");
        let path = dir.join("v.mp4");
        std::fs::write(&path, "0123456789").unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(784_111_777))
            .unwrap();
        drop(file);
        let root = dir.canonicalize().unwrap();
        let full = serve(&dir, Some(&root), "/v.mp4", &Conditions::default(), true).unwrap();
        let etag = full.header("etag").unwrap().to_string();
        assert_eq!(
            full.header("last-modified"),
            Some("Sun, 06 Nov 1994 08:49:37 GMT")
        );

        let with_if_range = |if_range: &str| Conditions {
            range: Some("bytes=0-1".into()),
            if_range: Some(if_range.into()),
            ..Conditions::default()
        };
        // 日時が `Last-Modified` と同じなら範囲で返す。
        let res = serve(
            &dir,
            Some(&root),
            "/v.mp4",
            &with_if_range("Sun, 06 Nov 1994 08:49:37 GMT"),
            true,
        )
        .unwrap();
        assert_eq!(res.status(), 206);
        // 弱い ETag・違う日時は合わない。全部を返す。
        for stale in [etag.as_str(), "\"x\"", "Sun, 06 Nov 1994 08:49:38 GMT"] {
            let res = serve(&dir, Some(&root), "/v.mp4", &with_if_range(stale), true).unwrap();
            assert_eq!(res.status(), 200, "{stale}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 更新されていなければif_modified_sinceで304() {
        let dir = temp_dir("ims");
        let path = dir.join("a.txt");
        std::fs::write(&path, "x").unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(784_111_777))
            .unwrap();
        drop(file);
        let root = dir.canonicalize().unwrap();
        let since = |value: &str| Conditions {
            if_modified_since: Some(value.into()),
            ..Conditions::default()
        };

        for fresh in [
            "Sun, 06 Nov 1994 08:49:37 GMT",
            "Mon, 07 Nov 1994 00:00:00 GMT",
        ] {
            let res = serve(&dir, Some(&root), "/a.txt", &since(fresh), true).unwrap();
            assert_eq!(res.status(), 304, "{fresh}");
            assert_eq!(
                res.header("last-modified"),
                Some("Sun, 06 Nov 1994 08:49:37 GMT")
            );
        }
        // 古い日時・読めない日時は 200。
        for stale in [
            "Sun, 06 Nov 1994 08:49:36 GMT",
            "Sunday, 06-Nov-94 08:49:37 GMT",
            "x",
        ] {
            let res = serve(&dir, Some(&root), "/a.txt", &since(stale), true).unwrap();
            assert_eq!(res.status(), 200, "{stale}");
        }
        // `If-None-Match` があればそちらが優先する（合わなければ日時は見ない）。
        let both = Conditions {
            if_none_match: Some("W/\"0-0\"".into()),
            if_modified_since: Some("Mon, 07 Nov 1994 00:00:00 GMT".into()),
            ..Conditions::default()
        };
        let res = serve(&dir, Some(&root), "/a.txt", &both, true).unwrap();
        assert_eq!(res.status(), 200);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 埋め込みも範囲で返す() {
        let res = serve_embedded(files(), "/css/app.css", &range("bytes=1-3")).unwrap();
        assert_eq!(res.status(), 206);
        assert_eq!(res.body(), b"ody");
        assert_eq!(res.header("content-range"), Some("bytes 1-3/6"));
        assert_eq!(res.header("accept-ranges"), Some("bytes"));
        let res = serve_embedded(files(), "/css/app.css", &range("bytes=6-")).unwrap();
        assert_eq!(res.status(), 416);
        // 埋め込みには更新時刻が無いので `Last-Modified` は付けない。
        let res = serve_embedded(files(), "/css/app.css", &Conditions::default()).unwrap();
        assert_eq!(res.header("last-modified"), None);
        assert_eq!(res.header("accept-ranges"), Some("bytes"));
    }

    #[test]
    fn http_dateの往復() {
        assert_eq!(http_date(784_111_777), "Sun, 06 Nov 1994 08:49:37 GMT");
        assert_eq!(http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(
            parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(784_111_777)
        );
        assert_eq!(parse_http_date("Sun, 6 Nov 1994 08:49:37 GMT"), None);
        assert_eq!(parse_http_date("Sun, 06 Nov 1994 08:49:37 UTC"), None);
    }
}
