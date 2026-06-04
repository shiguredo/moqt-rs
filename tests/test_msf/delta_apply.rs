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
        generated_at: Some(1000),
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![loc_track("v", Some("ns"))],
        }],
    };
    catalog
        .apply_delta(&delta, Some("catalog-ns"))
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
        generated_at: None,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![loc_track("v", Some("ns"))],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn apply_delta_remove_deletes_track() {
    // draft-ietf-moq-msf-01 §5.1.6 (Delta update): remove は既存トラックを削除する
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(loc_track("v", None));
    let delta = MsfDeltaUpdate {
        generated_at: None,
        operations: vec![MsfDeltaOperation::Remove {
            tracks: vec![MsfRemoveTrack::new("v".to_string())],
        }],
    };
    catalog
        .apply_delta(&delta, Some("catalog-ns"))
        .expect("remove は成功する");
    assert!(catalog.tracks.is_empty());
}

#[test]
fn apply_delta_remove_missing_rejected() {
    // 未宣言トラックの remove は reject される
    let mut catalog = MsfCatalog::new();
    let delta = MsfDeltaUpdate {
        generated_at: None,
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
        generated_at: None,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    };
    catalog
        .apply_delta(&delta, Some("catalog-ns"))
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

#[test]
fn apply_delta_clone_uses_parent_namespace() {
    // draft-ietf-moq-msf-01 §5.2.34 (Parent namespace): parentNamespace で親を特定する
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(loc_track("v", Some("other-ns")));
    let mut clone = MsfCloneTrack::new("v2".to_string(), "v".to_string());
    clone.parent_namespace = Some("other-ns".to_string());
    clone.namespace = Some("other-ns".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: None,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    };
    catalog
        .apply_delta(&delta, Some("catalog-ns"))
        .expect("clone は成功する");
    assert!(catalog.tracks.iter().any(|t| t.name == "v2"));
}

#[test]
fn apply_delta_clone_missing_parent_rejected() {
    // 親トラックが見つからない clone は reject される
    let mut catalog = MsfCatalog::new();
    let clone = MsfCloneTrack::new("v2".to_string(), "missing".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: None,
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
        generated_at: None,
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
    clone.depends = Some(vec!["v".to_string()]);
    let delta = MsfDeltaUpdate {
        generated_at: None,
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
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency): 同一 renderGroup の targetLatency は一致
    let mut first = loc_track("v1", None);
    first.render_group = Some(1);
    first.target_latency = Some(500);
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(first);
    let mut second = loc_track("v2", None);
    second.render_group = Some(1);
    second.target_latency = Some(1000);
    let delta = MsfDeltaUpdate {
        generated_at: None,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![second],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, None),
        Err(MessageError::InvalidCatalog(_))
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
        generated_at: None,
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
        generated_at: None,
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
        generated_at: None,
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
        generated_at: None,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![loc_track("v", Some("ns"))],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, Some("catalog-ns")),
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
        generated_at: None,
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
        generated_at: None,
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
        generated_at: None,
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
        generated_at: None,
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
        generated_at: None,
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
        generated_at: None,
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
        generated_at: None,
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
