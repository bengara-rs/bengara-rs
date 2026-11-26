//! パーセントの符号化と復号。
//! 確かめること：パニックしない。`encode` → `decode_strict` の往復で元に戻る。
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let _ = bengara::fuzzing::percent_decode(&text);
    let _ = bengara::fuzzing::percent_decode_strict(&text);

    let encoded = bengara::fuzzing::percent_encode(&text);
    assert!(encoded.is_ascii(), "符号化した結果は ASCII だけ: {encoded:?}");
    assert_eq!(
        bengara::fuzzing::percent_decode_strict(&encoded),
        text,
        "往復で元に戻る"
    );
});
