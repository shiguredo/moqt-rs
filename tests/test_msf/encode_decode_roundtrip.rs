use super::*;

#[test]
fn full_catalog_with_all_fields() {
    let mut track = MsfTrack::new("video".to_string(), MsfPackaging::Loc, true);
    track.namespace = Some("ns".to_string());
    track.role = Some("video".to_string());
    track.target_latency = Some(2000);
    track.label = Some("HD".to_string());
    track.render_group = Some(1);
    track.alt_group = Some(1);
    track.codec = Some("av01".to_string());
    track.framerate = Some(30.0);
    track.bitrate = Some(5_000_000);
    track.avg_bitrate = Some(3_000_000);
    track.max_gop_duration = Some(2000);
    track.max_group_duration = Some(4000);
    track.connection_uri = Some("moqt://example.com:4443".to_string());
    track.token = Some("DUMMY-TOKEN".to_string());
    track.encryption_scheme = Some("moq-secure-objects".to_string());
    track.cipher_suite = Some("aes-128-gcm-sha256".to_string());
    track.key_id = Some("key-1".to_string());
    track.track_base_key = Some("AAEC".to_string());
    track.auth_info = Some(vec![MsfAuthInfo {
        scheme: "cat".to_string(),
        value_raw: b"\"%cat-token%\"".to_vec(),
    }]);
    track.timescale = Some(90_000);
    track.width = Some(1920);
    track.height = Some(1080);
    track.samplerate = Some(48_000);
    track.channel_config = Some("stereo".to_string());
    track.display_width = Some(1280);
    track.display_height = Some(720);
    track.lang = Some("en".to_string());

    let doc = MsfCatalogDocument::Full(MsfCatalog {
        version: MSF_VERSION.to_string(),
        generated_at: Some(1_700_000_000_000),
        is_complete: false,
        tracks: vec![track],
        publish_tracks: Vec::new(),
        init_data_list: Vec::new(),
    });
    let encoded = doc.encode().expect("encode に成功すること");
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded, doc);
}

#[test]
fn full_catalog_with_max_durations_absent() {
    // maxGopDuration / maxGroupDuration の省略時はキーが出力されず None に戻ること
    // (直前後のフィールドは出力される)
    let mut track = MsfTrack::new("video".to_string(), MsfPackaging::Loc, true);
    track.avg_bitrate = Some(3_000_000);
    track.width = Some(640);

    let doc = MsfCatalogDocument::Full(MsfCatalog {
        version: MSF_VERSION.to_string(),
        generated_at: None,
        is_complete: false,
        tracks: vec![track],
        publish_tracks: Vec::new(),
        init_data_list: Vec::new(),
    });
    let encoded = doc.encode().expect("encode に成功すること");
    assert!(!encoded.windows(16).any(|w| w == b"\"maxGopDuration\""));
    assert!(!encoded.windows(18).any(|w| w == b"\"maxGroupDuration\""));
    assert!(encoded.windows(12).any(|w| w == b"\"avgBitrate\""));
    assert!(encoded.windows(7).any(|w| w == b"\"width\""));
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded, doc);
}

#[test]
fn full_catalog_with_init_data_list_and_buffers() {
    let mut track = MsfTrack::new("video".to_string(), MsfPackaging::Loc, true);
    track.init_ref = Some("init1".to_string());
    track.depends = vec!["audio".to_string()];
    track.temporal_id = Some(1);
    track.spatial_id = Some(0);
    track.buffers = Some(MsfBuffers {
        target: Some(1000),
        min: Some(500),
        max: Some(2000),
    });

    let doc = MsfCatalogDocument::Full(MsfCatalog {
        version: MSF_VERSION.to_string(),
        generated_at: None,
        is_complete: true,
        tracks: vec![track],
        publish_tracks: Vec::new(),
        init_data_list: vec![MsfInitData {
            id: "init1".to_string(),
            kind: MsfInitDataKind::Inline,
            data: "AAEC".to_string(),
        }],
    });
    let encoded = doc.encode().expect("encode に成功すること");
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded, doc);
}

#[test]
fn full_catalog_with_media_timeline_track() {
    let mut track = MsfTrack::new("timeline".to_string(), MsfPackaging::MediaTimeline, true);
    track.depends = vec!["video".to_string()];
    track.mime_type = Some("application/json".to_string());
    track.timescale = Some(1_000);

    let doc = MsfCatalogDocument::Full(MsfCatalog {
        version: MSF_VERSION.to_string(),
        generated_at: None,
        is_complete: false,
        tracks: vec![track],
        publish_tracks: Vec::new(),
        init_data_list: Vec::new(),
    });
    let encoded = doc.encode().expect("encode に成功すること");
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded, doc);
}

#[test]
fn delta_all_operations() {
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: Some(1_700_000_000_000),
        operations: vec![
            MsfDeltaOperation::Add {
                tracks: vec![MsfTrack::new("new".to_string(), MsfPackaging::Loc, true)],
            },
            MsfDeltaOperation::Remove {
                tracks: vec![MsfRemoveTrack {
                    name: "old".to_string(),
                    namespace: Some("ns".to_string()),
                }],
            },
            MsfDeltaOperation::Clone {
                tracks: vec![MsfCloneTrack {
                    name: "clone".to_string(),
                    parent_name: "original".to_string(),
                    parent_namespace: None,
                    packaging: Some(MsfPackaging::Loc),
                    is_live: Some(true),
                    namespace: None,
                    event_type: None,
                    role: None,
                    target_latency: None,
                    buffers: None,
                    label: None,
                    render_group: None,
                    alt_group: None,
                    init_ref: None,
                    depends: None,
                    template: None,
                    temporal_id: None,
                    spatial_id: None,
                    codec: None,
                    mime_type: None,
                    framerate: None,
                    timescale: None,
                    bitrate: None,
                    avg_bitrate: None,
                    max_gop_duration: None,
                    max_group_duration: None,
                    width: None,
                    height: None,
                    samplerate: None,
                    channel_config: None,
                    display_width: None,
                    display_height: None,
                    lang: None,
                    track_duration: None,
                    connection_uri: None,
                    token: None,
                    encryption_scheme: None,
                    cipher_suite: None,
                    key_id: None,
                    track_base_key: None,
                    auth_info: None,
                    accessibility: None,
                }],
            },
        ],
    });
    let encoded = doc.encode().expect("encode に成功すること");
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded, doc);
}

#[test]
fn is_complete_not_emitted_when_false() {
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        version: MSF_VERSION.to_string(),
        generated_at: None,
        is_complete: false,
        tracks: vec![],
        publish_tracks: Vec::new(),
        init_data_list: Vec::new(),
    });
    let encoded = doc.encode().expect("encode に成功すること");
    assert!(!encoded.windows(10).any(|w| w == b"isComplete"));
}

#[test]
fn is_complete_emitted_when_true() {
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        version: MSF_VERSION.to_string(),
        generated_at: None,
        is_complete: true,
        tracks: vec![],
        publish_tracks: Vec::new(),
        init_data_list: Vec::new(),
    });
    let encoded = doc.encode().expect("encode に成功すること");
    assert!(encoded.windows(10).any(|w| w == b"isComplete"));
}

/// isLive=false の MsfTrack は targetLatency / buffers をエンコードしないこと
///
/// draft-ietf-moq-msf-01 §5.2.8 / §5.2.9: isLive=false なら両フィールドは無視される。
/// decode 側は None に正規化するため、encode 側も出力しないことで往復を一致させる。
#[test]
fn target_latency_and_buffers_not_emitted_when_is_live_false() {
    let mut track = MsfTrack::new("v".to_string(), MsfPackaging::Loc, false);
    track.target_latency = Some(500);
    let encoded = encode_full_catalog_with_track(track);
    assert!(!encoded.windows(15).any(|w| w == b"\"targetLatency\""));

    let mut track = MsfTrack::new("b".to_string(), MsfPackaging::Loc, false);
    track.buffers = Some(MsfBuffers {
        target: Some(1000),
        min: None,
        max: None,
    });
    let encoded = encode_full_catalog_with_track(track);
    assert!(!encoded.windows(9).any(|w| w == b"\"buffers\""));
    // 正の制御: isLive 自体は出力される (真空判定でないこと)
    assert!(encoded.windows(8).any(|w| w == b"\"isLive\""));
}

/// isLive=Some(false) の MsfCloneTrack は targetLatency / buffers をエンコードしないこと
///
/// isLive=None (親から継承) と isLive=Some(true) では出力する。
#[test]
fn clone_target_latency_and_buffers_not_emitted_when_is_live_false() {
    let mut clone = MsfCloneTrack::new("clone".to_string(), "original".to_string());
    clone.is_live = Some(false);
    clone.target_latency = Some(500);
    let encoded = encode_delta_with_clone(clone);
    assert!(!encoded.windows(15).any(|w| w == b"\"targetLatency\""));

    let mut clone = MsfCloneTrack::new("clone".to_string(), "original".to_string());
    clone.is_live = Some(false);
    clone.buffers = Some(MsfBuffers {
        target: Some(1000),
        min: None,
        max: None,
    });
    let encoded = encode_delta_with_clone(clone);
    assert!(!encoded.windows(9).any(|w| w == b"\"buffers\""));

    // isLive=None (親から継承) と isLive=Some(true) では出力する
    let mut clone = MsfCloneTrack::new("clone".to_string(), "original".to_string());
    clone.target_latency = Some(500);
    let encoded = encode_delta_with_clone(clone);
    assert!(encoded.windows(15).any(|w| w == b"\"targetLatency\""));

    let mut clone = MsfCloneTrack::new("clone".to_string(), "original".to_string());
    clone.is_live = Some(true);
    clone.target_latency = Some(500);
    let encoded = encode_delta_with_clone(clone);
    assert!(encoded.windows(15).any(|w| w == b"\"targetLatency\""));
}

/// full と clone で共通メンバーが同一キー名で出力されること
///
/// 既存 roundtrip テストはキー順に依存しないため、共通化後に 3 キーが双方に出ることを
/// 確認する smoke テストとして残す。空 depends / accessibility の各規則は既存テスト
/// (`depends_empty_not_emitted` / `clone_depends_empty_some_roundtrip` 等) が検証する。
#[test]
fn track_and_clone_share_common_member_keys() {
    let mut full = MsfTrack::new("v".to_string(), MsfPackaging::Loc, true);
    full.max_gop_duration = Some(2000);
    full.lang = Some("en".to_string());
    full.bitrate = Some(5_000_000);

    let mut clone = MsfCloneTrack::new("c".to_string(), "v".to_string());
    clone.max_gop_duration = Some(2000);
    clone.lang = Some("en".to_string());
    clone.bitrate = Some(5_000_000);

    let full_json = encode_full_catalog_with_track(full);
    let clone_json = encode_delta_with_clone(clone);
    for key in ["maxGopDuration", "lang", "bitrate"] {
        assert!(
            full_json.windows(key.len()).any(|w| w == key.as_bytes()),
            "full に {key} が出力されること"
        );
        assert!(
            clone_json.windows(key.len()).any(|w| w == key.as_bytes()),
            "clone に {key} が出力されること"
        );
    }
}

fn encode_full_catalog_with_track(track: MsfTrack) -> Vec<u8> {
    MsfCatalogDocument::Full(MsfCatalog {
        version: MSF_VERSION.to_string(),
        generated_at: None,
        is_complete: true,
        tracks: vec![track],
        publish_tracks: Vec::new(),
        init_data_list: Vec::new(),
    })
    .encode()
    .expect("encode に成功すること")
}

fn encode_delta_with_clone(clone: MsfCloneTrack) -> Vec<u8> {
    MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: None,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    })
    .encode()
    .expect("encode に成功すること")
}

#[test]
fn clone_track_roundtrip_all_fields() {
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: None,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![MsfCloneTrack {
                name: "clone".to_string(),
                parent_name: "original".to_string(),
                // draft-ietf-moq-msf-01 §5.2.34 (Parent namespace):
                // parentNamespace 付き clone の roundtrip を検証する
                parent_namespace: Some("parent-ns".to_string()),
                packaging: Some(MsfPackaging::EventTimeline),
                namespace: Some("ns".to_string()),
                event_type: Some("com.example.cue".to_string()),
                role: Some("caption".to_string()),
                is_live: Some(true),
                target_latency: None,
                buffers: Some(MsfBuffers {
                    target: Some(1000),
                    min: None,
                    max: None,
                }),
                label: Some("Clone".to_string()),
                render_group: Some(1),
                alt_group: Some(2),
                init_ref: Some("init1".to_string()),
                depends: Some(vec!["original".to_string()]),
                template: Some(MsfTemplate {
                    start_media_time: 0,
                    delta_media_time: 2002,
                    start_group_id: 0,
                    start_object_id: 0,
                    delta_group_id: 1,
                    delta_object_id: 0,
                    start_wallclock: 1759924158381,
                    delta_wallclock: 2002,
                }),
                temporal_id: Some(1),
                spatial_id: Some(0),
                codec: Some("av01".to_string()),
                mime_type: Some("application/json".to_string()),
                framerate: Some(30.0),
                timescale: Some(90_000),
                bitrate: Some(5_000_000),
                avg_bitrate: Some(3_000_000),
                max_gop_duration: Some(2000),
                max_group_duration: Some(4000),
                connection_uri: Some("moqt://logs.example.com:4443".to_string()),
                token: Some("DUMMY-TOKEN".to_string()),
                encryption_scheme: Some("moq-secure-objects".to_string()),
                cipher_suite: Some("aes-128-gcm-sha256".to_string()),
                key_id: Some("key-2024-q1".to_string()),
                track_base_key: Some("dGhpc2lzYXNhbXBsZWJhc2VrZXk=".to_string()),
                auth_info: None,
                accessibility: None,
                width: Some(1920),
                height: Some(1080),
                samplerate: Some(48_000),
                channel_config: Some("stereo".to_string()),
                display_width: Some(1280),
                display_height: Some(720),
                lang: Some("en".to_string()),
                track_duration: None,
            }],
        }],
    });
    let encoded = doc.encode().expect("encode に成功すること");
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded, doc);
}

#[test]
fn clone_track_roundtrip_with_target_latency() {
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: None,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![MsfCloneTrack {
                name: "clone".to_string(),
                parent_name: "original".to_string(),
                parent_namespace: None,
                packaging: Some(MsfPackaging::Loc),
                namespace: None,
                event_type: None,
                role: None,
                is_live: Some(true),
                target_latency: Some(500),
                buffers: None,
                label: None,
                render_group: None,
                alt_group: None,
                init_ref: None,
                depends: None,
                template: None,
                temporal_id: None,
                spatial_id: None,
                codec: None,
                mime_type: None,
                framerate: None,
                timescale: None,
                bitrate: None,
                avg_bitrate: None,
                max_gop_duration: None,
                max_group_duration: None,
                width: None,
                height: None,
                samplerate: None,
                channel_config: None,
                display_width: None,
                display_height: None,
                lang: None,
                track_duration: None,
                connection_uri: None,
                token: None,
                encryption_scheme: None,
                cipher_suite: None,
                key_id: None,
                track_base_key: None,
                auth_info: None,
                accessibility: None,
            }],
        }],
    });
    let encoded = doc.encode().expect("encode に成功すること");
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded, doc);
}

#[test]
fn clone_track_roundtrip_with_track_duration() {
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: None,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![MsfCloneTrack {
                name: "clone".to_string(),
                parent_name: "original".to_string(),
                parent_namespace: None,
                packaging: Some(MsfPackaging::Loc),
                namespace: None,
                event_type: None,
                role: None,
                is_live: Some(false),
                target_latency: None,
                buffers: None,
                label: None,
                render_group: None,
                alt_group: None,
                init_ref: None,
                depends: None,
                template: None,
                temporal_id: None,
                spatial_id: None,
                codec: None,
                mime_type: None,
                framerate: None,
                timescale: None,
                bitrate: None,
                avg_bitrate: None,
                max_gop_duration: None,
                max_group_duration: None,
                width: None,
                height: None,
                samplerate: None,
                channel_config: None,
                display_width: None,
                display_height: None,
                lang: None,
                track_duration: Some(60_000),
                connection_uri: None,
                token: None,
                encryption_scheme: None,
                cipher_suite: None,
                key_id: None,
                track_base_key: None,
                auth_info: None,
                accessibility: None,
            }],
        }],
    });
    let encoded = doc.encode().expect("encode に成功すること");
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded, doc);
}

#[test]
fn depends_empty_not_emitted() {
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        version: MSF_VERSION.to_string(),
        generated_at: None,
        is_complete: false,
        tracks: vec![MsfTrack::new("t".to_string(), MsfPackaging::Loc, true)],
        publish_tracks: Vec::new(),
        init_data_list: Vec::new(),
    });
    let encoded = doc.encode().expect("encode に成功すること");
    assert!(!encoded.windows(7).any(|w| w == b"depends"));
}

#[test]
fn clone_depends_empty_some_roundtrip() {
    // clone の Some([]) は省略せず保持され roundtrip すること (None = 継承と区別する)
    let mut clone = MsfCloneTrack::new("c".to_string(), "p".to_string());
    clone.depends = Some(Vec::new());
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: None,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    });
    let encoded = doc.encode().expect("encode に成功すること");
    let text = String::from_utf8(encoded.clone()).expect("テストフィクスチャの前提条件を満たす");
    assert!(
        text.contains("\"depends\""),
        "Some([]) はキー付きで出力されること"
    );
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Delta(re_delta) = decoded else {
        panic!("Delta update がデコードされること");
    };
    let MsfDeltaOperation::Clone { tracks } = &re_delta.operations[0] else {
        panic!("Clone 操作がデコードされること");
    };
    assert_eq!(tracks[0].depends, Some(Vec::new()));
}

#[test]
fn clone_depends_none_omitted() {
    // clone の None 時はキーが出力されないこと (親から継承)
    let clone = MsfCloneTrack::new("c".to_string(), "p".to_string());
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: None,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    });
    let encoded = doc.encode().expect("encode に成功すること");
    let text = String::from_utf8(encoded).expect("テストフィクスチャの前提条件を満たす");
    assert!(!text.contains("depends"));
}

#[test]
fn publish_tracks_roundtrip() {
    // draft-ietf-moq-msf-01 §5.6.16 を適用した例 (version を draft-01 化 ・ isLive 補完 ・
    // token / connectionUri 除外 ・ %resourceId% は文字列として保持。
    // token / connectionUri なし版の roundtrip (あり版は connection_uri_token_roundtrip) 。
    // 仕様例の track 名と namespace の MUST 形式 (§9.2 / §10.2) の妥当性検証は対象外とし、
    // roundtrip 検証のため簡略化している) が roundtrip すること
    let mut metrics = MsfTrack::new("metrics".to_string(), MsfPackaging::MoqMetrics, true);
    metrics.namespace = Some("%resourceId%".to_string());
    metrics.role = Some("metrics".to_string());
    let mut log = MsfTrack::new("log".to_string(), MsfPackaging::MoqLog, true);
    log.namespace = Some("%resourceId%".to_string());
    log.role = Some("log".to_string());

    let doc = MsfCatalogDocument::Full(MsfCatalog {
        version: MSF_VERSION.to_string(),
        generated_at: None,
        is_complete: false,
        tracks: vec![],
        publish_tracks: vec![metrics, log],
        init_data_list: Vec::new(),
    });
    let encoded = doc.encode().expect("encode に成功すること");
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded, doc);
}

#[test]
fn publish_tracks_absent_not_emitted() {
    // publishTracks の省略時はキーが出力されないこと
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        version: MSF_VERSION.to_string(),
        generated_at: None,
        is_complete: false,
        tracks: vec![],
        publish_tracks: Vec::new(),
        init_data_list: Vec::new(),
    });
    let encoded = doc.encode().expect("encode に成功すること");
    assert!(!encoded.windows(15).any(|w| w == b"\"publishTracks\""));
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded, doc);
}

#[test]
fn publish_tracks_non_array_rejected() {
    // publishTracks が配列でない場合は reject されること
    let json = br#"{"version":"draft-01","tracks":[],"publishTracks":{}}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn publish_tracks_empty_array_roundtrip() {
    // 明示の空配列は受理され、再 encode でキーが消えること
    let json = br#"{"version":"draft-01","tracks":[],"publishTracks":[]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let encoded = doc.encode().expect("encode に成功すること");
    assert!(!encoded.windows(15).any(|w| w == b"\"publishTracks\""));
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded, doc);
}

#[test]
fn publish_tracks_duplicate_with_tracks_rejected() {
    // tracks と publishTracks の間の (namespace, name) 重複は reject されること
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true}],"publishTracks":[{"name":"t","packaging":"moqlog","isLive":true}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn publish_tracks_key_order() {
    // 出力順は tracks → publishTracks → initDataList であること
    // (エンコーダの慣例の固定であり、§5.1.7 の MUST は initDataList の tracks 後配置のみ)
    let mut log = MsfTrack::new("log".to_string(), MsfPackaging::MoqLog, true);
    log.role = Some("log".to_string());
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        version: MSF_VERSION.to_string(),
        generated_at: None,
        is_complete: false,
        tracks: vec![MsfTrack::new("v".to_string(), MsfPackaging::Loc, true)],
        publish_tracks: vec![log],
        init_data_list: vec![MsfInitData {
            id: "init1".to_string(),
            kind: MsfInitDataKind::Inline,
            data: "AAEC".to_string(),
        }],
    });
    let encoded = doc.encode().expect("encode に成功すること");
    let text = String::from_utf8(encoded).expect("テストフィクスチャの前提条件を満たす");
    let tracks_pos = text
        .find("\"tracks\"")
        .expect("テストフィクスチャの前提条件を満たす");
    let publish_pos = text
        .find("\"publishTracks\"")
        .expect("テストフィクスチャの前提条件を満たす");
    let init_pos = text
        .find("\"initDataList\"")
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(tracks_pos < publish_pos && publish_pos < init_pos);
}

#[test]
fn connection_uri_token_roundtrip() {
    // §5.6.16 を適用した完全版 (token / connectionUri 付き) が roundtrip すること
    // token 値は打切りダミーとし、実在し得る認証情報を使わない
    let mut metrics = MsfTrack::new("metrics".to_string(), MsfPackaging::MoqMetrics, true);
    metrics.namespace = Some("%resourceId%".to_string());
    metrics.role = Some("metrics".to_string());
    metrics.connection_uri = Some("moqt://logs.example.com:4443".to_string());
    metrics.token = Some("DUMMY-TOKEN".to_string());

    let doc = MsfCatalogDocument::Full(MsfCatalog {
        version: MSF_VERSION.to_string(),
        generated_at: None,
        is_complete: false,
        tracks: vec![],
        publish_tracks: vec![metrics],
        init_data_list: Vec::new(),
    });
    let encoded = doc.encode().expect("encode に成功すること");
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded, doc);
}

#[test]
fn publish_tracks_unknown_fields_ignored() {
    // 未知フィールドは無視され受理されること (§5 の MUST ignore)
    let json = br#"{"version":"draft-01","tracks":[],"publishTracks":[{"name":"6","packaging":"moqlog","isLive":true,"unknownFutureKey":"x","role":"log"}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(cat.publish_tracks.len(), 1);
    assert_eq!(cat.publish_tracks[0].role.as_deref(), Some("log"));
}

#[test]
fn group_validation_separated_between_tracks_and_publish_tracks() {
    // tracks と publishTracks で同一 group 番号・異なる値でも受理されること (分離の意図固定)
    let json = br#"{"version":"draft-01","tracks":[{"name":"v","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":100}],"publishTracks":[{"name":"m","packaging":"moqlog","isLive":true,"renderGroup":1,"targetLatency":200}]}"#;
    assert!(MsfCatalogDocument::decode(json).is_ok());
}

#[test]
fn group_validation_inside_publish_tracks_rejected() {
    // publishTracks 内部の同一 group 不一致は reject されること
    let json = br#"{"version":"draft-01","tracks":[],"publishTracks":[{"name":"a","packaging":"moqlog","isLive":true,"renderGroup":1,"targetLatency":100},{"name":"b","packaging":"moqlog","isLive":true,"renderGroup":1,"targetLatency":200}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn encryption_signaling_roundtrip() {
    // §5.6.8 を適用した例 (version を draft-01 化) が roundtrip すること
    // trackBaseKey 値は打切りダミーとし、実在し得る鍵情報を使わない
    let mut track = MsfTrack::new("video".to_string(), MsfPackaging::Loc, true);
    track.encryption_scheme = Some("moq-secure-objects".to_string());
    track.cipher_suite = Some("aes-128-gcm-sha256".to_string());
    track.key_id = Some("key-2024-q1".to_string());
    track.track_base_key = Some("dGhpc2lzYXNhbXBsZWJhc2VrZXk=".to_string());

    let doc = MsfCatalogDocument::Full(MsfCatalog {
        version: MSF_VERSION.to_string(),
        generated_at: None,
        is_complete: false,
        tracks: vec![track],
        publish_tracks: Vec::new(),
        init_data_list: Vec::new(),
    });
    let encoded = doc.encode().expect("encode に成功すること");
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded, doc);
}

#[test]
fn encryption_clone_partial_accepted() {
    // clone の継承時 (欠如あり) は reject されないこと (親未知のため断定不可)
    let json = br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","encryptionScheme":"moq-secure-objects"}]}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Delta(delta) = doc else {
        panic!("Delta update がデコードされること");
    };
    let MsfDeltaOperation::Clone { tracks } = &delta.operations[0] else {
        panic!("Clone 操作がデコードされること");
    };
    assert_eq!(
        tracks[0].encryption_scheme.as_deref(),
        Some("moq-secure-objects")
    );
    assert_eq!(tracks[0].cipher_suite, None);
}

#[test]
fn encryption_orphan_field_accepted() {
    // encryptionScheme なしの cipherSuite 単独は受理され roundtrip すること (片方向 MUST のみ)
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"cipherSuite":"aes-128-gcm-sha256"}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(
        cat.tracks[0].cipher_suite.as_deref(),
        Some("aes-128-gcm-sha256")
    );
    let encoded = MsfCatalogDocument::Full(cat)
        .encode()
        .expect("encode に成功すること");
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(re_cat) = decoded else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(
        re_cat.tracks[0].cipher_suite.as_deref(),
        Some("aes-128-gcm-sha256")
    );
}

#[test]
fn encryption_cipher_suite_missing_rejected() {
    // encryptionScheme 指定時の cipherSuite 欠如は scheme によらず reject されること
    // (secure-objects 時の cipherSuite ケースと合わせて scheme 非依存を固定する)
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"encryptionScheme":"com.example.custom"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn encryption_secure_objects_incomplete_rejected() {
    // moq-secure-objects 時の必須組欠如は各単独で reject されること
    for missing in ["cipherSuite", "keyId", "trackBaseKey"] {
        let mut fields = vec![
            "\"encryptionScheme\":\"moq-secure-objects\"".to_string(),
            "\"cipherSuite\":\"aes-128-gcm-sha256\"".to_string(),
            "\"keyId\":\"key-1\"".to_string(),
            "\"trackBaseKey\":\"AAEC\"".to_string(),
        ];
        fields.retain(|f| !f.contains(missing));
        let json = format!(
            "{{\"version\":\"draft-01\",\"tracks\":[{{\"name\":\"t\",\"packaging\":\"loc\",\"isLive\":true,{}}}]}}",
            fields.join(",")
        );
        assert!(
            matches!(
                MsfCatalogDocument::decode(json.as_bytes()),
                Err(MessageError::InvalidCatalog(_))
            ),
            "{missing} の欠如で reject されること"
        );
    }
}

#[test]
fn encryption_custom_scheme_accepted() {
    // 未知 scheme は任意文字列として受理されること (custom 許容。 RDNN 検証なし)
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"encryptionScheme":"com.example.custom","cipherSuite":"custom-suite"}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(
        cat.tracks[0].encryption_scheme.as_deref(),
        Some("com.example.custom")
    );
    let encoded = MsfCatalogDocument::Full(cat)
        .encode()
        .expect("encode に成功すること");
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(re_cat) = decoded else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(
        re_cat.tracks[0].encryption_scheme.as_deref(),
        Some("com.example.custom")
    );
    assert_eq!(
        re_cat.tracks[0].cipher_suite.as_deref(),
        Some("custom-suite")
    );
}

#[test]
fn auth_info_roundtrip() {
    // §5.6.15 を適用した例 (version を draft-01 化、`%...%` は文字列として保持) が
    // decode / encode で roundtrip すること。両エントリとも文字列値とする
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"authInfo":{"cat":"%cat-token%","privacy-pass":"%pp-token%"}}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    let entries = cat.tracks[0]
        .auth_info
        .as_ref()
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(entries.len(), 2);
    assert_eq!(
        find_auth(entries, "cat").value_raw.as_slice(),
        b"\"%cat-token%\"".as_slice()
    );
    assert_eq!(
        find_auth(entries, "privacy-pass").value_raw.as_slice(),
        b"\"%pp-token%\"".as_slice()
    );
    let encoded = MsfCatalogDocument::Full(cat)
        .encode()
        .expect("encode に成功すること");
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(re_cat) = decoded else {
        panic!("Full catalog がデコードされること");
    };
    let re_entries = re_cat.tracks[0]
        .auth_info
        .as_ref()
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        find_auth(re_entries, "cat").value_raw.as_slice(),
        b"\"%cat-token%\"".as_slice()
    );
    assert_eq!(
        find_auth(re_entries, "privacy-pass").value_raw.as_slice(),
        b"\"%pp-token%\"".as_slice()
    );
}

#[test]
fn auth_info_object_value_roundtrip() {
    // object 値も生 JSON として保持され roundtrip すること
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"authInfo":{"cat":{"token":"%cat-token%"}}}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    let entries = cat.tracks[0]
        .auth_info
        .as_ref()
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        find_auth(entries, "cat").value_raw.as_slice(),
        b"{\"token\":\"%cat-token%\"}".as_slice()
    );
    let encoded = MsfCatalogDocument::Full(cat)
        .encode()
        .expect("encode に成功すること");
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(re_cat) = decoded else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(
        find_auth(
            re_cat.tracks[0]
                .auth_info
                .as_ref()
                .expect("テストフィクスチャの前提条件を満たす"),
            "cat"
        )
        .value_raw
        .as_slice(),
        b"{\"token\":\"%cat-token%\"}".as_slice()
    );
}

#[test]
fn auth_info_non_object_rejected() {
    // 非 object の authInfo は reject されること
    for json in [
        br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"authInfo":[]}]}"#.as_slice(),
        br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"authInfo":"x"}]}"#.as_slice(),
        br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"authInfo":null}]}"#.as_slice(),
        br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"authInfo":0}]}"#.as_slice(),
        br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"authInfo":true}]}"#.as_slice(),
    ] {
        assert!(
            matches!(
                MsfCatalogDocument::decode(json),
                Err(MessageError::InvalidCatalog(_))
            ),
            "非 object の authInfo は reject されること"
        );
    }
}

#[test]
fn auth_info_null_value_accepted() {
    // 値 null も受理され roundtrip すること (値位置の型は問わない)
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"authInfo":{"cat":null}}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(find_auth_value_track(&cat.tracks, "cat"), b"null");
    let encoded = MsfCatalogDocument::Full(cat)
        .encode()
        .expect("encode に成功すること");
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(re_cat) = decoded else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(find_auth_value_track(&re_cat.tracks, "cat"), b"null");
}

fn find_auth_value_track<'a>(tracks: &'a [MsfTrack], scheme: &str) -> &'a [u8] {
    find_auth(
        tracks[0]
            .auth_info
            .as_ref()
            .expect("テストフィクスチャの前提条件を満たす"),
        scheme,
    )
    .value_raw
    .as_slice()
}
#[test]
fn auth_info_empty_object_accepted() {
    // 空 object は空 Vec として受理され roundtrip すること
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"authInfo":{}}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(cat.tracks[0].auth_info, Some(Vec::new()));
    let encoded = MsfCatalogDocument::Full(cat)
        .encode()
        .expect("encode に成功すること");
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(re_cat) = decoded else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(re_cat.tracks[0].auth_info, Some(Vec::new()));
}

#[test]
fn auth_info_duplicate_scheme_kept_in_order() {
    // 重複 scheme は順序どおりに保持されること
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"authInfo":{"cat":"a","cat":"b"}}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    let entries = cat.tracks[0]
        .auth_info
        .as_ref()
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].value_raw.as_slice(), b"\"a\"".as_slice());
    assert_eq!(entries[1].value_raw.as_slice(), b"\"b\"".as_slice());
    let encoded = MsfCatalogDocument::Full(cat)
        .encode()
        .expect("encode に成功すること");
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(re_cat) = decoded else {
        panic!("Full catalog がデコードされること");
    };
    let re_entries = re_cat.tracks[0]
        .auth_info
        .as_ref()
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(re_entries.len(), 2);
    assert_eq!(re_entries[0].value_raw.as_slice(), b"\"a\"".as_slice());
    assert_eq!(re_entries[1].value_raw.as_slice(), b"\"b\"".as_slice());
}

#[test]
fn auth_info_clone_roundtrip() {
    // clone 経路でも authInfo が roundtrip すること
    let json = br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","authInfo":{"cat":"%cat-token%"}}]}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Delta(delta) = doc else {
        panic!("Delta update がデコードされること");
    };
    let MsfDeltaOperation::Clone { tracks } = &delta.operations[0] else {
        panic!("Clone 操作がデコードされること");
    };
    assert_eq!(find_auth_value(tracks, "cat"), b"\"%cat-token%\"");
    // clone 経路の encode 往復でも値が保たれること
    let doc = MsfCatalogDocument::Delta(delta);
    let encoded = doc.encode().expect("encode に成功すること");
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Delta(re_delta) = decoded else {
        panic!("Delta update がデコードされること");
    };
    let MsfDeltaOperation::Clone { tracks: re_tracks } = &re_delta.operations[0] else {
        panic!("Clone 操作がデコードされること");
    };
    assert_eq!(find_auth_value(re_tracks, "cat"), b"\"%cat-token%\"");
}

fn find_auth_value<'a>(tracks: &'a [MsfCloneTrack], scheme: &str) -> &'a [u8] {
    find_auth(
        tracks[0]
            .auth_info
            .as_ref()
            .expect("テストフィクスチャの前提条件を満たす"),
        scheme,
    )
    .value_raw
    .as_slice()
}

fn find_auth<'a>(entries: &'a [MsfAuthInfo], scheme: &str) -> &'a MsfAuthInfo {
    entries
        .iter()
        .find(|e| e.scheme == scheme)
        .expect("テストフィクスチャの前提条件を満たす")
}

#[test]
fn accessibility_roundtrip() {
    // §5.6.11 と §5.6.12 の記述子を併せた合成例 (version のみ draft-01 化) が decode / encode で
    // roundtrip すること。608 と 708 の両記述子を含む
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"accessibility":[{"scheme":"urn:scte:dash:cc:cea-608:2015","value":"CC1=eng;CC3=spa"},{"scheme":"urn:scte:dash:cc:cea-708:2015","value":"1=lang:eng;2=lang:spa;3=lang:fra"}]}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(cat.tracks[0].accessibility.len(), 2);
    assert_eq!(
        cat.tracks[0].accessibility[0].scheme,
        "urn:scte:dash:cc:cea-608:2015"
    );
    assert_eq!(cat.tracks[0].accessibility[0].value, "CC1=eng;CC3=spa");
    assert_eq!(
        cat.tracks[0].accessibility[1].scheme,
        "urn:scte:dash:cc:cea-708:2015"
    );
    assert_eq!(
        cat.tracks[0].accessibility[1].value,
        "1=lang:eng;2=lang:spa;3=lang:fra"
    );
    let encoded = MsfCatalogDocument::Full(cat)
        .encode()
        .expect("encode に成功すること");
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(re_cat) = decoded else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(re_cat.tracks[0].accessibility.len(), 2);
    assert_eq!(
        re_cat.tracks[0].accessibility[0].scheme,
        "urn:scte:dash:cc:cea-608:2015"
    );
    assert_eq!(re_cat.tracks[0].accessibility[0].value, "CC1=eng;CC3=spa");
    assert_eq!(
        re_cat.tracks[0].accessibility[1].scheme,
        "urn:scte:dash:cc:cea-708:2015"
    );
    assert_eq!(
        re_cat.tracks[0].accessibility[1].value,
        "1=lang:eng;2=lang:spa;3=lang:fra"
    );
}

#[test]
fn accessibility_clone_roundtrip() {
    // clone 経路でも accessibility が roundtrip すること
    let json = br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","accessibility":[{"scheme":"urn:scte:dash:cc:cea-608:2015","value":"CC1=eng"}]}]}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Delta(delta) = doc else {
        panic!("Delta update がデコードされること");
    };
    let MsfDeltaOperation::Clone { tracks } = &delta.operations[0] else {
        panic!("Clone 操作がデコードされること");
    };
    let descs = tracks[0]
        .accessibility
        .as_ref()
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(descs.len(), 1);
    assert_eq!(descs[0].scheme, "urn:scte:dash:cc:cea-608:2015");
    assert_eq!(descs[0].value, "CC1=eng");
    let encoded = MsfCatalogDocument::Delta(delta)
        .encode()
        .expect("encode に成功すること");
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Delta(re_delta) = decoded else {
        panic!("Delta update がデコードされること");
    };
    let MsfDeltaOperation::Clone { tracks: re_tracks } = &re_delta.operations[0] else {
        panic!("Clone 操作がデコードされること");
    };
    let re_descs = re_tracks[0]
        .accessibility
        .as_ref()
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(re_descs.len(), 1);
    assert_eq!(re_descs[0].scheme, "urn:scte:dash:cc:cea-608:2015");
    assert_eq!(re_descs[0].value, "CC1=eng");
}

#[test]
fn accessibility_empty_some_roundtrip() {
    // clone の Some([]) は省略せず保持され roundtrip すること (None と区別する)
    let track = MsfCloneTrack {
        name: "c".to_string(),
        parent_name: "p".to_string(),
        parent_namespace: None,
        packaging: None,
        is_live: None,
        namespace: None,
        event_type: None,
        role: None,
        target_latency: None,
        buffers: None,
        label: None,
        render_group: None,
        alt_group: None,
        init_ref: None,
        depends: None,
        template: None,
        temporal_id: None,
        spatial_id: None,
        codec: None,
        mime_type: None,
        framerate: None,
        timescale: None,
        bitrate: None,
        avg_bitrate: None,
        max_gop_duration: None,
        max_group_duration: None,
        width: None,
        height: None,
        samplerate: None,
        channel_config: None,
        display_width: None,
        display_height: None,
        lang: None,
        track_duration: None,
        connection_uri: None,
        token: None,
        encryption_scheme: None,
        cipher_suite: None,
        key_id: None,
        track_base_key: None,
        auth_info: None,
        accessibility: Some(Vec::new()),
    };
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: None,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![track],
        }],
    });
    let encoded = doc.encode().expect("encode に成功すること");
    let text = String::from_utf8(encoded.clone()).expect("テストフィクスチャの前提条件を満たす");
    // Some([]) はキー付きで出力されること (書式は実装定義のためキー存在のみ見る)
    assert!(text.contains("\"accessibility\""));
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Delta(re_delta) = decoded else {
        panic!("Delta update がデコードされること");
    };
    let MsfDeltaOperation::Clone { tracks: re_tracks } = &re_delta.operations[0] else {
        panic!("Clone 操作がデコードされること");
    };
    assert_eq!(re_tracks[0].accessibility, Some(Vec::new()));
}

#[test]
fn accessibility_clone_none_omitted() {
    // clone の None 時はキーが出力されないこと
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: None,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![MsfCloneTrack {
                name: "c".to_string(),
                parent_name: "p".to_string(),
                parent_namespace: None,
                packaging: None,
                is_live: None,
                namespace: None,
                event_type: None,
                role: None,
                target_latency: None,
                buffers: None,
                label: None,
                render_group: None,
                alt_group: None,
                init_ref: None,
                depends: None,
                template: None,
                temporal_id: None,
                spatial_id: None,
                codec: None,
                mime_type: None,
                framerate: None,
                timescale: None,
                bitrate: None,
                avg_bitrate: None,
                max_gop_duration: None,
                max_group_duration: None,
                width: None,
                height: None,
                samplerate: None,
                channel_config: None,
                display_width: None,
                display_height: None,
                lang: None,
                track_duration: None,
                connection_uri: None,
                token: None,
                encryption_scheme: None,
                cipher_suite: None,
                key_id: None,
                track_base_key: None,
                auth_info: None,
                accessibility: None,
            }],
        }],
    });
    let encoded = doc.encode().expect("encode に成功すること");
    let text = String::from_utf8(encoded.clone()).expect("テストフィクスチャの前提条件を満たす");
    assert!(!text.contains("accessibility"));
}

#[test]
fn accessibility_full_empty_normalized() {
    // Full の明示 [] は受理され、再 encode で省略されること
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"accessibility":[]}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(cat.tracks[0].accessibility, Vec::new());
    let encoded = MsfCatalogDocument::Full(cat)
        .encode()
        .expect("encode に成功すること");
    let text = String::from_utf8(encoded.clone()).expect("テストフィクスチャの前提条件を満たす");
    assert!(!text.contains("accessibility"));
}

#[test]
fn accessibility_missing_field_rejected() {
    // scheme / value の欠如・型不正は reject されること
    for json in [
        br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"accessibility":[{"value":"CC1=eng"}]}]}"#.as_slice(),
        br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"accessibility":[{"scheme":"urn:scte:dash:cc:cea-608:2015"}]}]}"#.as_slice(),
        br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"accessibility":[{"scheme":"x","value":0}]}]}"#.as_slice(),
        br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"accessibility":{}}]}"#.as_slice(),
        br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"accessibility":"x"}]}"#.as_slice(),
        br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"accessibility":null}]}"#.as_slice(),
        br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"accessibility":[0]}]}"#.as_slice(),
        br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"accessibility":[{"scheme":0,"value":"x"}]}]}"#.as_slice(),
    ] {
        assert!(
            matches!(
                MsfCatalogDocument::decode(json),
                Err(MessageError::InvalidCatalog(_))
            ),
            "記述子内の必須欠如は reject されること"
        );
    }
}
