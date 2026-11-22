use super::*;

// ─── codec / role に応じた必須フィールド ──────────────────────────────────────

/// role="video" は codec 必須 (draft-ietf-moq-msf-02 §5.2.19 (Codec))
#[test]
fn video_role_missing_codec_rejected() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"v","packaging":"loc","isLive":true,"role":"video","framerate":30,"bitrate":1000}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) =
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
    else {
        panic!("role 由来の codec 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("codec") && reason.contains("role 'video'"),
        "role 由来の codec 要求を述べること: {reason}"
    );
}

/// role="video" は bitrate 必須 (draft-ietf-moq-msf-02 §5.2.23 (Maximum Bitrate))
#[test]
fn video_role_missing_bitrate_rejected() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"v","packaging":"loc","isLive":true,"role":"video","framerate":30,"codec":"av01"}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) =
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
    else {
        panic!("role 由来の bitrate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("bitrate") && reason.contains("role 'video'"),
        "role 由来の bitrate 要求を述べること: {reason}"
    );
}

/// role="audiodescription" は audio として扱い samplerate / channelConfig を要求する
///
/// draft-ietf-moq-msf-02 §5.2.7 Table 10 は `audiodescription` を
/// "An audio description for visually impaired users" と定める。
#[test]
fn audiodescription_role_requires_audio_fields_rejected() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"a","packaging":"loc","isLive":true,"role":"audiodescription","lang":"en","codec":"opus","bitrate":32000,"channelConfig":"2"}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) =
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
    else {
        panic!("role 由来の samplerate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("samplerate") && reason.contains("role 'audiodescription'"),
        "role 由来の samplerate 要求を述べること: {reason}"
    );
    let json = br#"{"version":"draft-02","tracks":[{"name":"a","packaging":"loc","isLive":true,"role":"audiodescription","lang":"en","codec":"opus","bitrate":32000,"samplerate":48000}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) =
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
    else {
        panic!("role 由来の channelConfig 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("channelConfig") && reason.contains("role 'audiodescription'"),
        "role 由来の channelConfig 要求を述べること: {reason}"
    );
}

/// role="signlanguage" は visual track のため video として扱い codec / bitrate を要求する
///
/// draft-ietf-moq-msf-02 §5.2.7 Table 10 は `signlanguage` を
/// "A visual track for hearing impaired users." と定める。codec には登録外の文字列を使い、
/// codec 由来ではなく role 由来の要求であることを固定する。
#[test]
fn signlanguage_role_is_treated_as_video() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"v","packaging":"loc","isLive":true,"role":"signlanguage","framerate":30}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) =
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
    else {
        panic!("role=signlanguage は codec を要求すること");
    };
    assert!(
        reason.contains("codec") && reason.contains("role 'signlanguage'"),
        "role 由来の codec 要求を述べること: {reason}"
    );
    let json = br#"{"version":"draft-02","tracks":[{"name":"v","packaging":"loc","isLive":true,"role":"signlanguage","framerate":30,"codec":"unregistered"}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) =
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
    else {
        panic!("role=signlanguage は bitrate を要求すること");
    };
    assert!(
        reason.contains("bitrate") && reason.contains("role 'signlanguage'"),
        "role 由来の bitrate 要求を述べること: {reason}"
    );
    let json = br#"{"version":"draft-02","tracks":[{"name":"v","packaging":"loc","isLive":true,"role":"signlanguage","framerate":30,"codec":"unregistered","bitrate":1000}]}"#;
    assert!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None).is_ok(),
        "role=signlanguage でも必須フィールドを満たせば受理されること"
    );
}

/// role="audio" は samplerate / channelConfig 必須 (§5.2.28 / §5.2.29)
#[test]
fn audio_role_missing_samplerate_rejected() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"a","packaging":"loc","isLive":true,"role":"audio","codec":"opus","bitrate":32000,"channelConfig":"2"}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) =
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
    else {
        panic!("role 由来の samplerate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("samplerate") && reason.contains("role 'audio'"),
        "role 由来の samplerate 要求を述べること: {reason}"
    );
    let json = br#"{"version":"draft-02","tracks":[{"name":"a","packaging":"loc","isLive":true,"role":"audio","codec":"opus","bitrate":32000,"samplerate":48000}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) =
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
    else {
        panic!("role 由来の channelConfig 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("channelConfig") && reason.contains("role 'audio'"),
        "role 由来の channelConfig 要求を述べること: {reason}"
    );
}

/// role="video" / "audio" が必須フィールドを満たせば受理される
#[test]
fn media_role_with_required_fields_accepted() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"v","packaging":"loc","isLive":true,"role":"video","framerate":30,"codec":"av01","bitrate":1000},{"name":"a","packaging":"loc","isLive":true,"role":"audio","codec":"opus","bitrate":32000,"samplerate":48000,"channelConfig":"2"}]}"#;
    assert!(MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None).is_ok());
}

/// codec も role も無いトラックは種別を判定できないため codec / bitrate を要求しない
///
/// draft-ietf-moq-msf-02 §5.2.19 (Codec) の
/// "It is not required for raw data tracks or event streams." と整合する。
#[test]
fn missing_role_does_not_require_media_fields() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true}]}"#;
    assert!(MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None).is_ok());
}

/// encode 時にも role に応じた必須フィールドが検証される
#[test]
fn media_role_missing_fields_rejected_on_encode() {
    let mut track = MsfTrack::new("v".to_string(), MsfPackaging::Loc, true);
    track.role = Some("video".to_string());
    track.codec = Some("av01".to_string());
    track.framerate = Some(30.0);
    // bitrate を欠いたまま encode するとエラーになる
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        tracks: vec![track],
        ..MsfCatalog::new()
    });
    let Err(MessageError::InvalidCatalog(reason)) =
        doc.encode(shiguredo_moqt::msf::MsfCompression::None)
    else {
        panic!("role 由来の bitrate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("bitrate") && reason.contains("role 'video'"),
        "role 由来の bitrate 要求を述べること: {reason}"
    );
}

/// role 省略 + codec `opus` は samplerate 必須 (§5.2.28 (Audio sample rate))
#[test]
fn codec_audio_missing_samplerate_rejected() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"a","packaging":"loc","isLive":true,"codec":"opus","bitrate":32000,"channelConfig":"2"}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) =
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
    else {
        panic!("codec 由来の samplerate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("samplerate") && reason.contains("codec 'opus'"),
        "codec 由来の samplerate 要求を述べること: {reason}"
    );
}

/// role 省略 + codec `opus` は channelConfig 必須 (§5.2.29 (Channel configuration))
#[test]
fn codec_audio_missing_channel_config_rejected() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"a","packaging":"loc","isLive":true,"codec":"opus","bitrate":32000,"samplerate":48000}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) =
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
    else {
        panic!("codec 由来の channelConfig 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("channelConfig") && reason.contains("codec 'opus'"),
        "codec 由来の channelConfig 要求を述べること: {reason}"
    );
}

/// role 省略 + codec `opus` は bitrate 必須 (§5.2.22 (Maximum Bitrate))
#[test]
fn codec_audio_missing_bitrate_rejected() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"a","packaging":"loc","isLive":true,"codec":"opus","samplerate":48000,"channelConfig":"2"}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) =
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
    else {
        panic!("codec 由来の bitrate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("bitrate") && reason.contains("codec 'opus'"),
        "codec 由来の bitrate 要求を述べること: {reason}"
    );
}

/// role 省略 + codec `av01.0.08M.08` は bitrate 必須 (§5.2.22 (Maximum Bitrate))
#[test]
fn codec_video_missing_bitrate_rejected() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"v","packaging":"loc","isLive":true,"codec":"av01.0.08M.08"}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) =
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
    else {
        panic!("codec 由来の bitrate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("bitrate") && reason.contains("codec 'av01.0.08M.08'"),
        "codec 由来の bitrate 要求を述べること: {reason}"
    );
}

/// レジストリの登録名だけの codec `av01` も video と判定して bitrate を要求する
///
/// draft-ietf-moq-msf-02 §5.6.2 のカタログ例は `"codec":"av01"` を使う。
#[test]
fn registry_name_only_video_codec_missing_bitrate_rejected() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"v","packaging":"loc","isLive":true,"codec":"av01"}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) =
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
    else {
        panic!("codec 由来の bitrate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("bitrate") && reason.contains("codec 'av01'"),
        "codec 由来の bitrate 要求を述べること: {reason}"
    );
}

/// role 省略 + codec `opus` で bitrate / samplerate / channelConfig を揃えれば受理される
#[test]
fn codec_audio_with_required_fields_accepted() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"a","packaging":"loc","isLive":true,"codec":"opus","bitrate":32000,"samplerate":48000,"channelConfig":"2"}]}"#;
    assert!(MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None).is_ok());
}

/// role 省略 + codec `av01.0.08M.08` で bitrate を揃えれば受理される
#[test]
fn codec_video_with_required_fields_accepted() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"v","packaging":"loc","isLive":true,"codec":"av01.0.08M.08","bitrate":1000}]}"#;
    assert!(MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None).is_ok());
}

/// 登録名に一致しない codec は audio / video と判定せず、codec に基づく要求を行わない
#[test]
fn unregistered_codec_does_not_require_media_fields() {
    for codec in ["opusx", "opus.2", "vp8x", "flacx"] {
        let json = format!(
            r#"{{"version":"draft-02","tracks":[{{"name":"t","packaging":"loc","isLive":true,"codec":"{codec}"}}]}}"#
        );
        assert!(
            MsfCatalogDocument::decode(json.as_bytes(), shiguredo_moqt::msf::MsfCompression::None)
                .is_ok(),
            "登録外の codec '{codec}' は codec に基づく要求を行わない"
        );
    }
}

/// WEBCODECS-CODEC-REGISTRY の audio 登録名を audio と判定すること
///
/// bitrate だけを与え、audio 固有の samplerate 要求で拒否されることで audio 判定を固定する
/// (video と取り違えていれば bitrate だけで受理される)。
/// 登録内容は WEBCODECS-CODEC-REGISTRY (Registry Draft, 2026-02-12) §3 (Audio Codec Registry)。
#[test]
fn registry_audio_codec_names_are_classified() {
    // `*` 付き登録名は登録名単独と可変サフィックス付きの両方を渡す
    let audio_codecs = [
        "flac",
        "mp3",
        "mp4a",
        "mp4a.40.2",
        "opus",
        "vorbis",
        "ulaw",
        "alaw",
        "pcm",
        "pcm-s16",
    ];
    for codec in audio_codecs {
        let json = format!(
            r#"{{"version":"draft-02","tracks":[{{"name":"t","packaging":"loc","isLive":true,"codec":"{codec}","bitrate":32000}}]}}"#
        );
        let Err(MessageError::InvalidCatalog(reason)) =
            MsfCatalogDocument::decode(json.as_bytes(), shiguredo_moqt::msf::MsfCompression::None)
        else {
            panic!("audio の登録名 '{codec}' は samplerate を要求すること");
        };
        assert!(
            reason.contains("samplerate") && reason.contains(&format!("codec '{codec}'")),
            "codec 由来の samplerate 要求を述べること: {reason}"
        );
    }
}

/// WEBCODECS-CODEC-REGISTRY の video 登録名を video と判定すること
///
/// bitrate を与えれば受理され (audio と取り違えていない)、bitrate を欠けば video 判定により
/// 拒否される (未判定なら受理されてしまう) の両方向で video 判定を固定する。
/// 登録内容は WEBCODECS-CODEC-REGISTRY (Registry Draft, 2026-02-12) §4 (Video Codec Registry)。
#[test]
fn registry_video_codec_names_are_classified() {
    // `*` 付き登録名は登録名単独と可変サフィックス付きの両方を渡す
    let video_codecs = [
        "av01",
        "av01.0.08M.08",
        "avc1",
        "avc1.640028",
        "avc3",
        "avc3.640028",
        "hev1",
        "hev1.1.6.L120.B0",
        "hvc1",
        "hvc1.1.6.L120.B0",
        "vp8",
        "vp09",
        "vp09.0.10.08",
    ];
    for codec in video_codecs {
        let json = format!(
            r#"{{"version":"draft-02","tracks":[{{"name":"t","packaging":"loc","isLive":true,"codec":"{codec}","bitrate":1000}}]}}"#
        );
        assert!(
            MsfCatalogDocument::decode(json.as_bytes(), shiguredo_moqt::msf::MsfCompression::None)
                .is_ok(),
            "video の登録名 '{codec}' は bitrate だけで受理されること (audio と取り違えない)"
        );
        let json = format!(
            r#"{{"version":"draft-02","tracks":[{{"name":"t","packaging":"loc","isLive":true,"codec":"{codec}"}}]}}"#
        );
        let Err(MessageError::InvalidCatalog(reason)) =
            MsfCatalogDocument::decode(json.as_bytes(), shiguredo_moqt::msf::MsfCompression::None)
        else {
            panic!(
                "video の登録名 '{codec}' は bitrate を要求すること (未判定なら受理されてしまう)"
            );
        };
        assert!(
            reason.contains("bitrate") && reason.contains(&format!("codec '{codec}'")),
            "codec 由来の bitrate 要求を述べること: {reason}"
        );
    }
}

/// `*` 付き登録名の区切り文字だけで可変サフィックスが空でも一致とすること
///
/// 登録表記の `*` に隣接する区切り文字までを導入記号として前方一致するため、サフィックスが
/// 空の `名前.` / `名前-` も一致とみなす。登録上は完全修飾形ではないが、一致とみなす側に
/// 倒れても「より厳しく拒否する」方向にしか働かない。
#[test]
fn codec_separator_only_suffix_is_classified() {
    // audio 側は bitrate を与えても samplerate を欠けば拒否される (audio 判定を固定)
    for codec in ["mp4a.", "pcm-"] {
        let json = format!(
            r#"{{"version":"draft-02","tracks":[{{"name":"t","packaging":"loc","isLive":true,"codec":"{codec}","bitrate":32000}}]}}"#
        );
        let Err(MessageError::InvalidCatalog(reason)) =
            MsfCatalogDocument::decode(json.as_bytes(), shiguredo_moqt::msf::MsfCompression::None)
        else {
            panic!("登録名 '{codec}' は samplerate を要求すること");
        };
        assert!(
            reason.contains("samplerate") && reason.contains(&format!("codec '{codec}'")),
            "codec 由来の samplerate 要求を述べること: {reason}"
        );
    }
    // video 側は bitrate だけで受理される (video 判定を固定)
    for codec in ["av01.", "avc1."] {
        let json = format!(
            r#"{{"version":"draft-02","tracks":[{{"name":"t","packaging":"loc","isLive":true,"codec":"{codec}","bitrate":1000}}]}}"#
        );
        assert!(
            MsfCatalogDocument::decode(json.as_bytes(), shiguredo_moqt::msf::MsfCompression::None)
                .is_ok(),
            "登録名 '{codec}' は bitrate だけで受理されること (audio と取り違えない)"
        );
    }
}

/// 登録名の直後が区切り文字でない codec は audio / video と判定しないこと
///
/// 前方一致だけで判定すると `mp4ax` / `pcmx` / `av01x` などを誤判定する。
#[test]
fn codec_boundary_separator_is_required() {
    for codec in [
        "mp4ax", "pcmx", "av01x", "avc1x", "avc3x", "hev1x", "hvc1x", "vp09x",
    ] {
        let json = format!(
            r#"{{"version":"draft-02","tracks":[{{"name":"t","packaging":"loc","isLive":true,"codec":"{codec}"}}]}}"#
        );
        assert!(
            MsfCatalogDocument::decode(json.as_bytes(), shiguredo_moqt::msf::MsfCompression::None)
                .is_ok(),
            "区切り文字が無い codec '{codec}' は codec に基づく要求を行わない"
        );
    }
}

/// role と codec の判定が食い違う場合は両方の要求を重ねて適用する
///
/// role=`video` + codec=`opus` は video の bitrate と audio の samplerate /
/// channelConfig をすべて要求する。bitrate を満たしても samplerate / channelConfig を
/// 欠けば、codec を根拠に拒否される。
#[test]
fn role_and_codec_requirements_are_combined() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"role":"video","framerate":30,"codec":"opus","bitrate":32000}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) =
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
    else {
        panic!("samplerate 欠如は codec を根拠に拒否されること");
    };
    assert!(
        reason.contains("codec 'opus'") && reason.contains("samplerate"),
        "codec 由来の samplerate 要求を述べること: {reason}"
    );
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"role":"video","framerate":30,"codec":"opus","bitrate":32000,"samplerate":48000}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) =
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
    else {
        panic!("channelConfig 欠如は codec を根拠に拒否されること");
    };
    assert!(
        reason.contains("codec 'opus'") && reason.contains("channelConfig"),
        "codec 由来の channelConfig 要求を述べること: {reason}"
    );
    // 両方の要求を満たせば受理される
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"role":"video","framerate":30,"codec":"opus","bitrate":32000,"samplerate":48000,"channelConfig":"2"}]}"#;
    assert!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None).is_ok(),
        "role と codec の要求を満たすカタログは受理されること"
    );
}

/// role 省略 + codec `opus` の bitrate 欠如は encode 経路でも拒否される
#[test]
fn codec_audio_missing_bitrate_rejected_on_encode() {
    let mut track = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    track.codec = Some("opus".to_string());
    track.samplerate = Some(48_000);
    track.channel_config = Some("2".to_string());
    // bitrate を欠いたまま encode するとエラーになる
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        tracks: vec![track],
        ..MsfCatalog::new()
    });
    let Err(MessageError::InvalidCatalog(reason)) =
        doc.encode(shiguredo_moqt::msf::MsfCompression::None)
    else {
        panic!("codec 由来の bitrate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("bitrate") && reason.contains("codec 'opus'"),
        "codec 由来の bitrate 要求を述べること: {reason}"
    );
}

/// role 省略 + codec `opus` の samplerate 欠如は encode 経路でも拒否される
#[test]
fn codec_audio_missing_samplerate_rejected_on_encode() {
    let mut track = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    track.codec = Some("opus".to_string());
    track.bitrate = Some(32_000);
    track.channel_config = Some("2".to_string());
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        tracks: vec![track],
        ..MsfCatalog::new()
    });
    let Err(MessageError::InvalidCatalog(reason)) =
        doc.encode(shiguredo_moqt::msf::MsfCompression::None)
    else {
        panic!("codec 由来の samplerate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("samplerate") && reason.contains("codec 'opus'"),
        "codec 由来の samplerate 要求を述べること: {reason}"
    );
}

/// role 省略 + codec `opus` の channelConfig 欠如は encode 経路でも拒否される
#[test]
fn codec_audio_missing_channel_config_rejected_on_encode() {
    let mut track = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    track.codec = Some("opus".to_string());
    track.bitrate = Some(32_000);
    track.samplerate = Some(48_000);
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        tracks: vec![track],
        ..MsfCatalog::new()
    });
    let Err(MessageError::InvalidCatalog(reason)) =
        doc.encode(shiguredo_moqt::msf::MsfCompression::None)
    else {
        panic!("codec 由来の channelConfig 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("channelConfig") && reason.contains("codec 'opus'"),
        "codec 由来の channelConfig 要求を述べること: {reason}"
    );
}

/// role 省略 + codec `av01` の bitrate 欠如は encode 経路でも拒否される
#[test]
fn registry_name_only_video_codec_missing_bitrate_rejected_on_encode() {
    let mut track = MsfTrack::new("v".to_string(), MsfPackaging::Loc, true);
    track.codec = Some("av01".to_string());
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        tracks: vec![track],
        ..MsfCatalog::new()
    });
    let Err(MessageError::InvalidCatalog(reason)) =
        doc.encode(shiguredo_moqt::msf::MsfCompression::None)
    else {
        panic!("codec 由来の bitrate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("bitrate") && reason.contains("codec 'av01'"),
        "codec 由来の bitrate 要求を述べること: {reason}"
    );
}

/// role も codec も無いトラックを持つカタログは encode 経路でも受理される
///
/// codec に基づく要求は codec を持つトラックにのみ課されるため、raw data トラックは
/// bitrate を要求しない (draft-ietf-moq-msf-02 §5.2.19 (Codec))。
#[test]
fn raw_track_without_codec_accepted_on_encode() {
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        tracks: vec![MsfTrack::new("raw".to_string(), MsfPackaging::Loc, true)],
        ..MsfCatalog::new()
    });
    assert!(
        doc.encode(shiguredo_moqt::msf::MsfCompression::None)
            .is_ok(),
        "codec も role も無いトラックは codec に基づく要求を行わないこと"
    );
}

/// role="video" / "signlanguage" は Table 6 で framerate を要求する
#[test]
fn video_and_signlanguage_roles_require_framerate() {
    for role in ["video", "signlanguage"] {
        let json = format!(
            r#"{{"version":"draft-02","tracks":[{{"name":"v","packaging":"loc","isLive":true,"role":"{role}","codec":"av01","bitrate":1000}}]}}"#
        );
        let Err(MessageError::InvalidCatalog(reason)) =
            MsfCatalogDocument::decode(json.as_bytes(), shiguredo_moqt::msf::MsfCompression::None)
        else {
            panic!("role={role} の framerate 欠如は InvalidCatalog であること");
        };
        assert!(
            reason.contains("framerate") && reason.contains(role),
            "role={role} 由来の framerate 要求を述べること: {reason}"
        );
    }
}

/// role="audiodescription" は Table 6 で lang を要求する
#[test]
fn audiodescription_role_requires_lang() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"a","packaging":"loc","isLive":true,"role":"audiodescription","codec":"opus","bitrate":32000,"samplerate":48000,"channelConfig":"2"}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) =
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None)
    else {
        panic!("lang 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("lang") && reason.contains("audiodescription"),
        "role 由来の lang 要求を述べること: {reason}"
    );
}

/// role="data" の最小 track は追加必須フィールドを要求されない
#[test]
fn data_role_minimal_track_accepted() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"d","packaging":"loc","isLive":true,"role":"data"}]}"#;
    assert!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None).is_ok(),
        "data role は codec / bitrate / framerate / lang を要求しないこと"
    );
}

/// 未知のカスタム role の最小 track は追加必須フィールドを要求されない
#[test]
fn custom_role_minimal_track_accepted() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"x","packaging":"loc","isLive":true,"role":"foo"}]}"#;
    assert!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None).is_ok(),
        "指定 role と衝突しないカスタム role を受理すること"
    );
}

/// encode 時にも role="video" の framerate 欠如が拒否される
#[test]
fn encode_rejects_video_role_without_framerate() {
    let mut track = MsfTrack::new("v".to_string(), MsfPackaging::Loc, true);
    track.role = Some("video".to_string());
    track.codec = Some("av01".to_string());
    track.bitrate = Some(1000);
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        tracks: vec![track],
        ..MsfCatalog::new()
    });
    let Err(MessageError::InvalidCatalog(reason)) =
        doc.encode(shiguredo_moqt::msf::MsfCompression::None)
    else {
        panic!("role 由来の framerate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("framerate") && reason.contains("role 'video'"),
        "role 由来の framerate 要求を述べること: {reason}"
    );
}
