use super::*;

// ─── delta update 新構造 ──────────────────────────────────────────────────────

#[test]
fn delta_update_empty_array_rejected() {
    // draft-ietf-moq-msf-02 §5.3 (Delta updates): 少なくとも 1 つの操作が必要
    let json = br#"{"generatedAt":1,"deltaUpdate":[]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn delta_update_unknown_op_rejected() {
    let json = br#"{"generatedAt":1,"deltaUpdate":[{"op":"unknown","tracks":[]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn delta_update_contains_version_rejected() {
    // draft-ietf-moq-msf-02 §5.3 (Delta updates): delta update に version は含めない
    let json =
        br#"{"generatedAt":1,"deltaUpdate":[{"op":"add","tracks":[]}],"version":"draft-02"}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn delta_update_contains_tracks_rejected() {
    // draft-ietf-moq-msf-02 §5.3 (Delta updates): delta update に tracks は含めない
    let json = br#"{"generatedAt":1,"deltaUpdate":[{"op":"add","tracks":[]}],"tracks":[]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn delta_operation_missing_op_rejected() {
    let json = br#"{"generatedAt":1,"deltaUpdate":[{"tracks":[]}]}}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn delta_operation_missing_tracks_rejected() {
    let json = br#"{"generatedAt":1,"deltaUpdate":[{"op":"add"}]}}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn clone_track_target_latency_and_buffers_together_rejected() {
    // draft-ietf-moq-msf-02 §5.2.9 / §5.2.10: clone の isLive:true 共存も reject されること
    let json = br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","isLive":true,"targetLatency":500,"buffers":{"target":1000}}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(reason))
        if reason == "targetLatency and buffers MUST NOT be present together"
    ));
}

#[test]
fn add_target_latency_and_buffers_together_rejected() {
    // draft-ietf-moq-msf-02 §5.2.9 / §5.2.10: delta add 経路の共存も reject されること
    let json = br#"{"generatedAt":1,"deltaUpdate":[{"op":"add","tracks":[{"name":"t","packaging":"loc","isLive":false,"targetLatency":500,"buffers":{"target":1000}}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(reason))
        if reason == "targetLatency and buffers MUST NOT be present together"
    ));
}

#[test]
fn clone_omitted_kept() {
    // clone の isLive 省略時は単独値を保持すること
    let json = br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","targetLatency":500}]}]}"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Delta(delta) = doc else {
        panic!("Delta update がデコードされること");
    };
    let MsfDeltaOperation::Clone { tracks } = &delta.operations[0] else {
        panic!("Clone 操作がデコードされること");
    };
    assert_eq!(tracks[0].target_latency, Some(500));
}

#[test]
fn clone_track_track_duration_when_is_live_true_rejected() {
    let json = br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","isLive":true,"trackDuration":1000}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn clone_track_event_type_with_non_eventtimeline_rejected() {
    let json = br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","packaging":"loc","eventType":"x"}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}
