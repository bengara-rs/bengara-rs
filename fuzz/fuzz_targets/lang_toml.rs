//! bengara-build の言語ファイル（TOML の一部）の読み取り。
//! 確かめること：パニックしない（bengara-build はパニック禁止）。
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let _ = bengara_build::fuzzing::lang_toml(&text);
});
