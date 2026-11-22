use super::*;

// ─── framerate の非有限値拒否 ─────────────────────────────────────────────────

/// framerate に infinity (1e400 は f64 で infinity に丸められる) を指定すると拒否される
#[test]
fn framerate_infinity_rejected() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"framerate":1e400}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}
