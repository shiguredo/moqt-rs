#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // DPoP proof の JWT (JWS compact) としてデコードする
    if let Ok(text) = core::str::from_utf8(data) {
        let _ = shiguredo_moqt::c4m::dpop::DpopProof::decode(text);
        // JWK 単体のデコードも同時に fuzz する
        let _ = shiguredo_moqt::c4m::jwk::Jwk::decode(text);
    }
});
