#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // 非圧縮 (signal なし) と Gzip signal の両方の catalog decode 経路を駆動する。
    // signal なしでは gzip magic による自動展開を行わない。
    use shiguredo_moqt::msf::MsfCompression;
    let _ = shiguredo_moqt::msf::MsfCatalogDocument::decode_with_compression(
        data,
        MsfCompression::None,
    );
    let _ = shiguredo_moqt::msf::MsfCatalogDocument::decode_with_compression(
        data,
        MsfCompression::Gzip,
    );
});
