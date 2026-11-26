//! `multipart/form-data` の解析。content-type の boundary も入力から作る。
//! 確かめること：パニックしない。
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // 最初の 1 バイトで boundary の長さ（0〜79）を決め、続くバイトを boundary にする。
    let Some((&length, rest)) = data.split_first() else {
        return;
    };
    let length = usize::from(length % 80).min(rest.len());
    let (boundary, body) = rest.split_at(length);
    let boundary = String::from_utf8_lossy(boundary);
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let _ = bengara::fuzzing::multipart(body, &content_type);
    // 引用符で囲んだ形も試す。
    let quoted = format!("multipart/form-data; boundary=\"{boundary}\"");
    let _ = bengara::fuzzing::multipart(body, &quoted);
});
