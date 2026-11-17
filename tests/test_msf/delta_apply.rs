use super::*;

/// namespace 指定付きの LOC トラックを作るヘルパー
fn loc_track(name: &str, namespace: Option<&str>) -> MsfTrack {
    let mut track = MsfTrack::new(name.to_string(), MsfPackaging::Loc, true);
    track.namespace = namespace.map(str::to_string);
    track
}

#[test]
fn apply_delta_add_appends_track() {
    // draft-ietf-moq-msf-01 §5.1.6 (Delta update): add は新規トラックを追加する
    let mut catalog = MsfCatalog::new();
    let delta = MsfDeltaUpdate {
        generated_at: 1000,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![loc_track("v", Some("ns"))],
        }],
    };
    catalog
        .apply_delta(&delta, Some(&test_catalog_namespace("catalog-ns")))
        .expect("add は成功する");
    assert_eq!(catalog.tracks.len(), 1);
    assert_eq!(catalog.tracks[0].name, "v");
    // delta の generatedAt が適用後のカタログに反映される
    assert_eq!(catalog.generated_at, Some(1000));
}

#[test]
fn apply_delta_add_duplicate_rejected() {
    // 既存トラックと同名の add は reject される
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(loc_track("v", Some("ns")));
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![loc_track("v", Some("ns"))],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// draft-ietf-moq-msf-01 §5.2.35 (Track duration): isLive=true なら trackDuration は禁止。
/// add 経路でも encode 時まで待たず apply_delta 時点で拒否する。
#[test]
fn apply_delta_add_is_live_with_track_duration_rejected() {
    let mut catalog = MsfCatalog::new();
    // loc_track は isLive=true で作られる
    let mut track = loc_track("v", Some("ns"));
    track.track_duration = Some(1000);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![track],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
    assert!(
        catalog.tracks.is_empty(),
        "拒否時はトラックが追加されないこと"
    );
}

/// draft-ietf-moq-msf-01 §5.2.18 (Codec): video トラックは codec が必須。
/// add 経路でも encode 時まで待たず apply_delta 時点で拒否する。
#[test]
fn apply_delta_add_video_without_codec_rejected() {
    let mut catalog = MsfCatalog::new();
    let mut track = loc_track("v", Some("ns"));
    track.role = Some("video".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![track],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
    assert!(
        catalog.tracks.is_empty(),
        "拒否時はトラックが追加されないこと"
    );
}

/// draft-ietf-moq-msf-01 §5.2.28 (Audio sample rate): codec から audio と判定したトラックは
/// role が無くても samplerate が必須。add 経路でも encode 時まで待たず apply_delta 時点で
/// 拒否する。
#[test]
fn apply_delta_add_codec_audio_without_samplerate_rejected() {
    let mut catalog = MsfCatalog::new();
    let mut track = loc_track("a", Some("ns"));
    track.codec = Some("opus".to_string());
    track.bitrate = Some(32_000);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![track],
        }],
    };
    let Err(MessageError::InvalidCatalog(reason)) = catalog.apply_delta(&delta, None) else {
        panic!("codec 由来の samplerate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("samplerate") && reason.contains("codec 'opus'"),
        "codec 由来の samplerate 要求を述べること: {reason}"
    );
    assert!(
        catalog.tracks.is_empty(),
        "拒否時はトラックが追加されないこと"
    );
}

#[test]
fn apply_delta_remove_deletes_track() {
    // draft-ietf-moq-msf-01 §5.1.6 (Delta update): remove は既存トラックを削除する
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(loc_track("v", None));
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Remove {
            tracks: vec![MsfRemoveTrack::new("v".to_string())],
        }],
    };
    catalog
        .apply_delta(&delta, Some(&test_catalog_namespace("catalog-ns")))
        .expect("remove は成功する");
    assert!(catalog.tracks.is_empty());
}

#[test]
fn apply_delta_remove_missing_rejected() {
    // 未宣言トラックの remove は reject される
    let mut catalog = MsfCatalog::new();
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Remove {
            tracks: vec![MsfRemoveTrack::new("missing".to_string())],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn apply_delta_clone_inherits_and_overrides() {
    // draft-ietf-moq-msf-01 §5.1.6 (Delta update): clone は親の属性を継承し、
    // 再定義された属性で上書きする
    let mut parent = loc_track("v", None);
    parent.codec = Some("av01".to_string());
    parent.bitrate = Some(1_000_000);
    parent.render_group = Some(1);
    parent.target_latency = Some(500);
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(parent);

    let mut clone = MsfCloneTrack::new("v2".to_string(), "v".to_string());
    clone.bitrate = Some(2_000_000);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    };
    catalog
        .apply_delta(&delta, Some(&test_catalog_namespace("catalog-ns")))
        .expect("clone は成功する");
    let cloned = catalog
        .tracks
        .iter()
        .find(|t| t.name == "v2")
        .expect("clone 後のトラックが存在する");
    // 親から継承した属性
    assert_eq!(cloned.codec.as_deref(), Some("av01"));
    assert_eq!(cloned.render_group, Some(1));
    assert_eq!(cloned.target_latency, Some(500));
    // 上書きした属性
    assert_eq!(cloned.bitrate, Some(2_000_000));
    // 解決済みトラックには parentName を残さない
    assert_eq!(cloned.parent_name, None);
}

/// clone が codec で audio と判定される属性を追加した場合も samplerate が必須
///
/// 親が codec を持たない raw data トラックでも、clone が codec を追加すれば §5.2.28 の
/// MUST が解決後のトラックに適用される。
#[test]
fn apply_delta_clone_added_audio_codec_without_samplerate_rejected() {
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(loc_track("base", None));

    let mut clone = MsfCloneTrack::new("a".to_string(), "base".to_string());
    clone.codec = Some("opus".to_string());
    clone.bitrate = Some(32_000);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    };
    let Err(MessageError::InvalidCatalog(reason)) =
        catalog.apply_delta(&delta, Some(&test_catalog_namespace("catalog-ns")))
    else {
        panic!("codec 由来の samplerate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("samplerate") && reason.contains("codec 'opus'"),
        "codec 由来の samplerate 要求を述べること: {reason}"
    );
    assert_eq!(
        catalog.tracks.len(),
        1,
        "拒否時は clone されたトラックが追加されないこと"
    );
}

#[test]
fn apply_delta_clone_uses_parent_namespace() {
    // draft-ietf-moq-msf-01 §5.2.34 (Parent namespace): parentNamespace で親を特定する
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(loc_track("v", Some("other-ns")));
    let mut clone = MsfCloneTrack::new("v2".to_string(), "v".to_string());
    clone.parent_namespace = Some("other-ns".to_string());
    clone.namespace = Some("other-ns".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    };
    catalog
        .apply_delta(&delta, Some(&test_catalog_namespace("catalog-ns")))
        .expect("clone は成功する");
    assert!(catalog.tracks.iter().any(|t| t.name == "v2"));
}

#[test]
fn apply_delta_clone_missing_parent_rejected() {
    // 親トラックが見つからない clone は reject される
    let mut catalog = MsfCatalog::new();
    let clone = MsfCloneTrack::new("v2".to_string(), "missing".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn apply_delta_clone_is_live_false_drops_inherited_latency() {
    // draft-ietf-moq-msf-01 §5.2.8 / §5.2.9: isLive=false なら targetLatency / buffers は無視される
    let mut parent = loc_track("v", None);
    parent.target_latency = Some(500);
    parent.buffers = Some(MsfBuffers {
        target: Some(1000),
        min: None,
        max: None,
    });
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(parent);
    let mut clone = MsfCloneTrack::new("v2".to_string(), "v".to_string());
    clone.is_live = Some(false);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    };
    catalog.apply_delta(&delta, None).expect("clone は成功する");
    let cloned = catalog
        .tracks
        .iter()
        .find(|t| t.name == "v2")
        .expect("clone 後のトラックが存在する");
    assert!(!cloned.is_live);
    assert_eq!(cloned.target_latency, None);
    assert_eq!(cloned.buffers, None);
}

#[test]
fn apply_delta_clone_event_timeline_without_event_type_rejected() {
    // draft-ietf-moq-msf-01 §5.2.5 (Event timeline type): eventtimeline なら eventType 必須
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(loc_track("v", None));
    let mut clone = MsfCloneTrack::new("e".to_string(), "v".to_string());
    clone.packaging = Some(MsfPackaging::EventTimeline);
    clone.mime_type = Some("application/json".to_string());
    clone.depends = Some(vec![MsfDependency::TrackName("v".to_string())]);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn apply_delta_group_latency_mismatch_rejected() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency): 同一 renderGroup の宣言された targetLatency は一致 (省略は比較対象外)
    let mut first = loc_track("v1", None);
    first.render_group = Some(1);
    first.target_latency = Some(500);
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(first);
    let mut second = loc_track("v2", None);
    second.render_group = Some(1);
    second.target_latency = Some(1000);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![second],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// 同一 renderGroup で片方だけ targetLatency を宣言した delta 適用は受理されること
#[test]
fn apply_delta_group_target_latency_omitted_accepted() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency): フィールドが無く isLive=true なら
    // player が遅延を選んでよい (MAY)。delta 適用後の検証でも省略は不一致としない
    let mut first = loc_track("v1", None);
    first.render_group = Some(1);
    first.target_latency = Some(500);
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(first);
    let mut second = loc_track("v2", None);
    second.render_group = Some(1);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![second],
        }],
    };
    catalog
        .apply_delta(&delta, None)
        .expect("省略された targetLatency は不一致として拒否されない");
}

/// 同一 renderGroup で片方だけ buffers を宣言した delta 適用は受理されること
#[test]
fn apply_delta_group_buffers_omitted_accepted() {
    // draft-ietf-moq-msf-01 §5.2.9 (Buffers): フィールドが無く isLive=true なら
    // player がバッファを選んでよい (MAY)。delta 適用後の検証でも省略は不一致としない
    let mut first = loc_track("v1", None);
    first.render_group = Some(1);
    first.buffers = Some(MsfBuffers {
        target: Some(500),
        min: None,
        max: None,
    });
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(first);
    let mut second = loc_track("v2", None);
    second.render_group = Some(1);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![second],
        }],
    };
    catalog
        .apply_delta(&delta, None)
        .expect("省略された buffers は不一致として拒否されない");
}

/// 省略トラックを挟んでも宣言された targetLatency の不一致は delta 適用後の検証で拒否されること
#[test]
fn apply_delta_group_omitted_between_different_target_latency_rejected() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency): 省略トラックを挟んでも
    // 宣言された 2 値の不一致は delta 適用後の検証で検出すること
    let mut first = loc_track("v1", None);
    first.render_group = Some(1);
    first.target_latency = Some(500);
    let mut omitted = loc_track("v2", None);
    omitted.render_group = Some(1);
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(first);
    catalog.tracks.push(omitted);
    let mut third = loc_track("v3", None);
    third.render_group = Some(1);
    third.target_latency = Some(1000);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![third],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// 省略トラックを挟んでも宣言された buffers の不一致は delta 適用後の検証で拒否されること
#[test]
fn apply_delta_group_omitted_between_different_buffers_rejected() {
    // draft-ietf-moq-msf-01 §5.2.9 (Buffers): 省略トラックを挟んでも
    // 宣言された 2 値の不一致は delta 適用後の検証で検出すること
    let mut first = loc_track("v1", None);
    first.render_group = Some(1);
    first.buffers = Some(MsfBuffers {
        target: Some(500),
        min: None,
        max: None,
    });
    let mut omitted = loc_track("v2", None);
    omitted.render_group = Some(1);
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(first);
    catalog.tracks.push(omitted);
    let mut third = loc_track("v3", None);
    third.render_group = Some(1);
    third.buffers = Some(MsfBuffers {
        target: Some(1000),
        min: None,
        max: None,
    });
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![third],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// 同一 altGroup で片方だけ targetLatency を宣言した delta 適用は受理されること
#[test]
fn apply_delta_alt_group_target_latency_omitted_accepted() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency): delta 適用後の検証でも
    // altGroup の省略は不一致としない
    let mut first = loc_track("v1", None);
    first.alt_group = Some(2);
    first.target_latency = Some(500);
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(first);
    let mut second = loc_track("v2", None);
    second.alt_group = Some(2);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![second],
        }],
    };
    catalog
        .apply_delta(&delta, None)
        .expect("altGroup でも省略された targetLatency は拒否されない");
}

/// 同一 altGroup で片方だけ buffers を宣言した delta 適用は受理されること
#[test]
fn apply_delta_alt_group_buffers_omitted_accepted() {
    // draft-ietf-moq-msf-01 §5.2.9 (Buffers): delta 適用後の検証でも
    // altGroup の省略は不一致としない
    let mut first = loc_track("v1", None);
    first.alt_group = Some(2);
    first.buffers = Some(MsfBuffers {
        target: Some(500),
        min: None,
        max: None,
    });
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(first);
    let mut second = loc_track("v2", None);
    second.alt_group = Some(2);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![second],
        }],
    };
    catalog
        .apply_delta(&delta, None)
        .expect("altGroup でも省略された buffers は拒否されない");
}

/// altGroup でも省略トラックを挟んだ宣言値の不一致は delta 適用後の検証で拒否されること
#[test]
fn apply_delta_alt_group_omitted_between_different_buffers_rejected() {
    // draft-ietf-moq-msf-01 §5.2.9 (Buffers): altGroup でも省略トラックを挟んだ
    // 宣言値の不一致は delta 適用後の検証で検出すること
    let mut first = loc_track("v1", None);
    first.alt_group = Some(2);
    first.buffers = Some(MsfBuffers {
        target: Some(500),
        min: None,
        max: None,
    });
    let mut omitted = loc_track("v2", None);
    omitted.alt_group = Some(2);
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(first);
    catalog.tracks.push(omitted);
    let mut third = loc_track("v3", None);
    third.alt_group = Some(2);
    third.buffers = Some(MsfBuffers {
        target: Some(1000),
        min: None,
        max: None,
    });
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![third],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// altGroup でも省略トラックを挟んだ targetLatency の不一致は delta 適用後の検証で拒否されること
#[test]
fn apply_delta_alt_group_omitted_between_different_target_latency_rejected() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency): altGroup でも省略トラックを挟んだ
    // 宣言値の不一致は delta 適用後の検証で検出すること
    let mut first = loc_track("v1", None);
    first.alt_group = Some(2);
    first.target_latency = Some(500);
    let mut omitted = loc_track("v2", None);
    omitted.alt_group = Some(2);
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(first);
    catalog.tracks.push(omitted);
    let mut third = loc_track("v3", None);
    third.alt_group = Some(2);
    third.target_latency = Some(1000);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![third],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// clone が親から継承した宣言値を別 group へ持ち込み衝突する場合、delta 適用後の検証で拒否されること
#[test]
fn apply_delta_clone_inherited_group_conflict_rejected() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency): 適用前のカタログは正当であり、
    // clone が親から継承した targetLatency を renderGroup 1 に持ち込むことで初めて衝突する
    let mut parent = loc_track("v1", None);
    parent.target_latency = Some(500);
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(parent);
    let mut other = loc_track("v2", None);
    other.render_group = Some(1);
    other.target_latency = Some(1000);
    catalog.tracks.push(other);
    let mut clone = MsfCloneTrack::new("v3".to_string(), "v1".to_string());
    clone.render_group = Some(1);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(reason)) if reason.contains("renderGroup 1")
    ));
}

#[test]
fn apply_delta_clone_inherited_target_latency_and_buffers_rejected() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency) / §5.2.9 (Buffers):
    // 親の targetLatency と clone の buffers が継承解決後に共存するため reject される
    let mut parent = loc_track("v", None);
    parent.target_latency = Some(500);
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(parent);
    let mut clone = MsfCloneTrack::new("v2".to_string(), "v".to_string());
    clone.buffers = Some(MsfBuffers {
        target: Some(1000),
        min: None,
        max: None,
    });
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(reason))
            if reason == "targetLatency and buffers MUST NOT be present together"
    ));
}

#[test]
fn apply_delta_clone_inherited_buffers_and_target_latency_rejected() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency) / §5.2.9 (Buffers):
    // 親の buffers と clone の targetLatency が継承解決後に共存するため reject される
    let mut parent = loc_track("v", None);
    parent.buffers = Some(MsfBuffers {
        target: Some(1000),
        min: None,
        max: None,
    });
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(parent);
    let mut clone = MsfCloneTrack::new("v2".to_string(), "v".to_string());
    clone.target_latency = Some(500);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(reason))
            if reason == "targetLatency and buffers MUST NOT be present together"
    ));
}

#[test]
fn apply_delta_clone_is_live_false_avoids_coexistence() {
    // draft-ietf-moq-msf-01 §5.2.8 / §5.2.9: isLive=false なら双方とも無視され共存しない
    let mut parent = loc_track("v", None);
    parent.target_latency = Some(500);
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(parent);
    let mut clone = MsfCloneTrack::new("v2".to_string(), "v".to_string());
    clone.is_live = Some(false);
    clone.buffers = Some(MsfBuffers {
        target: Some(1000),
        min: None,
        max: None,
    });
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    };
    catalog
        .apply_delta(&delta, None)
        .expect("isLive=false なら継承した targetLatency も buffers も落とされる");
    let cloned = catalog
        .tracks
        .iter()
        .find(|t| t.name == "v2")
        .expect("clone 後のトラックが存在する");
    assert_eq!(cloned.target_latency, None);
    assert_eq!(cloned.buffers, None);
}

#[test]
fn apply_delta_add_colliding_with_publish_track_rejected() {
    // draft-ietf-moq-msf-01 §5.2.3 (Track name): tracks と publishTracks をまたいでも一意
    let mut catalog = MsfCatalog::new();
    catalog.publish_tracks.push(loc_track("v", Some("ns")));
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![loc_track("v", Some("ns"))],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, Some(&test_catalog_namespace("catalog-ns"))),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn apply_delta_clone_colliding_with_publish_track_rejected() {
    // draft-ietf-moq-msf-01 §5.2.3 (Track name): clone も publishTracks との一意性検査対象
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(loc_track("v", None));
    catalog.publish_tracks.push(loc_track("v2", None));
    let clone = MsfCloneTrack::new("v2".to_string(), "v".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn apply_delta_add_missing_init_ref_rejected() {
    // draft-ietf-moq-msf-01 §5.2.13 (Initialization reference): initRef は initDataList の id を指す
    let mut catalog = MsfCatalog::new();
    let mut track = loc_track("v", None);
    track.init_ref = Some("missing".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![track],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn apply_delta_add_matching_init_ref_accepted() {
    // initRef が initDataList の id と一致すれば適用できる
    let mut catalog = MsfCatalog::new();
    catalog.init_data_list.push(MsfInitData {
        id: "init1".to_string(),
        kind: MsfInitDataKind::Inline,
        data: "AAEC".to_string(),
    });
    let mut track = loc_track("v", None);
    track.init_ref = Some("init1".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![track],
        }],
    };
    catalog
        .apply_delta(&delta, None)
        .expect("initRef が initDataList に存在すれば成功する");
}

#[test]
fn apply_delta_add_on_complete_catalog_rejected() {
    // draft-ietf-moq-msf-01 §5.1.3 (Is Complete): 完了済みカタログへトラックを追加できない
    let mut catalog = MsfCatalog::new();
    catalog.is_complete = true;
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![loc_track("v", None)],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn apply_delta_clone_on_complete_catalog_rejected() {
    // draft-ietf-moq-msf-01 §5.1.3 (Is Complete): clone による追加も拒否される
    let mut catalog = MsfCatalog::new();
    catalog.is_complete = true;
    catalog.tracks.push(loc_track("v", None));
    let clone = MsfCloneTrack::new("v2".to_string(), "v".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn apply_delta_remove_on_complete_catalog_allowed() {
    // draft-ietf-moq-msf-01 §5.1.3 (Is Complete) は追加を禁じるのみで削除は禁じない
    let mut catalog = MsfCatalog::new();
    catalog.is_complete = true;
    catalog.tracks.push(loc_track("v", None));
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Remove {
            tracks: vec![MsfRemoveTrack::new("v".to_string())],
        }],
    };
    catalog
        .apply_delta(&delta, None)
        .expect("完了済みカタログからの削除は許可される");
    assert!(catalog.tracks.is_empty());
}

#[test]
fn apply_delta_operations_applied_in_order() {
    // draft-ietf-moq-msf-01 §5.3 (Delta updates): 操作は配列順に適用される
    let mut catalog = MsfCatalog::new();
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![
            MsfDeltaOperation::Add {
                tracks: vec![loc_track("v", None)],
            },
            MsfDeltaOperation::Remove {
                tracks: vec![MsfRemoveTrack::new("v".to_string())],
            },
            MsfDeltaOperation::Add {
                tracks: vec![loc_track("v", None)],
            },
        ],
    };
    catalog
        .apply_delta(&delta, None)
        .expect("操作は配列順に適用される");
    assert_eq!(catalog.tracks.len(), 1);
}

/// 複数トラックの add で 1 件が不正な場合、部分適用せずに拒否する
#[test]
fn apply_delta_add_batch_with_invalid_track_rejected_without_partial_apply() {
    let mut catalog = MsfCatalog::new();
    let mut invalid = loc_track("bad", Some("ns"));
    // isLive=true (loc_track の既定) なので trackDuration は不正
    invalid.track_duration = Some(1000);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![loc_track("ok", Some("ns")), invalid],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
    assert!(
        catalog.tracks.is_empty(),
        "不正トラックを含むバッチは部分適用しないこと"
    );
}

/// 有効な複数トラックの add は全件・投入順で追加される
#[test]
fn apply_delta_add_multiple_tracks_appends_in_order() {
    let mut catalog = MsfCatalog::new();
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![loc_track("v1", Some("ns")), loc_track("v2", Some("ns"))],
        }],
    };
    catalog
        .apply_delta(&delta, None)
        .expect("複数トラックの add は成功する");
    assert_eq!(catalog.tracks.len(), 2);
    assert_eq!(catalog.tracks[0].name, "v1");
    assert_eq!(catalog.tracks[1].name, "v2");
}

/// add 操作内でトラック名が重複する場合も部分適用せずに拒否する
#[test]
fn apply_delta_add_batch_with_duplicate_name_rejected_without_partial_apply() {
    let mut catalog = MsfCatalog::new();
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![loc_track("v", Some("ns")), loc_track("v", Some("ns"))],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
    assert!(
        catalog.tracks.is_empty(),
        "重複名を含むバッチは部分適用しないこと"
    );
}

/// clone の継承解決結果でも lang (BCP 47) を検証する
///
/// draft-ietf-moq-msf-01 §5.2.32 (Language): 継承した lang が不正な場合は
/// encode 時まで待たず apply_delta 時点で拒否する。
#[test]
fn apply_delta_clone_invalid_lang_rejected() {
    let mut catalog = MsfCatalog::new();
    let mut parent = loc_track("v", Some("ns"));
    parent.lang = Some("en-US".to_string());
    catalog.tracks.push(parent);

    // clone 側で不正な lang を指定する
    let mut clone = MsfCloneTrack::new("v2".to_string(), "v".to_string());
    clone.lang = Some("1".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    };
    // namespace は親と同じ "ns" を渡し、親が見つからないエラーと区別する
    assert!(matches!(
        catalog.apply_delta(&delta, Some(&test_catalog_namespace("ns"))),
        Err(MessageError::InvalidCatalog(reason)) if reason.contains("invalid language tag")
    ));
}

/// 親の lang が不正な場合も clone の継承解決で拒否する
#[test]
fn apply_delta_clone_inherited_invalid_lang_rejected() {
    let mut catalog = MsfCatalog::new();
    let mut parent = loc_track("v", Some("ns"));
    // 親は encode 前検証を通っていない (プログラムから直接組み立てられた) 想定
    parent.lang = Some("-en".to_string());
    catalog.tracks.push(parent);

    let clone = MsfCloneTrack::new("v2".to_string(), "v".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, Some(&test_catalog_namespace("ns"))),
        Err(MessageError::InvalidCatalog(reason)) if reason.contains("invalid language tag")
    ));
}

/// 途中の操作が失敗したらカタログを変更しない (原子性)
///
/// 例: [Add(有効), Remove(不存在)] は Err を返し、Add の結果も残らない。
#[test]
fn apply_delta_is_atomic_when_later_operation_fails() {
    let mut catalog = MsfCatalog::new();
    let delta = MsfDeltaUpdate {
        generated_at: 1000,
        operations: vec![
            MsfDeltaOperation::Add {
                tracks: vec![loc_track("v", Some("ns"))],
            },
            MsfDeltaOperation::Remove {
                tracks: vec![MsfRemoveTrack {
                    name: "missing".to_string(),
                    namespace: Some("ns".to_string()),
                }],
            },
        ],
    };
    let before = catalog.clone();
    assert!(matches!(
        catalog.apply_delta(&delta, Some(&test_catalog_namespace("ns"))),
        Err(MessageError::InvalidCatalog(_))
    ));
    assert_eq!(
        catalog, before,
        "Err を返したときカタログは呼び出し前と一致すること"
    );
}

/// 成功時は複製への適用結果がそのまま反映される (tracks / generatedAt)
#[test]
fn apply_delta_applies_all_operations_on_success() {
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(loc_track("v", Some("ns")));
    let delta = MsfDeltaUpdate {
        generated_at: 2000,
        operations: vec![
            MsfDeltaOperation::Clone {
                tracks: vec![MsfCloneTrack::new("v2".to_string(), "v".to_string())],
            },
            MsfDeltaOperation::Add {
                tracks: vec![loc_track("a", Some("ns"))],
            },
        ],
    };
    catalog
        .apply_delta(&delta, Some(&test_catalog_namespace("ns")))
        .expect("有効な delta は成功する");
    assert_eq!(catalog.generated_at, Some(2000));
    assert_eq!(catalog.tracks.len(), 3);
    assert!(catalog.tracks.iter().any(|t| t.name == "v2"));
    assert!(catalog.tracks.iter().any(|t| t.name == "a"));
}

/// namespace 指定付きの remove 参照を作るヘルパー
fn remove_ref(name: &str, namespace: &str) -> MsfRemoveTrack {
    MsfRemoveTrack {
        name: name.to_string(),
        namespace: Some(namespace.to_string()),
    }
}

/// 同一 (namespace, name) の属性変更を検出するための codec / width を持つトラック
///
/// codec `av01` は video と判定されるため、§5.2.22 (Maximum Bitrate) の MUST を満たす
/// bitrate も持たせる。
fn video_track(name: &str, namespace: &str) -> MsfTrack {
    let mut track = loc_track(name, Some(namespace));
    track.codec = Some("av01".to_string());
    track.bitrate = Some(1_000_000);
    track.width = Some(1920);
    track.height = Some(1080);
    track
}

/// namespace を省略した video_track (カタログの namespace を継承する)
fn video_track_omitted_ns(name: &str) -> MsfTrack {
    let mut track = video_track(name, "cns");
    track.namespace = None;
    track
}

/// remove → add で属性を変更した delta update は拒否される
///
/// draft-ietf-moq-msf-01 §5.3 (Delta updates): "The tuple of Track Namespace and Track Name
/// defines a fixed set of Track attributes which MUST NOT be modified after being declared.
/// To modify any attribute, a new track with a different Namespace|Name tuple is created by
/// Adding or Cloning and then the old track is removed."
/// remove → add で属性を変更した delta update を適用できる
///
/// draft-ietf-moq-msf-02 §5.3 (Delta updates) は v01 の属性不変 MUST を削除した。
#[test]
fn apply_delta_readd_with_changed_attribute_succeeds() {
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(video_track("v", "ns"));

    let mut changed = video_track("v", "ns");
    changed.codec = Some("vp09".to_string());
    changed.label = Some("main".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![
            MsfDeltaOperation::Remove {
                tracks: vec![remove_ref("v", "ns")],
            },
            MsfDeltaOperation::Add {
                tracks: vec![changed],
            },
        ],
    };
    catalog
        .apply_delta(&delta, None)
        .expect("属性変更を伴う再追加を適用できること");
    assert_eq!(catalog.tracks.len(), 1);
    assert_eq!(catalog.tracks[0].codec.as_deref(), Some("vp09"));
    assert_eq!(catalog.tracks[0].label.as_deref(), Some("main"));
}

/// remove → add で宣言済みの targetLatency を省略した再追加を適用できる
#[test]
fn apply_delta_readd_omitted_target_latency_succeeds() {
    let mut catalog = MsfCatalog::new();
    let mut original = video_track("v", "ns");
    original.render_group = Some(1);
    original.target_latency = Some(500);
    catalog.tracks.push(original);

    let mut omitted = video_track("v", "ns");
    omitted.render_group = Some(1);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![
            MsfDeltaOperation::Remove {
                tracks: vec![remove_ref("v", "ns")],
            },
            MsfDeltaOperation::Add {
                tracks: vec![omitted],
            },
        ],
    };
    catalog
        .apply_delta(&delta, None)
        .expect("targetLatency の省略を伴う再追加を適用できること");
    assert_eq!(catalog.tracks[0].target_latency, None);
}

/// remove → clone で同じ (namespace, name) を属性変更して再追加できる
#[test]
fn apply_delta_readd_by_clone_with_changed_attribute_succeeds() {
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(video_track("v", "ns"));
    let mut parent = video_track("src", "ns");
    parent.codec = Some("vp09".to_string());
    parent.width = Some(1280);
    catalog.tracks.push(parent);

    let mut clone = MsfCloneTrack::new("v".to_string(), "src".to_string());
    clone.parent_namespace = Some("ns".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![
            MsfDeltaOperation::Remove {
                tracks: vec![remove_ref("v", "ns")],
            },
            MsfDeltaOperation::Clone {
                tracks: vec![clone],
            },
        ],
    };
    catalog
        .apply_delta(&delta, None)
        .expect("clone による属性変更を伴う再追加を適用できること");
    assert_eq!(catalog.tracks.len(), 2);
    assert_eq!(catalog.tracks[0].codec.as_deref(), Some("vp09"));
    assert_eq!(catalog.tracks[0].width, Some(1280));
}

#[test]
fn apply_delta_readd_with_is_live_regression_rejected() {
    let mut catalog = MsfCatalog::new();
    let mut offline = loc_track("v", Some("ns"));
    offline.is_live = false;
    catalog.tracks.push(offline);

    let mut live = loc_track("v", Some("ns"));
    live.is_live = true;
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![
            MsfDeltaOperation::Remove {
                tracks: vec![remove_ref("v", "ns")],
            },
            MsfDeltaOperation::Add { tracks: vec![live] },
        ],
    };
    let Err(MessageError::InvalidCatalog(reason)) = catalog.apply_delta(&delta, None) else {
        panic!("isLive の逆行は InvalidCatalog であること");
    };
    assert!(
        reason.contains("isLive"),
        "§5.2.7 違反を述べること: {reason}"
    );
    assert!(
        !reason.contains("attributes MUST NOT be modified"),
        "属性差分の文言と区別できること: {reason}"
    );
}

/// delta update をまたぐ remove → add でも属性変更が検出される
/// delta update をまたぐ remove → add でも属性を変更できる
#[test]
fn apply_delta_readd_attribute_change_across_deltas_succeeds() {
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(video_track("v", "ns"));

    let remove_delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Remove {
            tracks: vec![remove_ref("v", "ns")],
        }],
    };
    catalog
        .apply_delta(&remove_delta, None)
        .expect("remove は成功する");
    assert!(catalog.tracks.is_empty());

    let mut changed = video_track("v", "ns");
    changed.codec = Some("vp09".to_string());
    let add_delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![changed],
        }],
    };
    catalog
        .apply_delta(&add_delta, None)
        .expect("delta をまたぐ属性変更を適用できること");
    assert_eq!(catalog.tracks[0].codec.as_deref(), Some("vp09"));
}

#[test]
fn apply_delta_readd_with_same_attributes_succeeds() {
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(video_track("v", "ns"));

    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![
            MsfDeltaOperation::Remove {
                tracks: vec![remove_ref("v", "ns")],
            },
            MsfDeltaOperation::Add {
                tracks: vec![video_track("v", "ns")],
            },
        ],
    };
    catalog
        .apply_delta(&delta, None)
        .expect("同一属性の再追加は成功する");
    assert_eq!(catalog.tracks.len(), 1);
    assert_eq!(catalog.tracks[0].codec.as_deref(), Some("av01"));
}

/// isLive=false のトラックは targetLatency の有無だけが異なる再追加を受理する
///
/// draft-ietf-moq-msf-01 §5.2.8 (Target latency) は isLive=false のとき targetLatency を
/// 無視するため、比較の両辺で isLive=false なら targetLatency を `None` とみなす。
#[test]
fn apply_delta_readd_is_live_false_ignores_target_latency() {
    let mut catalog = MsfCatalog::new();
    let mut original = loc_track("v", Some("ns"));
    original.is_live = false;
    catalog.tracks.push(original);

    // 再追加では targetLatency を付ける (isLive=false のため無視される)
    let mut readded = loc_track("v", Some("ns"));
    readded.is_live = false;
    readded.target_latency = Some(500);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![
            MsfDeltaOperation::Remove {
                tracks: vec![remove_ref("v", "ns")],
            },
            MsfDeltaOperation::Add {
                tracks: vec![readded],
            },
        ],
    };
    catalog
        .apply_delta(&delta, None)
        .expect("isLive=false では targetLatency の有無だけの差異は属性変更とみなさない");
    assert_eq!(catalog.tracks.len(), 1);
    assert_eq!(catalog.tracks[0].target_latency, Some(500));
}

/// namespace を省略したトラックでも、継承した namespace で再追加の属性変更を検出する
///
/// draft-ietf-moq-msf-01 §5.2.2 (Track namespace): namespace 省略時はカタログの namespace を
/// 継承する。履歴のキーも同じ規則で解決しないと照合が外れる。
/// namespace を省略したトラックでも、継承した namespace で再追加できる
#[test]
fn apply_delta_readd_attribute_change_with_inherited_namespace_succeeds() {
    let mut catalog = MsfCatalog::new();
    let mut original = loc_track("v", None);
    original.codec = Some("av01".to_string());
    original.bitrate = Some(1_000_000);
    catalog.tracks.push(original);

    let mut changed = loc_track("v", None);
    changed.codec = Some("vp09".to_string());
    changed.bitrate = Some(1_000_000);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![
            MsfDeltaOperation::Remove {
                tracks: vec![remove_ref("v", "cns")],
            },
            MsfDeltaOperation::Add {
                tracks: vec![changed],
            },
        ],
    };
    catalog
        .apply_delta(&delta, Some(&test_catalog_namespace("cns")))
        .expect("継承した namespace でも属性変更を適用できること");
    assert_eq!(catalog.tracks[0].codec.as_deref(), Some("vp09"));
}

/// 省略した namespace と明示した namespace は同じ tuple として扱われる
#[test]
fn apply_delta_readd_with_explicit_namespace_matches_inherited() {
    let mut catalog = MsfCatalog::new();
    let mut original = loc_track("v", None);
    original.codec = Some("av01".to_string());
    original.bitrate = Some(1_000_000);
    catalog.tracks.push(original);

    let mut changed = video_track("v", "cns");
    changed.codec = Some("vp09".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![
            MsfDeltaOperation::Remove {
                tracks: vec![remove_ref("v", "cns")],
            },
            MsfDeltaOperation::Add {
                tracks: vec![changed],
            },
        ],
    };
    catalog
        .apply_delta(&delta, Some(&test_catalog_namespace("cns")))
        .expect("省略形と明示形を同じ tuple として扱うこと");
    assert_eq!(catalog.tracks[0].codec.as_deref(), Some("vp09"));
}

#[test]
fn apply_delta_readd_with_different_namespace_representation_succeeds() {
    // 宣言は明示、再追加は省略
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(video_track("v", "cns"));
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![
            MsfDeltaOperation::Remove {
                tracks: vec![MsfRemoveTrack::new("v".to_string())],
            },
            MsfDeltaOperation::Add {
                tracks: vec![video_track_omitted_ns("v")],
            },
        ],
    };
    catalog
        .apply_delta(&delta, Some(&test_catalog_namespace("cns")))
        .expect("明示形から省略形への再追加は成功する");
    assert_eq!(catalog.tracks.len(), 1);
    assert_eq!(catalog.tracks[0].codec.as_deref(), Some("av01"));

    // 宣言は省略、再追加は明示
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(video_track_omitted_ns("v"));
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![
            MsfDeltaOperation::Remove {
                tracks: vec![remove_ref("v", "cns")],
            },
            MsfDeltaOperation::Add {
                tracks: vec![video_track("v", "cns")],
            },
        ],
    };
    catalog
        .apply_delta(&delta, Some(&test_catalog_namespace("cns")))
        .expect("省略形から明示形への再追加は成功する");
    assert_eq!(catalog.tracks.len(), 1);
    assert_eq!(catalog.tracks[0].codec.as_deref(), Some("av01"));
}

/// isLive=false のトラックは buffers の有無だけが異なる再追加を受理する
///
/// draft-ietf-moq-msf-01 §5.2.9 (Buffers) は isLive=false のとき buffers を無視するため、
/// 比較の両辺で isLive=false なら buffers を `None` とみなす。
#[test]
fn apply_delta_readd_is_live_false_ignores_buffers() {
    let mut catalog = MsfCatalog::new();
    let mut original = loc_track("v", Some("ns"));
    original.is_live = false;
    catalog.tracks.push(original);

    let mut readded = loc_track("v", Some("ns"));
    readded.is_live = false;
    readded.buffers = Some(MsfBuffers {
        target: Some(100),
        min: None,
        max: Some(200),
    });
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![
            MsfDeltaOperation::Remove {
                tracks: vec![remove_ref("v", "ns")],
            },
            MsfDeltaOperation::Add {
                tracks: vec![readded],
            },
        ],
    };
    catalog
        .apply_delta(&delta, None)
        .expect("isLive=false では buffers の有無だけの差異は属性変更とみなさない");
    assert_eq!(catalog.tracks.len(), 1);
    assert_eq!(
        catalog.tracks[0].buffers.as_ref().and_then(|b| b.target),
        Some(100)
    );
}

/// 多フィールドのカタログ namespace を継承した remove が同じ Full Track Name を解決すること
///
/// draft-ietf-moq-msf-02 §5.2.2 (Track namespace) / §11.2.2 (Comparing names): namespace の
/// 比較は `/` 区切り文字列ではなく、写像後の Track Namespace のバイト値で行う。
#[test]
fn apply_delta_remove_inherits_multi_field_catalog_namespace() {
    let mut catalog = MsfCatalog::new();
    catalog
        .tracks
        .push(loc_track("video", Some("customer/livestream/123")));
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Remove {
            tracks: vec![MsfRemoveTrack::new("video".to_string())],
        }],
    };
    catalog
        .apply_delta(
            &delta,
            Some(&test_catalog_namespace("customer/livestream/123")),
        )
        .expect("継承した multi-field namespace で remove できること");
    assert!(catalog.tracks.is_empty(), "track が削除されること");
}

/// 省略 namespace を継承した add が明示 namespace の同名 track と重複すること
#[test]
fn apply_delta_add_duplicates_explicit_namespace_track() {
    let mut catalog = MsfCatalog::new();
    catalog
        .tracks
        .push(loc_track("video", Some("customer/livestream/123")));
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            // namespace を省略すると catalog track の namespace を継承する
            tracks: vec![loc_track("video", None)],
        }],
    };
    assert!(
        matches!(
            catalog.apply_delta(
                &delta,
                Some(&test_catalog_namespace("customer/livestream/123")),
            ),
            Err(MessageError::InvalidCatalog(_))
        ),
        "継承を解決すると同じ Full Track Name になるため重複として拒否すること"
    );
    assert_eq!(catalog.tracks.len(), 1, "拒否時は track が増えないこと");
}

/// namespace が異なる同名 track の add は別の Full Track Name として受理すること
#[test]
fn apply_delta_add_same_name_different_multi_field_namespace() {
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(loc_track("video", None));
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![loc_track("video", Some("customer/livestream/456"))],
        }],
    };
    catalog
        .apply_delta(
            &delta,
            Some(&test_catalog_namespace("customer/livestream/123")),
        )
        .expect("namespace が異なる同名 track は受理されること");
    assert_eq!(catalog.tracks.len(), 2);
}

/// 省略 namespace を継承した clone が multi-field namespace の親を解決すること
#[test]
fn apply_delta_clone_inherits_multi_field_catalog_namespace() {
    let mut catalog = MsfCatalog::new();
    catalog
        .tracks
        .push(loc_track("video", Some("customer/livestream/123")));
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![MsfCloneTrack::new(
                "video-hd".to_string(),
                "video".to_string(),
            )],
        }],
    };
    catalog
        .apply_delta(
            &delta,
            Some(&test_catalog_namespace("customer/livestream/123")),
        )
        .expect("継承した multi-field namespace で clone できること");
    assert_eq!(catalog.tracks.len(), 2);
    assert_eq!(
        catalog.tracks[1].namespace.as_deref(),
        Some("customer/livestream/123")
    );
}

/// clone が namespace を省略したときに親トラックの namespace を継承すること
///
/// draft-ietf-moq-msf-02 §5.1.6 (Delta update): "The cloned track inherits all attributes from
/// the parent except the Track Name which MUST be new." namespace も属性であり、clone 自身が
/// namespace を宣言しない場合は親の namespace を継承する (§5.2.2 の catalog track 継承より
/// clone 固有の規則を優先する)。
#[test]
fn apply_delta_clone_inherits_parent_namespace() {
    let mut catalog = MsfCatalog::new();
    catalog
        .tracks
        .push(loc_track("video", Some("parent/group/1")));
    let mut clone = MsfCloneTrack::new("video-hd".to_string(), "video".to_string());
    clone.parent_namespace = Some("parent/group/1".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    };
    catalog
        .apply_delta(&delta, Some(&test_catalog_namespace("catalog/group/1")))
        .expect("親 namespace を継承した clone を適用できること");
    assert_eq!(
        catalog.tracks[1].namespace.as_deref(),
        Some("parent/group/1"),
        "clone は catalog track ではなく親トラックの namespace を継承すること"
    );
}

/// add する track の Full Track Name が 4,096 バイトを超える場合に拒否すること
///
/// draft-ietf-moq-transport-23 §8.7 (Track Namespace Structure): namespace と name の合計が
/// 上限を超える track はカタログ不正として拒否する。
#[test]
fn apply_delta_add_rejects_overlong_full_track_name() {
    let mut catalog = MsfCatalog::new();
    let mut track = loc_track("b", Some(&"a".repeat(4096)));
    track.name = "b".to_string();
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![track],
        }],
    };
    assert!(
        matches!(
            catalog.apply_delta(&delta, None),
            Err(MessageError::InvalidCatalog(_))
        ),
        "add の Full Track Name 4,097 バイトを拒否すること"
    );
    assert!(catalog.tracks.is_empty(), "拒否時は track が増えないこと");
}

/// 継承する catalog namespace と name の合計が 4,096 バイトを超える場合に拒否すること
#[test]
fn apply_delta_rejects_overlong_inherited_catalog_namespace() {
    let base = test_catalog_namespace(&"a".repeat(4096));
    let mut catalog = MsfCatalog::new();
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![loc_track("b", None)],
        }],
    };
    assert!(
        matches!(
            catalog.apply_delta(&delta, Some(&base)),
            Err(MessageError::InvalidCatalog(_))
        ),
        "継承した catalog namespace との合計 4,097 バイトを拒否すること"
    );
    assert!(catalog.tracks.is_empty(), "拒否時は track が増えないこと");
}

/// clone が親 namespace を継承した結果が 4,096 バイトを超える場合に拒否すること
#[test]
fn apply_delta_clone_rejects_overlong_inherited_parent_namespace() {
    let parent_namespace = "a".repeat(4095);
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(loc_track("p", Some(&parent_namespace)));
    let mut clone = MsfCloneTrack::new("bb".to_string(), "p".to_string());
    clone.parent_namespace = Some(parent_namespace);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    };
    assert!(
        matches!(
            catalog.apply_delta(&delta, None),
            Err(MessageError::InvalidCatalog(_))
        ),
        "clone の実効 Full Track Name 4,097 バイトを拒否すること"
    );
    assert_eq!(catalog.tracks.len(), 1, "拒否時は track が増えないこと");
}

/// clone の namespace と parentNamespace を両方省略した場合に、継承する catalog namespace で
/// Full Track Name の長さを検証すること
///
/// encode / decode は catalog namespace を知らないため name 単体までしか検証できないが、
/// apply は実際の catalog namespace を解決して最終検証する。
#[test]
fn apply_delta_clone_both_namespaces_omitted_checks_inherited_catalog_namespace() {
    let base = test_catalog_namespace(&"a".repeat(4095));
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(loc_track("p", None));
    let clone = MsfCloneTrack::new("bb".to_string(), "p".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    };
    assert!(
        matches!(
            catalog.apply_delta(&delta, Some(&base)),
            Err(MessageError::InvalidCatalog(_))
        ),
        "継承した catalog namespace との合計 4,097 バイトを拒否すること"
    );
    assert_eq!(catalog.tracks.len(), 1, "拒否時は track が増えないこと");
}

/// clone の packagingMetadata が明示値で上書きされ、省略時は親から継承されること
///
/// draft-ietf-moq-msf-02 §5.2.5 (Packaging metadata) / §5.1.6 (Delta update): clone は
/// 親の全属性を継承し、再定義された属性だけを上書きする。
#[test]
fn apply_delta_clone_inherits_or_overrides_packaging_metadata() {
    let mut catalog = MsfCatalog::new();
    let mut parent = loc_track("parent", None);
    parent.packaging_metadata = Some("streamMapping=object".to_string());
    catalog.tracks.push(parent);

    let mut inherited = MsfCloneTrack::new("inherited".to_string(), "parent".to_string());
    inherited.packaging_metadata = None;
    let mut overridden = MsfCloneTrack::new("overridden".to_string(), "parent".to_string());
    overridden.packaging_metadata = Some("streamMapping=group".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![inherited, overridden],
        }],
    };
    catalog
        .apply_delta(&delta, None)
        .expect("packagingMetadata の継承と上書きを適用できること");
    assert_eq!(
        catalog.tracks[1].packaging_metadata.as_deref(),
        Some("streamMapping=object"),
        "clone が省略した packagingMetadata は親から継承すること"
    );
    assert_eq!(
        catalog.tracks[2].packaging_metadata.as_deref(),
        Some("streamMapping=group"),
        "clone が明示した packagingMetadata は親を上書きすること"
    );
}

/// delta 適用後に未解決の depends が残る場合は拒否されること
#[test]
fn apply_delta_rejects_unresolved_depends() {
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(loc_track("existing", None));
    let mut added = loc_track("added", None);
    added.depends = vec![MsfDependency::TrackName("missing".to_string())];
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![added],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// delta 適用後に media timeline の renderGroup 参照規則違反が残る場合は拒否されること
#[test]
fn apply_delta_rejects_media_timeline_naming_all_render_group_tracks() {
    let mut catalog = MsfCatalog::new();
    let mut v1 = loc_track("v1", None);
    v1.render_group = Some(1);
    let mut v2 = loc_track("v2", None);
    v2.render_group = Some(1);
    catalog.tracks.push(v1);
    catalog.tracks.push(v2);

    let mut timeline = loc_track("timeline", None);
    timeline.packaging = MsfPackaging::MediaTimeline;
    timeline.depends = vec![
        MsfDependency::TrackName("v1".to_string()),
        MsfDependency::TrackName("v2".to_string()),
    ];
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![timeline],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// clone が accessibleWindowTime を継承 / 上書きできること
#[test]
fn apply_delta_clone_inherits_or_overrides_accessible_window_time() {
    let mut catalog = MsfCatalog::new();
    let mut parent = loc_track("parent", None);
    parent.alt_group = Some(1);
    parent.accessible_window_time = Some(60_000);
    catalog.tracks.push(parent);

    let mut inherited = MsfCloneTrack::new("inherited".to_string(), "parent".to_string());
    inherited.accessible_window_time = None;
    let mut overridden = MsfCloneTrack::new("overridden".to_string(), "parent".to_string());
    overridden.alt_group = Some(2);
    overridden.accessible_window_time = Some(120_000);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![inherited, overridden],
        }],
    };
    catalog
        .apply_delta(&delta, None)
        .expect("accessibleWindowTime の継承と上書きを適用できること");
    assert_eq!(
        catalog.tracks[1].accessible_window_time,
        Some(60_000),
        "clone が省略した値は親から継承すること"
    );
    assert_eq!(
        catalog.tracks[2].accessible_window_time,
        Some(120_000),
        "clone が明示した値は親を上書きすること"
    );
}

/// delta 適用後に同じ altGroup の accessibleWindowTime が不一致なら拒否されること
#[test]
fn apply_delta_rejects_accessible_window_time_mismatch() {
    let mut catalog = MsfCatalog::new();
    let mut existing = loc_track("existing", None);
    existing.alt_group = Some(1);
    existing.accessible_window_time = Some(60_000);
    catalog.tracks.push(existing);

    let mut added = loc_track("added", None);
    added.alt_group = Some(1);
    added.accessible_window_time = Some(120_000);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![added],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// update 操作が既存 track の属性を上書きすること
#[test]
fn apply_delta_update_overrides_attributes() {
    let mut catalog = MsfCatalog::new();
    let mut parent = loc_track("v", None);
    parent.label = Some("old".to_string());
    parent.codec = Some("av01".to_string());
    parent.bitrate = Some(1_000);
    catalog.tracks.push(parent);

    let mut update = MsfCloneTrack::new("v".to_string(), "v".to_string());
    update.label = Some("new".to_string());
    update.role = Some("video".to_string());
    update.framerate = Some(30.0);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Update {
            tracks: vec![MsfUpdateTrack { track: update }],
        }],
    };
    catalog
        .apply_delta(&delta, None)
        .expect("update を適用できること");
    let track = &catalog.tracks[0];
    assert_eq!(track.label.as_deref(), Some("new"));
    assert_eq!(track.role.as_deref(), Some("video"));
    assert_eq!(track.framerate, Some(30.0));
    assert_eq!(
        track.codec.as_deref(),
        Some("av01"),
        "未指定の属性は継承すること"
    );
    assert_eq!(track.bitrate, Some(1_000), "未指定の属性は継承すること");
}

/// update の parentName が存在しない場合は拒否されること
#[test]
fn apply_delta_update_missing_parent_rejected() {
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(loc_track("v", None));
    let mut update = MsfCloneTrack::new("missing".to_string(), "missing".to_string());
    update.label = Some("new".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Update {
            tracks: vec![MsfUpdateTrack { track: update }],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// update で isLive を false から true に変更できないこと
#[test]
fn apply_delta_update_rejects_is_live_false_to_true() {
    let mut catalog = MsfCatalog::new();
    let mut parent = loc_track("v", None);
    parent.is_live = false;
    catalog.tracks.push(parent);
    let mut update = MsfCloneTrack::new("v".to_string(), "v".to_string());
    update.is_live = Some(true);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Update {
            tracks: vec![MsfUpdateTrack { track: update }],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// update のマージ結果が通常の track の MUST に反する場合は拒否されること
#[test]
fn apply_delta_update_validates_merged_track() {
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(loc_track("v", None));

    // role=video にするのに framerate を指定しない
    let mut missing_framerate = MsfCloneTrack::new("v".to_string(), "v".to_string());
    missing_framerate.role = Some("video".to_string());
    missing_framerate.codec = Some("av01".to_string());
    missing_framerate.bitrate = Some(1_000);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Update {
            tracks: vec![MsfUpdateTrack {
                track: missing_framerate,
            }],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));

    // isLive=true の track に trackDuration を追加する
    let mut with_duration = MsfCloneTrack::new("v".to_string(), "v".to_string());
    with_duration.track_duration = Some(1_000);
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Update {
            tracks: vec![MsfUpdateTrack {
                track: with_duration,
            }],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// update の namespace が解決後の対象 namespace と一致しない場合は拒否されること
#[test]
fn apply_delta_update_rejects_namespace_change() {
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(loc_track("v", Some("a")));
    let mut update = MsfCloneTrack::new("v".to_string(), "v".to_string());
    update.parent_namespace = Some("a".to_string());
    update.namespace = Some("a".to_string());
    // parentNamespace と namespace は一致するが、対象 track と一致するので変更にはならない。
    // ここでは parentNamespace は一致、namespace だけ別にするケースを作れないため、
    // 対象 track の namespace を変える意図の入力を直接は表現できない。別ケースで確認する。
    update.label = Some("new".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Update {
            tracks: vec![MsfUpdateTrack { track: update }],
        }],
    };
    catalog
        .apply_delta(&delta, None)
        .expect("同じ namespace の update は適用できること");

    // parentNamespace を省略し、namespace に別の値を指定する
    let mut update = MsfCloneTrack::new("v".to_string(), "v".to_string());
    update.namespace = Some("b".to_string());
    update.label = Some("new".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Update {
            tracks: vec![MsfUpdateTrack { track: update }],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// 削除のたびに履歴を最新の isLive で上書きすること
#[test]
fn removed_tracks_history_keeps_latest_is_live() {
    let mut catalog = MsfCatalog::new();
    let mut live = loc_track("v", Some("ns"));
    live.is_live = true;
    catalog.tracks.push(live);

    let remove = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Remove {
            tracks: vec![remove_ref("v", "ns")],
        }],
    };
    catalog
        .apply_delta(&remove, None)
        .expect("remove は成功する");

    let mut offline = loc_track("v", Some("ns"));
    offline.is_live = false;
    let add_offline = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![offline],
        }],
    };
    catalog
        .apply_delta(&add_offline, None)
        .expect("isLive=false の再追加は成功する");

    let remove_again = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Remove {
            tracks: vec![remove_ref("v", "ns")],
        }],
    };
    catalog
        .apply_delta(&remove_again, None)
        .expect("2 回目の remove は成功する");
    assert!(
        catalog.removed_tracks.values().all(|track| !track.is_live),
        "履歴は削除時点の最新 isLive=false を保持すること"
    );
}

/// true → remove → false で再追加 → remove → true で再追加、の順で拒否されること
#[test]
fn apply_delta_readd_is_live_regression_uses_latest_history() {
    let mut catalog = MsfCatalog::new();
    let mut live = loc_track("v", Some("ns"));
    live.is_live = true;
    catalog.tracks.push(live);

    let remove = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Remove {
            tracks: vec![remove_ref("v", "ns")],
        }],
    };
    catalog
        .apply_delta(&remove, None)
        .expect("remove は成功する");

    let mut offline = loc_track("v", Some("ns"));
    offline.is_live = false;
    let add_offline = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![offline],
        }],
    };
    catalog
        .apply_delta(&add_offline, None)
        .expect("isLive=false の再追加は成功する");

    let remove_again = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Remove {
            tracks: vec![remove_ref("v", "ns")],
        }],
    };
    catalog
        .apply_delta(&remove_again, None)
        .expect("2 回目の remove は成功する");

    let mut live_again = loc_track("v", Some("ns"));
    live_again.is_live = true;
    let add_live = MsfDeltaUpdate {
        generated_at: 1,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![live_again],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&add_live, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}
