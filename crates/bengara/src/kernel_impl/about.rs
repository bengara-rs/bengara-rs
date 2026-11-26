//! 画面に出すコマンド：`route:list`・`about`・`--help`。

use super::{AppCommand, Builtin, Target, BUILTINS};
use crate::application::Application;
use crate::env_vars::env;
use crate::{paths, Hooks};

/// 登録されているルートを表で出す。
pub(super) fn print_routes(app: &Application) {
    let routes = app.routes();
    if routes.is_empty() {
        println!("ルートは1本も登録されていません。routes/web.rs を確かめてください。");
        return;
    }
    // 桁は**端末での幅**でそろえる。文字数で数えると、日本語のルート名で崩れます
    // （`about` や `schedule:list` と同じ数え方にそろえてあります）。
    use crate::support::text;

    let method_width = routes
        .iter()
        .map(|r| text::width(r.method))
        .max()
        .unwrap_or(6)
        .max(6);
    let path_width = routes
        .iter()
        .map(|r| text::width(&r.path))
        .max()
        .unwrap_or(4)
        .max(4);
    let name_width = routes
        .iter()
        .map(|r| text::width(r.name.as_deref().unwrap_or("")))
        .max()
        .unwrap_or(4)
        .max(4);
    // ミドルウェアが1本も無いなら、列ごと出さない。
    let any_middleware = routes.iter().any(|r| !r.middleware.is_empty());
    if any_middleware {
        println!(
            "{}  {}  {}  MIDDLEWARE",
            text::pad("METHOD", method_width),
            text::pad("URI", path_width),
            text::pad("NAME", name_width)
        );
    } else {
        println!(
            "{}  {}  NAME",
            text::pad("METHOD", method_width),
            text::pad("URI", path_width)
        );
    }
    for route in routes {
        let name = route.name.as_deref().unwrap_or("");
        if any_middleware {
            println!(
                "{}  {}  {}  {}",
                text::pad(route.method, method_width),
                text::pad(&route.path, path_width),
                text::pad(name, name_width),
                route.middleware.join(", ")
            );
        } else {
            println!(
                "{}  {}  {}",
                text::pad(route.method, method_width),
                text::pad(&route.path, path_width),
                name
            );
        }
    }
    println!("\n{} 本", routes.len());
}

/// `about`：いまの設定と置き場所を1画面で出す。
///
/// 本番で「どこを見ているのか」を確かめるためのコマンドです。
/// **秘密は出しません**（`APP_KEY` は設定の有無だけ）。
pub(super) fn print_about(hooks: &Hooks) {
    use crate::support::text;

    let app = crate::config_registry::app_config();
    let storage = paths::storage_root();

    let rows: Vec<(&str, String)> = vec![
        ("bengara", env!("CARGO_PKG_VERSION").to_string()),
        ("アプリ名", app.name.clone()),
        ("環境", app.env.clone()),
        ("デバッグ表示", yes_no(app.debug)),
        ("URL", app.url.clone()),
        // 鍵そのものは出さない。署名に使えるか（あり／なし／短すぎる）だけを出す。
        ("APP_KEY", app_key_label(app)),
        (
            "基準ディレクトリ",
            format!(
                "{}（{}）",
                paths::base_path().display(),
                paths::base_source().label()
            ),
        ),
        (
            "storage",
            format!(
                "{}{}",
                storage.display(),
                if storage.is_dir() {
                    ""
                } else {
                    "  ← ありません（storage:init）"
                }
            ),
        ),
        ("public", paths::app_path("public").display().to_string()),
        (
            "言語",
            format!(
                "{} / {:?}",
                crate::Lang::current(),
                crate::Lang::available()
            ),
        ),
        // 書いた値ではなく、実際に使う方式を出す（知らない名前は file になる）。
        ("セッション", session_driver_label()),
        ("キャッシュ", cache_driver_label()),
        // いま実際に使う方式と強さ（`HASH_DRIVER`）。
        ("パスワード", crate::auth::hash_driver_label()),
        // `MAIL_MAILER` → `MAIL_DRIVER` の順に見る（`config/mail.rs` があればそちら）。
        ("メール", crate::mail::driver_label()),
        ("停止の上限", crate::server::shutdown_timeout_label()),
        // Laravel の `about` の「Logs」に当たる（`LOG_CHANNEL`・`LOG_DAILY_DAYS`）。
        ("ログ", crate::logging::about_label()),
        ("マイグレーション", (hooks.migrations)().len().to_string()),
        ("シーダー", (hooks.seeders)().len().to_string()),
        ("ジョブ", (hooks.jobs)().len().to_string()),
        ("自作コマンド", (hooks.commands)().len().to_string()),
    ];

    let width = rows
        .iter()
        .map(|(label, _)| text::width(label))
        .max()
        .unwrap_or(4);
    for (label, value) in &rows {
        println!("{}  {value}", text::pad(label, width));
    }
}

/// 真偽を日本語にする。
fn yes_no(value: bool) -> String {
    if value { "あり" } else { "なし" }.to_string()
}

/// `about` に出す `APP_KEY` の状態。署名に使うときと同じ判定（`signing_key`）を通す。
///
/// 以前は「空でなければあり」でした。短すぎる鍵も「あり」と出て、
/// 起動するとセッションが 500 になるのに気づけませんでした。
fn app_key_label(app: &crate::config_registry::AppConfig) -> String {
    match app.signing_key() {
        Ok(_) => "あり".to_string(),
        Err(_) if app.key.is_empty() => "なし".to_string(),
        Err(_) => format!("短すぎる（{} 文字）", app.key.chars().count()),
    }
}

/// 方式の名前を、実際に使うものに直す。
///
/// セッションもキャッシュも `memory`（別名 `array`）と `file` だけを知っていて、
/// 知らない名前は警告を出して `file` にします（`session/middleware.rs`・`cache/mod.rs`）。
/// 読む側（`StartSession`・`Cache`）と同じく、前後の空白を落とし、大文字小文字を区別しない。
fn driver_label(driver: &str) -> String {
    let driver = driver.trim();
    match driver.to_ascii_lowercase().as_str() {
        "memory" | "array" => "memory".to_string(),
        "file" => "file".to_string(),
        other => format!("file（`{other}` は知らないため）"),
    }
}

/// いま使うセッションの方式。`StartSession::from_env` と同じく `SESSION_DRIVER` を見る
/// （前後の空白を落とし、大文字小文字を区別しない）。
fn session_driver_label() -> String {
    driver_label(&env::<String>("SESSION_DRIVER", "file").to_ascii_lowercase())
}

/// いま使うキャッシュの方式。`config/cache.rs` があればその値、無ければ `CACHE_DRIVER`。
fn cache_driver_label() -> String {
    let driver = crate::try_config::<crate::cache::CacheConfig>()
        .map(|config| config.driver.clone())
        .unwrap_or_else(|| crate::cache::CacheConfig::default().driver);
    driver_label(&driver)
}

pub(super) fn print_help(hooks: Hooks) {
    let describe = |list: &[(&str, &str)]| {
        list.iter()
            .map(|(name, description)| format!("  {name:<20} {description}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    // `cargo artisan list` から呼ばれたら、artisan の一覧と重なるものと案内を出さない。
    let from_artisan = paths::set_var_os(crate::cli::ARTISAN_LIST_ENV).is_some();
    let kernel = kernel_help(from_artisan);
    let database = describe(crate::database::commands::COMMANDS);
    let extras = describe(crate::commands::COMMANDS);
    let footer = if from_artisan {
        ""
    } else {
        "\n開発用のコマンド（serve の自動再ビルドなど）は `cargo artisan list` に出ます。\n"
    };
    println!(
        "\
本体のコマンド

{kernel}
データベースのコマンド
{database}
  どれにも --database=<接続の名前> を付けられます。

周辺機能のコマンド
{extras}
{footer}"
    );
    // アプリが app/Console/Commands/ に置いたものも出す。
    crate::console::print_list((hooks.commands)());
}

/// このファイルが受け持つコマンドの説明（ヘルプの最初のかたまり）。
///
/// [`BUILTINS`] から作ります。名前を表とヘルプの 2 か所に書かないためです。
/// 桁は端末での幅でそろえます（名前の欄に日本語が入るため）。
///
/// `from_artisan` のときは、artisan が自分で受けるもの（[`overlaps_artisan`]）を出しません。
fn kernel_help(from_artisan: bool) -> String {
    use crate::support::text;

    let mut out = String::new();
    for builtin in BUILTINS {
        if from_artisan && overlaps_artisan(builtin) {
            continue;
        }
        out.push_str(&format!(
            "  {} {}\n",
            text::pad(builtin.label, 20),
            builtin.description
        ));
        for (flag, description) in builtin.flags {
            out.push_str(&format!("      {} {description}\n", text::pad(flag, 16)));
        }
    }
    out
}

/// artisan が自分で受けるもの（`serve`・`init`・`--version`・`--help`）。
///
/// `cargo artisan list` は artisan の一覧の後に本体の一覧を続けて出すので、
/// これらが 2 回並ばないようにします。
fn overlaps_artisan(builtin: &Builtin) -> bool {
    matches!(
        builtin.target,
        Target::Standalone(_) | Target::App(AppCommand::Serve)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artisanから呼ばれたら重なるものを出さない() {
        let help = kernel_help(true);
        // 名前の欄（行頭の 2 文字の空白の後ろ）で見る。`storage:init` は残る。
        for hidden in [
            "  (引数なし) / serve",
            "  --help ",
            "  init ",
            "  --version ",
        ] {
            assert!(
                !help.lines().any(|line| line.starts_with(hidden)),
                "{hidden}: {help}"
            );
        }
        assert!(help.contains("storage:init"), "{help}");
        assert!(help.contains("route:list"), "{help}");
    }

    #[test]
    fn ヘルプは表から作る() {
        let help = kernel_help(false);
        // 表にあるものは、全部ヘルプに出る。
        for builtin in BUILTINS {
            assert!(help.contains(builtin.label), "{}", builtin.label);
            assert!(help.contains(builtin.description), "{}", builtin.label);
        }
        // 桁は端末での幅でそろえる（名前の欄に日本語が入る）。
        assert!(
            help.contains("  (引数なし) / serve   HTTP サーバーを起動する（"),
            "{help}"
        );
        assert!(
            help.contains("      --host <ホスト>  待ち受けるホスト"),
            "{help}"
        );
        assert!(
            help.contains("  route:list           登録されているルートを一覧にする\n"),
            "{help}"
        );
        assert!(
            help.contains("  --help               このヘルプを出す\n"),
            "{help}"
        );
    }

    #[test]
    fn aboutの方式は実際に使うものを出す() {
        assert_eq!(driver_label("memory"), "memory");
        assert_eq!(driver_label("array"), "memory");
        assert_eq!(driver_label("file"), "file");
        // 読む側と同じく、空白と大文字小文字は気にしない。
        assert_eq!(driver_label(" Memory "), "memory");
        // 知らない名前は file で動くので、書いた名前のまま出さない。
        let label = driver_label("redis");
        assert!(
            label.starts_with("file") && label.contains("redis"),
            "{label}"
        );
    }

    #[test]
    fn aboutのapp_keyは署名に使えるかで出す() {
        let with_key = |key: &str| crate::config_registry::AppConfig {
            name: String::new(),
            env: String::new(),
            debug: false,
            url: String::new(),
            key: key.to_string(),
        };
        assert_eq!(app_key_label(&with_key("")), "なし");
        assert_eq!(app_key_label(&with_key("short")), "短すぎる（5 文字）");
        assert_eq!(
            app_key_label(&with_key(super::super::test_boot::TEST_APP_KEY)),
            "あり"
        );
    }
}
