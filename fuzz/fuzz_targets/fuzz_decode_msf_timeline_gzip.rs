#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Media / Event の両方について、signal なし (None) と Gzip の復号経路を駆動する。
    // signal なしでは gzip magic による自動展開を行わない。
    use shiguredo_moqt::msf::MsfCompression;
    let _ = shiguredo_moqt::msf::decode_media_timeline(data, MsfCompression::None);
    let _ = shiguredo_moqt::msf::decode_media_timeline(data, MsfCompression::Gzip);
    let _ = shiguredo_moqt::msf::decode_event_timeline(data, MsfCompression::None);
    let _ = shiguredo_moqt::msf::decode_event_timeline(data, MsfCompression::Gzip);
});
