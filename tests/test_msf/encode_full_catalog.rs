use super::*;

// ─── encode 時の完全カタログ検証 (decode_full_catalog と同等) ──────────────────

/// tracks 内の (namespace, name) 重複は encode でも拒否されること
#[test]
fn encode_full_duplicate_name_within_tracks_rejected() {
    // draft-ietf-moq-msf-02 §5.2.3 (Track name)
    let a = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    let b = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    assert!(matches!(
        full_doc(vec![a, b], vec![], vec![]).encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// tracks と publishTracks をまたぐ (namespace, name) 重複は encode でも拒否されること
#[test]
fn encode_full_duplicate_name_across_tracks_and_publish_tracks_rejected() {
    // draft-ietf-moq-msf-02 §5.2.3 (Track name): "Within the catalog"
    let a = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    let b = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    assert!(matches!(
        full_doc(vec![a], vec![b], vec![]).encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// initDataList の id 重複は encode でも拒否されること
#[test]
fn encode_full_duplicate_init_data_list_id_rejected() {
    // draft-ietf-moq-msf-02 §5.1.7 (Initialization Data List)
    let entry = || MsfInitData {
        id: "init1".to_string(),
        kind: MsfInitDataKind::Inline,
        data: "AAEC".to_string(),
    };
    assert!(matches!(
        full_doc(vec![], vec![], vec![entry(), entry()])
            .encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// 一致しない initRef は encode でも拒否されること
#[test]
fn encode_full_missing_init_ref_rejected() {
    // draft-ietf-moq-msf-02 §5.2.14 (Initialization reference)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.init_ref = Some("missing".to_string());
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// targetLatency と buffers の共存は encode でも拒否されること
#[test]
fn encode_full_target_latency_and_buffers_together_rejected() {
    // draft-ietf-moq-msf-02 §5.2.9 (Target latency) / §5.2.10 (Buffers)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.target_latency = Some(500);
    track.buffers = Some(MsfBuffers {
        target: Some(1000),
        min: None,
        max: None,
    });
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(reason))
            if reason == "targetLatency and buffers MUST NOT be present together"
    ));
}

/// 同一 renderGroup の targetLatency 不一致は encode でも拒否されること
#[test]
fn encode_full_group_target_latency_mismatch_rejected() {
    // draft-ietf-moq-msf-02 §5.2.9 (Target latency)
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.render_group = Some(1);
    a.target_latency = Some(500);
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.render_group = Some(1);
    b.target_latency = Some(1000);
    assert!(matches!(
        full_doc(vec![a, b], vec![], vec![]).encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// publishTracks 内の同一 altGroup の buffers 不一致は encode でも拒否されること
#[test]
fn encode_full_publish_tracks_group_buffers_mismatch_rejected() {
    // draft-ietf-moq-msf-02 §5.2.10 (Buffers)
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
        full_doc(vec![], vec![a, b], vec![]).encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// 同一 renderGroup で片方だけ targetLatency を宣言した手組みカタログは encode でも受理されること
#[test]
fn encode_full_group_target_latency_omitted_accepted() {
    // draft-ietf-moq-msf-02 §5.2.9 (Target latency): 省略は player の裁量 (MAY) であり不一致ではない
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.render_group = Some(1);
    a.target_latency = Some(500);
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.render_group = Some(1);
    full_doc(vec![a, b], vec![], vec![])
        .encode(shiguredo_moqt::msf::MsfCompression::None)
        .expect("省略された targetLatency は encode できること");
}

/// 同一 renderGroup で片方だけ buffers を宣言した手組みカタログは encode でも受理されること
#[test]
fn encode_full_group_buffers_omitted_accepted() {
    // draft-ietf-moq-msf-02 §5.2.10 (Buffers): 省略は player の裁量 (MAY) であり不一致ではない
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.render_group = Some(1);
    a.buffers = Some(MsfBuffers {
        target: Some(500),
        min: None,
        max: None,
    });
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.render_group = Some(1);
    full_doc(vec![a, b], vec![], vec![])
        .encode(shiguredo_moqt::msf::MsfCompression::None)
        .expect("省略された buffers は encode できること");
}

/// 同一 altGroup で片方だけ buffers を宣言した手組みカタログは encode でも受理されること
#[test]
fn encode_full_alt_group_buffers_omitted_accepted() {
    // draft-ietf-moq-msf-02 §5.2.10 (Buffers): altGroup でも省略は不一致としない
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.alt_group = Some(2);
    a.buffers = Some(MsfBuffers {
        target: Some(500),
        min: None,
        max: None,
    });
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.alt_group = Some(2);
    full_doc(vec![a, b], vec![], vec![])
        .encode(shiguredo_moqt::msf::MsfCompression::None)
        .expect("altGroup でも省略された buffers は encode できること");
}

/// 同一 altGroup で片方だけ targetLatency を宣言した手組みカタログは encode でも受理されること
#[test]
fn encode_full_alt_group_target_latency_omitted_accepted() {
    // draft-ietf-moq-msf-02 §5.2.9 (Target latency): altGroup でも省略は不一致としない
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.alt_group = Some(2);
    a.target_latency = Some(500);
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.alt_group = Some(2);
    full_doc(vec![a, b], vec![], vec![])
        .encode(shiguredo_moqt::msf::MsfCompression::None)
        .expect("altGroup でも省略された targetLatency は encode できること");
}

/// renderGroup と altGroup を同時に持つトラックでも renderGroup の不一致は拒否されること
#[test]
fn encode_full_group_render_conflict_rejected() {
    // renderGroup の不一致が altGroup の相違に隠されないこと (altGroup は衝突しない)
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.render_group = Some(1);
    a.alt_group = Some(2);
    a.target_latency = Some(500);
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.render_group = Some(1);
    b.alt_group = Some(2);
    b.target_latency = Some(500);
    let mut c = MsfTrack::new("c".to_string(), MsfPackaging::Loc, true);
    c.render_group = Some(1);
    c.alt_group = Some(3);
    c.target_latency = Some(1000);
    // c は renderGroup 1 で a / b と衝突する (altGroup は異なる)
    assert!(matches!(
        full_doc(vec![a, b, c], vec![], vec![]).encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(reason)) if reason.contains("renderGroup 1")
    ));
}

/// renderGroup が衝突しない構成でも altGroup の不一致は拒否されること
#[test]
fn encode_full_group_alt_conflict_rejected() {
    // altGroup の不一致が renderGroup の相違に隠されないこと
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.render_group = Some(1);
    a.alt_group = Some(2);
    a.target_latency = Some(500);
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.render_group = Some(2);
    b.alt_group = Some(2);
    b.target_latency = Some(1000);
    // renderGroup は 1 と 2 で衝突せず、altGroup 2 のみが衝突する
    assert!(matches!(
        full_doc(vec![a, b], vec![], vec![]).encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(reason)) if reason.contains("altGroup 2")
    ));
}

/// group 値 0 も通常の group 値として宣言値の一致を検証すること
#[test]
fn encode_full_group_zero_target_latency_mismatch_rejected() {
    // group 値は 0 も有効な値であり、省略 (None) と混同して比較対象外にしないこと
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.render_group = Some(0);
    a.target_latency = Some(500);
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.render_group = Some(0);
    b.target_latency = Some(1000);
    assert!(matches!(
        full_doc(vec![a, b], vec![], vec![]).encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// group 値 0 の buffers でも通常の group 値として宣言値の一致を検証すること
#[test]
fn encode_full_group_zero_buffers_mismatch_rejected() {
    // targetLatency 側だけでなく buffers 側でも 0 を省略と混同しないこと
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.render_group = Some(0);
    a.buffers = Some(MsfBuffers {
        target: Some(500),
        min: None,
        max: None,
    });
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.render_group = Some(0);
    b.buffers = Some(MsfBuffers {
        target: Some(1000),
        min: None,
        max: None,
    });
    assert!(matches!(
        full_doc(vec![a, b], vec![], vec![]).encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// group 未所属のトラックは宣言値が異なっても group 検証で拒否されないこと
#[test]
fn encode_full_without_group_different_target_latency_accepted() {
    // group 値が無い (None) トラック同士は group 検証の対象外であること
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.target_latency = Some(500);
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.target_latency = Some(1000);
    full_doc(vec![a, b], vec![], vec![])
        .encode(shiguredo_moqt::msf::MsfCompression::None)
        .expect("group 未所属のトラックは group 検証の対象外であること");
}

/// publishTracks 内で片方だけ targetLatency を宣言した手組みカタログは encode でも受理されること
#[test]
fn encode_full_publish_tracks_group_target_latency_omitted_accepted() {
    // draft-ietf-moq-msf-02 §5.2.9 (Target latency): tracks と publishTracks は分離して検証するが
    // 省略を比較対象外とする規則は publishTracks 側にも適用される
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::MoqLog, true);
    a.render_group = Some(1);
    a.target_latency = Some(500);
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::MoqLog, true);
    b.render_group = Some(1);
    full_doc(vec![], vec![a, b], vec![])
        .encode(shiguredo_moqt::msf::MsfCompression::None)
        .expect("publishTracks でも省略された targetLatency は encode できること");
}

/// 省略トラックを挟んでも宣言された targetLatency の不一致は encode でも拒否されること
#[test]
fn encode_full_group_omitted_between_different_target_latency_rejected() {
    // draft-ietf-moq-msf-02 §5.2.9 (Target latency)
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.render_group = Some(1);
    a.target_latency = Some(500);
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.render_group = Some(1);
    let mut c = MsfTrack::new("c".to_string(), MsfPackaging::Loc, true);
    c.render_group = Some(1);
    c.target_latency = Some(1000);
    assert!(matches!(
        full_doc(vec![a, b, c], vec![], vec![]).encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// 省略トラックを挟んでも宣言された buffers の不一致は encode でも拒否されること
#[test]
fn encode_full_group_omitted_between_different_buffers_rejected() {
    // draft-ietf-moq-msf-02 §5.2.10 (Buffers)
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.render_group = Some(1);
    a.buffers = Some(MsfBuffers {
        target: Some(500),
        min: None,
        max: None,
    });
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.render_group = Some(1);
    let mut c = MsfTrack::new("c".to_string(), MsfPackaging::Loc, true);
    c.render_group = Some(1);
    c.buffers = Some(MsfBuffers {
        target: Some(1000),
        min: None,
        max: None,
    });
    assert!(matches!(
        full_doc(vec![a, b, c], vec![], vec![]).encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// isLive=false のトラックの targetLatency は手組みカタログの encode でも比較対象外であること
#[test]
fn encode_full_group_target_latency_is_live_false_ignored() {
    // draft-ietf-moq-msf-02 §5.2.9 (Target latency): isLive=false では targetLatency は無視される。
    // decode 経路は decode_track が isLive=false の値を None に正規化するため、
    // 正規化を通らない手組みの MsfCatalog で除外そのものを固定する
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.render_group = Some(1);
    a.target_latency = Some(500);
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, false);
    b.render_group = Some(1);
    b.target_latency = Some(1000);
    full_doc(vec![a, b], vec![], vec![])
        .encode(shiguredo_moqt::msf::MsfCompression::None)
        .expect("isLive=false の targetLatency は encode の比較対象外であること");
}

/// isLive=false のトラックの buffers は手組みカタログの encode でも比較対象外であること
#[test]
fn encode_full_group_buffers_is_live_false_ignored() {
    // draft-ietf-moq-msf-02 §5.2.10 (Buffers): isLive=false では buffers は無視される
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.render_group = Some(1);
    a.buffers = Some(MsfBuffers {
        target: Some(500),
        min: None,
        max: None,
    });
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, false);
    b.render_group = Some(1);
    b.buffers = Some(MsfBuffers {
        target: Some(1000),
        min: None,
        max: None,
    });
    full_doc(vec![a, b], vec![], vec![])
        .encode(shiguredo_moqt::msf::MsfCompression::None)
        .expect("isLive=false の buffers は encode の比較対象外であること");
}

/// 非 eventtimeline での eventType は encode でも拒否されること
#[test]
fn encode_full_event_type_with_non_eventtimeline_rejected() {
    // draft-ietf-moq-msf-02 §5.2.6 (Event timeline type)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.event_type = Some("x".to_string());
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// eventtimeline で eventType が無い場合は encode でも拒否されること
#[test]
fn encode_full_event_timeline_missing_event_type_rejected() {
    // draft-ietf-moq-msf-02 §5.2.6 (Event timeline type)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::EventTimeline, true);
    track.depends = vec![MsfDependency::TrackName("v".to_string())];
    track.mime_type = Some("application/json".to_string());
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// mediatimeline で depends が無い場合は encode でも拒否されること
#[test]
fn encode_full_media_timeline_missing_depends_rejected() {
    // draft-ietf-moq-msf-02 §7.2 (Media Timeline Catalog requirements)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::MediaTimeline, true);
    track.mime_type = Some("application/json".to_string());
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// encryptionScheme 指定時の cipherSuite 欠如は encode でも拒否されること
#[test]
fn encode_full_custom_scheme_without_cipher_suite_accepted() {
    // draft-ietf-moq-msf-02 §5.2.39: cipherSuite は moq-secure-objects のときだけ必須
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.encryption_scheme = Some("com.example.custom".to_string());
    assert!(
        full_doc(vec![track], vec![], vec![])
            .encode(shiguredo_moqt::msf::MsfCompression::None)
            .is_ok(),
        "custom scheme は cipherSuite なしで encode できること"
    );
}

/// moq-secure-objects の必須組欠如は encode でも拒否されること
#[test]
fn encode_full_secure_objects_incomplete_rejected() {
    // draft-ietf-moq-msf-02 §4.4.3 (Recommended encryption scheme)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.encryption_scheme = Some("moq-secure-objects".to_string());
    track.cipher_suite = Some("aes-128-gcm-sha256".to_string());
    // keyId / trackBaseKey を欠く
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// 不正な lang は encode でも拒否されること
#[test]
fn encode_full_invalid_lang_rejected() {
    // draft-ietf-moq-msf-02 §5.2.33 (Language)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.lang = Some("1".to_string());
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// irregular タグは encode でも受理され、decode で同じ値に戻ること
#[test]
fn encode_full_irregular_lang_roundtrip() {
    // draft-ietf-moq-msf-02 §5.2.33 (Language)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.lang = Some("i-klingon".to_string());
    let bytes = full_doc(vec![track], vec![], vec![])
        .encode(shiguredo_moqt::msf::MsfCompression::None)
        .expect("irregular タグは encode できること");
    let decoded = MsfCatalogDocument::decode(&bytes, shiguredo_moqt::msf::MsfCompression::None)
        .expect("encode 結果は decode できること");
    let MsfCatalogDocument::Full(catalog) = decoded else {
        panic!("Full catalog に戻ること");
    };
    assert_eq!(
        catalog.tracks[0].lang.as_deref(),
        Some("i-klingon"),
        "encode / decode で lang が保持されること"
    );
}

/// 通常トラックの parentName は encode でも拒否されること
#[test]
fn encode_full_parent_name_rejected() {
    // draft-ietf-moq-msf-02 §5.2.34 (Parent name)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.parent_name = Some("p".to_string());
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// isLive=true の trackDuration は encode でも拒否されること
#[test]
fn encode_full_track_duration_when_is_live_true_rejected() {
    // draft-ietf-moq-msf-02 §5.2.36 (Track duration)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.track_duration = Some(1000);
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// moq-secure-objects で cipherSuite が無い場合は encode で拒否されること
#[test]
fn encode_full_secure_objects_without_cipher_suite_rejected() {
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.encryption_scheme = Some("moq-secure-objects".to_string());
    track.key_id = Some("key-1".to_string());
    track.track_base_key = Some("AAEC".to_string());
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}
