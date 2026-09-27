#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // デコードだけでなく、決定論的エンコードの往復も fuzz する
    if let Ok(value) = shiguredo_moqt::c4m::cbor::decode(data) {
        let _ = shiguredo_moqt::c4m::cbor::encode(&value);
    }
});
