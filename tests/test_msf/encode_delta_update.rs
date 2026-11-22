use super::*;

// ─── encode 時のデルタ更新検証 (decode_delta と同等) ──────────────────────────

/// 空の operations は encode でも拒否されること
#[test]
fn encode_delta_empty_operations_rejected() {
    // 仕様: draft-ietf-moq-msf-02 §5.3 (Delta updates)
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: 1,
        operations: Vec::new(),
    });
    assert!(matches!(
        doc.encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// delta add の不正トラックは encode でも拒否されること
#[test]
fn encode_delta_add_invalid_track_rejected() {
    // draft-ietf-moq-msf-02 §7.2 (Media Timeline Catalog requirements): depends 必須
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::MediaTimeline, true);
    track.mime_type = Some("application/json".to_string());
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![track],
        }],
    });
    assert!(matches!(
        doc.encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// delta clone の targetLatency / buffers 共存は encode でも拒否されること
#[test]
fn encode_delta_clone_target_latency_and_buffers_rejected() {
    // 仕様: draft-ietf-moq-msf-02 §5.2.9 (Target latency) / §5.2.10 (Buffers)
    let mut clone = MsfCloneTrack::new("c".to_string(), "p".to_string());
    clone.target_latency = Some(500);
    clone.buffers = Some(MsfBuffers {
        target: Some(1000),
        min: None,
        max: None,
    });
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    });
    assert!(matches!(
        doc.encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(reason))
            if reason == "targetLatency and buffers MUST NOT be present together"
    ));
}

/// delta clone の isLive=true + trackDuration は encode でも拒否されること
#[test]
fn encode_delta_clone_track_duration_when_is_live_true_rejected() {
    // 仕様: draft-ietf-moq-msf-02 §5.2.36 (Track duration)
    let mut clone = MsfCloneTrack::new("c".to_string(), "p".to_string());
    clone.is_live = Some(true);
    clone.track_duration = Some(1000);
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    });
    assert!(matches!(
        doc.encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// delta clone の非 eventtimeline packaging + eventType は encode でも拒否されること
#[test]
fn encode_delta_clone_event_type_with_non_eventtimeline_rejected() {
    // 仕様: draft-ietf-moq-msf-02 §5.2.6 (Event timeline type)
    let mut clone = MsfCloneTrack::new("c".to_string(), "p".to_string());
    clone.packaging = Some(MsfPackaging::Loc);
    clone.event_type = Some("x".to_string());
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    });
    assert!(matches!(
        doc.encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// delta clone の不正 lang は encode でも拒否されること
#[test]
fn encode_delta_clone_invalid_lang_rejected() {
    // 仕様: draft-ietf-moq-msf-02 §5.2.33 (Language)
    let mut clone = MsfCloneTrack::new("c".to_string(), "p".to_string());
    clone.lang = Some("1".to_string());
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    });
    assert!(matches!(
        doc.encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// delta add / clone の irregular タグは encode でも受理され、decode で同じ値に戻ること
#[test]
fn encode_delta_irregular_lang_roundtrip() {
    // 仕様: draft-ietf-moq-msf-02 §5.2.33 (Language)
    let mut add = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    add.lang = Some("i-klingon".to_string());
    let mut clone = MsfCloneTrack::new("c".to_string(), "p".to_string());
    // 大文字表記も encode / decode で正規化されずそのまま保持されることを確認する
    clone.lang = Some("I-AMI".to_string());
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![
            MsfDeltaOperation::Add { tracks: vec![add] },
            MsfDeltaOperation::Clone {
                tracks: vec![clone],
            },
        ],
    });
    let bytes = doc
        .encode(shiguredo_moqt::msf::MsfCompression::None)
        .expect("irregular タグは encode できること");
    let decoded = MsfCatalogDocument::decode(&bytes, shiguredo_moqt::msf::MsfCompression::None)
        .expect("encode 結果は decode できること");
    assert_eq!(decoded, doc, "encode / decode で lang が保持されること");
}
