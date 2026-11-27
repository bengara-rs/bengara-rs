//! `Cookie` ヘッダーの解析。確かめること：パニックしない。
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let _ = bengara::fuzzing::cookie_header(&text);
});
