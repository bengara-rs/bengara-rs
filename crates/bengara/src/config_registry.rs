//! 設定の保管場所。
//!
//! `config/` の各ファイルの `config()` を起動時に1回ずつ呼び、戻り値を型ごとに保管します。
//! 以後は読むだけなので、ロックは要りません。

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::OnceLock;

use crate::env_vars::env;

/// 型ごとに設定を1つ保管する入れ物。
#[derive(Default)]
pub struct Registry {
    values: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl Registry {
    /// 空の入れ物を作る。
    pub fn new() -> Self {
        Self::default()
    }

    /// 設定を登録する。同じ型を2回登録すると、後のものが残る。
    ///
    /// 2 回目のときは警告のログを出します。`config/` の 2 つのファイルが同じ型を
    /// 返すと、片方の中身が黙って捨てられ、どちらが効いているか分からないためです。
    /// 呼ぶのは起動時の登録だけなので、リクエストの処理には響きません。
    pub fn set<T: Any + Send + Sync>(&mut self, value: T) {
        if self
            .values
            .insert(TypeId::of::<T>(), Box::new(value))
            .is_some()
        {
            tracing::warn!("{}", duplicate_message(std::any::type_name::<T>()));
        }
    }

    /// 登録済みの設定を借りる。
    pub fn get<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.values.get(&TypeId::of::<T>())?.downcast_ref::<T>()
    }

    /// 登録されている設定の数。
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// 何も登録されていないか。
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("len", &self.values.len())
            .finish()
    }
}

/// 同じ型の設定が 2 回登録されたときの警告の文。
fn duplicate_message(type_name: &str) -> String {
    format!(
        "設定 {type_name} が 2 回登録されました。後に登録したものを使います。\
         config/ の 2 つのファイルが同じ型を返していないか確かめてください"
    )
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();

/// 起動時に1回だけ呼ぶ。2回目以降は何もしない。
pub(crate) fn install(registry: Registry) {
    let _ = REGISTRY.set(registry);
}

/// すでに設定が入っているか。
pub(crate) fn installed() -> bool {
    REGISTRY.get().is_some()
}

/// 設定を型で読み出す。
///
/// ```ignore
/// let name = &config::<AppConfig>().name;
/// ```
///
/// # パニック
///
/// その型の設定が登録されていないときはパニックします。`config/` に対応する
/// ファイルを置いて、`pub fn config() -> その型` を定義してください。
pub fn config<T: Any + Send + Sync>() -> &'static T {
    try_config::<T>().unwrap_or_else(|| {
        panic!(
            "設定 {} が登録されていません。config/ の中に `pub fn config() -> {}` を返すファイルを置いてください",
            std::any::type_name::<T>(),
            std::any::type_name::<T>()
        )
    })
}

/// 設定を型で読み出す。登録されていなければ `None`。
pub fn try_config<T: Any + Send + Sync>() -> Option<&'static T> {
    REGISTRY.get()?.get::<T>()
}

/// `config/app.rs` が返す基本設定。Laravel の `config/app.php` に当たります。
///
/// `{:?}` で出しても `key` の中身は出ません（[`Debug`] の実装を参照）。
#[derive(Clone)]
pub struct AppConfig {
    /// アプリ名。
    pub name: String,
    /// 実行環境（`local`、`production` など）。
    pub env: String,
    /// 詳しいエラーページを出すか。
    pub debug: bool,
    /// アプリの URL。
    pub url: String,
    /// 署名に使う鍵（`APP_KEY`）。
    ///
    /// セッションと署名付き URL で使います。**空のまま、または短すぎると、
    /// それらを使うときにエラーになります。**
    /// `cargo artisan key:generate` で作ってください。
    pub key: String,
}

/// `config/app.rs` が無いときの `APP_URL` の既定。
///
/// `init` の雛形（`config/app.rs` と `.env`）と同じ値にします。`serve` の既定の
/// ポートも 8000 です。以前は `http://localhost` で、`config/app.rs` を消すと
/// `url()` が作る URL からポートが落ちていました。
const DEFAULT_URL: &str = "http://localhost:8000";

/// `APP_KEY` に求める最低の長さ（文字数）。
///
/// `cargo artisan key:generate` は 32 バイトを16進にした 64 文字を作ります。
/// それより短い値は、手で書いたものと考えて断ります。
/// `APP_KEY=x` のような 1 文字でも HMAC は動いてしまうので、入口で止めます。
pub(crate) const MIN_KEY_CHARS: usize = 64;

/// `key`（`APP_KEY`）は伏せて、有無と長さだけを出す。
///
/// 自動の `Debug` だと、`tracing::debug!("{config:?}")` やパニックの文で鍵がそのまま
/// ログに残ります。鍵が漏れると、セッションも署名付き URL も偽造できます。
impl std::fmt::Debug for AppConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let key = if self.key.is_empty() {
            "(空)".to_string()
        } else {
            format!("(伏せ字、{} 文字)", self.key.chars().count())
        };
        f.debug_struct("AppConfig")
            .field("name", &self.name)
            .field("env", &self.env)
            .field("debug", &self.debug)
            .field("url", &self.url)
            .field("key", &format_args!("{key}"))
            .finish()
    }
}

impl Default for AppConfig {
    /// `config/app.rs` が無いときに使われる値。環境変数から組み立てます。
    fn default() -> Self {
        Self {
            name: env("APP_NAME", "bengara"),
            env: env("APP_ENV", "production"),
            debug: env("APP_DEBUG", false),
            url: env("APP_URL", DEFAULT_URL),
            key: env("APP_KEY", ""),
        }
    }
}

impl AppConfig {
    /// 本番環境か。
    pub fn is_production(&self) -> bool {
        self.env == "production"
    }

    /// 手元の開発環境か。
    pub fn is_local(&self) -> bool {
        self.env == "local"
    }

    /// テストの最中か。
    ///
    /// `#[bengara::test]` は、`APP_ENV` が本物の環境変数に無いとき `testing` を入れます。
    /// `.env` を読む前に入れるので、`.env` の `APP_ENV=local` より強いです
    /// （Laravel の `phpunit.xml` と同じ優先順位）。
    pub fn is_testing(&self) -> bool {
        self.env == "testing"
    }

    /// 署名に使う鍵を取り出す。設定されていなければ、直し方を書いたエラーを返す。
    ///
    /// 長さも確かめます。短い鍵でも HMAC は計算できてしまうので、
    /// 気づけるのはここだけです。
    pub(crate) fn signing_key(&self) -> crate::error::Result<&[u8]> {
        if self.key.is_empty() {
            return Err(crate::Error::msg(
                "APP_KEY が設定されていません。`cargo artisan key:generate` で作って .env に入れてください",
            ));
        }
        let length = self.key.chars().count();
        if length < MIN_KEY_CHARS {
            return Err(crate::Error::msg(format!(
                "APP_KEY が短すぎます（いま {length} 文字、{MIN_KEY_CHARS} 文字以上が必要です）。\
                 Laravel の `base64:...` の鍵はそのまま使えません。\
                 `cargo artisan key:generate --force` で作り直して .env に入れてください"
            )));
        }
        Ok(self.key.as_bytes())
    }

    /// 用途ごとに別の鍵を作る。
    ///
    /// `APP_KEY` の生バイトを、セッション Cookie の署名・署名付き URL の署名・暗号化で
    /// そのまま使い回すと、どれか1つに署名を作らせる隙ができたときに他へ波及します。
    /// 用途を表す `label` を混ぜて、別々の鍵にしておきます。
    ///
    /// 中身は `hmac_sha256(APP_KEY, label)` です。`label` は用途ごとに決め打ちで、
    /// 外から来た値は渡しません。
    ///
    /// | 用途 | `label` |
    /// |---|---|
    /// | セッション Cookie の署名 | `bengara:session` |
    /// | 署名付き URL の署名 | `bengara:signed-url` |
    /// | 暗号化（`encrypt` / `decrypt`） | `bengara:encryption` |
    pub(crate) fn derived_key(&self, label: &str) -> crate::error::Result<Vec<u8>> {
        let app_key = self.signing_key()?;
        Ok(crate::support::crypto::hmac_sha256(app_key, label.as_bytes()).to_vec())
    }
}

/// 基本設定を取り出す。`config/app.rs` が無ければ既定値を使います。
pub(crate) fn app_config() -> &'static AppConfig {
    static FALLBACK: OnceLock<AppConfig> = OnceLock::new();
    try_config::<AppConfig>().unwrap_or_else(|| FALLBACK.get_or_init(AppConfig::default))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Sample(u32);

    #[test]
    fn 型ごとに出し入れできる() {
        let mut r = Registry::new();
        assert!(r.is_empty());
        r.set(Sample(1));
        assert_eq!(r.get::<Sample>(), Some(&Sample(1)));
        r.set(Sample(2));
        assert_eq!(r.get::<Sample>(), Some(&Sample(2)));
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn 同じ型の2回目は警告の文で知らせる() {
        // 以前は黙って後のものが残り、どちらのファイルが効いているか分からなかった。
        let text = duplicate_message(std::any::type_name::<Sample>());
        assert!(text.contains("Sample"), "{text}");
        assert!(text.contains("2 回登録"), "{text}");
        assert!(text.contains("後に登録したもの"), "{text}");
    }

    #[test]
    fn app_urlの既定はinitの雛形とそろえる() {
        // 以前は `http://localhost` で、雛形の `http://localhost:8000` と違っていた。
        assert!(
            include_str!("cli/stubs/config_app.stub")
                .contains(&format!("env(\"APP_URL\", \"{DEFAULT_URL}\")")),
            "config/app.rs の雛形"
        );
        assert!(
            include_str!("cli/stubs/env.stub").contains(&format!("\nAPP_URL={DEFAULT_URL}\n")),
            ".env の雛形"
        );
    }

    #[test]
    fn 未登録の型は見つからない() {
        let r = Registry::new();
        assert_eq!(r.get::<Sample>(), None);
    }

    /// 鍵だけを差し替えた設定を作る。
    fn with_key(key: &str) -> AppConfig {
        AppConfig {
            name: "test".into(),
            env: "testing".into(),
            debug: false,
            url: "http://localhost".into(),
            key: key.to_string(),
        }
    }

    #[test]
    fn debugで出しても鍵は出ない() {
        let key = "s3cr3t".repeat(20);
        let text = format!("{:?}", with_key(&key));
        assert!(!text.contains("s3cr3t"), "{text}");
        assert!(text.contains("120 文字"), "{text}");
        assert!(text.contains("name: \"test\""), "{text}");
        assert!(format!("{:?}", with_key("")).contains("(空)"));
    }

    #[test]
    fn app_keyが空なら作り方を知らせる() {
        let error = with_key("").signing_key().unwrap_err();
        assert!(error.to_string().contains("設定されていません"), "{error}");
        assert!(error.to_string().contains("key:generate"), "{error}");
    }

    #[test]
    fn app_keyが短ければ長さを添えて断る() {
        let error = with_key("x").signing_key().unwrap_err();
        let text = error.to_string();
        assert!(text.contains("短すぎます"), "{text}");
        assert!(text.contains("いま 1 文字"), "{text}");
        assert!(text.contains("64 文字以上"), "{text}");
        assert!(text.contains("key:generate"), "{text}");

        // 1 文字足りないだけでも断る。
        let error = with_key(&"a".repeat(63)).signing_key().unwrap_err();
        assert!(error.to_string().contains("いま 63 文字"), "{error}");
    }

    #[test]
    fn app_keyがちょうどの長さなら通る() {
        let key = "a".repeat(MIN_KEY_CHARS);
        assert_eq!(with_key(&key).signing_key().unwrap(), key.as_bytes());
    }

    #[test]
    fn app_keyが長くても通る() {
        let key = "a".repeat(200);
        assert_eq!(with_key(&key).signing_key().unwrap(), key.as_bytes());
    }

    #[test]
    fn 用途ごとに違う鍵になる() {
        let config = with_key(&"a".repeat(MIN_KEY_CHARS));
        let session = config.derived_key("bengara:session").unwrap();
        let encryption = config.derived_key("bengara:encryption").unwrap();

        assert_eq!(session.len(), 32, "SHA-256 の長さ");
        assert_ne!(session, encryption, "ラベルが違えば鍵も違う");
        assert_ne!(
            session,
            config.key.as_bytes(),
            "APP_KEY の生バイトはそのまま使わない"
        );
        // 同じラベルなら毎回同じ（決め打ちの導出なので、保存した値が読めなくならない）。
        assert_eq!(session, config.derived_key("bengara:session").unwrap());
    }

    #[test]
    fn 鍵が短ければ導出もできない() {
        let error = with_key("x").derived_key("bengara:session").unwrap_err();
        assert!(error.to_string().contains("短すぎます"), "{error}");
    }
}
