//! `key:generate`：`APP_KEY` を作って `.env` に書き込む。
//!
//! `.env` の読み方は、起動のときの読み込み（`env_vars`）とそろえます。

use crate::env_vars;
use crate::error::Result;
use crate::paths;

/// `APP_KEY` を作って `.env` に書き込む。
///
/// すでにある場合は、`--force` が無いかぎり上書きしません。
/// 鍵を変えると、いま動いているセッションと署名付き URL が全部無効になるためです。
pub(super) fn generate_key(rest: &[String]) -> Result<()> {
    let force = parse_force(rest)?;
    let path = paths::base_path().join(".env");

    if !path.is_file() {
        return Err(crate::Error::msg(format!(
            "{} がありません。先に .env を作ってください",
            path.display()
        )));
    }

    let body = std::fs::read_to_string(&path)?;

    if app_key_is_set(&body) && !force {
        println!(
            "APP_KEY はすでに設定されています。作り直すと、いまのセッションと\n\
             署名付き URL がすべて無効になります。それでもよければ --force を付けてください。"
        );
        return Ok(());
    }

    // 書き換える**前**の `.env` で見分ける（書いた後だと、新しい鍵と比べてしまう）。
    let real_key = real_app_key_present(&body);
    let key = crate::support::crypto::random_token();
    replace_file(&path, &write_app_key(&body, &key))?;
    println!("APP_KEY を {} に書き込みました。", path.display());
    if let Some(warning) = real_app_key_warning(real_key) {
        eprintln!("{warning}");
    }
    Ok(())
}

/// 本物の環境変数に `APP_KEY` が入っているか（`.env` から入れたものを除く）。
///
/// 起動のときに `.env` を読み込んでいるので、`std::env::var` だけでは見分けられません。
/// 書き換える前の `.env`（`body`）の値と同じなら、`.env` から来たものと見なします。
fn real_app_key_present(body: &str) -> bool {
    std::env::var("APP_KEY").is_ok_and(|value| is_real_app_key(&value, body))
}

/// 環境変数の値 `value` が、`.env`（`body`）から読み込んだものでないか。
fn is_real_app_key(value: &str, body: &str) -> bool {
    if value.trim().is_empty() {
        return false;
    }
    let from_dotenv = body
        .lines()
        .find_map(app_key_value)
        .map(env_vars::dotenv_value);
    from_dotenv.as_deref() != Some(value)
}

/// `.env` に書いても、本物の環境変数の `APP_KEY` が勝つときの警告。
///
/// `.env` は設定済みの変数を上書きしません。書いた鍵が使われないまま
/// 「書き込みました」で終わると、気づけないためです。
fn real_app_key_warning(present: bool) -> Option<&'static str> {
    present.then_some(
        "警告: 環境変数 APP_KEY が設定されています。環境変数 APP_KEY が優先されます。\n\
         .env に書いた鍵を使うには、環境変数 APP_KEY を外してください。",
    )
}

/// `.env` の中身で、`APP_KEY` がすでに設定されているか。
///
/// **`.env` の読み込みと同じ関数で読みます**（`env_vars::parse_dotenv`）。
/// `=` の右側をそのまま見ていたころは、`APP_KEY=""` や `APP_KEY= # 後で` を
/// 「設定済み」と見て書き込みませんでした。読み込みではどちらも空なので、
/// 「設定されています」と言われたのに起動すると「設定されていません」になります。
///
/// 見るのは**最初の `APP_KEY` の行**です。読み込みは先に出てきた行を採るためです。
///
/// `${VAR}` も読み込みと同じく「本物の環境変数 > `.env` の前の行」で展開します。
/// 本物の環境変数は、`.env` を読み込む**前**に写したもの（[`REAL_ENV`]）を使います。
/// 読み込んだ後の環境変数で展開すると、`APP_KEY=${LATER}`（`LATER` は後の行）が
/// 起動では空なのに、ここでは「設定済み」になっていました。
fn app_key_is_set(body: &str) -> bool {
    match REAL_ENV.get() {
        Some(real) => app_key_is_set_with(body, &|name| real.get(name).cloned()),
        None => app_key_is_set_with(body, &|name| std::env::var(name).ok()),
    }
}

/// [`app_key_is_set`] の本体。本物の環境変数を `real` で受けます（テストのため）。
fn app_key_is_set_with(body: &str, real: &dyn Fn(&str) -> Option<String>) -> bool {
    // 読み込みと同じく、空の環境変数は無いものと見る。
    let real = |name: &str| real(name).filter(|value| !value.trim().is_empty());
    let body = body.strip_prefix('\u{feff}').unwrap_or(body);
    let (pairs, _) = env_vars::parse_dotenv(body, &real);
    pairs
        .into_iter()
        .find(|(key, _)| key == "APP_KEY")
        .is_some_and(|(_, value)| !value.trim().is_empty())
}

/// `.env` を読み込む前の環境変数（`key:generate` のときだけ写す）。
pub(super) static REAL_ENV: std::sync::OnceLock<std::collections::HashMap<String, String>> =
    std::sync::OnceLock::new();

/// ファイルの中身を、一時ファイルに書いてから置き換える。
///
/// `std::fs::write` は切り詰めてから書くので、途中で落ちると `.env` が空か
/// 途中までになります。`.env` が壊れると、次の起動で設定が全部既定値に戻ります。
/// 同じディレクトリに書いてから `rename` で置き換えます
/// （`support::fs::write_atomic`。`schedule.rs` の前回時刻と同じ作りです）。
///
/// - **リンクなら、指している先を置き換えます。** リンクそのものを置き換えると、
///   共有の `.env` へのリンクが普通のファイルに変わってしまいます。
/// - **権限は元のファイルから引き継ぎます。** 一時ファイルは、中身を書く前に
///   所有者だけに絞ります（鍵が入るため）。
/// - 読み取り専用のファイルは、これまでどおり書き込まずにエラーにします。
///
/// `init` が `Cargo.toml` に足りない行を足すときも使います（`cli::cargo_toml`）。
/// 途中で落ちて `Cargo.toml` が壊れると、`cargo` 自体が動かなくなるためです。
pub(crate) fn replace_file(path: &std::path::Path, body: &str) -> Result<()> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let permissions = std::fs::metadata(&target)?.permissions();
    if permissions.readonly() {
        return Err(crate::Error::msg(format!(
            "{} は読み取り専用なので書き込めません",
            path.display()
        )));
    }

    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".env".to_string());
    let temp = target.with_file_name(format!("{name}.tmp.{}", std::process::id()));

    // 所有者だけに絞って書き、元の権限を付け直してから置き換える。
    // 失敗したときの書きかけは `write_atomic` が消します。
    crate::support::fs::write_atomic(
        &temp,
        &target,
        body.as_bytes(),
        crate::support::fs::Access::Keep(permissions),
    )?;
    Ok(())
}

/// `.env` の中身に `APP_KEY` を入れた文字列を作る。
///
/// すでに行があれば差し替え、無ければ末尾に足します。改行は元のままです。
fn write_app_key(body: &str, key: &str) -> String {
    let newline = newline_of(body);
    if body.lines().any(|line| app_key_value(line).is_some()) {
        let mut out: Vec<String> = body
            .lines()
            .map(|line| replace_app_key(line, key))
            .collect();
        out.push(String::new());
        return out.join(newline);
    }
    let mut out = body.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push_str(newline);
    }
    out.push_str("APP_KEY=");
    out.push_str(key);
    out.push_str(newline);
    out
}

/// `key:generate` の旗を読む。`--force` だけを受け付けます。
///
/// **受け取った引数だけを見ます。** `std::env::args()` を読み直すと、
/// `--forse` のような打ち間違いが黙って「上書きしない」側に落ちます。
fn parse_force(rest: &[String]) -> Result<bool> {
    let mut force = false;
    for arg in rest {
        if arg == "--force" {
            force = true;
        } else {
            return Err(crate::Error::msg(format!(
                "key:generate は `{arg}` を受け付けません（使えるのは --force だけです）"
            )));
        }
    }
    Ok(force)
}

/// 書き戻すときの改行を決める。
///
/// 元の改行を保ちます。`.gitignore` と `Cargo.toml` の書き換えと同じ方針です。
/// LF にそろえると、CRLF の `.env` が git の差分で全行変わって見えます。
fn newline_of(body: &str) -> &'static str {
    if body.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

/// `.env` の1行が `APP_KEY` の行なら、その値を返す。
///
/// `.env` の読み込みと**同じ前処理**をします（前後の空白と先頭の `export` を外す）。
/// そろえないと `export APP_KEY=…` を見落とし、末尾に 2 行目を足してしまいます。
/// 読み込みは先に出てくる値を採るので、鍵は変わらないのに「書き込みました」と出ます。
///
/// 先頭の BOM も外します（読み込みが外すため。Windows のメモ帳が付けます）。
/// 残すと 1 行目の `APP_KEY=` を見落とし、同じく末尾に 2 行目を足します。
///
/// キーの切り出しも読み込みと同じです（**最初の `=` で分け、キーの前後の空白を外す**）。
/// `APP_KEY=` で始まるかだけを見ていたころは、`APP_KEY = ` や `APP_KEY =値` を
/// 見落としていました。読み込みはどちらも `APP_KEY` と読むので、末尾に足した
/// 2 行目は使われず、空の鍵のまま「書き込みました」と出ていました。
fn app_key_value(line: &str) -> Option<&str> {
    let line = env_vars::strip_export(line.trim_start_matches('\u{feff}').trim());
    if line.starts_with('#') {
        return None;
    }
    let (key, rest) = line.split_once('=')?;
    (key.trim() == "APP_KEY").then_some(rest)
}

/// `APP_KEY` の行なら値を差し替える。そうでなければそのまま返す。
///
/// 最初の `=` までは残します（`export `・行頭の空白・`=` の前の空白）。
/// 利用者の書き方を変えないためです。
fn replace_app_key(line: &str, key: &str) -> String {
    if app_key_value(line).is_none() {
        return line.to_string();
    }
    match line.find('=') {
        Some(at) => format!("{}{key}", &line[..=at]),
        None => line.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 空の値やコメントだけの鍵は未設定と見る() {
        // `.env` の読み込みでは、どれも空になる。以前は「設定済み」と見て書き込まなかった。
        assert!(!app_key_is_set("APP_KEY=\n"));
        assert!(!app_key_is_set("APP_KEY=\"\"\n"));
        assert!(!app_key_is_set("APP_KEY=''\n"));
        assert!(!app_key_is_set("APP_KEY= # 後で\n"));
        assert!(!app_key_is_set("APP_KEY=#後で\n"));
        assert!(!app_key_is_set("APP_KEY=\"   \"\n"));
        assert!(!app_key_is_set("APP_NAME=myapp\n"), "行が無い");
        assert!(!app_key_is_set(""));

        assert!(app_key_is_set("APP_KEY=abc\n"));
        assert!(app_key_is_set("APP_KEY=\"abc\"\n"));
        assert!(app_key_is_set("export APP_KEY=abc # 本番用\n"));
        assert!(app_key_is_set("APP_NAME=myapp\r\nAPP_KEY=abc\r\n"));

        // 読み込みは先に出てきた行を採る。判定も同じ行を見る。
        assert!(!app_key_is_set("APP_KEY=\nAPP_KEY=abc\n"));
        assert!(app_key_is_set("APP_KEY=abc\nAPP_KEY=\n"));
    }

    #[test]
    fn 鍵の中の変数は読み込みと同じ規則で展開する() {
        let none = |_: &str| None;
        // 後の行の変数は使えない（読み込みでは空になる）。以前は読み込んだ後の
        // 環境変数で展開していたので、ここだけ「設定済み」になっていた。
        assert!(!app_key_is_set_with("APP_KEY=${LATER}\nLATER=abc\n", &none));
        // 前の行の変数は使える。
        assert!(app_key_is_set_with(
            "EARLIER=abc\nAPP_KEY=${EARLIER}\n",
            &none
        ));
        // 本物の環境変数が先。空の環境変数は無いものと見る。
        let real = |name: &str| (name == "FROM_REAL").then(|| "xyz".to_string());
        assert!(app_key_is_set_with("APP_KEY=\"${FROM_REAL}\"\n", &real));
        let blank = |_: &str| Some("  ".to_string());
        assert!(!app_key_is_set_with("APP_KEY=${ANY}\n", &blank));
        // `'...'` の中は展開しない（文字のまま入るので設定済み）。
        assert!(app_key_is_set_with("APP_KEY='${LATER}'\n", &none));
        // 読めない行は読み込みと同じく飛ばす。
        assert!(!app_key_is_set_with("APP KEY=abc\n", &none));
    }

    #[test]
    fn bomつきの1行目の鍵も見つける() {
        // メモ帳が付ける BOM。見落とすと末尾に 2 行目を足し、空の 1 行目が勝つ。
        let body = "\u{feff}APP_KEY=\nAPP_NAME=myapp\n";
        assert_eq!(app_key_value("\u{feff}APP_KEY=abc"), Some("abc"));
        let updated = write_app_key(body, "new");
        assert_eq!(updated, "\u{feff}APP_KEY=new\nAPP_NAME=myapp\n");
        assert_eq!(updated.matches("APP_KEY=").count(), 1);
    }

    /// テスト用の空ディレクトリ。
    fn temp_dir(label: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("bengara-kernel-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn envは一時ファイル経由で置き換える() {
        let dir = temp_dir("replace");
        let path = dir.join(".env");
        std::fs::write(&path, "APP_NAME=myapp\nAPP_KEY=old\n").unwrap();

        replace_file(&path, "APP_NAME=myapp\nAPP_KEY=new\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "APP_NAME=myapp\nAPP_KEY=new\n"
        );

        // 書きかけ（`.env.tmp.*`）が残らない。
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, [".env"], "残っている: {left:?}");

        // 無いファイルは作らない（`key:generate` は先に `.env` の有無を確かめる）。
        assert!(replace_file(&dir.join("無い"), "x").is_err());
        assert!(!dir.join("無い").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 読み取り専用のenvには書かない() {
        let dir = temp_dir("readonly");
        let path = dir.join(".env");
        std::fs::write(&path, "APP_KEY=old\n").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions).unwrap();

        // 置き換え（rename）は、読み取り専用でも通ってしまう OS がある。
        // 直接書いていたころと同じく、書かずにエラーにする。
        let error = replace_file(&path, "APP_KEY=new\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("読み取り専用"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "APP_KEY=old\n");

        // 後片付けのために書ける状態へ戻す。
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        let _ = std::fs::set_permissions(&path, permissions);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exportつきの鍵の行も見つける() {
        assert_eq!(app_key_value("APP_KEY=abc"), Some("abc"));
        assert_eq!(app_key_value("export APP_KEY=abc"), Some("abc"));
        assert_eq!(app_key_value("export\tAPP_KEY=abc"), Some("abc"));
        assert_eq!(app_key_value("  APP_KEY=abc"), Some("abc"));
        assert_eq!(app_key_value("APP_KEY="), Some(""));
        // コメントと別のキーは触らない。
        assert_eq!(app_key_value("# APP_KEY=abc"), None);
        assert_eq!(app_key_value("APP_KEY_OLD=abc"), None);
    }

    #[test]
    fn exportつきの行が置き換わる() {
        // 以前は見落として末尾に2行目を足し、先に出てくる古い値が勝っていた。
        let body = "APP_NAME=myapp\nexport APP_KEY=old\n";
        let updated = write_app_key(body, "new");
        assert_eq!(updated, "APP_NAME=myapp\nexport APP_KEY=new\n");
        // `APP_KEY` の行は1本だけ。
        assert_eq!(updated.matches("APP_KEY=").count(), 1);
    }

    #[test]
    fn イコールの前後に空白がある行も読み込みと同じに見つける() {
        // 読み込みは最初の `=` で分けてキーの空白を外す。判定と置き換えもそろえる。
        // 以前は見落として末尾に 2 行目を足し、空の 1 行目が勝っていた。
        assert_eq!(app_key_value("APP_KEY = "), Some(""));
        assert_eq!(app_key_value("APP_KEY =abc"), Some("abc"));
        assert_eq!(app_key_value("  export  APP_KEY\t= abc"), Some(" abc"));
        assert_eq!(app_key_value("APP_KEYS=abc"), None);
        assert_eq!(app_key_value("#APP_KEY=abc"), None);
        assert!(!app_key_is_set("APP_KEY = \n"));
        assert!(app_key_is_set("APP_KEY = abc\n"));

        // `=` までの書き方は残し、`APP_KEY` の行は 1 本のまま。
        let updated = write_app_key("APP_NAME=myapp\nAPP_KEY = \n", "new");
        assert_eq!(updated, "APP_NAME=myapp\nAPP_KEY =new\n");
        assert_eq!(updated.matches("APP_KEY").count(), 1);
        assert_eq!(
            write_app_key("export APP_KEY =old\n", "new"),
            "export APP_KEY =new\n"
        );
        // 値に `=` が入っていても、最初の `=` の後ろを差し替える。
        assert_eq!(write_app_key("APP_KEY=a=b\n", "new"), "APP_KEY=new\n");
    }

    #[test]
    fn 行が無ければ末尾に足す() {
        assert_eq!(write_app_key("A=1\n", "new"), "A=1\nAPP_KEY=new\n");
        // 改行で終わっていない中身にも足せる。
        assert_eq!(write_app_key("A=1", "new"), "A=1\nAPP_KEY=new\n");
    }

    #[test]
    fn crlfのenvはcrlfのまま保つ() {
        assert_eq!(newline_of("A=1\r\n"), "\r\n");
        assert_eq!(newline_of("A=1\n"), "\n");
        let body = "APP_NAME=myapp\r\nAPP_KEY=old\r\n";
        assert_eq!(
            write_app_key(body, "new"),
            "APP_NAME=myapp\r\nAPP_KEY=new\r\n"
        );
        // 足すときも元の改行にそろえる。
        assert_eq!(write_app_key("A=1\r\n", "new"), "A=1\r\nAPP_KEY=new\r\n");
    }

    #[test]
    fn 本物の環境変数のapp_keyを見分ける() {
        // `.env` から読み込んだ値と同じなら、本物の環境変数ではない。
        assert!(!is_real_app_key("abc", "APP_KEY=abc\n"));
        assert!(!is_real_app_key("abc", "APP_KEY=\"abc\" # コメント\n"));
        // 違う値・`.env` に無い値は本物の環境変数。
        assert!(is_real_app_key("abc", "APP_KEY=xyz\n"));
        assert!(is_real_app_key("abc", "APP_NAME=x\n"));
        // 空は「無い」。
        assert!(!is_real_app_key("  ", "APP_NAME=x\n"));
        // 警告の文。
        assert!(real_app_key_warning(false).is_none());
        let warning = real_app_key_warning(true).unwrap();
        assert!(
            warning.contains("環境変数 APP_KEY が優先されます"),
            "{warning}"
        );
    }

    #[test]
    fn 知らない旗はエラーにする() {
        assert!(!parse_force(&[]).unwrap());
        assert!(parse_force(&["--force".to_string()]).unwrap());
        // 打ち間違いが黙って通らないこと。
        let error = parse_force(&["--forse".to_string()]).unwrap_err();
        assert!(error.to_string().contains("--forse"), "{error}");
    }
}
