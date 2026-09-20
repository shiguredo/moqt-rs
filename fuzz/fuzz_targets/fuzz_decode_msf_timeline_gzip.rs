#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Media / Event 両方の自動復号経路を駆動する
    let _ = shiguredo_moqt::msf::decode_media_timeline(data);
    let _ = shiguredo_moqt::msf::decode_event_timeline(data);
});
