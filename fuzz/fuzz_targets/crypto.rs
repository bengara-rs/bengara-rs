//! 自前の暗号の部品を、RustCrypto（sha2・hmac・pbkdf2）と突き合わせる。
//! 確かめること：同じ値になる。`constant_time_eq` は `==` と同じ真偽。16 進は往復で戻る。
#![no_main]

use hmac::{Hmac, KeyInit, Mac};
use libfuzzer_sys::fuzz_target;
use sha2::{Digest, Sha256};

fuzz_target!(|data: &[u8]| {
    // 1 バイト目：PBKDF2 の回数（1〜4）。2 バイト目：鍵の長さ。残り：鍵と本文。
    let Some((&first, rest)) = data.split_first() else {
        return;
    };
    let iterations = u32::from(first % 4) + 1;
    let Some((&key_length, rest)) = rest.split_first() else {
        return;
    };
    let key_length = usize::from(key_length).min(rest.len());
    let (key, message) = rest.split_at(key_length);

    // SHA-256
    let expected: [u8; 32] = Sha256::digest(message).into();
    assert_eq!(bengara::fuzzing::sha256(message), expected, "sha256");

    // HMAC-SHA256
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC は鍵の長さを問わない");
    mac.update(message);
    let expected: [u8; 32] = mac.finalize().into_bytes().into();
    assert_eq!(
        bengara::fuzzing::hmac_sha256(key, message),
        expected,
        "hmac_sha256"
    );

    // PBKDF2-HMAC-SHA256（回数は 1〜4）
    let mut expected = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(key, message, iterations, &mut expected);
    assert_eq!(
        bengara::fuzzing::pbkdf2_sha256(key, message, iterations),
        expected,
        "pbkdf2_sha256"
    );

    // 定数時間の比較は == と同じ真偽。
    assert_eq!(
        bengara::fuzzing::constant_time_eq(key, message),
        key == message
    );
    assert!(bengara::fuzzing::constant_time_eq(message, message));

    // 16 進は往復で戻る。読めない文字列でもパニックしない。
    let hex = bengara::fuzzing::to_hex(message);
    assert_eq!(bengara::fuzzing::from_hex(&hex).as_deref(), Some(message));
    let _ = bengara::fuzzing::from_hex(&String::from_utf8_lossy(message));
});
