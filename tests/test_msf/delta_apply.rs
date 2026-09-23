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

/// draft-ietf-moq-msf-01 §5.2.35 (Track duration): isLive=true なら trackDuration は禁止。
/// add 経路でも encode 時まで待たず apply_delta 時点で拒否する。
#[test]
fn apply_delta_add_is_live_with_track_duration_rejected() {
    let mut catalog = MsfCatalog::new();
    // loc_track は isLive=true で作られる
    let mut track = loc_track("v", Some("ns"));
    track.track_duration = Some(1000);
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
        generated_at: None,
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

/// 複数トラックの add で 1 件が不正な場合、部分適用せずに拒否する
#[test]
fn apply_delta_add_batch_with_invalid_track_rejected_without_partial_apply() {
    let mut catalog = MsfCatalog::new();
    let mut invalid = loc_track("bad", Some("ns"));
    // isLive=true (loc_track の既定) なので trackDuration は不正
    invalid.track_duration = Some(1000);
    let delta = MsfDeltaUpdate {
        generated_at: None,
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
        generated_at: None,
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
        generated_at: None,
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
        generated_at: None,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    };
    // namespace は親と同じ "ns" を渡し、親が見つからないエラーと区別する
    assert!(matches!(
        catalog.apply_delta(&delta, Some("ns")),
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
        generated_at: None,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    };
    assert!(matches!(
        catalog.apply_delta(&delta, Some("ns")),
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
        generated_at: Some(1000),
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
        catalog.apply_delta(&delta, Some("ns")),
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
        generated_at: Some(2000),
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
        .apply_delta(&delta, Some("ns"))
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
fn video_track(name: &str, namespace: &str) -> MsfTrack {
    let mut track = loc_track(name, Some(namespace));
    track.codec = Some("av01".to_string());
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
#[test]
fn apply_delta_readd_with_changed_attribute_rejected() {
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(video_track("v", "ns"));

    // 1 回の delta で remove → add し、label と codec を変更する
    let mut changed = video_track("v", "ns");
    changed.codec = Some("vp09".to_string());
    changed.label = Some("main".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: None,
        operations: vec![
            MsfDeltaOperation::Remove {
                tracks: vec![remove_ref("v", "ns")],
            },
            MsfDeltaOperation::Add {
                tracks: vec![changed],
            },
        ],
    };
    let Err(MessageError::InvalidCatalog(reason)) = catalog.apply_delta(&delta, None) else {
        panic!("属性変更を伴う再追加は InvalidCatalog であること");
    };
    assert!(
        reason.contains("attributes MUST NOT be modified"),
        "§5.3 の属性不変を述べること: {reason}"
    );
    // 失敗時はカタログを変更しない (copy-on-write)
    assert_eq!(catalog.tracks.len(), 1, "失敗時にトラックが消えないこと");
    assert_eq!(
        catalog.tracks[0].codec.as_deref(),
        Some("av01"),
        "失敗時に属性が変わらないこと"
    );
}

/// remove → clone で同じ (namespace, name) を再追加し属性が変わる delta update は拒否される
#[test]
fn apply_delta_readd_by_clone_with_changed_attribute_rejected() {
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(video_track("v", "ns"));
    // clone の親になる別トラック (codec が異なる)
    let mut parent = video_track("src", "ns");
    parent.codec = Some("vp09".to_string());
    parent.width = Some(1280);
    catalog.tracks.push(parent);

    let mut clone = MsfCloneTrack::new("v".to_string(), "src".to_string());
    clone.parent_namespace = Some("ns".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: None,
        operations: vec![
            MsfDeltaOperation::Remove {
                tracks: vec![remove_ref("v", "ns")],
            },
            MsfDeltaOperation::Clone {
                tracks: vec![clone],
            },
        ],
    };
    let Err(MessageError::InvalidCatalog(reason)) = catalog.apply_delta(&delta, None) else {
        panic!("clone による属性変更を伴う再追加は InvalidCatalog であること");
    };
    assert!(
        reason.contains("attributes MUST NOT be modified"),
        "§5.3 の属性不変を述べること: {reason}"
    );
    assert_eq!(catalog.tracks.len(), 2, "失敗時にトラックが消えないこと");
}

/// remove → add で isLive を false から true に戻す delta update は拒否される
///
/// draft-ietf-moq-msf-01 §5.2.7 (Is Live): "A True value MUST never follow a False value."
/// 属性差分の文言とは区別できる専用のメッセージで返す。
#[test]
fn apply_delta_readd_with_is_live_regression_rejected() {
    let mut catalog = MsfCatalog::new();
    let mut offline = loc_track("v", Some("ns"));
    offline.is_live = false;
    catalog.tracks.push(offline);

    let mut live = loc_track("v", Some("ns"));
    live.is_live = true;
    let delta = MsfDeltaUpdate {
        generated_at: None,
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
#[test]
fn apply_delta_readd_attribute_change_across_deltas_rejected() {
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(video_track("v", "ns"));

    // 1 回目の delta: remove のみ
    let remove_delta = MsfDeltaUpdate {
        generated_at: None,
        operations: vec![MsfDeltaOperation::Remove {
            tracks: vec![remove_ref("v", "ns")],
        }],
    };
    catalog
        .apply_delta(&remove_delta, None)
        .expect("remove は成功する");
    assert!(catalog.tracks.is_empty());

    // 2 回目の delta: 属性を変えて add
    let mut changed = video_track("v", "ns");
    changed.codec = Some("vp09".to_string());
    let add_delta = MsfDeltaUpdate {
        generated_at: None,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![changed],
        }],
    };
    let Err(MessageError::InvalidCatalog(reason)) = catalog.apply_delta(&add_delta, None) else {
        panic!("delta をまたぐ属性変更は InvalidCatalog であること");
    };
    assert!(
        reason.contains("attributes MUST NOT be modified"),
        "§5.3 の属性不変を述べること: {reason}"
    );
    assert!(
        catalog.tracks.is_empty(),
        "失敗時にトラックが追加されないこと"
    );
}

/// 同一属性での remove → add は引き続き成功する
#[test]
fn apply_delta_readd_with_same_attributes_succeeds() {
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(video_track("v", "ns"));

    let delta = MsfDeltaUpdate {
        generated_at: None,
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
        generated_at: None,
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
#[test]
fn apply_delta_readd_attribute_change_with_inherited_namespace_rejected() {
    let mut catalog = MsfCatalog::new();
    let mut original = loc_track("v", None);
    original.codec = Some("av01".to_string());
    catalog.tracks.push(original);

    // remove は namespace を明示し、add は省略する (どちらも継承した "cns" に解決される)。
    // remove 側の解決に catalog namespace の継承が効いていないと履歴を引けず、属性変更を
    // 見逃す。
    let mut changed = loc_track("v", None);
    changed.codec = Some("vp09".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: None,
        operations: vec![
            MsfDeltaOperation::Remove {
                tracks: vec![remove_ref("v", "cns")],
            },
            MsfDeltaOperation::Add {
                tracks: vec![changed],
            },
        ],
    };
    let Err(MessageError::InvalidCatalog(reason)) = catalog.apply_delta(&delta, Some("cns")) else {
        panic!("継承した namespace でも属性変更を検出すること");
    };
    assert!(
        reason.contains("attributes MUST NOT be modified"),
        "§5.3 の属性不変を述べること: {reason}"
    );
}

/// 省略した namespace と明示した namespace は同じ tuple として扱われる
///
/// 履歴のキーは解決済みの namespace なので、宣言時に省略し、remove と再追加で明示しても
/// 同じトラックとして属性変更を検出する。
#[test]
fn apply_delta_readd_with_explicit_namespace_matches_inherited() {
    let mut catalog = MsfCatalog::new();
    let mut original = loc_track("v", None);
    original.codec = Some("av01".to_string());
    catalog.tracks.push(original);

    let mut changed = video_track("v", "cns");
    changed.codec = Some("vp09".to_string());
    let delta = MsfDeltaUpdate {
        generated_at: None,
        operations: vec![
            MsfDeltaOperation::Remove {
                tracks: vec![remove_ref("v", "cns")],
            },
            MsfDeltaOperation::Add {
                tracks: vec![changed],
            },
        ],
    };
    let Err(MessageError::InvalidCatalog(reason)) = catalog.apply_delta(&delta, Some("cns")) else {
        panic!("省略形と明示形は同じ tuple として扱うこと");
    };
    assert!(
        reason.contains("attributes MUST NOT be modified"),
        "§5.3 の属性不変を述べること: {reason}"
    );
}

/// namespace の表現 (省略形 / 明示形) が宣言と再追加で異なっても、同一属性なら再追加できる
///
/// キーは解決済み namespace で同一性を確認するため、生の表現差 (省略形と明示形) は
/// 属性変更とみなさない。
#[test]
fn apply_delta_readd_with_different_namespace_representation_succeeds() {
    // 宣言は明示、再追加は省略
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(video_track("v", "cns"));
    let delta = MsfDeltaUpdate {
        generated_at: None,
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
        .apply_delta(&delta, Some("cns"))
        .expect("明示形から省略形への再追加は成功する");
    assert_eq!(catalog.tracks.len(), 1);
    assert_eq!(catalog.tracks[0].codec.as_deref(), Some("av01"));

    // 宣言は省略、再追加は明示
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(video_track_omitted_ns("v"));
    let delta = MsfDeltaUpdate {
        generated_at: None,
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
        .apply_delta(&delta, Some("cns"))
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
        generated_at: None,
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
