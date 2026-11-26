//! `bengara::app!()` が定義する `main` の中身。
//!
//! 本体のバイナリは、サーバーの起動と、実行時のコマンド（`route:list` など）を担います。
//! `serve` の監視と再ビルドは `artisan` 側です（`crate::cli`）。
//!
//! | 置き場所 | 中身 |
//! |---|---|
//! | このファイル | コマンドの表（[`BUILTINS`]）・振り分け・`main` |
//! | `kernel_impl/key_generate.rs` | `key:generate` |
//! | `kernel_impl/about.rs` | `route:list`・`about`・`--help` の表示 |
//! | `kernel_impl/test_boot.rs` | `#[bengara::test]` の初期化 |
//!
//! コマンドの表はこのファイルに置いたままにします（`bengara-build` のテストがここを読んで、
//! 写しの名前とそろっているかを確かめるため）。

use crate::application::Application;
use crate::config_registry::{self, Registry};
use crate::console::Command;
use crate::env_vars::{self, env};
use crate::error::Result;
use crate::{paths, server, Hooks};

mod about;
mod key_generate;
mod test_boot;

use about::{print_about, print_help, print_routes};
pub(crate) use key_generate::replace_file;
use key_generate::{generate_key, REAL_ENV};
pub(crate) use test_boot::{boot_for_tests, test_hooks};

/// 置き場所が無くても使えるコマンド。
///
/// `init` はこれから置き場所を作るところなので、`--version` と `--help` は
/// どこで実行しても答えられるべきなので、基準ディレクトリの確認も、
/// アプリの組み立て（`bootstrap/app.rs`）も飛ばします。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standalone {
    Init,
    Version,
    Help,
}

/// アプリを組み立ててから動かすコマンド。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppCommand {
    Serve,
    RouteList,
    KeyGenerate,
    SessionGc,
    StorageInit,
    About,
    /// DB のコマンド（`database/commands.rs` の一覧）。
    Database,
    /// 周辺のコマンド（`commands.rs` の一覧）。
    Extra,
    /// アプリが `app/Console/Commands/` に置いた自作コマンド。
    Custom,
}

impl AppCommand {
    /// アプリ（`bootstrap/app.rs`）を組み立ててから動かすか。
    ///
    /// `key:generate` と `storage:init` だけは組み立てません。使うのは `.env` と
    /// 置き場所（`paths`）だけで、ルートもイベントも見ないためです。
    /// 組み立ての中のパニック（ルートの重複、知らないミドルウェアの別名など）で、
    /// 新しい環境の準備に要るこの 2 つまで止まらないようにします。
    fn builds_application(self) -> bool {
        !matches!(self, Self::KeyGenerate | Self::StorageInit)
    }
}

/// 打たれた名前が指すもの。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Standalone(Standalone),
    App(AppCommand),
}

/// このファイルが受け持つコマンド 1 つ。
struct Builtin {
    /// 受け付ける名前（別名を含む）。
    names: &'static [&'static str],
    target: Target,
    /// ヘルプの名前の欄に出す文字。
    label: &'static str,
    /// ヘルプに出す説明。
    description: &'static str,
    /// 説明の下に並べる旗（旗、説明）。
    flags: &'static [(&'static str, &'static str)],
}

/// このファイルが受け持つコマンドの表。**名前はここにだけ書きます。**
///
/// 置き場所が要るか（[`needs_base`]）・振り分け（[`resolve`]）・ヘルプ
/// （[`kernel_help`]）・同名の自作コマンドの検査（[`shadowed`]）は、全部
/// この表から決めます。以前は名前が別々の場所に散っていて、`list` `help`
/// `init` `version` だけが自作コマンドより**後ろ**で照合されていました。
/// `app/Console/Commands/List.rs` を置くと `cargo artisan list` が自作の
/// ほうを実行していました。
///
/// DB のコマンドと周辺のコマンドの名前は、それぞれのファイルの `COMMANDS` に
/// あります（[`builtin`] がまとめて見ます）。並びはヘルプに出す順です。
const BUILTINS: &[Builtin] = &[
    Builtin {
        names: &["serve"],
        target: Target::App(AppCommand::Serve),
        label: "(引数なし) / serve",
        // 名前を省いて旗だけでも起動する（`--port 9000`）。`split_command` を参照。
        description: "HTTP サーバーを起動する（名前は省ける。例: --port 9000）",
        flags: &[
            (
                "--host <ホスト>",
                "待ち受けるホスト（既定: APP_HOST か 127.0.0.1）",
            ),
            (
                "--port <ポート>",
                "待ち受けるポート（既定: APP_PORT か 8000）",
            ),
        ],
    },
    Builtin {
        names: &["route:list"],
        target: Target::App(AppCommand::RouteList),
        label: "route:list",
        description: "登録されているルートを一覧にする",
        flags: &[],
    },
    Builtin {
        names: &["key:generate"],
        target: Target::App(AppCommand::KeyGenerate),
        label: "key:generate",
        description: "APP_KEY を作って .env に書き込む（--force で上書き）",
        flags: &[],
    },
    Builtin {
        names: &["session:gc"],
        target: Target::App(AppCommand::SessionGc),
        label: "session:gc",
        description: "期限切れのセッションを消す",
        flags: &[],
    },
    Builtin {
        names: &["storage:init"],
        target: Target::App(AppCommand::StorageInit),
        label: "storage:init",
        description: "storage/ の下の書き込み先を作る（デプロイ時に1回）",
        flags: &[],
    },
    Builtin {
        names: &["about"],
        target: Target::App(AppCommand::About),
        label: "about",
        description: "いまの設定と置き場所を出す",
        flags: &[],
    },
    Builtin {
        names: &["init"],
        target: Target::Standalone(Standalone::Init),
        label: "init",
        description: "Laravel と同じ構成のファイルを作る（最初の1回だけ）",
        flags: &[],
    },
    Builtin {
        names: &["--version", "-V", "version"],
        target: Target::Standalone(Standalone::Version),
        label: "--version",
        description: "版番号を出す",
        flags: &[],
    },
    Builtin {
        names: &["--help", "-h", "help", "list"],
        target: Target::Standalone(Standalone::Help),
        label: "--help",
        description: "このヘルプを出す",
        flags: &[],
    },
];

/// フレームワークのコマンドなら、指すものを返す。
///
/// このファイルの表・DB のコマンド・周辺のコマンドの 3 つをまとめて見ます。
/// **フレームワークのコマンドかどうかは、ここだけで決めます。**
fn builtin(name: &str) -> Option<Target> {
    if let Some(found) = BUILTINS.iter().find(|b| b.names.contains(&name)) {
        return Some(found.target);
    }
    if crate::database::commands::is_command(name) {
        return Some(Target::App(AppCommand::Database));
    }
    if crate::commands::is_command(name) {
        return Some(Target::App(AppCommand::Extra));
    }
    None
}

/// 打たれた名前から、動かすものを決める。どれでもなければ `None`。
///
/// **フレームワークのコマンドが必ず先です**（`docs/console-commands.md`）。
/// 自作コマンドを見るのは、フレームワークのどのコマンドでもなかったときだけです。
fn resolve(name: &str, custom: &[Command]) -> Option<Target> {
    if let Some(target) = builtin(name) {
        return Some(target);
    }
    crate::console::find(custom, name).map(|_| Target::App(AppCommand::Custom))
}

/// フレームワークのコマンドと同じ名前の自作コマンド。**これらは呼ばれません。**
///
/// 黙って無視すると、`app/Console/Commands/List.rs` を書いた人が「動かない」
/// 理由を探すことになるので、起動のときに知らせます。
fn shadowed(custom: &[Command]) -> Vec<&'static str> {
    custom
        .iter()
        .map(|command| command.name)
        .filter(|name| builtin(name).is_some())
        .collect()
}

/// 基準ディレクトリが要るコマンドか。
///
/// 要らないのは [`Standalone`] だけです。知らない名前と自作コマンドは「要る」に
/// 倒します（置き場所が決まらないまま動き出さないため）。
fn needs_base(command: &str) -> bool {
    !matches!(builtin(command), Some(Target::Standalone(_)))
}

/// 本体の入口。`bengara::app!()` から呼ばれます。
pub fn main(hooks: Hooks, factory: fn() -> Application) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (command, _) = split_command(&args);

    // 知らない名前は、置き場所を確かめる**前**にヘルプを出して終える。
    // 先に確かめると、打ち間違いでも「基準ディレクトリが決まりません」と言われ、
    // 本当の原因（そんなコマンドは無い）が見えないため。
    //
    // 振り分けもここで 1 回だけ決め、[`dispatch`] へ渡す。
    let Some(target) = resolve(command, (hooks.commands)()) else {
        eprintln!("`{command}` というコマンドはありません。");
        print_help(hooks);
        std::process::exit(1);
    };

    // `init` / `--version` / `--help` は、`.env`・ログ・設定（`config/*.rs`）を使わない。
    // 読む前に答えて終える。設定の中のパニック（環境変数が無いなど）や
    // `.env` の誤りで、版番号やヘルプまで出せなくならないように。
    if let Target::Standalone(standalone) = target {
        if let Err(error) = run_standalone(standalone, hooks) {
            eprintln!("エラー: {error}");
            std::process::exit(1);
        }
        return;
    }

    // 置き場所が決まらないまま動き出さない（決定記録 #064）。
    // **`.env` を読む前**に確かめる。読む場所そのものが決まっていないため。
    // つまり `APP_BASE_PATH` は `.env` に書いても効きません。
    let needs_base = needs_base(command);
    if needs_base {
        if let Err(error) = paths::guard() {
            eprintln!("エラー: {error}");
            std::process::exit(1);
        }
    }

    let base = paths::base_path().to_path_buf();
    // `key:generate` は `.env` の `${VAR}` を読み込みと同じ規則で展開する。
    // 読み込むと `.env` の値が環境変数に混ざるので、その前に写しておく（`app_key_is_set`）。
    // UTF-8 でないものは `std::env::var` でも読めないので、写さない。
    if target == Target::App(AppCommand::KeyGenerate) {
        let real = std::env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
            .collect();
        let _ = REAL_ENV.set(real);
    }
    // `.env` の読み込みは、ログの購読より先に済ませる（`init_tracing` が
    // `APP_DEBUG` を見るため）。そのため警告はその場では出せないので、
    // 一覧で受け取って購読の**後**に流し直す。
    let (dotenv, warnings) = env_vars::load_dotenv(&base);
    // ログの設定（`LOG_CHANNEL` など）の警告も、購読を立てた**後**に流す。
    let log_warnings = crate::logging::init();
    if let Some(path) = &dotenv {
        tracing::debug!("{} を読み込みました", path.display());
    }
    for warning in warnings.into_iter().chain(log_warnings) {
        tracing::warn!("{warning}");
    }
    // フレームワークのコマンドと同じ名前の自作コマンドは呼ばれない。
    // 黙って無視せず、どのコマンドを動かしたときも知らせる。
    for name in shadowed((hooks.commands)()) {
        tracing::warn!("{}", shadowed_message(name));
    }

    // `APP_STORAGE_PATH` は `.env` に書いても効くので、読み込んだ**後**に確かめる。
    if needs_base {
        if let Err(error) = paths::guard_storage() {
            eprintln!("エラー: {error}");
            std::process::exit(1);
        }
        // 日ごとのテキストと SQLite は、`storage/` の場所が決まった後に始める。
        // それより前のログ（`.env` の警告など）は標準出力にだけ出ます。
        crate::logging::start_outputs();
    }

    let mut registry = Registry::new();
    (hooks.configs)(&mut registry);
    config_registry::install(registry);
    // 言語の表を固定する。以後は読むだけ。
    crate::lang::install((hooks.lang)());
    // 埋め込んだ public/ を固定する。以後は読むだけ。
    crate::http::statics::install_embedded((hooks.public)());
    // 埋め込んだ resources/views/ も固定する。
    crate::view::install_embedded((hooks.views)());

    if let Err(error) = dispatch(target, &args, hooks, factory) {
        // `exit` はキューを待たないので、先に書き切る（上限 5 秒）。
        crate::logging::shutdown();
        eprintln!("エラー: {error}");
        std::process::exit(1);
    }
    crate::logging::shutdown();
}

/// 引数を「コマンドの名前」と「残り」に分ける。
///
/// - 引数なしは `serve`。
/// - 最初が `serve` の旗（`--host` / `--port`）なら、名前を省いた `serve` と見る。
///   ヘルプの「(引数なし) / serve」のとおり、`cargo run -- --port 9000` でも起動します。
///   以前は `--port` をコマンドの名前と読み、「ありません」で止まっていました。
fn split_command(args: &[String]) -> (&str, &[String]) {
    match args.split_first() {
        Some((first, _)) if is_serve_flag(first) => ("serve", args),
        Some((first, rest)) => (first.as_str(), rest),
        None => ("serve", &[]),
    }
}

/// `serve` の旗か（`--host` `--port` と、`=` で値を続けた形）。
///
/// `artisan` も同じ読み方をします（`cli::split_artisan`）。
pub(crate) fn is_serve_flag(arg: &str) -> bool {
    ["--host", "--port"]
        .iter()
        .any(|flag| arg == *flag || arg.strip_prefix(flag).is_some_and(|r| r.starts_with('=')))
}

/// `target` は [`main`] が [`resolve`] で決めたもの（フレームワークのコマンドが必ず先）。
/// 知らない名前は `main` が先に弾いているので、ここには来ません。
fn dispatch(
    target: Target,
    args: &[String],
    hooks: Hooks,
    factory: fn() -> Application,
) -> Result<()> {
    // 引数なしと、旗から始まる引数はサーバーの起動。
    let (command, rest) = split_command(args);

    let kind = match target {
        Target::Standalone(standalone) => return run_standalone(standalone, hooks),
        Target::App(kind) => kind,
    };

    // 組み立てずに動かすもの（[`AppCommand::builds_application`]）。
    if !kind.builds_application() {
        return run_without_application(kind, command, rest);
    }

    // アプリの組み立て（`bootstrap/app.rs`）は、ここで**1回だけ**行う。
    //
    // 組み立てのときにイベントの聞く側（`Event::listen`）が固定されるので、
    // 飛ばすと `queue:work` や自作コマンドの中の `Event::dispatch` が
    // 何も呼ばずに終わります（決定記録）。`serve` と `route:list` のほかにも
    // 要るので、上の 2 つを除く [`AppCommand`] の全部で組み立てます。
    let app = build(factory);

    match kind {
        AppCommand::Serve => {
            let options = ServeOptions::parse(rest)?;
            // 使う絶対パスを出す。設定の間違いはここを見れば分かる。
            crate::ops::log_paths();
            crate::logging::note_sqlite_missing();
            server::serve(app, &options.host, options.port)
        }
        AppCommand::RouteList => {
            crate::commands::no_flags(command, rest)?;
            print_routes(&app);
            Ok(())
        }
        AppCommand::KeyGenerate | AppCommand::StorageInit => {
            run_without_application(kind, command, rest)
        }
        AppCommand::SessionGc => {
            crate::commands::no_flags(command, rest)?;
            sweep_sessions()
        }
        AppCommand::About => {
            crate::commands::no_flags(command, rest)?;
            print_about(&hooks);
            Ok(())
        }
        // DB のコマンド（migrate / db:seed など）。
        AppCommand::Database => {
            crate::database::commands::run(command, rest, (hooks.migrations)(), (hooks.seeders)())
        }
        // 周辺のコマンド（cache:clear / queue:work / schedule:run など）。
        AppCommand::Extra => crate::commands::run(command, rest, hooks),
        AppCommand::Custom => crate::commands::run_custom(command, rest, hooks),
    }
}

/// 置き場所は要るが、アプリの組み立ては要らないコマンドを動かす。
///
/// 使うのは `.env` と置き場所（`paths`）だけです（[`AppCommand::builds_application`]）。
fn run_without_application(kind: AppCommand, command: &str, rest: &[String]) -> Result<()> {
    match kind {
        AppCommand::KeyGenerate => generate_key(rest),
        AppCommand::StorageInit => {
            crate::commands::no_flags(command, rest)?;
            crate::ops::storage_init()
        }
        other => Err(crate::Error::msg(format!(
            "`{command}` はアプリを組み立ててから動かすコマンドです（{other:?}）"
        ))),
    }
}

/// 置き場所もアプリの組み立ても要らないコマンドを動かす。
fn run_standalone(command: Standalone, hooks: Hooks) -> Result<()> {
    match command {
        Standalone::Init => crate::cli::init_here(),
        Standalone::Version => {
            println!("bengara {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Standalone::Help => {
            print_help(hooks);
            // ログを始める前に終えるので、同名の自作コマンドの警告は標準エラーへ直に出す。
            for name in shadowed((hooks.commands)()) {
                eprintln!("警告: {}", shadowed_message(name));
            }
            Ok(())
        }
    }
}

/// 同じ名前の自作コマンドがあるときの警告。
fn shadowed_message(name: &str) -> String {
    format!(
        "自作コマンド `{name}` は呼ばれません。フレームワークに同じ名前のコマンドがあります\
         （app/Console/Commands/ のファイル名を変えてください）"
    )
}

/// そのコマンドでアプリを組み立てるか。
///
/// 組み立てると、ルート・ミドルウェア・**イベントの聞く側**が固定されます。
/// `Event::dispatch` はどのコマンドからでも呼べることになっているので、
/// 次のものを除いてすべて真にします。
///
/// - 置き場所が要らないコマンド（`init` / `--version` / `--help` など）
/// - `key:generate` と `storage:init`（[`AppCommand::builds_application`]）
/// - 知らない名前（ヘルプを出して終わる）
///
/// 振り分けそのものは型（[`Standalone`] と [`AppCommand`]）で分かれています。
/// この関数は、その対応をテストで名前から確かめるためのものです。
#[cfg(test)]
fn needs_application(command: &str, custom: &[Command]) -> bool {
    matches!(resolve(command, custom), Some(Target::App(kind)) if kind.builds_application())
}

/// `bootstrap/app.rs` を呼んでアプリを固定する。
fn build(factory: fn() -> Application) -> Application {
    let app = factory();
    app.install_names();
    app
}

/// `serve` の引数。
struct ServeOptions {
    host: String,
    port: u16,
}

impl ServeOptions {
    fn parse(args: &[String]) -> Result<Self> {
        let mut options = Self {
            host: env("APP_HOST", "127.0.0.1"),
            port: env("APP_PORT", 8000u16),
        };
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--host" => {
                    options.host = next_value(&mut iter, "--host")?;
                }
                "--port" => {
                    let raw = next_value(&mut iter, "--port")?;
                    options.port = raw.parse().map_err(|_| {
                        crate::Error::msg(format!("--port の値 `{raw}` が数値ではありません"))
                    })?;
                }
                other => {
                    if let Some(value) = other.strip_prefix("--host=") {
                        options.host = value.to_string();
                    } else if let Some(value) = other.strip_prefix("--port=") {
                        options.port = value.parse().map_err(|_| {
                            crate::Error::msg(format!("--port の値 `{value}` が数値ではありません"))
                        })?;
                    } else {
                        return Err(crate::Error::msg(format!(
                            "serve は `{other}` を受け付けません"
                        )));
                    }
                }
            }
        }
        Ok(options)
    }
}

fn next_value<'a>(iter: &mut impl Iterator<Item = &'a String>, name: &str) -> Result<String> {
    iter.next()
        .map(|v| v.to_string())
        .ok_or_else(|| crate::Error::msg(format!("{name} の値がありません")))
}

fn sweep_sessions() -> Result<()> {
    let dir = paths::storage_path("framework/sessions");
    let removed = crate::session::sweep_expired(&dir)?;
    println!(
        "期限切れのセッションを {removed} 件消しました（{}）",
        dir.display()
    );
    Ok(())
}

/// `RUST_LOG` の値と既定から、ログの細かさを決める（`log-filter` を外したとき用）。
///
/// 読めない値と空は既定に落とします。`off` は「読めない」ではなく、
/// **何も出さない**という指定として扱います。
///
/// テストでも使うので、`log-filter` が入っているときもテストにだけ残します
/// （既定のビルドでは `log-filter` が入るので、外した側の枝は普段コンパイルされません）。
#[cfg(any(test, not(feature = "log-filter")))]
pub(crate) fn max_level(raw: Option<&str>, default: &str) -> tracing::level_filters::LevelFilter {
    raw.and_then(parse_level)
        .or_else(|| parse_level(default))
        .unwrap_or(tracing::level_filters::LevelFilter::INFO)
}

/// `info` などの1語をログの細かさに変える。読めなければ `None`。
///
/// 返すのは `Level` ではなく `LevelFilter` です。`Level` には「何も出さない」が
/// 無いので、以前は `off` を `None`（読めない）にするしかなく、既定の細かさに
/// 戻っていました。`RUST_LOG=off` と書いたのに `info` で出続ける形です。
#[cfg(any(test, not(feature = "log-filter")))]
fn parse_level(raw: &str) -> Option<tracing::level_filters::LevelFilter> {
    use tracing::level_filters::LevelFilter;

    match raw.trim().to_ascii_lowercase().as_str() {
        "trace" => Some(LevelFilter::TRACE),
        "debug" => Some(LevelFilter::DEBUG),
        "info" => Some(LevelFilter::INFO),
        "warn" | "warning" => Some(LevelFilter::WARN),
        "error" => Some(LevelFilter::ERROR),
        "off" | "none" => Some(LevelFilter::OFF),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn イベントの聞く側が要るコマンドは組み立てる() {
        // `Event::dispatch` はどのコマンドからでも呼べる。組み立てを飛ばすと
        // 聞く側が登録されず、何も起きずに終わっていた。
        for command in [
            "serve",
            "route:list",
            "queue:work",
            "schedule:run",
            "migrate",
            "db:seed",
            "cache:clear",
            "session:gc",
            "about",
        ] {
            assert!(needs_application(command, &[]), "{command}");
        }
        // 置き場所が要らないコマンドは組み立てない。
        for command in [
            "init",
            "--version",
            "-V",
            "version",
            "--help",
            "-h",
            "help",
            "list",
        ] {
            assert!(!needs_application(command, &[]), "{command}");
        }
    }

    #[test]
    fn 自作コマンドは組み立ててから動かす() {
        // `bengara-build` が生成できる名前（`app/Console/Commands/MyCommand.rs` → `my-command`）。
        // 以前のテストは `app:my-command` を使っていたが、これは生成できない名前で、
        // 表に無いので「知らない名前」に落ち、自作コマンドの道を通っていなかった。
        let commands = [custom("my-command")];
        assert_eq!(
            resolve("my-command", &commands),
            Some(Target::App(AppCommand::Custom))
        );
        assert!(needs_application("my-command", &commands));
        assert!(needs_base("my-command"));
        // 登録していなければ知らない名前。組み立てずにヘルプを出して終わる。
        assert_eq!(resolve("my-command", &[]), None);
        assert!(!needs_application("my-command", &[]));
    }

    #[test]
    fn 準備のコマンドはアプリを組み立てない() {
        // `bootstrap/app.rs` の組み立てがパニックしても（ルートの重複など）、
        // 新しい環境の準備に要るこの 2 つは動く。置き場所は要る。
        for command in ["key:generate", "storage:init"] {
            assert!(
                matches!(resolve(command, &[]), Some(Target::App(_))),
                "{command}"
            );
            assert!(!needs_application(command, &[]), "{command}");
            assert!(needs_base(command), "{command}");
        }
    }

    #[test]
    fn 置き場所の要否と組み立ての要否はそろえる() {
        // 表にある名前の全部で、2 つの要否が一致する。
        // 例外は、置き場所だけが要る準備のコマンド（`key:generate` と `storage:init`）。
        for builtin in BUILTINS {
            for command in builtin.names {
                if matches!(builtin.target, Target::App(kind) if !kind.builds_application()) {
                    continue;
                }
                assert_eq!(
                    needs_base(command),
                    needs_application(command, &[]),
                    "{command}"
                );
            }
        }
        // 置き場所が要らないのは、この 8 つだけ。
        let without_base: Vec<&str> = BUILTINS
            .iter()
            .flat_map(|builtin| builtin.names.iter().copied())
            .filter(|command| !needs_base(command))
            .collect();
        assert_eq!(
            without_base,
            [
                "init",
                "--version",
                "-V",
                "version",
                "--help",
                "-h",
                "help",
                "list"
            ]
        );
        // 知らない名前と自作コマンドは「要る」に倒す。
        assert!(needs_base("greet"));
        assert!(needs_base("migrate"));
        assert!(needs_base("queue:work"));
    }

    /// テスト用の自作コマンド。
    fn custom(name: &'static str) -> Command {
        Command {
            name,
            description: "",
            handle: |_| Box::pin(async { Ok(()) }),
        }
    }

    #[test]
    fn 同じ名前の自作コマンドよりフレームワークが先() {
        // `app/Console/Commands/List.rs` などを置いた形。以前は `list` `help`
        // `init` `version` だけ自作コマンドの後ろで照合していて、自作のほうが動いた。
        let commands = [
            custom("list"),
            custom("help"),
            custom("init"),
            custom("version"),
            custom("serve"),
            custom("about"),
            custom("migrate"),
            custom("queue:work"),
            custom("greet"),
        ];
        assert_eq!(
            resolve("list", &commands),
            Some(Target::Standalone(Standalone::Help))
        );
        assert_eq!(
            resolve("help", &commands),
            Some(Target::Standalone(Standalone::Help))
        );
        assert_eq!(
            resolve("init", &commands),
            Some(Target::Standalone(Standalone::Init))
        );
        assert_eq!(
            resolve("version", &commands),
            Some(Target::Standalone(Standalone::Version))
        );
        assert_eq!(
            resolve("serve", &commands),
            Some(Target::App(AppCommand::Serve))
        );
        assert_eq!(
            resolve("about", &commands),
            Some(Target::App(AppCommand::About))
        );
        assert_eq!(
            resolve("migrate", &commands),
            Some(Target::App(AppCommand::Database))
        );
        assert_eq!(
            resolve("queue:work", &commands),
            Some(Target::App(AppCommand::Extra))
        );
        // ぶつからない名前は自作コマンドに届く。
        assert_eq!(
            resolve("greet", &commands),
            Some(Target::App(AppCommand::Custom))
        );
        assert_eq!(resolve("nope", &commands), None);
        assert_eq!(resolve("greet", &[]), None);

        // 同名の自作コマンドがあっても、置き場所の要否は変わらない
        // （以前は `list` が置き場所の確認を飛ばしたまま自作コマンドを動かした）。
        assert!(!needs_base("list"));
    }

    #[test]
    fn 呼ばれない自作コマンドを見つける() {
        let commands = [
            custom("greet"),
            custom("list"),
            custom("cache:clear"),
            custom("db:seed"),
            custom("send-report"),
        ];
        assert_eq!(shadowed(&commands), ["list", "cache:clear", "db:seed"]);
        assert!(shadowed(&[custom("greet")]).is_empty());
        assert!(shadowed(&[]).is_empty());

        let text = shadowed_message("list");
        assert!(text.contains("`list`"), "{text}");
        assert!(text.contains("呼ばれません"), "{text}");
        assert!(text.contains("app/Console/Commands/"), "{text}");
    }

    #[test]
    fn コマンドの名前は表の中で重ならない() {
        // 重なると、後ろのほうは決して選ばれない。
        let mut names: Vec<&str> = BUILTINS
            .iter()
            .flat_map(|builtin| builtin.names.iter().copied())
            .chain(crate::database::commands::COMMANDS.iter().map(|(n, _)| *n))
            .chain(crate::commands::COMMANDS.iter().map(|(n, _)| *n))
            .collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "同じ名前が 2 回出ている");
        // どの名前も、フレームワークのコマンドとして見つかる。
        for name in names {
            assert!(builtin(name).is_some(), "{name}");
        }
    }

    #[test]
    fn 旗から始まる引数はserveと見る() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // 引数なしは serve。
        assert_eq!(split_command(&[]), ("serve", &[] as &[String]));

        // 名前を省いて旗だけ。旗は残りとして serve に渡す。
        let given = args(&["--port", "9000"]);
        assert_eq!(split_command(&given), ("serve", given.as_slice()));
        let given = args(&["--host=0.0.0.0"]);
        assert_eq!(split_command(&given), ("serve", given.as_slice()));
        let given = args(&["--host", "0.0.0.0", "--port=9000"]);
        let (command, rest) = split_command(&given);
        assert_eq!(command, "serve");
        assert!(ServeOptions::parse(rest).is_ok_and(|o| o.port == 9000 && o.host == "0.0.0.0"));

        // 名前があればこれまでどおり。
        let given = args(&["serve", "--port", "9000"]);
        assert_eq!(split_command(&given), ("serve", &given[1..]));
        let given = args(&["migrate"]);
        assert_eq!(split_command(&given), ("migrate", &[] as &[String]));

        // 似ているだけの言葉は旗と見ない（知らないコマンドとして断る）。
        for word in ["--hostname", "--ports", "--help", "-p"] {
            assert!(!is_serve_flag(word), "{word}");
        }
        assert!(is_serve_flag("--port=8080"));
    }

    #[test]
    fn offはログを止める指定として読む() {
        use tracing::level_filters::LevelFilter;

        // 以前は `off` が「読めない値」と同じ扱いで、既定の細かさに戻っていた。
        assert_eq!(parse_level("off"), Some(LevelFilter::OFF));
        assert_eq!(parse_level(" OFF "), Some(LevelFilter::OFF));
        assert_eq!(parse_level("none"), Some(LevelFilter::OFF));
        assert_eq!(max_level(Some("off"), "info"), LevelFilter::OFF);
        assert_eq!(max_level(Some("off"), "debug"), LevelFilter::OFF);

        // ほかの語はこれまでどおり。
        assert_eq!(max_level(Some("warn"), "info"), LevelFilter::WARN);
        assert_eq!(max_level(Some("TRACE"), "info"), LevelFilter::TRACE);
        // 読めない値・空・指定なしは既定に落ちる。
        assert_eq!(max_level(Some("myapp=debug"), "debug"), LevelFilter::DEBUG);
        assert_eq!(max_level(Some(""), "info"), LevelFilter::INFO);
        assert_eq!(max_level(None, "debug"), LevelFilter::DEBUG);
    }
}
