use super::*;

// ─── cmaf packaging (CMSF-01) ────────────────────────────────────────────────

/// inline の initDataList entry を作る
fn inline_init(id: &str) -> MsfInitData {
    MsfInitData {
        id: id.to_string(),
        kind: MsfInitDataKind::Inline,
        data: "AAEC".to_string(),
    }
}

/// cmaf の track を必須フィールドだけで作る
fn cmaf_track(name: &str, init_ref: &str) -> MsfTrack {
    let mut track = MsfTrack::new(name.to_string(), MsfPackaging::Cmaf, true);
    track.init_ref = Some(init_ref.to_string());
    track
}

#[test]
fn cmaf_packaging_str_roundtrip() {
    // draft-ietf-moq-cmsf-01 §3.5.1 (CMAF packaging type): cmaf を許容値として扱うこと
    let json = br#"{
            "version": "draft-02",
            "tracks": [{"name": "v", "packaging": "cmaf", "isLive": true, "initRef": "i1"}],
            "initDataList": [{"id": "i1", "type": "inline", "data": "AAEC"}]
        }"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("cmaf のカタログが decode できること");
    let MsfCatalogDocument::Full(catalog) = &doc else {
        panic!("完全カタログとして decode されること");
    };
    assert_eq!(catalog.tracks[0].packaging, MsfPackaging::Cmaf);

    // encode で "cmaf" がそのまま出力されること
    let encoded = doc
        .encode(shiguredo_moqt::msf::MsfCompression::None)
        .expect("cmaf のカタログが encode できること");
    let text = String::from_utf8(encoded).expect("encode の出力は UTF-8 であること");
    assert!(
        text.contains("\"packaging\":\"cmaf\""),
        "packaging が cmaf として出力されること: {text}"
    );
}

#[test]
fn cmaf_catalog_roundtrip_with_sap_starting_types() {
    // draft-ietf-moq-cmsf-01 §3.5.2 (Max SAP starting types): cmaf の track では
    // maxGrpSapStartingType / maxObjSapStartingType を往復できること
    let mut track = cmaf_track("v", "i1");
    track.max_grp_sap_starting_type = Some(2);
    track.max_obj_sap_starting_type = Some(3);
    let doc = full_doc(vec![track], Vec::new(), vec![inline_init("i1")]);

    let encoded = doc
        .encode(shiguredo_moqt::msf::MsfCompression::None)
        .expect("encode に成功すること");
    let decoded = MsfCatalogDocument::decode(&encoded, shiguredo_moqt::msf::MsfCompression::None)
        .expect("encode した cmaf のカタログが decode できること");
    let MsfCatalogDocument::Full(catalog) = &decoded else {
        panic!("完全カタログとして decode されること");
    };
    assert_eq!(catalog.tracks[0].max_grp_sap_starting_type, Some(2));
    assert_eq!(catalog.tracks[0].max_obj_sap_starting_type, Some(3));
    assert_eq!(decoded, doc, "cmaf のカタログが往復一致すること");
}

#[test]
fn cmsf_5_1_example_roundtrip() {
    // draft-ietf-moq-cmsf-01 §5.1 の例を、version を draft-02 にし、
    // 省略記号の data を有効な Base64 に置き換えて往復できること
    let json = br#"{
      "version": "draft-02",
      "generatedAt": 1746104606044,
      "tracks":[
        {
          "name": "hd",
          "renderGroup": 1,
          "packaging": "cmaf",
          "isLive": true,
          "targetLatency": 2000,
          "initRef": "init-hd",
          "role": "video",
          "codec":"avc1.640028",
          "width":1920,
          "height":1080,
          "bitrate":5000000,
          "framerate":30,
          "altGroup":1
        },
        {
          "name": "md",
          "renderGroup": 1,
          "packaging": "cmaf",
          "isLive": true,
          "targetLatency": 2000,
          "initRef": "init-md",
          "role": "video",
          "codec":"avc1.64001e",
          "width":720,
          "height":640,
          "bitrate":3000000,
          "framerate":30,
          "altGroup":1
        },
        {
          "name": "sd",
          "renderGroup": 1,
          "packaging": "cmaf",
          "isLive": true,
          "targetLatency": 2000,
          "initRef": "init-sd",
          "role": "video",
          "codec":"avc1.64000d",
          "width":192,
          "height":144,
          "bitrate":500000,
          "framerate":30,
          "altGroup":1
        },
        {
          "name": "audio",
          "renderGroup": 1,
          "packaging": "cmaf",
          "isLive": true,
          "targetLatency": 2000,
          "initRef": "init-audio",
          "role": "audio",
          "codec":"mp4a.40.5",
          "samplerate":48000,
          "channelConfig":"2",
          "bitrate":67071
        }
      ],
      "initDataList": [
        {"id": "init-hd", "type": "inline", "data": "AAEC"},
        {"id": "init-md", "type": "inline", "data": "AAED"},
        {"id": "init-sd", "type": "inline", "data": "AAEE"},
        {"id": "init-audio", "type": "inline", "data": "AAEF"}
      ]
    }"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("CMSF-01 §5.1 の例が decode できること");
    let encoded = doc
        .encode(shiguredo_moqt::msf::MsfCompression::None)
        .expect("CMSF-01 §5.1 の例が encode できること");
    let decoded = MsfCatalogDocument::decode(&encoded, shiguredo_moqt::msf::MsfCompression::None)
        .expect("encode した結果が decode できること");
    assert_eq!(decoded, doc, "CMSF-01 §5.1 の例が往復一致すること");
}

#[test]
fn cmaf_track_without_init_ref_rejected() {
    // draft-ietf-moq-cmsf-01 §3.1: cmaf の track は initRef 必須
    let json = br#"{
            "version": "draft-02",
            "tracks": [{"name": "v", "packaging": "cmaf", "isLive": true}]
        }"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn cmaf_publish_track_without_init_ref_rejected() {
    // draft-ietf-moq-cmsf-01 §3.1: publishTracks も同じ規則の対象
    let json = br#"{
            "version": "draft-02",
            "tracks": [],
            "publishTracks": [{"name": "v", "packaging": "cmaf", "isLive": true}],
            "initDataList": [{"id": "i1", "type": "inline", "data": "AAEC"}]
        }"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn cmaf_track_with_non_inline_init_ref_rejected() {
    // draft-ietf-moq-cmsf-01 §3.1: 参照先の initDataList entry は inline でなければならない
    for kind in ["track-property", "object-property"] {
        let json = format!(
            r#"{{
                "version": "draft-02",
                "tracks": [{{"name": "v", "packaging": "cmaf", "isLive": true, "initRef": "i1"}}],
                "initDataList": [{{"id": "i1", "type": "{kind}", "data": "0x1"}}]
            }}"#
        );
        assert!(
            matches!(
                MsfCatalogDocument::decode(
                    json.as_bytes(),
                    shiguredo_moqt::msf::MsfCompression::None
                ),
                Err(MessageError::InvalidCatalog(_))
            ),
            "cmaf の track が {kind} を参照したら拒否されること"
        );
    }
}

#[test]
fn cmaf_track_with_missing_init_ref_id_rejected() {
    // draft-ietf-moq-cmsf-01 §3.1: initRef は initDataList の id を指す
    let json = br#"{
            "version": "draft-02",
            "tracks": [{"name": "v", "packaging": "cmaf", "isLive": true, "initRef": "missing"}],
            "initDataList": [{"id": "i1", "type": "inline", "data": "AAEC"}]
        }"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn cmaf_max_grp_sap_starting_type_out_of_range_rejected() {
    // draft-ietf-moq-cmsf-01 §3.4: Group は SAP type 1 か 2 で始まるため、
    // maxGrpSapStartingType は 1〜2 に限る
    for value in [0, 3] {
        let json = format!(
            r#"{{
                "version": "draft-02",
                "tracks": [{{"name": "v", "packaging": "cmaf", "isLive": true,
                             "initRef": "i1", "maxGrpSapStartingType": {value}}}],
                "initDataList": [{{"id": "i1", "type": "inline", "data": "AAEC"}}]
            }}"#
        );
        assert!(
            matches!(
                MsfCatalogDocument::decode(
                    json.as_bytes(),
                    shiguredo_moqt::msf::MsfCompression::None
                ),
                Err(MessageError::InvalidCatalog(_))
            ),
            "maxGrpSapStartingType={value} が拒否されること"
        );
    }
}

#[test]
fn cmaf_max_obj_sap_starting_type_zero_rejected() {
    // draft-ietf-moq-cmsf-01 §3.5.2.2: track の Object には必ず Group 先頭の Object
    // (SAP type 1 か 2) が含まれるため、maxObjSapStartingType は 1 以上に限る
    let json = br#"{
            "version": "draft-02",
            "tracks": [{"name": "v", "packaging": "cmaf", "isLive": true,
                        "initRef": "i1", "maxObjSapStartingType": 0}],
            "initDataList": [{"id": "i1", "type": "inline", "data": "AAEC"}]
        }"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn cmaf_sap_starting_type_order_rejected() {
    // draft-ietf-moq-cmsf-01 §3.5.2: Group 先頭の Object は track の Object の一部なので、
    // maxObjSapStartingType は maxGrpSapStartingType を下回らない
    let json = br#"{
            "version": "draft-02",
            "tracks": [{"name": "v", "packaging": "cmaf", "isLive": true,
                        "initRef": "i1", "maxGrpSapStartingType": 2,
                        "maxObjSapStartingType": 1}],
            "initDataList": [{"id": "i1", "type": "inline", "data": "AAEC"}]
        }"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn cmaf_sap_starting_type_equal_accepted() {
    // maxObjSapStartingType == maxGrpSapStartingType は違反ではない
    let json = br#"{
            "version": "draft-02",
            "tracks": [{"name": "v", "packaging": "cmaf", "isLive": true,
                        "initRef": "i1", "maxGrpSapStartingType": 2,
                        "maxObjSapStartingType": 2}],
            "initDataList": [{"id": "i1", "type": "inline", "data": "AAEC"}]
        }"#;
    assert!(MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None).is_ok());
}

#[test]
fn non_cmaf_track_sap_starting_type_dropped_on_decode() {
    // draft-ietf-moq-msf-02 §5.2.4.1 (SHOULD NOT) / §5 の MUST ignore:
    // cmaf 以外の track の SAP starting type は未知フィールドとして捨てる
    let json = br#"{
            "version": "draft-02",
            "tracks": [{"name": "v", "packaging": "loc", "isLive": true,
                        "maxGrpSapStartingType": 2, "maxObjSapStartingType": 3}]
        }"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("cmaf 以外の SAP starting type は無視されて decode できること");
    let MsfCatalogDocument::Full(catalog) = &doc else {
        panic!("完全カタログとして decode されること");
    };
    assert_eq!(catalog.tracks[0].max_grp_sap_starting_type, None);
    assert_eq!(catalog.tracks[0].max_obj_sap_starting_type, None);

    // decode で捨てた結果を encode しても値が復活しないこと (往復が崩れないこと)
    let encoded = doc
        .encode(shiguredo_moqt::msf::MsfCompression::None)
        .expect("encode に成功すること");
    let text = String::from_utf8(encoded).expect("encode の出力は UTF-8 であること");
    assert!(
        !text.contains("maxGrpSapStartingType") && !text.contains("maxObjSapStartingType"),
        "cmaf 以外の track に SAP starting type が出力されないこと: {text}"
    );
}

#[test]
fn non_cmaf_track_sap_starting_type_with_wrong_type_dropped_on_decode() {
    // draft-ietf-moq-msf-02 §5 の MUST ignore: cmaf 以外の track にとってこのフィールドは
    // 未知フィールドである。値の型が数値でなくてもカタログ全体を拒否せず、真に未知の
    // フィールドと同じように捨てること。
    for value in ["\"2\"", "null", "true", "[2]", "{\"a\":1}"] {
        let json = format!(
            r#"{{
                "version": "draft-02",
                "tracks": [{{"name": "v", "packaging": "loc", "isLive": true,
                             "maxGrpSapStartingType": {value}}}]
            }}"#
        );
        let doc =
            MsfCatalogDocument::decode(json.as_bytes(), shiguredo_moqt::msf::MsfCompression::None)
                .unwrap_or_else(|error| {
                    panic!(
                        "cmaf 以外の未知フィールドは型を問わず捨てられること (値 {value}): {error}"
                    )
                });
        let MsfCatalogDocument::Full(catalog) = &doc else {
            panic!("完全カタログとして decode されること");
        };
        assert_eq!(catalog.tracks[0].max_grp_sap_starting_type, None);
    }
}

#[test]
fn cmaf_track_sap_starting_type_with_wrong_type_rejected_on_decode() {
    // cmaf の track ではこのフィールドを解釈するため、数値以外は受け付けない
    let json = br#"{
            "version": "draft-02",
            "tracks": [{"name": "v", "packaging": "cmaf", "isLive": true,
                        "initRef": "i1", "maxGrpSapStartingType": "2"}],
            "initDataList": [{"id": "i1", "type": "inline", "data": "AAEC"}]
        }"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn non_cmaf_track_sap_starting_type_rejected_on_encode() {
    // cmaf 以外の track に値があれば encode 前の検証で拒否する。
    // こうすると decode で捨てる規則と対称になり、往復が崩れない。
    let mut track = MsfTrack::new("v".to_string(), MsfPackaging::Loc, true);
    track.max_grp_sap_starting_type = Some(2);
    let doc = full_doc(vec![track], Vec::new(), Vec::new());
    assert!(matches!(
        doc.encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn delta_add_cmaf_track_without_init_ref_rejected_after_apply() {
    // delta で add した cmaf の track も、適用後のカタログで initRef が検証されること
    let json = br#"{
            "version": "draft-02",
            "tracks": [],
            "initDataList": []
        }"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(mut catalog) = doc else {
        panic!("完全カタログとして decode されること");
    };
    let before = catalog.clone();
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![cmaf_track("v", "missing")],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
    // 適用後検証で失敗したら、追加した track と generatedAt が巻き戻ること
    assert_eq!(catalog, before, "失敗した delta はカタログを変更しないこと");
}

#[test]
fn delta_add_cmaf_track_with_non_inline_init_ref_rejected_after_apply() {
    // delta で add した cmaf の track が、inline 以外の entry を参照したら拒否されること
    let json = br#"{
            "version": "draft-02",
            "tracks": [],
            "initDataList": [{"id": "tp", "type": "track-property", "data": "0x1"}]
        }"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(mut catalog) = doc else {
        panic!("完全カタログとして decode されること");
    };
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![cmaf_track("v", "tp")],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn delta_add_cmaf_track_with_inline_init_ref_accepted() {
    // 正しい cmaf の track を delta で add できること
    let json = br#"{
            "version": "draft-02",
            "tracks": [],
            "initDataList": [{"id": "i1", "type": "inline", "data": "AAEC"}]
        }"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(mut catalog) = doc else {
        panic!("完全カタログとして decode されること");
    };
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![cmaf_track("v", "i1")],
        }],
    };
    catalog
        .apply_delta(&delta, None)
        .expect("cmaf の track を delta で add できること");
    assert_eq!(catalog.tracks.len(), 1);
    assert_eq!(catalog.tracks[0].packaging, MsfPackaging::Cmaf);
}

#[test]
fn clone_without_packaging_inherits_sap_starting_types_from_cmaf_parent() {
    // packaging を省略した clone は SAP starting type を保持し、cmaf の親から継承できること
    let mut parent = cmaf_track("v", "i1");
    parent.max_grp_sap_starting_type = Some(2);
    parent.max_obj_sap_starting_type = Some(3);

    // MsfCloneTrack::new は packaging を None で作るため、省略した clone になる
    let fragment = MsfCloneTrack::new("v2".to_string(), "v".to_string());
    let resolved = fragment
        .into_track(&parent)
        .expect("cmaf の親から SAP starting type を継承できること");
    assert_eq!(resolved.max_grp_sap_starting_type, Some(2));
    assert_eq!(resolved.max_obj_sap_starting_type, Some(3));
}

#[test]
fn clone_cmaf_parent_as_loc_drops_sap_starting_types() {
    // cmaf の親を packaging: "loc" で clone した delta が受理され、
    // 結果の track から SAP starting type が落ちていること
    let mut parent = cmaf_track("v", "i1");
    parent.max_grp_sap_starting_type = Some(2);
    parent.max_obj_sap_starting_type = Some(3);

    let mut fragment = MsfCloneTrack::new("v2".to_string(), "v".to_string());
    fragment.packaging = Some(MsfPackaging::Loc);
    let resolved = fragment
        .into_track(&parent)
        .expect("cmaf の親を loc で clone できること");
    assert_eq!(resolved.packaging, MsfPackaging::Loc);
    assert_eq!(resolved.max_grp_sap_starting_type, None);
    assert_eq!(resolved.max_obj_sap_starting_type, None);
}

#[test]
fn clone_without_packaging_breaking_order_rejected() {
    // 継承の解決後に大小関係が崩れる delta が拒否されること。
    // cmaf の親 (maxObjSapStartingType: 1) を packaging 省略で
    // maxGrpSapStartingType: 2 と clone すると 2 > 1 になる。
    let mut parent = cmaf_track("v", "i1");
    parent.max_obj_sap_starting_type = Some(1);

    let mut fragment = MsfCloneTrack::new("v2".to_string(), "v".to_string());
    fragment.max_grp_sap_starting_type = Some(2);
    assert!(matches!(
        fragment.into_track(&parent),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn clone_explicit_non_cmaf_packaging_drops_sap_starting_types() {
    // packaging を cmaf 以外と明示した clone の断片では、値が未知フィールドとして
    // 捨てられること (decode 側の挙動)。delta update は root field を持たないため、
    // 単独のドキュメントとして decode する。
    let json = br#"{
            "generatedAt": 1,
            "deltaUpdate": [{
                "op": "clone",
                "tracks": [{
                    "name": "v2", "parentName": "v", "packaging": "loc",
                    "maxGrpSapStartingType": 3
                }]
            }]
        }"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("cmaf 以外を明示した断片の SAP starting type は捨てられること");
    let MsfCatalogDocument::Delta(delta) = &doc else {
        panic!("delta update として decode されること");
    };
    let MsfDeltaOperation::Clone { tracks } = &delta.operations[0] else {
        panic!("clone 操作がデコードされること");
    };
    assert_eq!(tracks[0].max_grp_sap_starting_type, None);
}

#[test]
fn clone_explicit_cmaf_with_out_of_range_value_rejected() {
    // packaging を cmaf と明示した clone の断片で、値域外の値 (maxGrpSapStartingType: 3) が
    // decode の時点で拒否されること
    let json = br#"{
            "generatedAt": 1,
            "deltaUpdate": [{
                "op": "clone",
                "tracks": [{
                    "name": "v2", "parentName": "v", "packaging": "cmaf",
                    "maxGrpSapStartingType": 3
                }]
            }]
        }"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn clone_without_packaging_out_of_range_value_inherits_from_loc_parent_and_dropped() {
    // packaging を省略した clone で値域外の値を持つ場合、親が loc なら受理され値が捨てられること
    let parent = MsfTrack::new("v".to_string(), MsfPackaging::Loc, true);

    let mut fragment = MsfCloneTrack::new("v2".to_string(), "v".to_string());
    fragment.max_grp_sap_starting_type = Some(3);
    let resolved = fragment
        .into_track(&parent)
        .expect("親が loc なら値域外の値も受理されて捨てられること");
    assert_eq!(resolved.max_grp_sap_starting_type, None);
}

#[test]
fn clone_without_packaging_out_of_range_value_with_cmaf_parent_rejected() {
    // packaging を省略した clone で値域外の値を持つ場合、親が cmaf なら
    // 継承の解決後に拒否されること
    let parent = cmaf_track("v", "i1");

    let mut fragment = MsfCloneTrack::new("v2".to_string(), "v".to_string());
    fragment.max_grp_sap_starting_type = Some(3);
    assert!(matches!(
        fragment.into_track(&parent),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn clone_explicit_non_cmaf_with_value_rejected_on_encode() {
    // packaging を cmaf 以外と明示した断片に値があれば、encode 前の検証で拒否すること
    let mut fragment = MsfCloneTrack::new("v2".to_string(), "v".to_string());
    fragment.packaging = Some(MsfPackaging::Loc);
    fragment.max_grp_sap_starting_type = Some(2);
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![fragment],
        }],
    });
    assert!(matches!(
        doc.encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn update_merge_drops_sap_starting_types_when_packaging_changes_from_cmaf() {
    // cmaf の親を packaging: "loc" で update しても、マージ結果から値が落ちること
    let mut parent = cmaf_track("v", "i1");
    parent.max_grp_sap_starting_type = Some(2);
    parent.max_obj_sap_starting_type = Some(3);

    let mut fragment = MsfCloneTrack::new("v".to_string(), "v".to_string());
    fragment.packaging = Some(MsfPackaging::Loc);
    let resolved = fragment
        .into_track(&parent)
        .expect("cmaf の親を loc で update できること");
    assert_eq!(resolved.max_grp_sap_starting_type, None);
    assert_eq!(resolved.max_obj_sap_starting_type, None);
}

#[test]
fn cmaf_track_without_media_fields_accepted() {
    // draft-ietf-moq-msf-02 §5.2.4.1 (Table 6) は cmaf の行を定めないため、
    // Table 6 由来の追加必須フィールド (framerate / lang) は cmaf に課さない。
    // role / codec / mimeType から導く条件付き必須は従来どおり課す。
    let mut track = cmaf_track("v", "i1");
    track.role = Some("video".to_string());
    track.codec = Some("avc1.640028".to_string());
    track.bitrate = Some(5_000_000);
    // framerate を省略しても受理されること (cmaf は loc の Table 6 の対象外)
    let doc = full_doc(vec![track], Vec::new(), vec![inline_init("i1")]);
    doc.encode(shiguredo_moqt::msf::MsfCompression::None)
        .expect("cmaf の video は framerate なしでも encode できること");
}

#[test]
fn cmaf_track_without_codec_rejected() {
    // role / codec / mimeType から導く条件付き必須は cmaf でも課す
    let mut track = cmaf_track("v", "i1");
    track.role = Some("video".to_string());
    let doc = full_doc(vec![track], Vec::new(), vec![inline_init("i1")]);
    assert!(matches!(
        doc.encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn delta_clone_to_cmaf_with_unresolvable_init_ref_rejected_after_apply() {
    // draft-ietf-moq-cmsf-01 §3.1: cmaf の track は initRef を必要とし、参照先は inline。
    // clone の継承結果が cmaf になる場合も、差し替えた initRef の解決は
    // 適用後検証 (validate_after_delta) でだけ判定できる。
    // loc の親を cmaf と明示して clone し、存在しない initRef を指定する。
    let parent = MsfTrack::new("v".to_string(), MsfPackaging::Loc, true);
    let doc = full_doc(vec![parent], Vec::new(), vec![inline_init("i1")]);
    let MsfCatalogDocument::Full(mut catalog) = doc else {
        panic!("完全カタログとして decode されること");
    };
    let before = catalog.clone();

    let mut fragment = MsfCloneTrack::new("v2".to_string(), "v".to_string());
    fragment.packaging = Some(MsfPackaging::Cmaf);
    fragment.init_ref = Some("missing".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![fragment],
        }],
    };
    assert!(
        matches!(
            catalog.apply_delta(&delta, None),
            Err(MessageError::InvalidCatalog(_))
        ),
        "clone で cmaf になった track の initRef 解決失敗は適用後に検出されること"
    );
    assert_eq!(catalog, before, "失敗した delta はカタログを変更しないこと");
}

#[test]
fn delta_clone_to_cmaf_with_non_inline_init_ref_rejected_after_apply() {
    // draft-ietf-moq-cmsf-01 §3.1: 参照先の initDataList entry は inline でなければならない。
    // clone で cmaf になった track が track-property を参照したら適用後に拒否されること。
    let parent = MsfTrack::new("v".to_string(), MsfPackaging::Loc, true);
    let property_init = MsfInitData {
        id: "tp".to_string(),
        kind: MsfInitDataKind::TrackProperty,
        data: "0x1".to_string(),
    };
    let doc = full_doc(vec![parent], Vec::new(), vec![property_init]);
    let MsfCatalogDocument::Full(mut catalog) = doc else {
        panic!("完全カタログとして decode されること");
    };
    let before = catalog.clone();

    let mut fragment = MsfCloneTrack::new("v2".to_string(), "v".to_string());
    fragment.packaging = Some(MsfPackaging::Cmaf);
    fragment.init_ref = Some("tp".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![fragment],
        }],
    };
    assert!(
        matches!(
            catalog.apply_delta(&delta, None),
            Err(MessageError::InvalidCatalog(_))
        ),
        "clone で cmaf になった track の inline 以外の参照は適用後に検出されること"
    );
    assert_eq!(catalog, before, "失敗した delta はカタログを変更しないこと");
}

#[test]
fn delta_update_to_cmaf_inheriting_loc_init_ref_rejected_after_apply() {
    // loc の track を update で cmaf に変えると、親の initRef を継承する。
    // 継承した initRef が解決できない場合、適用後検証で拒否されること。
    let parent = MsfTrack::new("v".to_string(), MsfPackaging::Loc, true);
    let doc = full_doc(vec![parent], Vec::new(), Vec::new());
    let MsfCatalogDocument::Full(mut catalog) = doc else {
        panic!("完全カタログとして decode されること");
    };
    let before = catalog.clone();

    let mut fragment = MsfCloneTrack::new("v".to_string(), "v".to_string());
    fragment.packaging = Some(MsfPackaging::Cmaf);
    // initRef は指定しない。親 (loc) の initRef は None なので継承しても None のまま。
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Update {
            tracks: vec![MsfUpdateTrack { track: fragment }],
        }],
    };
    assert!(
        matches!(
            catalog.apply_delta(&delta, None),
            Err(MessageError::InvalidCatalog(_))
        ),
        "update で cmaf になった track の initRef 欠如は適用後に検出されること"
    );
    assert_eq!(catalog, before, "失敗した delta はカタログを変更しないこと");
}

#[test]
fn clone_fragment_explicit_cmaf_with_out_of_range_value_rejected_on_encode() {
    // draft-ietf-moq-cmsf-01 §3.5.2: packaging を cmaf と明示した clone の断片で、
    // 値域外の値は encode の時点で拒否されること。decode 側は
    // clone_explicit_cmaf_with_out_of_range_value_rejected が固定している。
    let mut fragment = MsfCloneTrack::new("v2".to_string(), "v".to_string());
    fragment.packaging = Some(MsfPackaging::Cmaf);
    fragment.max_grp_sap_starting_type = Some(3);
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![fragment],
        }],
    });
    assert!(
        matches!(
            doc.encode(shiguredo_moqt::msf::MsfCompression::None),
            Err(MessageError::InvalidCatalog(_))
        ),
        "cmaf を明示した clone 断片の値域外の値は encode で拒否されること"
    );
}

#[test]
fn clone_fragment_without_packaging_keeps_sap_starting_types_on_decode() {
    // packaging を省略した clone 断片は、親が cmaf かどうか分からないため
    // SAP starting type を保持すること (継承の解決まで判断を保留する)。
    let json = br#"{
            "generatedAt": 1,
            "deltaUpdate": [{
                "op": "clone",
                "tracks": [{
                    "name": "v2", "parentName": "v",
                    "maxGrpSapStartingType": 2, "maxObjSapStartingType": 3
                }]
            }]
        }"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("packaging 省略の clone 断片が decode できること");
    let MsfCatalogDocument::Delta(delta) = &doc else {
        panic!("delta update として decode されること");
    };
    let MsfDeltaOperation::Clone { tracks } = &delta.operations[0] else {
        panic!("clone 操作がデコードされること");
    };
    assert_eq!(tracks[0].max_grp_sap_starting_type, Some(2));
    assert_eq!(tracks[0].max_obj_sap_starting_type, Some(3));

    // encode しても値が保持されること
    let encoded = doc
        .encode(shiguredo_moqt::msf::MsfCompression::None)
        .expect("packaging 省略の clone 断片が encode できること");
    let text = String::from_utf8(encoded).expect("encode の出力は UTF-8 であること");
    assert!(
        text.contains("maxGrpSapStartingType") && text.contains("maxObjSapStartingType"),
        "packaging 省略時は SAP starting type が出力されること: {text}"
    );
}
