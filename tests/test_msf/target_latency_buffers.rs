use super::*;

// ─── targetLatency / buffers の相互制約 ───────────────────────────────────────

#[test]
fn render_group_different_target_latency_rejected() {
    // draft-ietf-moq-msf-02 §5.2.9 (Target latency): 同一 renderGroup 内の isLive=true トラックが宣言した targetLatency は一致必須 (省略は比較対象外)
    let json = br#"{"version":"draft-02","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":500},
            {"name":"b","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":1000}
        ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn alt_group_different_target_latency_rejected() {
    // draft-ietf-moq-msf-02 §5.2.9 (Target latency): 同一 altGroup 内の isLive=true トラックが宣言した targetLatency は一致必須 (省略は比較対象外)
    let json = br#"{"version":"draft-02","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"altGroup":2,"targetLatency":500},
            {"name":"b","packaging":"loc","isLive":true,"altGroup":2,"targetLatency":1000}
        ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn render_group_same_target_latency_accepted() {
    let json = br#"{"version":"draft-02","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":500},
            {"name":"b","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":500}
        ]}"#;
    assert!(MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None).is_ok());
}

#[test]
fn render_group_target_latency_omitted_accepted() {
    // draft-ietf-moq-msf-02 §5.2.9 (Target latency): フィールドが無く isLive=true なら
    // player が遅延を選んでよい (MAY)。省略は宣言値との不一致として拒否しない
    let json = br#"{"version":"draft-02","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":500},
            {"name":"b","packaging":"loc","isLive":true,"renderGroup":1}
        ]}"#;
    MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("省略された targetLatency は拒否されないこと");
}

#[test]
fn alt_group_target_latency_omitted_accepted() {
    // draft-ietf-moq-msf-02 §5.2.9 (Target latency): altGroup でも省略は不一致としない
    let json = br#"{"version":"draft-02","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"altGroup":2,"targetLatency":500},
            {"name":"b","packaging":"loc","isLive":true,"altGroup":2}
        ]}"#;
    MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("altGroup でも省略された targetLatency は拒否されないこと");
}

#[test]
fn render_group_target_latency_and_buffers_split_accepted() {
    // draft-ietf-moq-msf-02 §5.2.9 / §5.2.10: 省略を比較対象外とするため、同一 group で
    // 一方が targetLatency のみ、他方が buffers のみを宣言する構成も受理される (本実装の解釈)
    let json = br#"{"version":"draft-02","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":500},
            {"name":"b","packaging":"loc","isLive":true,"renderGroup":1,"buffers":{"target":1000}}
        ]}"#;
    MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("group 内で targetLatency と buffers を使い分けても拒否されないこと");
}

#[test]
fn render_group_omitted_between_different_target_latency_rejected() {
    // draft-ietf-moq-msf-02 §5.2.9 (Target latency): 省略トラックを挟んでも
    // 宣言された 2 値の不一致は検出すること
    let json = br#"{"version":"draft-02","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":500},
            {"name":"b","packaging":"loc","isLive":true,"renderGroup":1},
            {"name":"c","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":1000}
        ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn target_latency_and_buffers_together_rejected_when_is_live_true() {
    // draft-ietf-moq-msf-02 §5.2.9 (Target latency) / §5.2.10 (Buffers):
    // isLive=true の同一 track に targetLatency と buffers は共存できない
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"targetLatency":500,"buffers":{"target":1000}}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(reason))
        if reason == "targetLatency and buffers MUST NOT be present together"
    ));
}

#[test]
fn target_latency_and_buffers_together_rejected_when_is_live_false() {
    // draft-ietf-moq-msf-02 §5.2.9 / §5.2.10: isLive=false でも共存は reject されること
    // (本実装では presence 違反を優先する選択とする)
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":false,"targetLatency":500,"buffers":{"target":1000}}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(reason))
        if reason == "targetLatency and buffers MUST NOT be present together"
    ));
}

#[test]
fn target_latency_or_buffers_alone_ignored_when_is_live_false() {
    // draft-ietf-moq-msf-02 §5.2.9 / §5.2.10: 単独存在時は isLive=false で無視されること
    // (両単独とも確認する)
    for (name, json) in [
        (
            "targetLatency 単独",
            br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":false,"targetLatency":500}]}"#.as_slice(),
        ),
        (
            "buffers 単独",
            br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":false,"buffers":{"target":1000}}]}"#.as_slice(),
        ),
    ] {
        let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None).expect("テストフィクスチャの前提条件を満たす");
        let MsfCatalogDocument::Full(cat) = doc else {
            panic!("Full catalog がデコードされること");
        };
        assert_eq!(cat.tracks[0].target_latency, None, "{name} は無視されること");
        assert_eq!(cat.tracks[0].buffers, None, "{name} は無視されること");
    }
}

#[test]
fn clone_target_latency_or_buffers_alone_ignored_when_is_live_false() {
    // draft-ietf-moq-msf-02 §5.2.9 / §5.2.10: clone の単独存在時も isLive=false で無視されること
    // (両単独とも確認する)
    for (name, json, field) in [
        (
            "targetLatency 単独",
            br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","isLive":false,"targetLatency":500}]}]}"#.as_slice(),
            "targetLatency",
        ),
        (
            "buffers 単独",
            br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","isLive":false,"buffers":{"target":1000}}]}]}"#.as_slice(),
            "buffers",
        ),
    ] {
        let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None).expect("テストフィクスチャの前提条件を満たす");
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
    // draft-ietf-moq-msf-02 §5.2.9 / §5.2.10: clone でも共存は reject されること
    // (isLive 省略時を含む。理由文字列まで固定する)
    for (name, json) in [
        (
            "isLive:false",
            br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","isLive":false,"targetLatency":500,"buffers":{"target":1000}}]}]}"#.as_slice(),
        ),
        (
            "isLive 省略",
            br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","targetLatency":500,"buffers":{"target":1000}}]}]}"#.as_slice(),
        ),
    ] {
        assert!(
            matches!(
                MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
                Err(MessageError::InvalidCatalog(reason))
                if reason == "targetLatency and buffers MUST NOT be present together"
            ),
            "{name} の共存は reject されること"
        );
    }
}

#[test]
fn render_group_different_buffers_rejected() {
    // draft-ietf-moq-msf-02 §5.2.10 (Buffers): 同一 renderGroup 内の isLive=true トラックが宣言した buffers は一致必須 (省略は比較対象外)
    let json = br#"{"version":"draft-02","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"buffers":{"target":500}},
            {"name":"b","packaging":"loc","isLive":true,"renderGroup":1,"buffers":{"target":1000}}
        ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn render_group_same_buffers_accepted() {
    let json = br#"{"version":"draft-02","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"buffers":{"target":500}},
            {"name":"b","packaging":"loc","isLive":true,"renderGroup":1,"buffers":{"target":500}}
        ]}"#;
    assert!(MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None).is_ok());
}

#[test]
fn render_group_buffers_omitted_accepted() {
    // draft-ietf-moq-msf-02 §5.2.10 (Buffers): フィールドが無く isLive=true なら
    // player がバッファを選んでよい (MAY)。省略は宣言値との不一致として拒否しない
    let json = br#"{"version":"draft-02","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"buffers":{"target":500}},
            {"name":"b","packaging":"loc","isLive":true,"renderGroup":1}
        ]}"#;
    MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("省略された buffers は拒否されないこと");
}

#[test]
fn alt_group_buffers_omitted_accepted() {
    // draft-ietf-moq-msf-02 §5.2.10 (Buffers): altGroup でも省略は不一致としない
    let json = br#"{"version":"draft-02","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"altGroup":2,"buffers":{"target":500}},
            {"name":"b","packaging":"loc","isLive":true,"altGroup":2}
        ]}"#;
    MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("altGroup でも省略された buffers は拒否されないこと");
}

#[test]
fn render_group_omitted_between_different_buffers_rejected() {
    // draft-ietf-moq-msf-02 §5.2.10 (Buffers): 省略トラックを挟んでも
    // 宣言された 2 値の不一致は検出すること
    let json = br#"{"version":"draft-02","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"buffers":{"target":500}},
            {"name":"b","packaging":"loc","isLive":true,"renderGroup":1},
            {"name":"c","packaging":"loc","isLive":true,"renderGroup":1,"buffers":{"target":1000}}
        ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn render_group_mixed_live_ignored_for_buffers() {
    // isLive=false のトラックは group 検証の対象外。ただし b は buffers を宣言していないため、
    // 本テストだけでは isLive 除外と省略除外を区別できない。省略除外は
    // render_group_buffers_omitted_accepted、isLive 除外は
    // encode_full_group_buffers_is_live_false_ignored がそれぞれ固定する
    let json = br#"{"version":"draft-02","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"buffers":{"target":500}},
            {"name":"b","packaging":"loc","isLive":false,"renderGroup":1}
        ]}"#;
    MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("isLive=false の省略は拒否されないこと");
}

#[test]
fn render_group_mixed_live_ignored_for_target_latency() {
    // isLive=false のトラックは group 検証の対象外。ただし b は targetLatency を宣言していないため、
    // 本テストだけでは isLive 除外と省略除外を区別できない。省略除外は
    // render_group_target_latency_omitted_accepted、isLive 除外は
    // encode_full_group_target_latency_is_live_false_ignored がそれぞれ固定する
    let json = br#"{"version":"draft-02","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":500},
            {"name":"b","packaging":"loc","isLive":false,"renderGroup":1}
        ]}"#;
    MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("isLive=false の省略は拒否されないこと");
}

#[test]
fn buffers_non_number_value_rejected() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"buffers":{"target":"fast"}}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn alt_group_different_buffers_rejected() {
    // draft-ietf-moq-msf-02 §5.2.10 (Buffers): 同一 altGroup 内の isLive=true トラックが宣言した buffers は一致必須 (省略は比較対象外)
    let json = br#"{"version":"draft-02","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"altGroup":1,"buffers":{"target":500}},
            {"name":"b","packaging":"loc","isLive":true,"altGroup":1,"buffers":{"target":1000}}
        ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}
