#![no_main]

use libfuzzer_sys::fuzz_target;

// draft-ietf-moq-transport-22 §8.8 (Representing Namespace and Track Names) / draft-ietf-moq-msf-01 §11.1 (URL construction and interpretation):
// parse_name と parse_name_with_percent_encoding は任意の &str を受け取るためパニック非発生を担保する。
fuzz_target!(|data: &[u8]| {
    if let Ok(s) = core::str::from_utf8(data) {
        let _ = shiguredo_moqt::name::parse_name(s);
        let _ = shiguredo_moqt::name::parse_name_with_percent_encoding(s);
    }
});
