#![no_main]

use libfuzzer_sys::fuzz_target;

use shiguredo_moqt::msf::uri::{parse_msf_fragment, parse_msf_uri};
use shiguredo_moqt::msf::{parse_fragment_pairs, resolve_catalog_variables};

// MSF URI / fragment / 変数置換は任意の文字列を入力に取るため panic 非発生を担保する
fuzz_target!(|data: &[u8]| {
    if let Ok(text) = core::str::from_utf8(data) {
        let _ = parse_msf_fragment(text);
        let _ = parse_msf_uri(text);
        let _ = parse_fragment_pairs(text);
        // JSON は任意バイト列、fragment は同じ入力の UTF-8 表現を使う
        let _ = resolve_catalog_variables(data, text);
    }
});
