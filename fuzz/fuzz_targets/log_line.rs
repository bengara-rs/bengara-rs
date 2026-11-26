//! ログの行の組み立てと、リクエスト ID の形の確かめ。
//! 確かめること：パニックしない。行の終わりのほかに CR・LF が無い。UTF-8（`String` なので保証）。
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let _ = bengara::fuzzing::request_id_is_valid(&text);
    for line in bengara::fuzzing::log_lines(&text) {
        let body = line.strip_suffix('\n').expect("行は LF で終わる");
        assert!(
            !body.contains('\n') && !body.contains('\r'),
            "行の途中に改行がある: {line:?}"
        );
    }
});
