//! リクエスト ID（`X-Request-Id`）。
//!
//! - 受け取るのは、接続元が `TRUSTED_PROXIES` で信頼されていて、値の形が正しいときだけです。
//! - それ以外は作ります。32 文字の小文字の 16 進です。
//! - ログへは `tokio::task_local!` で伝えます。span にしないのは、`RUST_LOG` の絞り込みで
//!   span が無効になると（`RUST_LOG=myapp=debug` など）、ID が付かなくなるためです。

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

/// 応答に付け、受け取るヘッダーの名前（小文字）。
pub(crate) const HEADER: &str = "x-request-id";

/// 受け取る値の長さの上限。
const MAX_LEN: usize = 64;

tokio::task_local! {
    static CURRENT: Arc<str>;
}

/// いまの ID（リクエストの中だけ）。層が読みます。
pub(crate) fn current() -> Option<Arc<str>> {
    CURRENT.try_with(Arc::clone).ok()
}

/// `future` を、ID を持たせて動かす。`tokio::spawn` に渡す前にも包みます
/// （spawn すると task-local が切れるため）。
pub(crate) fn scope<F: Future>(id: Arc<str>, future: F) -> impl Future<Output = F::Output> {
    CURRENT.scope(id, future)
}

/// 受け取ってよい形か。`[0-9A-Za-z._-]` の 1〜64 文字です。
pub(crate) fn is_valid(value: &str) -> bool {
    (1..=MAX_LEN).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// 届いた `X-Request-Id` から、受け取る値を選ぶ。
///
/// 接続元が信頼できないときは受け取りません（相手が自由に決めた値をログに混ぜないため）。
/// 何本も来たら最初の 1 本だけを見ます。形が悪ければ受け取りません。
pub(crate) fn accepted<'a>(
    mut values: impl Iterator<Item = &'a str>,
    trusted_peer: bool,
) -> Option<&'a str> {
    if !trusted_peer {
        return None;
    }
    values.next().filter(|value| is_valid(value))
}

/// 作るための鍵。最初の 1 回だけ OS の乱数から決めます。
struct Keys {
    /// 前半 64 ビット。プロセスごとの乱数。
    prefix: u64,
    mask: u64,
    /// 奇数（掛け算を戻せるようにするため）。
    multiplier: u64,
    finish: u64,
}

fn keys() -> &'static Keys {
    static KEYS: OnceLock<Keys> = OnceLock::new();
    KEYS.get_or_init(|| {
        let bytes = crate::support::crypto::random_bytes::<32>();
        let word = |index: usize| {
            let mut out = [0u8; 8];
            out.copy_from_slice(&bytes[index * 8..index * 8 + 8]);
            u64::from_le_bytes(out)
        };
        Keys {
            prefix: word(0),
            mask: word(1),
            multiplier: word(2) | 1,
            finish: word(3),
        }
    })
}

/// 連番。
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// 連番を混ぜる。xor・奇数の掛け算・xor はどれも戻せるので、違う連番は違う値になります
/// （同じプロセスの中で重ならない）。
fn mix(sequence: u64, keys: &Keys) -> u64 {
    ((sequence ^ keys.mask).wrapping_mul(keys.multiplier)) ^ keys.finish
}

/// 新しい ID を作る。**リクエストごとに OS の乱数は呼びません。**
pub(crate) fn generate() -> Arc<str> {
    let keys = keys();
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Arc::from(format!("{:016x}{:016x}", keys.prefix, mix(sequence, keys)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 作ったidは32文字の小文字の16進() {
        let id = generate();
        assert_eq!(id.len(), 32);
        assert!(
            id.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "{id}"
        );
        assert!(is_valid(&id));
    }

    #[test]
    fn 十万件作っても重ならない() {
        let ids: std::collections::HashSet<Arc<str>> = (0..100_000).map(|_| generate()).collect();
        assert_eq!(ids.len(), 100_000);
    }

    #[test]
    fn 混ぜ方は連番ごとに違う値になる() {
        let keys = Keys {
            prefix: 0,
            mask: 0x1234,
            multiplier: 0x9E37_79B9_7F4A_7C15,
            finish: 0xFFFF,
        };
        let values: std::collections::HashSet<u64> = (0..10_000).map(|n| mix(n, &keys)).collect();
        assert_eq!(values.len(), 10_000);
    }

    #[test]
    fn 受け取る形() {
        assert!(is_valid("abc-DEF_1.2"));
        assert!(is_valid(&"a".repeat(64)));
        assert!(!is_valid(""));
        assert!(!is_valid(&"a".repeat(65)));
        assert!(!is_valid("a b"));
        assert!(!is_valid("a/b"));
        assert!(!is_valid("日本"));
        assert!(!is_valid("a\nb"));
    }

    #[test]
    fn 信頼できる接続元の正しい値だけを受け取る() {
        assert_eq!(accepted(["abc"].into_iter(), true), Some("abc"));
        // 何本も来たら最初の 1 本。
        assert_eq!(
            accepted(["first", "second"].into_iter(), true),
            Some("first")
        );
        // 信頼できない接続元。
        assert_eq!(accepted(["abc"].into_iter(), false), None);
        // 形が悪い・長すぎる。
        assert_eq!(accepted(["a b"].into_iter(), true), None);
        let long = "a".repeat(65);
        assert_eq!(accepted([long.as_str()].into_iter(), true), None);
        // 最初の 1 本が悪ければ、2 本目は見ない。
        assert_eq!(accepted(["bad value", "good"].into_iter(), true), None);
        assert_eq!(accepted(std::iter::empty(), true), None);
    }

    #[test]
    fn 範囲の中だけidが見える() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            assert!(current().is_none());
            let seen = scope(Arc::from("abc"), async { current() }).await;
            assert_eq!(seen.as_deref(), Some("abc"));
            assert!(current().is_none());
        });
    }
}
