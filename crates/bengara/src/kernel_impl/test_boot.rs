//! `#[bengara::test]` が呼ぶ初期化と、テスト用の既定。

use crate::config_registry::{self, Registry};
use crate::env_vars;
use crate::{paths, Hooks};

/// テストから使う初期化。**何度呼んでも、中身が走るのは 1 回だけ**です。
///
/// `#[bengara::test]` が付いたテストは**1 件ごとに**ここを呼びます。`Once` で囲うのは
/// `env_vars::load_dotenv` が `std::env::set_var` を呼ぶからです。テストは複数の
/// スレッドで同時に走るので、別のテストが `env()` で `std::env::var` を読んでいる間に
/// 書き込むと未定義動作になります（`setenv`/`getenv` はスレッド安全ではありません。
/// Rust 2024 で `set_var` が unsafe になったのはこのためです）。
/// 中で呼ぶ他の処理（`install_test_mailer` など）はそれぞれ二重呼び出しに備えていましたが、
/// `load_dotenv` だけが無防備でした。
///
/// **この関数は `bengara` の単体テストから呼んではいけません。** 言語の表・埋め込んだ
/// `public/`・設定の一覧を `OnceLock` でプロセス全体に固定するので、1 回呼ぶだけで
/// 同じテストバイナリの他のテストを壊します（以前、ここを呼ぶテストを足したら
/// `lang::tests` が 6 件落ちました。空の言語表が先に固定され、`lang` 側の
/// `TABLE.set` が無視されたためです）。動きの確認はアプリ側の `#[bengara::test]`
/// が全件この道を通ることで足ります。
pub(crate) fn boot_for_tests(hooks: Hooks) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // メールはテストでは送らず、溜めるだけにする（`.env` を読む前に決める）。
        crate::mail::install_test_mailer();
        // パスワードのハッシュの回数を下げるのは **`.env` を読む前**に決める。
        // `.env` の値まで尊重すると、開発用に下げた値（や上げた値）でテストが走り、
        // 1 件ごとに数秒かかってしまう。環境変数で明示したときだけ従う。
        crate::auth::install_test_iterations();
        // `APP_ENV`・セッション・キャッシュのテスト用の既定も **`.env` を読む前**に入れる。
        // `.env` は設定済みの変数を上書きしないので、`.env` の `APP_ENV=local` に負けない。
        install_test_env();

        let base = paths::base_path().to_path_buf();
        let (_, warnings) = env_vars::load_dotenv(&base);
        // テストでは標準出力にだけ書く（`LOG_CHANNEL` を見ない）。
        crate::logging::init_for_tests();
        for warning in warnings {
            tracing::warn!("{warning}");
        }
        // 設定を登録する**前**に入れる。`AppConfig` は登録のときに `APP_KEY` を読むため。
        install_test_app_key();
        if !config_registry::installed() {
            let mut registry = Registry::new();
            (hooks.configs)(&mut registry);
            config_registry::install(registry);
        }
        // 言語の表を固定する。
        crate::lang::install((hooks.lang)());
        // 埋め込んだ public/ を固定する。以後は読むだけ。
        crate::http::statics::install_embedded((hooks.public)());
        // 埋め込んだ resources/views/ も固定する。
        crate::view::install_embedded((hooks.views)());
        // マイグレーションの一覧を、あとで `refresh_database()` から使えるようにしておく。
        let _ = TEST_HOOKS.set(hooks);
    });
}

/// テストで `APP_KEY` が無いときに使う鍵。64 文字（`MIN_KEY_CHARS`）です。
///
/// 秘密ではありません。テストはソケットを開かず、Cookie も外に出ないので、
/// 固定の値で足ります。本物の環境変数や `.env` に値があればそちらを使います。
pub(super) const TEST_APP_KEY: &str =
    "bengara-test-key-0123456789abcdef0123456789abcdef0123456789abcdef";

/// `.env` を読んだあとも `APP_KEY` が空なら、テスト用の鍵を入れる。
///
/// 取得したばかりのリポジトリには `.env` が無い。そのままだとセッションと署名付き URL を
/// 使う全テストが「`APP_KEY` が設定されていません」の 500 で落ちる。
/// メールの溜め込みやハッシュの回数と同じく、テストが自分で動ける状態にしておく。
///
/// `boot_for_tests` の `Once` の中（ほかのスレッドが環境変数を読む前）からだけ呼ぶ。
fn install_test_app_key() {
    let present = std::env::var("APP_KEY")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    if present {
        return;
    }
    // edition 2021 では set_var は安全な関数（`load_dotenv` と同じ理由）。
    std::env::set_var("APP_KEY", TEST_APP_KEY);
    tracing::debug!("APP_KEY が無いので、テスト用の固定の鍵を使います");
}

/// テストで使う環境変数の既定。Laravel の `phpunit.xml` の `<env>` と同じ役目です。
///
/// | 変数 | 値 | 理由 |
/// |---|---|---|
/// | `APP_ENV` | `testing` | 本体の既定は `production`。本番だけの振る舞いで結果が変わらないように |
/// | `SESSION_DRIVER` | `memory` | 開発用の `storage/framework/sessions/` にファイルを溜めない |
/// | `CACHE_DRIVER` | `memory` | 開発用の `storage/framework/cache/` を汚さない |
const TEST_ENV: &[(&str, &str)] = &[
    ("APP_ENV", "testing"),
    ("SESSION_DRIVER", "memory"),
    ("CACHE_DRIVER", "memory"),
];

/// 本物の環境変数に無いものだけ、[`TEST_ENV`] の値を入れる。
///
/// **`.env` を読む前**に呼びます。`.env` は設定済みの変数を上書きしないので、
/// 優先順位は「本物の環境変数 > テスト用の既定 > `.env`」になります
/// （Laravel の `phpunit.xml` と同じ）。以前は `.env` の後に入れていたので、
/// README どおり `APP_ENV=local` の `.env` を置くとテストが `local` で走りました。
///
/// 空の環境変数は「無い」と見なします（`.env` の読み込みと同じ規則）。
///
/// `boot_for_tests` の `Once` の中からだけ呼ぶ。
fn install_test_env() {
    for (key, value) in TEST_ENV {
        if env_vars::is_unset(std::env::var_os(key).as_deref()) {
            // edition 2021 では set_var は安全な関数（`load_dotenv` と同じ理由）。
            std::env::set_var(key, value);
        }
    }
}

static TEST_HOOKS: std::sync::OnceLock<Hooks> = std::sync::OnceLock::new();

/// テストのときに覚えておいた `Hooks`。
pub(crate) fn test_hooks() -> Hooks {
    TEST_HOOKS.get().copied().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn テスト用の鍵は必要な長さを満たす() {
        // 1 文字でも短いと、セッションを使う全テストが 500 で落ちる。
        assert!(TEST_APP_KEY.chars().count() >= config_registry::MIN_KEY_CHARS);
    }

    #[test]
    fn テスト用の既定はenvより先に入れる値() {
        // Laravel の phpunit.xml と同じ 3 つ。セッションとキャッシュは開発用の
        // storage/ に書かない方式にする。
        let get = |key: &str| TEST_ENV.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        assert_eq!(get("APP_ENV"), Some("testing"));
        assert_eq!(get("SESSION_DRIVER"), Some("memory"));
        assert_eq!(get("CACHE_DRIVER"), Some("memory"));
    }
}
