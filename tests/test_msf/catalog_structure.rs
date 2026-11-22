use super::*;

// ─── カタログ基本構造と timeline の decode 検証 ───────────────────────────────

#[test]
fn missing_version() {
    assert!(matches!(
        MsfCatalogDocument::decode(
            b"{\"tracks\":[]}",
            shiguredo_moqt::msf::MsfCompression::None
        ),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn version_zero() {
    assert!(matches!(
        MsfCatalogDocument::decode(b"{\"version\":\"0\",\"tracks\":[]}", shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalogVersion(v)) if v == "0"
    ));
}

#[test]
fn version_99() {
    assert!(matches!(
        MsfCatalogDocument::decode(b"{\"version\":\"99\",\"tracks\":[]}", shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalogVersion(v)) if v == "99"
    ));
}

#[test]
fn version_not_string() {
    // draft-ietf-moq-msf-02 §5.1.1 (MSF version): version は JSON Type: String
    assert!(matches!(
        MsfCatalogDocument::decode(
            b"{\"version\":1,\"tracks\":[]}",
            shiguredo_moqt::msf::MsfCompression::None
        ),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn missing_tracks() {
    assert!(matches!(
        MsfCatalogDocument::decode(
            b"{\"version\":\"draft-02\"}",
            shiguredo_moqt::msf::MsfCompression::None
        ),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn track_missing_name() {
    let json = br#"{"version":"draft-02","tracks":[{"packaging":"loc"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn track_missing_packaging() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","isLive":true}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn track_missing_is_live() {
    // draft-ietf-moq-msf-02 §5.2.8 (Is Live): isLive は必須
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn not_json_object() {
    assert!(matches!(
        MsfCatalogDocument::decode(b"[1,2,3]", shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn empty_input() {
    assert!(matches!(
        MsfCatalogDocument::decode(b"", shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn trailing_data() {
    assert!(matches!(
        MsfCatalogDocument::decode(
            b"{\"version\":1,\"tracks\":[]}garbage",
            shiguredo_moqt::msf::MsfCompression::None
        ),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn media_timeline_not_array() {
    assert!(matches!(
        MsfMediaTimeline::decode(b"{}"),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn media_timeline_record_wrong_length() {
    // レコードが 2 要素 (3 要素必要)
    assert!(matches!(
        MsfMediaTimeline::decode(b"[[0,[0,0]]]"),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn media_timeline_location_wrong_length() {
    // ロケーションが 1 要素 (2 要素必要)
    assert!(matches!(
        MsfMediaTimeline::decode(b"[[0,[0],0]]"),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn event_timeline_missing_index() {
    assert!(matches!(
        MsfEventTimeline::decode(br#"[{"data":1}]"#),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn event_timeline_missing_data() {
    assert!(matches!(
        MsfEventTimeline::decode(br#"[{"T":1000}]"#),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn depends_entry_not_string_or_integer() {
    // draft-ietf-moq-msf-02 §5.2.15 (Dependencies): 要素は String または整数 Number
    for element in ["true", r#"{"a":1}"#, "1.5"] {
        let json = format!(
            r#"{{"version":"draft-02","tracks":[{{"name":"t","packaging":"loc","isLive":true,"depends":[{element}]}}]}}"#
        );
        assert!(
            matches!(
                MsfCatalogDocument::decode(
                    json.as_bytes(),
                    shiguredo_moqt::msf::MsfCompression::None
                ),
                Err(MessageError::InvalidCatalog(_))
            ),
            "depends の {element} 要素を拒否すること"
        );
    }
}

#[test]
fn event_timeline_missing_event_type_rejected() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"eventtimeline","isLive":true}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn media_timeline_missing_depends_rejected() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"mediatimeline","isLive":true,"mimeType":"application/json"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn media_timeline_arbitrary_mime_type_accepted() {
    // draft-ietf-moq-msf-02 §7.2: mimeType の値は要求しない
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"mediatimeline","isLive":true,"depends":["video"],"mimeType":"text/plain"},{"name":"video","packaging":"loc","isLive":true}]}"#;
    assert!(MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None).is_ok());
}

#[test]
fn event_type_with_non_eventtimeline_rejected() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"eventType":"x"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn eventtimeline_without_depends_accepted() {
    // draft-ietf-moq-msf-02 §8.3: depends は index が media timeline を参照する場合だけ必須
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"eventtimeline","isLive":true,"eventType":"cue","mimeType":"application/json"}]}"#;
    assert!(MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None).is_ok());
}

#[test]
fn eventtimeline_arbitrary_mime_type_accepted() {
    // draft-ietf-moq-msf-02 §8.3: mimeType の値は要求しない
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"eventtimeline","isLive":true,"eventType":"cue","mimeType":"text/plain"}]}"#;
    assert!(MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None).is_ok());
}

/// 通常トラックに parentName が含まれると不正カタログとして拒否される
///
/// draft-ietf-moq-msf-02 §5.2.34 (Parent name): "This field MUST only be included
/// inside a clone operation in a delta update Section 5.1.6."
#[test]
fn parent_name_in_track_rejected() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"parentName":"p"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(ref reason))
            if reason == "parentName MUST only be included inside a clone operation in a delta update"
    ));
}

/// 通常トラックに parentNamespace が含まれると不正カタログとして拒否される
///
/// draft-ietf-moq-msf-02 §5.2.35 (Parent namespace): "This field MUST only be included
/// inside a clone operation in a delta update Section 5.1.6." `parentName` と対称。
#[test]
fn parent_namespace_in_track_rejected() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"parentNamespace":"ns"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(ref reason))
            if reason == "parentNamespace MUST only be included inside a clone operation in a delta update"
    ));
}
