use super::*;

#[test]
fn full_catalog_minimal() {
    let json = br#"{"version":"draft-01","tracks":[]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full が期待された");
    };
    assert_eq!(cat.version, "draft-01");
    assert_eq!(cat.generated_at, None);
    assert!(!cat.is_complete);
    assert!(cat.tracks.is_empty());
}

#[test]
fn full_catalog_with_all_root_fields() {
    let json =
        br#"{"version":"draft-01","generatedAt":1700000000000,"isComplete":true,"tracks":[]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full が期待された");
    };
    assert_eq!(cat.generated_at, Some(1_700_000_000_000));
    assert!(cat.is_complete);
}

#[test]
fn full_catalog_unknown_fields_ignored() {
    // パーサは未知フィールドを無視する (draft-ietf-moq-msf-01 §5 (Catalog))
    let json = br#"{"version":"draft-01","tracks":[],"unknownField":42,"another":"value"}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    assert!(matches!(doc, MsfCatalogDocument::Full(_)));
}

#[test]
fn unknown_packaging_rejected() {
    // draft-ietf-moq-msf-01 §5.2.4 (Packaging): 許容値は loc / mediatimeline / eventtimeline / moqlog / moqmetrics のみ
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"custom","isLive":true}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn moqlog_packaging_accepted() {
    // draft-ietf-moq-msf-01 §5.2.4 (Packaging): moqlog は許容値
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"moqlog","isLive":true}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full が期待された");
    };
    assert_eq!(cat.tracks[0].packaging, MsfPackaging::MoqLog);
}

#[test]
fn moqmetrics_packaging_accepted() {
    // draft-ietf-moq-msf-01 §5.2.4 (Packaging): moqmetrics は許容値
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"moqmetrics","isLive":true}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full が期待された");
    };
    assert_eq!(cat.tracks[0].packaging, MsfPackaging::MoqMetrics);
}

#[test]
fn delta_minimal() {
    let json = br#"{"deltaUpdate":[{"op":"remove","tracks":[{"name":"x"}]}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Delta(d) = doc else {
        panic!("Delta が期待された");
    };
    assert_eq!(d.generated_at, None);
    assert_eq!(d.operations.len(), 1);
    let MsfDeltaOperation::Remove { tracks } = &d.operations[0] else {
        panic!("Remove が期待された");
    };
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].name, "x");
}

#[test]
fn remove_tracks_extra_field_rejected() {
    // draft-ietf-moq-msf-01 §5.1.6 (Delta update): remove operation の track object に name/namespace 以外は禁止
    let json = br#"{"deltaUpdate":[{"op":"remove","tracks":[{"name":"x","packaging":"loc"}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn delta_update_non_array_rejected() {
    // draft-ietf-moq-msf-01 §5.1.6 (Delta update): deltaUpdate は operation object の配列
    let json = br#"{"deltaUpdate":false,"version":"draft-01","tracks":[]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn duplicate_track_name_rejected() {
    // draft-ietf-moq-msf-01 §5.2.3 (Track name): track name は namespace ごとに一意
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"audio","packaging":"loc","isLive":true},
            {"name":"audio","packaging":"loc","isLive":true}
        ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn same_name_different_namespace_allowed() {
    // 異なる namespace 内の同名トラックは許容される
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"audio","packaging":"loc","namespace":"ns1","isLive":true},
            {"name":"audio","packaging":"loc","namespace":"ns2","isLive":true}
        ]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    assert!(matches!(doc, MsfCatalogDocument::Full(_)));
}

#[test]
fn is_complete_false_rejected() {
    // draft-ietf-moq-msf-01 §5.1.3 (Is Complete): isComplete が FALSE ならフィールド自体を含めてはならない
    let json = br#"{"version":"draft-01","isComplete":false,"tracks":[]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn clone_track_minimal_rfc_example() {
    // draft-ietf-moq-msf-01 §5.1.6 (Delta update): packaging/isLive 省略の clone track
    let json = br#"{
            "deltaUpdate": [
                {
                    "op": "clone",
                    "tracks": [
                        {
                            "parentName": "video-1080",
                            "name": "video-720",
                            "width": 1280,
                            "height": 720,
                            "bitrate": 600000
                        }
                    ]
                }
            ]
        }"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Delta(d) = doc else {
        panic!("Delta が期待された");
    };
    assert_eq!(d.operations.len(), 1);
    let MsfDeltaOperation::Clone { tracks } = &d.operations[0] else {
        panic!("Clone が期待された");
    };
    assert_eq!(tracks.len(), 1);
    let ct = &tracks[0];
    assert_eq!(ct.name, "video-720");
    assert_eq!(ct.parent_name, "video-1080");
    assert!(ct.packaging.is_none());
    assert!(ct.is_live.is_none());
    assert_eq!(ct.width, Some(1280));
    assert_eq!(ct.height, Some(720));
    assert_eq!(ct.bitrate, Some(600000));
}

#[test]
fn clone_track_with_packaging_and_is_live() {
    // packaging/isLive を明示的に指定した clone track
    let json = br#"{
            "deltaUpdate": [
                {
                    "op": "clone",
                    "tracks": [
                        {
                            "parentName": "parent",
                            "name": "child",
                            "packaging": "loc",
                            "isLive": true
                        }
                    ]
                }
            ]
        }"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Delta(d) = doc else {
        panic!("Delta が期待された");
    };
    let MsfDeltaOperation::Clone { tracks } = &d.operations[0] else {
        panic!("Clone が期待された");
    };
    assert_eq!(tracks[0].packaging, Some(MsfPackaging::Loc));
    assert_eq!(tracks[0].is_live, Some(true));
}

#[test]
fn clone_track_missing_parent_name_rejected() {
    // clone operation 内で parentName がないとエラー
    let json = br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"x"}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// packaging=eventtimeline で eventType を省略した clone は受理されること
///
/// draft-ietf-moq-msf-01 §5.1.6 (Delta update): "The cloned track inherits all attributes
/// from the parent except the Track Name which MUST be new. Attributes redefined in the
/// track object override inherited values." 親が eventtimeline (親は §5.2.5 の必須により
/// eventType を持つ) ならば正当な継承表現であり、decode 時点では親の属性を知らないため
/// 必須違反と断定できない。逆に親が非 eventtimeline ならば実効トラックは §5.2.5 違反に
/// なり得るが、それも decode 時点では判定不能であり、delta update の適用時 (親解決後) の
/// 検証対象となる。
#[test]
fn clone_track_eventtimeline_without_event_type_accepted() {
    let json = br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"child","parentName":"parent","packaging":"eventtimeline"}]}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Delta(d) = doc else {
        panic!("Delta が期待された");
    };
    let MsfDeltaOperation::Clone { tracks } = &d.operations[0] else {
        panic!("Clone が期待された");
    };
    assert_eq!(tracks[0].packaging, Some(MsfPackaging::EventTimeline));
    assert_eq!(tracks[0].event_type, None);
}
