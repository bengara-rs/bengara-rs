//! `.env` の読み取り（本物の環境変数は見ない）と、`key:generate` が使う値の読み取り。
//! 確かめること：パニックしない。
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let _ = bengara::fuzzing::dotenv(&text);
    for line in text.lines() {
        let _ = bengara::fuzzing::dotenv_value(line);
    }
});
