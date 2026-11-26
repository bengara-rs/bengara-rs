//! `init` と `serve` が使う `Cargo.toml` の読み取り。確かめること：パニックしない。
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let _ = bengara::fuzzing::cargo_toml(&text);
});
