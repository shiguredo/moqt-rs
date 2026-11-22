use super::*;

#[test]
fn all_optional_string_fields() {
    // parentName は cloneTracks 内でのみ許可されるため tracks には含めない
    let json = br#"{
            "version": "draft-02",
            "tracks": [{
                "name": "t",
                "packaging": "loc",
                "isLive": false,
                "namespace": "ns",
                "role": "video",
                "framerate": 30,
                "label": "Main Camera",
                "codec": "av01",
                "bitrate": 1000000,
                "mimeType": "video/mp4",
                "channelConfig": "stereo",
                "lang": "en",
                "initRef": "init1"
            }],
            "initDataList": [
                {"id": "init1", "type": "inline", "data": "AAEC"}
            ]
        }"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    let t = &cat.tracks[0];
    assert_eq!(t.namespace, Some("ns".to_string()));
    assert_eq!(t.role, Some("video".to_string()));
    assert_eq!(t.label, Some("Main Camera".to_string()));
    assert_eq!(t.codec, Some("av01".to_string()));
    assert_eq!(t.mime_type, Some("video/mp4".to_string()));
    assert_eq!(t.channel_config, Some("stereo".to_string()));
    assert_eq!(t.lang, Some("en".to_string()));
    assert_eq!(t.parent_name, None);
    assert_eq!(t.init_ref, Some("init1".to_string()));
    assert_eq!(cat.init_data_list.len(), 1);
    assert_eq!(cat.init_data_list[0].id, "init1");
    assert_eq!(cat.init_data_list[0].kind, MsfInitDataKind::Inline);
    assert_eq!(cat.init_data_list[0].data, "AAEC".to_string());
}

#[test]
fn all_optional_number_fields() {
    // isLive=true と trackDuration の同時指定は禁止のため trackDuration を除外
    let json = br#"{
            "version": "draft-02",
            "tracks": [{
                "name": "t",
                "packaging": "loc",
                "isLive": true,
                "targetLatency": 500,
                "renderGroup": 2,
                "altGroup": 3,
                "temporalId": 1,
                "spatialId": 0,
                "framerate": 29.97,
                "timescale": 90000,
                "bitrate": 5000000,
                "avgBitrate": 3000000,
                "maxGopDuration": 2000,
                "maxGroupDuration": 4000,
                "width": 1920,
                "height": 1080,
                "samplerate": 48000,
                "displayWidth": 1280,
                "displayHeight": 720
            }]
        }"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    let t = &cat.tracks[0];
    assert!(t.is_live);
    assert_eq!(t.target_latency, Some(500));
    assert_eq!(t.render_group, Some(2));
    assert_eq!(t.alt_group, Some(3));
    assert_eq!(t.temporal_id, Some(1));
    assert_eq!(t.spatial_id, Some(0));
    assert_eq!(t.timescale, Some(90000));
    assert_eq!(t.bitrate, Some(5_000_000));
    assert_eq!(t.avg_bitrate, Some(3_000_000));
    assert_eq!(t.max_gop_duration, Some(2000));
    assert_eq!(t.max_group_duration, Some(4000));
    assert_eq!(t.width, Some(1920));
    assert_eq!(t.height, Some(1080));
    assert_eq!(t.samplerate, Some(48000));
    assert_eq!(t.display_width, Some(1280));
    assert_eq!(t.display_height, Some(720));
    assert_eq!(t.track_duration, None);
    // framerate は浮動小数点
    let fps = t.framerate.expect("テストフィクスチャの前提条件を満たす");
    assert!((fps - 29.97).abs() < 1e-9);
}

#[test]
fn depends_array() {
    let json = br#"{"version":"draft-02","tracks":[{
            "name":"hd","packaging":"loc","isLive":true,"depends":["sd","md"]
        },{"name":"sd","packaging":"loc","isLive":true},{"name":"md","packaging":"loc","isLive":true}]}"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(
        cat.tracks[0].depends,
        vec![
            MsfDependency::TrackName("sd".to_string()),
            MsfDependency::TrackName("md".to_string())
        ]
    );
}

#[test]
fn packaging_values() {
    // loc は最小限のフィールドで OK
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true}]}"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(cat.tracks[0].packaging, MsfPackaging::Loc);

    // mediatimeline は depends と mimeType=application/json が必須
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"mediatimeline","isLive":true,"depends":["video"],"mimeType":"application/json"},{"name":"video","packaging":"loc","isLive":true}]}"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(cat.tracks[0].packaging, MsfPackaging::MediaTimeline);

    // eventtimeline は eventType, depends, mimeType=application/json が必須
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"eventtimeline","isLive":true,"eventType":"com.example.cue","depends":["video"],"mimeType":"application/json"},{"name":"video","packaging":"loc","isLive":true}]}"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(cat.tracks[0].packaging, MsfPackaging::EventTimeline);
    assert_eq!(cat.tracks[0].event_type.as_deref(), Some("com.example.cue"));

    // moqlog は depends / mimeType=application/json を要求しない
    let json =
        br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"moqlog","isLive":true}]}"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(cat.tracks[0].packaging, MsfPackaging::MoqLog);

    // moqmetrics は depends / mimeType=application/json を要求しない
    let json =
        br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"moqmetrics","isLive":true}]}"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(cat.tracks[0].packaging, MsfPackaging::MoqMetrics);

    // catalog は別のカタログトラックを参照する packaging で、media 向けの必須フィールドを要求しない
    let json =
        br#"{"version":"draft-02","tracks":[{"name":"catalog2","packaging":"catalog","isLive":true}]}"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(cat.tracks[0].packaging, MsfPackaging::Catalog);
}

#[test]
fn target_latency_ignored_when_is_live_false() {
    // draft-ietf-moq-msf-02 §5.2.9 (Target latency): isLive=false なら targetLatency は無視される
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":false,"targetLatency":500}]}"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(cat.tracks[0].target_latency, None);
}

#[test]
fn buffers_decoded() {
    // draft-ietf-moq-msf-02 §5.2.10 (Buffers): buffers は {target, min, max} の object
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"buffers":{"target":1000,"min":500,"max":2000}}]}"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    let buffers = cat.tracks[0]
        .buffers
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(buffers.target, Some(1000));
    assert_eq!(buffers.min, Some(500));
    assert_eq!(buffers.max, Some(2000));
}

#[test]
fn buffers_unknown_keys_ignored() {
    // draft-ietf-moq-msf-02 §5.2.10 (Buffers): 未知 key は無視する
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"buffers":{"target":1000,"unknown":42}}]}"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    let buffers = cat.tracks[0]
        .buffers
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(buffers.target, Some(1000));
    assert_eq!(buffers.min, None);
    assert_eq!(buffers.max, None);
}

#[test]
fn buffers_ignored_when_is_live_false() {
    // draft-ietf-moq-msf-02 §5.2.10 (Buffers): isLive=false なら buffers は無視される
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":false,"buffers":{"target":1000}}]}"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(cat.tracks[0].buffers, None);
}

#[test]
fn avg_bitrate_decoded() {
    // draft-ietf-moq-msf-02 §5.2.24 (Average Bitrate): avgBitrate
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"bitrate":5000000,"avgBitrate":3000000}]}"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(cat.tracks[0].bitrate, Some(5_000_000));
    assert_eq!(cat.tracks[0].avg_bitrate, Some(3_000_000));
}

#[test]
fn max_durations_decoded() {
    // draft-ietf-moq-msf-02 §5.2.25 (Maximum GOP Duration) / §5.2.26 (Maximum Group Duration)
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"maxGopDuration":2000,"maxGroupDuration":4000}]}"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(cat.tracks[0].max_gop_duration, Some(2000));
    assert_eq!(cat.tracks[0].max_group_duration, Some(4000));
}

#[test]
fn connection_uri_decoded_and_token_ignored() {
    // draft-ietf-moq-msf-02 Appendix A.1.3: track-level token は削除されたため、
    // v01 のカタログに token が現れても未知フィールドとして無視する
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"moqlog","isLive":true,"namespace":"%resourceId%","role":"log","connectionURI":"moqt://logs.example.com:4443","token":"DUMMY-TOKEN"}]}"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let encoded = doc
        .encode(shiguredo_moqt::msf::MsfCompression::None)
        .expect("token を無視して encode できること");
    let text = String::from_utf8(encoded).expect("JSON は UTF-8 であること");
    assert!(
        !text.contains("\"token\""),
        "token を再出力しないこと: {text}"
    );
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(
        cat.tracks[0].connection_uri.as_deref(),
        Some("moqt://logs.example.com:4443")
    );
}

#[test]
fn connection_uri_decoded_and_token_ignored_in_clone() {
    // clone 経路でもワイヤキー名でデコードされること
    let json = br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","connectionURI":"moqt://logs.example.com:4443","token":"DUMMY-TOKEN"}]}]}"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Delta(delta) = doc else {
        panic!("Delta update がデコードされること");
    };
    let MsfDeltaOperation::Clone { tracks } = &delta.operations[0] else {
        panic!("Clone 操作がデコードされること");
    };
    assert_eq!(
        tracks[0].connection_uri.as_deref(),
        Some("moqt://logs.example.com:4443")
    );
}

#[test]
fn connection_uri_invalid_value_rejected() {
    // draft-ietf-moq-msf-02 §5.2.37: MOQT endpoint URI でない文字列は catalog decode で拒否する
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"moqlog","isLive":true,"connectionURI":"not a uri"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn connection_uri_invalid_value_rejected_in_clone() {
    // clone 経路でも MOQT endpoint URI でない文字列は拒否する
    let json = br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","connectionURI":"not a uri"}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn connection_uri_spec_examples_accepted() {
    // draft-ietf-moq-msf-02 §5.2.37 の例はすべて受理する
    for uri in [
        "moqt://logs.example.com",
        "moqt://metrics.example.com:8443",
        "moqt://logs.example.com/moqt?a=1&b=2",
    ] {
        let json = format!(
            r#"{{"version":"draft-02","tracks":[{{"name":"t","packaging":"moqlog","isLive":true,"connectionURI":"{uri}"}}]}}"#
        );
        let doc =
            MsfCatalogDocument::decode(json.as_bytes(), shiguredo_moqt::msf::MsfCompression::None)
                .expect("テストフィクスチャの前提条件を満たす");
        let MsfCatalogDocument::Full(cat) = doc else {
            panic!("Full catalog がデコードされること");
        };
        assert_eq!(cat.tracks[0].connection_uri.as_deref(), Some(uri));
    }
}

#[test]
fn connection_uri_wrong_type_rejected_and_token_ignored() {
    // connectionURI の非文字列型は reject し、削除済みの token は未知フィールドとして無視する
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"moqlog","isLive":true,"connectionURI":4443}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"moqlog","isLive":true,"token":{}}]}"#;
    assert!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None).is_ok(),
        "削除済みの token は未知フィールドとして無視すること"
    );
}

#[test]
fn max_durations_decoded_in_clone() {
    // draft-ietf-moq-msf-02 §5.2.25 (Maximum GOP Duration) / §5.2.26 (Maximum Group Duration):
    // clone 経路でもワイヤキー名でデコードされること
    let json = br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","maxGopDuration":2000,"maxGroupDuration":4000}]}]}"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Delta(delta) = doc else {
        panic!("Delta update がデコードされること");
    };
    let MsfDeltaOperation::Clone { tracks } = &delta.operations[0] else {
        panic!("Clone 操作がデコードされること");
    };
    assert_eq!(tracks[0].max_gop_duration, Some(2000));
    assert_eq!(tracks[0].max_group_duration, Some(4000));
}

/// 旧 key は未知フィールドとして無視されること
#[test]
fn legacy_connection_uri_key_is_ignored() {
    // rg で旧 key を検索してもヒットしないよう、文字列を分割して組み立てる
    let legacy_key = concat!("connection", "Uri");
    let json = format!(
        r#"{{"version":"draft-02","tracks":[{{"name":"t","packaging":"moqlog","isLive":true,"{legacy_key}":"moqt://logs.example.com:4443"}}]}}"#
    );
    let doc =
        MsfCatalogDocument::decode(json.as_bytes(), shiguredo_moqt::msf::MsfCompression::None)
            .expect("旧 key は未知フィールドとして無視されること");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    assert_eq!(
        cat.tracks[0].connection_uri, None,
        "旧 key を connection_uri に取り込まないこと"
    );

    let json = format!(
        r#"{{"version":"draft-02","tracks":[{{"name":"t","packaging":"moqlog","isLive":true,"{legacy_key}":4443}}]}}"#
    );
    assert!(
        MsfCatalogDocument::decode(json.as_bytes(), shiguredo_moqt::msf::MsfCompression::None)
            .is_ok(),
        "旧 key の型違いも未知フィールドとして無視すること"
    );
}

/// streamMapping の既定と解釈
#[test]
fn stream_mapping_defaults_to_group_and_rejects_unknown_value() {
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    assert_eq!(
        track.stream_mapping().expect("既定は group"),
        StreamMapping::Group
    );
    track.packaging_metadata = Some("streamMapping=group".to_string());
    assert_eq!(
        track.stream_mapping().expect("group を解釈できること"),
        StreamMapping::Group
    );
    track.packaging_metadata = Some("streamMapping=object;other=value".to_string());
    assert_eq!(
        track.stream_mapping().expect("object を解釈できること"),
        StreamMapping::Object
    );
    track.packaging_metadata = Some("streamMapping=unknown".to_string());
    assert!(matches!(
        track.stream_mapping(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// loc packaging の streamMapping は group / object だけを受理すること
///
/// draft-ietf-moq-msf-02 §4.1.1 (LOC to MOQT Stream mapping) は LOC publisher に
/// Table 1 の mapping mode の選択を MUST とし、§5.2.5 (Packaging metadata) の
/// streamMapping で通知する。
#[test]
fn loc_stream_mapping_values_are_validated() {
    for (value, expected_ok) in [("group", true), ("object", true), ("bogus", false)] {
        let json = format!(
            r#"{{"version":"draft-02","tracks":[{{"name":"t","packaging":"loc","isLive":true,"packagingMetadata":"streamMapping={value}"}}]}}"#
        );
        let result =
            MsfCatalogDocument::decode(json.as_bytes(), shiguredo_moqt::msf::MsfCompression::None);
        assert_eq!(
            result.is_ok(),
            expected_ok,
            "loc の streamMapping={value} の受理結果が仕様と一致すること"
        );
    }

    // encode 経路でも同じ値域を検証すること
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true}]}"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("streamMapping 省略の loc track を受理すること");
    let MsfCatalogDocument::Full(mut cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    cat.tracks[0].packaging_metadata = Some("streamMapping=bogus".to_string());
    assert!(
        matches!(
            MsfCatalogDocument::Full(cat).encode(shiguredo_moqt::msf::MsfCompression::None),
            Err(MessageError::InvalidCatalog(_))
        ),
        "loc の不正な streamMapping を encode してはならない"
    );
}

/// loc 以外の packaging では streamMapping を未知キーとして無視すること
///
/// draft-ietf-moq-msf-02 §5.2.5 (Packaging metadata): "Subscribers MUST ignore any key
/// that is not defined for the current packaging value."
#[test]
fn stream_mapping_is_ignored_for_other_packaging() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"moqlog","isLive":true,"packagingMetadata":"streamMapping=bogus"}]}"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("loc 以外の packaging では streamMapping を無視すること");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    assert!(
        MsfCatalogDocument::Full(cat)
            .encode(shiguredo_moqt::msf::MsfCompression::None)
            .is_ok(),
        "loc 以外の packaging の streamMapping は encode でも無視すること"
    );
}

#[test]
fn mime_type_audio_requires_audio_fields_for_unregistered_codec() {
    // 登録外の audio codec でも mimeType が audio/ を示せば MUST を課す
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"codec":"ac-3","mimeType":"audio/mp4"}]}"#;
    let error = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect_err("audio の必須フィールド不足を拒否すること");
    assert!(
        error.to_string().contains("mimeType 'audio/mp4'"),
        "mimeType を根拠として示すこと: {error}"
    );
}

#[test]
fn mime_type_video_requires_bitrate_for_unregistered_codec() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"codec":"mp4v","mimeType":"video/mp4"}]}"#;
    let error = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect_err("video の bitrate 不足を拒否すること");
    assert!(
        error.to_string().contains("MUST specify bitrate"),
        "bitrate の不足として報告すること: {error}"
    );
}

#[test]
fn mime_type_without_codec_requires_codec() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"bitrate":64000,"samplerate":48000,"channelConfig":"2","mimeType":"audio/mp4"}]}"#;
    let error = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect_err("codec 不足を拒否すること");
    assert!(
        error.to_string().contains("MUST specify codec"),
        "codec の不足として報告すること: {error}"
    );
}

#[test]
fn mime_type_prefix_is_case_insensitive() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"codec":"ac-3","mimeType":"Audio/MP4"}]}"#;
    assert!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None).is_err(),
        "大文字の Audio/ も audio として扱うこと"
    );
}

#[test]
fn conflicting_codec_and_mime_type_require_both_sets() {
    // codec は音声、mimeType は映像を示すため、bitrate と音声の必須フィールドの両方を要求する
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"codec":"opus","bitrate":64000,"mimeType":"video/mp4"}]}"#;
    let error = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect_err("音声の samplerate 不足を拒否すること");
    assert!(
        error.to_string().contains("MUST specify samplerate"),
        "音声の要求も重ねて適用すること: {error}"
    );
}

#[test]
fn mime_type_with_required_fields_is_accepted() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"codec":"ac-3","bitrate":64000,"samplerate":48000,"channelConfig":"2","mimeType":"audio/mp4"}]}"#;
    MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("audio の必須フィールドを揃えた track を受理すること");
}

#[test]
fn unknown_codec_without_mime_type_does_not_require_media_fields() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"codec":"custom-data"}]}"#;
    MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("mimeType の無い登録外 codec は media と判定しないこと");
}

#[test]
fn role_and_mime_type_conflict_requires_both_sets() {
    let missing_audio = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"role":"video","codec":"av01.0.08M.08","bitrate":500000,"mimeType":"audio/mp4"}]}"#;
    assert!(
        MsfCatalogDocument::decode(missing_audio, shiguredo_moqt::msf::MsfCompression::None)
            .is_err(),
        "role 側の video と mimeType 側の audio の両方を要求すること"
    );
    let both = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"role":"video","codec":"av01.0.08M.08","framerate":30,"bitrate":500000,"samplerate":48000,"channelConfig":"2","mimeType":"audio/mp4"}]}"#;
    MsfCatalogDocument::decode(both, shiguredo_moqt::msf::MsfCompression::None)
        .expect("両方の必須フィールドを揃えた track を受理すること");
}

#[test]
fn encode_rejects_mime_type_audio_without_samplerate() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"codec":"ac-3","bitrate":64000,"samplerate":48000,"channelConfig":"2","mimeType":"audio/mp4"}]}"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(mut cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    cat.tracks[0].samplerate = None;
    assert!(
        MsfCatalogDocument::Full(cat)
            .encode(shiguredo_moqt::msf::MsfCompression::None)
            .is_err(),
        "encode でも mimeType 由来の samplerate 要求を適用すること"
    );
}

#[test]
fn codec_classification_public_api_matches_registry() {
    use shiguredo_moqt::msf::{is_audio_codec, is_video_codec};

    for codec in [
        "flac",
        "mp3",
        "opus",
        "vorbis",
        "ulaw",
        "alaw",
        "mp4a",
        "mp4a.40.2",
    ] {
        assert!(is_audio_codec(codec), "audio と判定すること: {codec}");
    }
    for codec in ["opusx", "opus.2", "mp4a-40", "ac-3"] {
        assert!(!is_audio_codec(codec), "audio と判定しないこと: {codec}");
    }
    for codec in [
        "vp8",
        "av01",
        "av01.0.08M.08",
        "avc3.640028",
        "vp09",
        "vp09.00.40.08",
    ] {
        assert!(is_video_codec(codec), "video と判定すること: {codec}");
    }
    for codec in ["av01x", "avc1x", "hvc1x", "hev1x", "vp8.0", "mp4v"] {
        assert!(!is_video_codec(codec), "video と判定しないこと: {codec}");
    }
}

#[test]
fn cipher_suite_must_be_supported_with_moq_secure_objects() {
    let bad = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"encryptionScheme":"moq-secure-objects","cipherSuite":"bad-suite","keyId":"k","trackBaseKey":"AAEC"}]}"#;
    assert!(
        MsfCatalogDocument::decode(bad, shiguredo_moqt::msf::MsfCompression::None).is_err(),
        "未対応の cipherSuite を拒否すること"
    );
    let good = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"encryptionScheme":"moq-secure-objects","cipherSuite":"aes-128-gcm-sha256","keyId":"k","trackBaseKey":"AAEC"}]}"#;
    MsfCatalogDocument::decode(good, shiguredo_moqt::msf::MsfCompression::None)
        .expect("対応 cipherSuite を受理すること");
}

#[test]
fn auth_info_scheme_must_not_be_duplicated() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"authInfo":{"c4m":"AA","c4m":"BB"}}]}"#;
    assert!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None).is_err(),
        "重複した authInfo scheme を拒否すること"
    );
}

#[test]
fn encode_rejects_unsupported_catalog_version() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true}]}"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(mut cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    cat.version = "unknown".to_string();
    assert!(
        MsfCatalogDocument::Full(cat)
            .encode(shiguredo_moqt::msf::MsfCompression::None)
            .is_err(),
        "未対応 version の encode を拒否すること"
    );
}

#[test]
fn encode_rejects_non_finite_framerate() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"codec":"av01.0.08M.08","bitrate":500000,"width":640,"height":480,"framerate":30}]}"#;
    let doc = MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(mut cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    cat.tracks[0].framerate = Some(f64::NAN);
    assert!(
        MsfCatalogDocument::Full(cat)
            .encode(shiguredo_moqt::msf::MsfCompression::None)
            .is_err(),
        "非有限の framerate を encode で拒否すること"
    );
}
