use super::*;

#[test]
fn missing_version() {
    assert!(matches!(
        MsfCatalogDocument::decode(b"{\"tracks\":[]}"),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn version_zero() {
    assert!(matches!(
        MsfCatalogDocument::decode(b"{\"version\":\"0\",\"tracks\":[]}"),
        Err(MessageError::InvalidCatalogVersion(v)) if v == "0"
    ));
}

#[test]
fn version_99() {
    assert!(matches!(
        MsfCatalogDocument::decode(b"{\"version\":\"99\",\"tracks\":[]}"),
        Err(MessageError::InvalidCatalogVersion(v)) if v == "99"
    ));
}

#[test]
fn version_not_string() {
    // draft-ietf-moq-msf-01 §5.1.1 (MSF version): version は JSON Type: String
    assert!(matches!(
        MsfCatalogDocument::decode(b"{\"version\":1,\"tracks\":[]}"),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn missing_tracks() {
    assert!(matches!(
        MsfCatalogDocument::decode(b"{\"version\":\"draft-01\"}"),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn track_missing_name() {
    let json = br#"{"version":"draft-01","tracks":[{"packaging":"loc"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn track_missing_packaging() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","isLive":true}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn track_missing_is_live() {
    // draft-ietf-moq-msf-01 §5.2.7 (Is Live): isLive は必須
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn not_json_object() {
    assert!(matches!(
        MsfCatalogDocument::decode(b"[1,2,3]"),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn empty_input() {
    assert!(matches!(
        MsfCatalogDocument::decode(b""),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn trailing_data() {
    assert!(matches!(
        MsfCatalogDocument::decode(b"{\"version\":1,\"tracks\":[]}garbage"),
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
        MsfEventTimeline::decode(br#"[{"t":1000}]"#),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn depends_entry_not_string() {
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"depends":[1,2]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn render_group_different_target_latency_rejected() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency): 同一 renderGroup 内の isLive=true トラックの targetLatency は一致必須
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":500},
            {"name":"b","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":1000}
        ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn alt_group_different_target_latency_rejected() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency): 同一 altGroup 内の isLive=true トラックの targetLatency は一致必須
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"altGroup":2,"targetLatency":500},
            {"name":"b","packaging":"loc","isLive":true,"altGroup":2,"targetLatency":1000}
        ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn render_group_same_target_latency_accepted() {
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":500},
            {"name":"b","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":500}
        ]}"#;
    assert!(MsfCatalogDocument::decode(json).is_ok());
}

// ─── lang BCP 47 軽量バリデーション ───────────────────────────────────────────

#[test]
fn lang_invalid_empty() {
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":""}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// primary language subtag が ASCII 英字でない場合 (数字) は拒否されることを確認する
#[test]
fn lang_invalid_numeric_primary() {
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"12"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// primary language subtag が 1 文字の場合は拒否されることを確認する
#[test]
fn lang_invalid_single_char() {
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"e"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn lang_invalid_special_chars() {
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"eng-!!"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn lang_invalid_leading_dash() {
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"-en"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn lang_invalid_trailing_dash() {
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"en-"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// RFC 5646 §2.1 (Syntax): language = 4ALPHA も 5*8ALPHA も正当な primary のため受理される
#[test]
fn lang_primary_four_to_eight_letters_accepted() {
    for lang in ["abcd", "abcde", "abcdefgh"] {
        let json = format!(
            r#"{{"version":"draft-01","tracks":[{{"name":"a","packaging":"loc","isLive":true,"lang":"{lang}"}}]}}"#
        );
        assert!(
            MsfCatalogDocument::decode(json.as_bytes()).is_ok(),
            "primary '{lang}' は受理されること"
        );
    }
}

/// RFC 5646 §2.1 (Syntax): primary が 9 文字以上なら拒否されること
#[test]
fn lang_invalid_primary_too_long() {
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"abcdefghi"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// RFC 5646 §2.1 (Syntax): privateuse tag ("x" + 1 個以上のサブタグ) は受理されること
#[test]
fn lang_privateuse_accepted() {
    for lang in ["x-foo", "x-foo-bar", "x-1"] {
        let json = format!(
            r#"{{"version":"draft-01","tracks":[{{"name":"a","packaging":"loc","isLive":true,"lang":"{lang}"}}]}}"#
        );
        assert!(
            MsfCatalogDocument::decode(json.as_bytes()).is_ok(),
            "privateuse '{lang}' は受理されること"
        );
    }
}

/// RFC 5646 §2.1 (Syntax): privateuse は最低 1 個のサブタグを必要とするため "x" 単独は拒否される
#[test]
fn lang_privateuse_without_subtag_rejected() {
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"x"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// add operation 経路でも lang バリデーションが発火することを確認する
#[test]
fn lang_invalid_in_add_tracks() {
    let json = br#"{"deltaUpdate":[{"op":"add","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"1"}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// clone operation 内の不正な lang も拒否されることを確認する
#[test]
fn lang_invalid_in_clone_track() {
    let json =
        br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"a","parentName":"parent","lang":"1"}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

// ─── targetLatency / buffers の相互制約 ───────────────────────────────────────

#[test]
fn target_latency_and_buffers_together_rejected_when_is_live_true() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency) / §5.2.9 (Buffers):
    // isLive=true の同一 track に targetLatency と buffers は共存できない
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"targetLatency":500,"buffers":{"target":1000}}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(reason))
        if reason == "targetLatency and buffers MUST NOT be present together"
    ));
}

#[test]
fn target_latency_and_buffers_together_rejected_when_is_live_false() {
    // draft-ietf-moq-msf-01 §5.2.8 / §5.2.9: isLive=false でも共存は reject されること
    // (本実装では presence 違反を優先する選択とする)
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":false,"targetLatency":500,"buffers":{"target":1000}}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(reason))
        if reason == "targetLatency and buffers MUST NOT be present together"
    ));
}

#[test]
fn target_latency_or_buffers_alone_ignored_when_is_live_false() {
    // draft-ietf-moq-msf-01 §5.2.8 / §5.2.9: 単独存在時は isLive=false で無視されること
    // (両単独とも確認する)
    for (name, json) in [
        (
            "targetLatency 単独",
            br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":false,"targetLatency":500}]}"#.as_slice(),
        ),
        (
            "buffers 単独",
            br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":false,"buffers":{"target":1000}}]}"#.as_slice(),
        ),
    ] {
        let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
        let MsfCatalogDocument::Full(cat) = doc else {
            panic!("Full catalog がデコードされること");
        };
        assert_eq!(cat.tracks[0].target_latency, None, "{name} は無視されること");
        assert_eq!(cat.tracks[0].buffers, None, "{name} は無視されること");
    }
}

#[test]
fn clone_target_latency_or_buffers_alone_ignored_when_is_live_false() {
    // draft-ietf-moq-msf-01 §5.2.8 / §5.2.9: clone の単独存在時も isLive=false で無視されること
    // (両単独とも確認する)
    for (name, json, field) in [
        (
            "targetLatency 単独",
            br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","isLive":false,"targetLatency":500}]}]}"#.as_slice(),
            "targetLatency",
        ),
        (
            "buffers 単独",
            br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","isLive":false,"buffers":{"target":1000}}]}]}"#.as_slice(),
            "buffers",
        ),
    ] {
        let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
        let MsfCatalogDocument::Delta(delta) = doc else {
            panic!("Delta update がデコードされること");
        };
        let MsfDeltaOperation::Clone { tracks } = &delta.operations[0] else {
            panic!("Clone 操作がデコードされること");
        };
        // 存在する側のみ無視を確認する (存在しない側の断定は空振りのため行わない)
        match field {
            "targetLatency" => assert_eq!(tracks[0].target_latency, None, "{name} は無視されること"),
            _ => assert_eq!(tracks[0].buffers, None, "{name} は無視されること"),
        }
    }
}

#[test]
fn target_latency_and_buffers_together_rejected_in_clone() {
    // draft-ietf-moq-msf-01 §5.2.8 / §5.2.9: clone でも共存は reject されること
    // (isLive 省略時を含む。理由文字列まで固定する)
    for (name, json) in [
        (
            "isLive:false",
            br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","isLive":false,"targetLatency":500,"buffers":{"target":1000}}]}]}"#.as_slice(),
        ),
        (
            "isLive 省略",
            br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","targetLatency":500,"buffers":{"target":1000}}]}]}"#.as_slice(),
        ),
    ] {
        assert!(
            matches!(
                MsfCatalogDocument::decode(json),
                Err(MessageError::InvalidCatalog(reason))
                if reason == "targetLatency and buffers MUST NOT be present together"
            ),
            "{name} の共存は reject されること"
        );
    }
}

#[test]
fn render_group_different_buffers_rejected() {
    // draft-ietf-moq-msf-01 §5.2.9 (Buffers): 同一 renderGroup 内の isLive=true トラックの buffers は一致必須
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"buffers":{"target":500}},
            {"name":"b","packaging":"loc","isLive":true,"renderGroup":1,"buffers":{"target":1000}}
        ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn render_group_same_buffers_accepted() {
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"buffers":{"target":500}},
            {"name":"b","packaging":"loc","isLive":true,"renderGroup":1,"buffers":{"target":500}}
        ]}"#;
    assert!(MsfCatalogDocument::decode(json).is_ok());
}

#[test]
fn render_group_mixed_live_ignored_for_buffers() {
    // isLive=false のトラックは group 検証の対象外
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"buffers":{"target":500}},
            {"name":"b","packaging":"loc","isLive":false,"renderGroup":1}
        ]}"#;
    assert!(MsfCatalogDocument::decode(json).is_ok());
}

#[test]
fn render_group_mixed_live_ignored_for_target_latency() {
    // isLive=false のトラックは group 検証の対象外
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":500},
            {"name":"b","packaging":"loc","isLive":false,"renderGroup":1}
        ]}"#;
    assert!(MsfCatalogDocument::decode(json).is_ok());
}

// ─── initDataList ─────────────────────────────────────────────────────────────

#[test]
fn init_data_list_duplicate_id_rejected() {
    // draft-ietf-moq-msf-01 §5.1.7 (Initialization Data List): id は catalog 内で一意
    let json = br#"{"version":"draft-01","tracks":[],"initDataList":[
            {"id":"init1","type":"inline","data":"AAEC"},
            {"id":"init1","type":"inline","data":"AAED"}
        ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn init_data_list_unknown_type_rejected() {
    let json = br#"{"version":"draft-01","tracks":[],"initDataList":[{"id":"init1","type":"unknown","data":"AAEC"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn init_ref_missing_id_rejected() {
    // draft-ietf-moq-msf-01 §5.2.13 (Initialization reference): initRef は
    // initDataList の id を指すため、一致する id が無ければ reject される
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"initRef":"missing"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn init_ref_matching_id_accepted() {
    // initRef が initDataList の id と一致すれば受理される
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"initRef":"init1"}],"initDataList":[{"id":"init1","type":"inline","data":"AAEC"}]}"#;
    assert!(MsfCatalogDocument::decode(json).is_ok());
}

// ─── delta update 新構造 ──────────────────────────────────────────────────────

#[test]
fn delta_update_empty_array_rejected() {
    // draft-ietf-moq-msf-01 §5.3 (Delta updates): 少なくとも 1 つの操作が必要
    let json = br#"{"deltaUpdate":[]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn delta_update_unknown_op_rejected() {
    let json = br#"{"deltaUpdate":[{"op":"unknown","tracks":[]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn delta_update_contains_version_rejected() {
    // draft-ietf-moq-msf-01 §5.3 (Delta updates): delta update に version は含めない
    let json = br#"{"deltaUpdate":[{"op":"add","tracks":[]}],"version":"draft-01"}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn delta_update_contains_tracks_rejected() {
    // draft-ietf-moq-msf-01 §5.3 (Delta updates): delta update に tracks は含めない
    let json = br#"{"deltaUpdate":[{"op":"add","tracks":[]}],"tracks":[]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn delta_operation_missing_op_rejected() {
    let json = br#"{"deltaUpdate":[{"tracks":[]}]}}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn delta_operation_missing_tracks_rejected() {
    let json = br#"{"deltaUpdate":[{"op":"add"}]}}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn init_data_list_missing_id_rejected() {
    let json =
        br#"{"version":"draft-01","tracks":[],"initDataList":[{"type":"inline","data":"AAEC"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn init_data_list_missing_type_rejected() {
    let json =
        br#"{"version":"draft-01","tracks":[],"initDataList":[{"id":"init1","data":"AAEC"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn init_data_list_missing_data_rejected() {
    let json =
        br#"{"version":"draft-01","tracks":[],"initDataList":[{"id":"init1","type":"inline"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn buffers_non_number_value_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"buffers":{"target":"fast"}}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn alt_group_different_buffers_rejected() {
    // draft-ietf-moq-msf-01 §5.2.9 (Buffers): 同一 altGroup 内の isLive=true トラックの buffers は一致必須
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"altGroup":1,"buffers":{"target":500}},
            {"name":"b","packaging":"loc","isLive":true,"altGroup":1,"buffers":{"target":1000}}
        ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn clone_track_target_latency_and_buffers_together_rejected() {
    // draft-ietf-moq-msf-01 §5.2.8 / §5.2.9: clone の isLive:true 共存も reject されること
    let json = br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","isLive":true,"targetLatency":500,"buffers":{"target":1000}}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(reason))
        if reason == "targetLatency and buffers MUST NOT be present together"
    ));
}

#[test]
fn add_target_latency_and_buffers_together_rejected() {
    // draft-ietf-moq-msf-01 §5.2.8 / §5.2.9: delta add 経路の共存も reject されること
    let json = br#"{"deltaUpdate":[{"op":"add","tracks":[{"name":"t","packaging":"loc","isLive":false,"targetLatency":500,"buffers":{"target":1000}}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(reason))
        if reason == "targetLatency and buffers MUST NOT be present together"
    ));
}

#[test]
fn clone_omitted_kept() {
    // clone の isLive 省略時は単独値を保持すること
    let json = br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","targetLatency":500}]}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
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
    let json = br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","isLive":true,"trackDuration":1000}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn clone_track_event_type_with_non_eventtimeline_rejected() {
    let json = br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","packaging":"loc","eventType":"x"}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn event_timeline_missing_event_type_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"eventtimeline","isLive":true}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn media_timeline_missing_depends_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"mediatimeline","isLive":true,"mimeType":"application/json"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn media_timeline_wrong_mime_type_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"mediatimeline","isLive":true,"depends":["video"],"mimeType":"text/plain"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn event_type_with_non_eventtimeline_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"eventType":"x"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn eventtimeline_missing_depends_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"eventtimeline","isLive":true,"eventType":"cue","mimeType":"application/json"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn eventtimeline_wrong_mime_type_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"eventtimeline","isLive":true,"eventType":"cue","depends":["video"],"mimeType":"text/plain"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

// ─── role に応じた必須フィールド ──────────────────────────────────────────────

/// role="video" は codec 必須 (draft-ietf-moq-msf-01 §5.2.18 (Codec))
#[test]
fn video_role_missing_codec_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"v","packaging":"loc","isLive":true,"role":"video","bitrate":1000}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// role="video" は bitrate 必須 (draft-ietf-moq-msf-01 §5.2.22 (Maximum Bitrate))
#[test]
fn video_role_missing_bitrate_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"v","packaging":"loc","isLive":true,"role":"video","codec":"av01"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// role="audio" は samplerate / channelConfig 必須 (§5.2.28 / §5.2.29)
#[test]
fn audio_role_missing_samplerate_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"role":"audio","codec":"opus","bitrate":32000,"channelConfig":"2"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
    let json = br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"role":"audio","codec":"opus","bitrate":32000,"samplerate":48000}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// role="video" / "audio" が必須フィールドを満たせば受理される
#[test]
fn media_role_with_required_fields_accepted() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"v","packaging":"loc","isLive":true,"role":"video","codec":"av01","bitrate":1000},{"name":"a","packaging":"loc","isLive":true,"role":"audio","codec":"opus","bitrate":32000,"samplerate":48000,"channelConfig":"2"}]}"#;
    assert!(MsfCatalogDocument::decode(json).is_ok());
}

/// role 省略時は codec / bitrate を要求しない (inherent codec を判定できないため)
#[test]
fn missing_role_does_not_require_media_fields() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true}]}"#;
    assert!(MsfCatalogDocument::decode(json).is_ok());
}

/// encode 時にも role に応じた必須フィールドが検証される
#[test]
fn media_role_missing_fields_rejected_on_encode() {
    let mut track = MsfTrack::new("v".to_string(), MsfPackaging::Loc, true);
    track.role = Some("video".to_string());
    track.codec = Some("av01".to_string());
    // bitrate を欠いたまま encode するとエラーになる
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        tracks: vec![track],
        ..MsfCatalog::new()
    });
    assert!(matches!(doc.encode(), Err(MessageError::InvalidCatalog(_))));
}

/// 通常トラックに parentName が含まれると不正カタログとして拒否される
///
/// draft-ietf-moq-msf-01 §5.2.33 (Parent name): "This field MUST only be included
/// inside a clone operation in a delta update Section 5.1.6."
#[test]
fn parent_name_in_track_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"parentName":"p"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(ref reason))
            if reason == "parentName MUST only be included inside a clone operation in a delta update"
    ));
}

/// 通常トラックに parentNamespace が含まれると不正カタログとして拒否される
///
/// draft-ietf-moq-msf-01 §5.2.34 (Parent namespace): "This field MUST only be included
/// inside a clone operation in a delta update Section 5.1.6." `parentName` と対称。
#[test]
fn parent_namespace_in_track_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"parentNamespace":"ns"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(ref reason))
            if reason == "parentNamespace MUST only be included inside a clone operation in a delta update"
    ));
}

// ─── framerate の非有限値拒否 ─────────────────────────────────────────────────

/// framerate に infinity (1e400 は f64 で infinity に丸められる) を指定すると拒否される
#[test]
fn framerate_infinity_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"framerate":1e400}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

// ─── authInfo value_raw の encode 時検証 ──────────────────────────────────────

/// 手組みの非 UTF-8 value_raw は encode で拒否され、panic しない
///
/// draft-ietf-moq-msf-01 §5.2.42 (Authorization Info)
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
        init_data_list: Vec::new(),
    });
    assert!(
        matches!(doc.encode(), Err(MessageError::InvalidCatalog(_))),
        "非 UTF-8 の value_raw は InvalidCatalog で拒否されること"
    );
}

/// publishTracks 側の手組み不正 value_raw も encode で拒否され、panic しない
///
/// draft-ietf-moq-msf-01 §5.1.5 (Publish tracks) / §5.2.42 (Authorization Info)
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
        init_data_list: Vec::new(),
    });
    assert!(
        matches!(doc.encode(), Err(MessageError::InvalidCatalog(_))),
        "publishTracks の非 UTF-8 value_raw は InvalidCatalog で拒否されること"
    );
}

/// 手組みの非 JSON 値 value_raw は encode で拒否される
///
/// draft-ietf-moq-msf-01 §5.2.42 (Authorization Info): 値は単独の JSON 値
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
        init_data_list: Vec::new(),
    });
    assert!(
        matches!(doc.encode(), Err(MessageError::InvalidCatalog(_))),
        "単独の JSON 値でない value_raw は InvalidCatalog で拒否されること"
    );
}

// ─── encode 時の完全カタログ検証 (decode_full_catalog と同等) ──────────────────

/// テスト用の Full カタログドキュメントを組み立てる
fn full_doc(
    tracks: Vec<MsfTrack>,
    publish_tracks: Vec<MsfTrack>,
    init_data_list: Vec<MsfInitData>,
) -> MsfCatalogDocument {
    MsfCatalogDocument::Full(MsfCatalog {
        version: MSF_VERSION.to_string(),
        generated_at: None,
        is_complete: false,
        tracks,
        publish_tracks,
        init_data_list,
    })
}

/// tracks 内の (namespace, name) 重複は encode でも拒否されること
#[test]
fn encode_full_duplicate_name_within_tracks_rejected() {
    // draft-ietf-moq-msf-01 §5.2.3 (Track name)
    let a = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    let b = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    assert!(matches!(
        full_doc(vec![a, b], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// tracks と publishTracks をまたぐ (namespace, name) 重複は encode でも拒否されること
#[test]
fn encode_full_duplicate_name_across_tracks_and_publish_tracks_rejected() {
    // draft-ietf-moq-msf-01 §5.2.3 (Track name): "Within the catalog"
    let a = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    let b = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    assert!(matches!(
        full_doc(vec![a], vec![b], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// initDataList の id 重複は encode でも拒否されること
#[test]
fn encode_full_duplicate_init_data_list_id_rejected() {
    // draft-ietf-moq-msf-01 §5.1.7 (Initialization Data List)
    let entry = || MsfInitData {
        id: "init1".to_string(),
        kind: MsfInitDataKind::Inline,
        data: "AAEC".to_string(),
    };
    assert!(matches!(
        full_doc(vec![], vec![], vec![entry(), entry()]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// 一致しない initRef は encode でも拒否されること
#[test]
fn encode_full_missing_init_ref_rejected() {
    // draft-ietf-moq-msf-01 §5.2.13 (Initialization reference)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.init_ref = Some("missing".to_string());
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// targetLatency と buffers の共存は encode でも拒否されること
#[test]
fn encode_full_target_latency_and_buffers_together_rejected() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency) / §5.2.9 (Buffers)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.target_latency = Some(500);
    track.buffers = Some(MsfBuffers {
        target: Some(1000),
        min: None,
        max: None,
    });
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(reason))
            if reason == "targetLatency and buffers MUST NOT be present together"
    ));
}

/// 同一 renderGroup の targetLatency 不一致は encode でも拒否されること
#[test]
fn encode_full_group_target_latency_mismatch_rejected() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency)
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.render_group = Some(1);
    a.target_latency = Some(500);
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.render_group = Some(1);
    b.target_latency = Some(1000);
    assert!(matches!(
        full_doc(vec![a, b], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// publishTracks 内の同一 altGroup の buffers 不一致は encode でも拒否されること
#[test]
fn encode_full_publish_tracks_group_buffers_mismatch_rejected() {
    // draft-ietf-moq-msf-01 §5.2.9 (Buffers)
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::MoqLog, true);
    a.alt_group = Some(1);
    a.buffers = Some(MsfBuffers {
        target: Some(500),
        min: None,
        max: None,
    });
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::MoqLog, true);
    b.alt_group = Some(1);
    b.buffers = Some(MsfBuffers {
        target: Some(1000),
        min: None,
        max: None,
    });
    assert!(matches!(
        full_doc(vec![], vec![a, b], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// 非 eventtimeline での eventType は encode でも拒否されること
#[test]
fn encode_full_event_type_with_non_eventtimeline_rejected() {
    // draft-ietf-moq-msf-01 §5.2.5 (Event timeline type)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.event_type = Some("x".to_string());
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// eventtimeline で eventType が無い場合は encode でも拒否されること
#[test]
fn encode_full_event_timeline_missing_event_type_rejected() {
    // draft-ietf-moq-msf-01 §5.2.5 (Event timeline type)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::EventTimeline, true);
    track.depends = vec!["v".to_string()];
    track.mime_type = Some("application/json".to_string());
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// mediatimeline で depends が無い場合は encode でも拒否されること
#[test]
fn encode_full_media_timeline_missing_depends_rejected() {
    // draft-ietf-moq-msf-01 §7.2 (Media Timeline Catalog requirements)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::MediaTimeline, true);
    track.mime_type = Some("application/json".to_string());
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// encryptionScheme 指定時の cipherSuite 欠如は encode でも拒否されること
#[test]
fn encode_full_encryption_scheme_missing_cipher_suite_rejected() {
    // draft-ietf-moq-msf-01 §5.2.39 (Cipher suite)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.encryption_scheme = Some("com.example.custom".to_string());
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// moq-secure-objects の必須組欠如は encode でも拒否されること
#[test]
fn encode_full_secure_objects_incomplete_rejected() {
    // draft-ietf-moq-msf-01 §4.3.3 (Recommended encryption scheme)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.encryption_scheme = Some("moq-secure-objects".to_string());
    track.cipher_suite = Some("aes-128-gcm-sha256".to_string());
    // keyId / trackBaseKey を欠く
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// 不正な lang は encode でも拒否されること
#[test]
fn encode_full_invalid_lang_rejected() {
    // draft-ietf-moq-msf-01 §5.2.32 (Language)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.lang = Some("1".to_string());
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// 通常トラックの parentName は encode でも拒否されること
#[test]
fn encode_full_parent_name_rejected() {
    // draft-ietf-moq-msf-01 §5.2.33 (Parent name)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.parent_name = Some("p".to_string());
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// isLive=true の trackDuration は encode でも拒否されること
#[test]
fn encode_full_track_duration_when_is_live_true_rejected() {
    // draft-ietf-moq-msf-01 §5.2.35 (Track duration)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.track_duration = Some(1000);
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

// ─── encode 時のデルタ更新検証 (decode_delta と同等) ──────────────────────────

/// 空の operations は encode でも拒否されること
#[test]
fn encode_delta_empty_operations_rejected() {
    // draft-ietf-moq-msf-01 §5.3 (Delta updates)
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: None,
        operations: Vec::new(),
    });
    assert!(matches!(doc.encode(), Err(MessageError::InvalidCatalog(_))));
}

/// delta add の不正トラックは encode でも拒否されること
#[test]
fn encode_delta_add_invalid_track_rejected() {
    // draft-ietf-moq-msf-01 §7.2 (Media Timeline Catalog requirements): depends 必須
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::MediaTimeline, true);
    track.mime_type = Some("application/json".to_string());
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: None,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![track],
        }],
    });
    assert!(matches!(doc.encode(), Err(MessageError::InvalidCatalog(_))));
}

/// delta clone の targetLatency / buffers 共存は encode でも拒否されること
#[test]
fn encode_delta_clone_target_latency_and_buffers_rejected() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency) / §5.2.9 (Buffers)
    let mut clone = MsfCloneTrack::new("c".to_string(), "p".to_string());
    clone.target_latency = Some(500);
    clone.buffers = Some(MsfBuffers {
        target: Some(1000),
        min: None,
        max: None,
    });
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: None,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    });
    assert!(matches!(
        doc.encode(),
        Err(MessageError::InvalidCatalog(reason))
            if reason == "targetLatency and buffers MUST NOT be present together"
    ));
}

/// delta clone の isLive=true + trackDuration は encode でも拒否されること
#[test]
fn encode_delta_clone_track_duration_when_is_live_true_rejected() {
    // draft-ietf-moq-msf-01 §5.2.35 (Track duration)
    let mut clone = MsfCloneTrack::new("c".to_string(), "p".to_string());
    clone.is_live = Some(true);
    clone.track_duration = Some(1000);
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: None,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    });
    assert!(matches!(doc.encode(), Err(MessageError::InvalidCatalog(_))));
}

/// delta clone の非 eventtimeline packaging + eventType は encode でも拒否されること
#[test]
fn encode_delta_clone_event_type_with_non_eventtimeline_rejected() {
    // draft-ietf-moq-msf-01 §5.2.5 (Event timeline type)
    let mut clone = MsfCloneTrack::new("c".to_string(), "p".to_string());
    clone.packaging = Some(MsfPackaging::Loc);
    clone.event_type = Some("x".to_string());
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: None,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    });
    assert!(matches!(doc.encode(), Err(MessageError::InvalidCatalog(_))));
}

/// delta clone の不正 lang は encode でも拒否されること
#[test]
fn encode_delta_clone_invalid_lang_rejected() {
    // draft-ietf-moq-msf-01 §5.2.32 (Language)
    let mut clone = MsfCloneTrack::new("c".to_string(), "p".to_string());
    clone.lang = Some("1".to_string());
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: None,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    });
    assert!(matches!(doc.encode(), Err(MessageError::InvalidCatalog(_))));
}
