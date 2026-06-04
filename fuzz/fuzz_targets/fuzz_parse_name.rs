#![no_main]

use libfuzzer_sys::fuzz_target;

// draft-ietf-moq-transport-21 §8.8.1 (Parsing Serialized Names): parse_name は任意の &str を受け取るためパニック非発生を担保する。
fuzz_target!(|data: &[u8]| {
    if let Ok(s) = core::str::from_utf8(data) {
        let _ = shiguredo_moqt::name::parse_name(s);
    }
});
