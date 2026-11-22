use super::*;

// ─── authInfo value_raw の encode 時検証 ──────────────────────────────────────

/// 手組みの非 UTF-8 value_raw は encode で拒否され、panic しない
///
/// draft-ietf-moq-msf-02 §5.2.42 (Authorization Info)
#[test]
fn auth_info_non_utf8_value_rejected_on_encode() {
    let mut track = MsfTrack::new("video".to_string(), MsfPackaging::Loc, true);
    track.auth_info = Some(vec![MsfAuthInfo {
        scheme: "x".to_string(),
        value_raw: vec![0xFF, 0xFE],
    }]);
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        version: MSF_VERSION.to_string(),
        generated_at: None,
        is_complete: false,
        tracks: vec![track],
        publish_tracks: Vec::new(),
        removed_tracks: Default::default(),
        init_data_list: Vec::new(),
    });
    assert!(
        matches!(
            doc.encode(shiguredo_moqt::msf::MsfCompression::None),
            Err(MessageError::InvalidCatalog(_))
        ),
        "非 UTF-8 の value_raw は InvalidCatalog で拒否されること"
    );
}

/// publishTracks 側の手組み不正 value_raw も encode で拒否され、panic しない
///
/// draft-ietf-moq-msf-02 §5.1.5 (Publish tracks) / §5.2.42 (Authorization Info)
#[test]
fn auth_info_non_utf8_value_in_publish_tracks_rejected_on_encode() {
    let mut track = MsfTrack::new("video".to_string(), MsfPackaging::Loc, true);
    track.auth_info = Some(vec![MsfAuthInfo {
        scheme: "x".to_string(),
        value_raw: vec![0xFF, 0xFE],
    }]);
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        version: MSF_VERSION.to_string(),
        generated_at: None,
        is_complete: false,
        tracks: Vec::new(),
        publish_tracks: vec![track],
        removed_tracks: Default::default(),
        init_data_list: Vec::new(),
    });
    assert!(
        matches!(
            doc.encode(shiguredo_moqt::msf::MsfCompression::None),
            Err(MessageError::InvalidCatalog(_))
        ),
        "publishTracks の非 UTF-8 value_raw は InvalidCatalog で拒否されること"
    );
}

/// 手組みの非 JSON 値 value_raw は encode で拒否される
///
/// draft-ietf-moq-msf-02 §5.2.42 (Authorization Info): 値は単独の JSON 値
#[test]
fn auth_info_non_json_value_rejected_on_encode() {
    let mut track = MsfTrack::new("video".to_string(), MsfPackaging::Loc, true);
    track.auth_info = Some(vec![MsfAuthInfo {
        scheme: "x".to_string(),
        value_raw: b"{invalid".to_vec(),
    }]);
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        version: MSF_VERSION.to_string(),
        generated_at: None,
        is_complete: false,
        tracks: vec![track],
        publish_tracks: Vec::new(),
        removed_tracks: Default::default(),
        init_data_list: Vec::new(),
    });
    assert!(
        matches!(
            doc.encode(shiguredo_moqt::msf::MsfCompression::None),
            Err(MessageError::InvalidCatalog(_))
        ),
        "単独の JSON 値でない value_raw は InvalidCatalog で拒否されること"
    );
}
