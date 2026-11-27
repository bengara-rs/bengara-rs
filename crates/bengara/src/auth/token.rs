//! API 用のトークン（Laravel Sanctum の個人用アクセストークン）。
//!
//! 表は Sanctum と同じ `personal_access_tokens` です。列も値の形も同じなので、
//! Laravel で発行したトークンをそのまま照合できます。
//!
//! | Sanctum | 紅殻 |
//! |---|---|
//! | `$user->createToken('名前', ['posts:write'])` | `user.create_token("名前", &["posts:write"], None)` |
//! | `->plainTextToken` | `.plain_text_token` |
//! | `$user->tokens()->delete()` | `user.tokens().delete()` |
//! | `PersonalAccessToken::findToken($token)` | `PersonalAccessToken::find_token(token)` |
//! | `$user->tokenCan('posts:write')` | `req.auth().token_can("posts:write")` |
//! | `$user->currentAccessToken()` | `req.auth().current_access_token()` |
//!
//! # トークンの形
//!
//! - 渡す値は `{id}|{平文}` です。
//! - 平文は、英数字 40 文字の乱数に、その crc32b（16 進 8 文字）をつないだ 48 文字です
//!   （Sanctum 4 の `generateTokenString` と同じ形）。
//! - 表の `token` 列には、平文の SHA-256（16 進 64 文字）だけを入れます。平文は残しません。

use std::future::Future;

use super::Authenticatable;
use crate::database::{FromValue, Model, ModelQuery, Row, Value};
use crate::error::{Error, Result};
use crate::support::{crypto, Carbon};

/// 表の名前。Sanctum と同じ。
const TABLE: &str = "personal_access_tokens";

/// 乱数の部分の長さ。Sanctum 4 と同じ 40 文字。
const ENTROPY_LENGTH: usize = 40;

/// `last_used_at` を書き直すまでの間隔（秒）。
///
/// Sanctum は要求のたびに書きます。ここでは、前に書いてから 60 秒たつまで書きません。
/// API は短い間に何本も呼ばれるので、そのたびに 1 行を書き換えると DB の書き込みが
/// 要求の数だけ増えるためです。
const LAST_USED_INTERVAL_SECONDS: i64 = 60;

/// 発行したトークン。表の 1 行です。
///
/// ```ignore
/// let token = req.auth().current_access_token();
/// ```
///
/// - `token` 列（平文の SHA-256）は持っていますが、外には出しません。
/// - `Debug` にも出しません。
#[derive(Clone)]
pub struct PersonalAccessToken {
    /// 主キー。渡す値の `|` の前の部分です。
    pub id: i64,
    /// 持ち主の種類（[`Authenticatable::morph_class`]）。Laravel では `App\Models\User`。
    pub tokenable_type: String,
    /// 持ち主の主キーの値。
    pub tokenable_id: String,
    /// トークンの名前。
    pub name: String,
    /// 能力の一覧。`"*"` はすべて。
    pub abilities: Vec<String>,
    /// 最後に使った日時。
    pub last_used_at: Option<Carbon>,
    /// 期限。`None` なら無期限。
    pub expires_at: Option<Carbon>,
    /// 作った日時。
    pub created_at: Option<Carbon>,
    /// 直した日時。
    pub updated_at: Option<Carbon>,
    /// 平文の SHA-256（16 進 64 文字）。
    token: String,
}

impl std::fmt::Debug for PersonalAccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersonalAccessToken")
            .field("id", &self.id)
            .field("tokenable_type", &self.tokenable_type)
            .field("tokenable_id", &self.tokenable_id)
            .field("name", &self.name)
            .field("abilities", &self.abilities)
            .field("last_used_at", &self.last_used_at)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl PersonalAccessToken {
    /// 表の名前。マイグレーションを書くときに使います。
    pub const TABLE: &'static str = TABLE;

    /// この表を作るマイグレーションの中身。列は Sanctum 4 と同じです。
    ///
    /// ```ignore
    /// pub fn up(schema: &mut Schema) {
    ///     PersonalAccessToken::define(schema);
    /// }
    /// ```
    ///
    /// `tokenable_id` は整数の列です（Laravel の `morphs`）。
    /// 文字列の主キーの持ち主には使えません。
    pub fn define(schema: &mut crate::database::Schema) {
        schema.create_if_not_exists(TABLE, |t| {
            t.id();
            t.string("tokenable_type");
            t.foreign_id("tokenable_id");
            t.index(&["tokenable_type", "tokenable_id"]);
            t.text("name");
            t.string_with("token", 64).unique();
            t.text("abilities").nullable();
            t.timestamp("last_used_at").nullable();
            t.timestamp("expires_at").nullable().index();
            t.timestamps();
        });
    }

    /// 渡されたトークンで行を探す。Sanctum の `findToken` と同じ手順です。
    ///
    /// | 渡す値 | 探し方 |
    /// |---|---|
    /// | `{id}|{平文}` | `id` で引き、`token` 列と平文の SHA-256 を定数時間で比べる |
    /// | `|` が無い | 値全体の SHA-256 で `token` 列を引く |
    ///
    /// **期限は見ません。** 期限切れかどうかは [`is_expired`](Self::is_expired) で
    /// 確かめてください（`Authenticate::token()` は確かめます）。
    pub async fn find_token(token: &str) -> Result<Option<Self>> {
        let Some((id, plain)) = token.split_once('|') else {
            return Self::query()
                .where_("token", hash_token(token))
                .first()
                .await;
        };
        let hashed = hash_token(plain);
        // 整数に読めない `id` は、問い合わせずに `None` です（`Model::find`）。
        let Some(found) = Self::find(id).await? else {
            return Ok(None);
        };
        Ok(crypto::constant_time_eq(found.token.as_bytes(), hashed.as_bytes()).then_some(found))
    }

    /// その能力を持っているか。`"*"` を持っていれば、どの能力も真です。
    ///
    /// Sanctum の `can` と同じく、名前は完全に一致したときだけ当たります
    /// （`posts:*` のような書き方は読みません）。
    pub fn can(&self, ability: &str) -> bool {
        self.abilities.iter().any(|a| a == "*" || a == ability)
    }

    /// その能力を持っていないか。
    pub fn cant(&self, ability: &str) -> bool {
        !self.can(ability)
    }

    /// 期限が過ぎているか。期限が無ければ偽。
    pub fn is_expired(&self) -> bool {
        self.expires_at.as_ref().is_some_and(Carbon::is_past)
    }

    /// `last_used_at` をいまの時刻にする。前に書いてから 60 秒たっていなければ書きません。
    ///
    /// 書くのはこの 2 列だけです（`last_used_at` と `updated_at`）。
    pub(crate) async fn touch_last_used(&self) -> Result<()> {
        let now = Carbon::now();
        if self
            .last_used_at
            .as_ref()
            .is_some_and(|at| (now.timestamp() - at.timestamp()).abs() < LAST_USED_INTERVAL_SECONDS)
        {
            return Ok(());
        }
        Self::query()
            .where_("id", self.id)
            .update(&[("last_used_at", now.into()), ("updated_at", now.into())])
            .await?;
        Ok(())
    }
}

#[cfg(test)]
impl PersonalAccessToken {
    /// 表を通さずに作る（テスト用）。
    pub(crate) fn fake(tokenable_type: &str, tokenable_id: &str, abilities: &[&str]) -> Self {
        Self {
            id: 1,
            tokenable_type: tokenable_type.to_string(),
            tokenable_id: tokenable_id.to_string(),
            name: "test".to_string(),
            abilities: abilities.iter().map(|a| (*a).to_string()).collect(),
            last_used_at: None,
            expires_at: None,
            created_at: None,
            updated_at: None,
            token: String::new(),
        }
    }
}

impl Model for PersonalAccessToken {
    const TABLE: &'static str = TABLE;
    const PRIMARY_KEY: &'static str = "id";
    const COLUMNS: &'static [&'static str] = &[
        "id",
        "tokenable_type",
        "tokenable_id",
        "name",
        "token",
        "abilities",
        "last_used_at",
        "expires_at",
        "created_at",
        "updated_at",
    ];

    fn from_row(row: &Row) -> Result<Self> {
        let abilities: Option<String> = row.get("abilities")?;
        Ok(Self {
            id: row.get("id")?,
            tokenable_type: row.get("tokenable_type")?,
            tokenable_id: row.get("tokenable_id")?,
            name: row.get("name")?,
            abilities: abilities
                .as_deref()
                .map(parse_abilities)
                .unwrap_or_default(),
            last_used_at: row.get("last_used_at")?,
            expires_at: row.get("expires_at")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
            token: row.get("token")?,
        })
    }

    fn attributes(&self) -> Vec<(&'static str, Value)> {
        let date = |at: &Option<Carbon>| at.map(Value::from).unwrap_or(Value::Null);
        vec![
            ("tokenable_type", Value::Text(self.tokenable_type.clone())),
            ("tokenable_id", key_to_value(&self.tokenable_id)),
            ("name", Value::Text(self.name.clone())),
            ("token", Value::Text(self.token.clone())),
            (
                "abilities",
                Value::Text(serde_json::to_string(&self.abilities).unwrap_or_default()),
            ),
            ("last_used_at", date(&self.last_used_at)),
            ("expires_at", date(&self.expires_at)),
            ("created_at", date(&self.created_at)),
            ("updated_at", date(&self.updated_at)),
        ]
    }

    fn key(&self) -> Value {
        Value::Int(self.id)
    }

    fn set_key(&mut self, value: Value) {
        if let Value::Int(id) = value {
            self.id = id;
        }
    }

    fn touch_timestamps(&mut self, creating: bool) {
        let now = Carbon::now();
        if creating {
            self.created_at = Some(now);
        }
        self.updated_at = Some(now);
    }
}

/// 作ったばかりのトークン。`create_token` が返します。
///
/// **平文が見えるのはこのときだけです。** 表には SHA-256 しか残らないので、
/// あとから取り出す方法はありません。
///
/// `Debug` では平文を伏せます。
pub struct NewAccessToken {
    /// 表に入れた行。
    pub access_token: PersonalAccessToken,
    /// 相手に渡す値（`{id}|{平文}`）。
    pub plain_text_token: String,
}

impl std::fmt::Debug for NewAccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewAccessToken")
            .field("access_token", &self.access_token)
            .field("plain_text_token", &"***")
            .finish()
    }
}

/// トークンを持てる利用者。Laravel の `HasApiTokens` に当たります。
///
/// [`Authenticatable`] を実装した型なら、何も書かずに使えます。
/// 使う所で `use bengara::auth::HasApiTokens;` してください。
///
/// ```ignore
/// use bengara::auth::HasApiTokens;
///
/// let token = user.create_token("iPhone", &["posts:write"], None).await?;
/// // token.plain_text_token を相手に渡す
///
/// // 全部の端末のトークンを消す
/// user.tokens().delete().await?;
/// ```
pub trait HasApiTokens: Authenticatable {
    /// トークンを作って表に入れる。Laravel の `createToken` に当たります。
    ///
    /// - `abilities` は能力の一覧です。何でもできるトークンは `&["*"]`。
    /// - `expires_at` が `None` なら無期限です。
    /// - 保存していない利用者（主キーが空）ではエラーです。
    fn create_token(
        &self,
        name: &str,
        abilities: &[&str],
        expires_at: Option<Carbon>,
    ) -> impl Future<Output = Result<NewAccessToken>> + Send;

    /// この利用者のトークンを引くクエリ。Laravel の `tokens()` に当たります。
    ///
    /// ```ignore
    /// let list = user.tokens().get().await?;
    /// user.tokens().delete().await?;
    /// ```
    fn tokens(&self) -> ModelQuery<PersonalAccessToken>;
}

impl<U: Authenticatable> HasApiTokens for U {
    fn create_token(
        &self,
        name: &str,
        abilities: &[&str],
        expires_at: Option<Carbon>,
    ) -> impl Future<Output = Result<NewAccessToken>> + Send {
        let unsaved = self.is_new();
        let tokenable_id = value_to_key(&self.key());
        let name = name.to_string();
        let abilities: Vec<String> = abilities.iter().map(|a| (*a).to_string()).collect();
        async move {
            if unsaved {
                return Err(Error::msg(
                    "保存していないモデルにはトークンを作れません。先に save() してください",
                ));
            }
            let plain = generate_token_string();
            let mut access_token = PersonalAccessToken {
                id: 0,
                tokenable_type: U::morph_class(),
                tokenable_id,
                name,
                abilities,
                last_used_at: None,
                expires_at,
                created_at: None,
                updated_at: None,
                token: hash_token(&plain),
            };
            access_token.save().await?;
            let plain_text_token = format!("{}|{plain}", access_token.id);
            Ok(NewAccessToken {
                access_token,
                plain_text_token,
            })
        }
    }

    fn tokens(&self) -> ModelQuery<PersonalAccessToken> {
        PersonalAccessToken::query()
            .where_("tokenable_type", U::morph_class())
            .where_("tokenable_id", key_to_value(&value_to_key(&self.key())))
    }
}

/// 平文の SHA-256 を 16 進にする。
fn hash_token(plain: &str) -> String {
    crypto::to_hex(&crypto::sha256(plain.as_bytes()))
}

/// 平文を作る。英数字 40 文字の乱数に、その crc32b をつなぐ（Sanctum 4 と同じ形）。
///
/// crc32b は秘密を守るためのものではありません。GitHub などの「秘密の書き込みを
/// 見つける仕組み」が、本物らしい値かを見分けるための印です。
fn generate_token_string() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    // 62 の倍数（248）以上の値は捨てる。そのまま余りを取ると、先頭の文字が出やすくなる。
    const LIMIT: u8 = (256 / ALPHABET.len() * ALPHABET.len()) as u8;
    let mut entropy = String::with_capacity(ENTROPY_LENGTH + 8);
    while entropy.len() < ENTROPY_LENGTH {
        for byte in crypto::random_bytes::<64>() {
            if byte < LIMIT && entropy.len() < ENTROPY_LENGTH {
                entropy.push(ALPHABET[usize::from(byte) % ALPHABET.len()] as char);
            }
        }
    }
    let checksum = crc32b(entropy.as_bytes());
    entropy.push_str(&format!("{checksum:08x}"));
    entropy
}

/// crc32b（PHP の `hash('crc32b', ..)`。zlib と同じ CRC-32）。
///
/// 1 回に 48 バイトしか通さないので、表を持たずに 1 ビットずつ計算します。
fn crc32b(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// 能力の列（JSON の配列）を読む。読めなければ空（何もできない）にする。
fn parse_abilities(text: &str) -> Vec<String> {
    match serde_json::from_str::<Vec<String>>(text) {
        Ok(list) => list,
        Err(_) => {
            tracing::warn!("personal_access_tokens.abilities を JSON の配列として読めませんでした");
            Vec::new()
        }
    }
}

/// 主キーの値を、`tokenable_id` に入れる文字列にする。`NULL` と読めない値は空。
fn value_to_key(value: &Value) -> String {
    if value.is_null() {
        return String::new();
    }
    <String as FromValue>::from_value(value).unwrap_or_default()
}

/// `tokenable_id` の文字列を、束縛する値に戻す。整数に読めれば整数にする
/// （PostgreSQL は `bigint = text` を断るため）。
fn key_to_value(key: &str) -> Value {
    key.parse::<i64>()
        .map(Value::Int)
        .unwrap_or_else(|_| Value::Text(key.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::{Driver, Schema};

    /// 指示書の値。PHP で Sanctum 4 と同じ組み立てで作ったもの。
    const LARAVEL_TOKEN: &str = "1|Kq3vX9mLr2TzP8wN5bYc1HdF7gJs4AeU6oRiVxQt20e242d5";
    const LARAVEL_HASH: &str = "f68f7bda9d73a8af744b98f43bf30e6ee41bcd2fd7c256e1c35a6b253fccb345";

    #[test]
    fn crc32bがphpと同じ値になる() {
        assert_eq!(crc32b(b""), 0);
        // よく知られた検査用の値。
        assert_eq!(crc32b(b"123456789"), 0xcbf4_3926);
        assert_eq!(
            format!(
                "{:08x}",
                crc32b(b"Kq3vX9mLr2TzP8wN5bYc1HdF7gJs4AeU6oRiVxQt")
            ),
            "20e242d5"
        );
    }

    #[test]
    fn laravelのトークンと同じハッシュになる() {
        let (_, plain) = LARAVEL_TOKEN.split_once('|').unwrap();
        assert_eq!(hash_token(plain), LARAVEL_HASH);
    }

    #[test]
    fn 作る平文はsanctum4の形() {
        let plain = generate_token_string();
        assert_eq!(plain.len(), 48);
        let (entropy, checksum) = plain.split_at(40);
        assert!(
            entropy.bytes().all(|b| b.is_ascii_alphanumeric()),
            "{entropy}"
        );
        assert_eq!(checksum, format!("{:08x}", crc32b(entropy.as_bytes())));
        assert_ne!(plain, generate_token_string(), "毎回変わる");
    }

    fn token(abilities: &[&str]) -> PersonalAccessToken {
        let mut token = PersonalAccessToken::fake("App\\Models\\User", "7", abilities);
        token.token = LARAVEL_HASH.into();
        token
    }

    #[test]
    fn 能力の判定() {
        let t = token(&["posts:read", "posts:write"]);
        assert!(t.can("posts:read"));
        assert!(t.cant("posts:delete"));
        assert!(t.cant("posts:*"), "書き方の展開はしない");

        let all = token(&["*"]);
        assert!(all.can("posts:delete"));
        assert!(all.can("anything"));

        assert!(token(&[]).cant("posts:read"));
    }

    #[test]
    fn 期限の判定() {
        let mut t = token(&["*"]);
        assert!(!t.is_expired(), "期限が無ければ切れない");
        t.expires_at = Some(Carbon::now().add_minutes(5));
        assert!(!t.is_expired());
        t.expires_at = Some(Carbon::now().sub_minutes(5));
        assert!(t.is_expired());
    }

    #[test]
    fn 能力の列を読む() {
        assert_eq!(parse_abilities(r#"["*"]"#), vec!["*".to_string()]);
        assert_eq!(parse_abilities(r#"["a","b"]"#), vec!["a", "b"]);
        assert!(
            parse_abilities("こわれた値").is_empty(),
            "読めなければ何もできない"
        );
        assert!(parse_abilities(r#"{"a":1}"#).is_empty());
    }

    #[test]
    fn 平文はdebugに出ない() {
        let new = NewAccessToken {
            access_token: token(&["*"]),
            plain_text_token: "1|ひみつ".into(),
        };
        let text = format!("{new:?}");
        assert!(!text.contains("ひみつ"), "{text}");
        assert!(!text.contains(LARAVEL_HASH), "ハッシュも出さない: {text}");
    }

    #[test]
    fn 表の定義が作れる() {
        let mut schema = Schema::new(Driver::Sqlite);
        PersonalAccessToken::define(&mut schema);
        let sql = schema.to_sql().join("\n");
        assert!(sql.contains("\"personal_access_tokens\""), "{sql}");
        for column in PersonalAccessToken::COLUMNS {
            assert!(sql.contains(&format!("\"{column}\"")), "{column}: {sql}");
        }
        assert_eq!(PersonalAccessToken::TABLE, "personal_access_tokens");
    }

    #[test]
    fn 主キーの値の行き来() {
        assert_eq!(value_to_key(&Value::Int(7)), "7");
        assert_eq!(value_to_key(&Value::Text("a1".into())), "a1");
        assert_eq!(key_to_value("7"), Value::Int(7));
        assert_eq!(key_to_value("a1"), Value::Text("a1".into()));
    }
}
