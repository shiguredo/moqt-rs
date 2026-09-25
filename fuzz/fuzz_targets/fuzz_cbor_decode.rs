#![no_main]

use libfuzzer_sys::fuzz_target;
use shiguredo_moqt::cbor::decode::decode;

fuzz_target!(|data: &[u8]| {
    // 任意入力のデコードがパニックしないことと、デコードに成功した値の
    // エンコードと診断記法がパニックしないことを検証する
    if let Ok(value) = decode(data) {
        let _ = value.to_bytes();
        let _ = value.to_canonical_bytes();
        let _ = value.to_string();
    }
});
