use super::*;

#[test]
fn empty() {
    let tl = MsfEventTimeline::new();
    let encoded = tl.encode().expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(encoded, b"[]");
    let decoded = MsfEventTimeline::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded, tl);
}

#[test]
fn wallclock_index() {
    let tl = MsfEventTimeline(vec![MsfEventTimelineEntry {
        index: MsfEventIndex::WallclockMs(1_756_885_678_361),
        data_raw: br#"{"v":true}"#.to_vec(),
    }]);
    let encoded = tl.encode().expect("テストフィクスチャの前提条件を満たす");
    let decoded = MsfEventTimeline::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded, tl);
}

#[test]
fn data_object_with_nested_values() {
    // draft-ietf-moq-msf-01 §8.1 (Event Timeline data format): data は Object でなければならない
    let json = br#"[{"L":[0,0],"data":{"lat":47.1812,"lon":8.4592}}]"#;
    let tl = MsfEventTimeline::decode(json).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(tl.0[0].index, MsfEventIndex::Location(0, 0));
    let data_str =
        core::str::from_utf8(&tl.0[0].data_raw).expect("テストフィクスチャの前提条件を満たす");
    assert!(data_str.starts_with('{'));
}

#[test]
fn multiple_index_keys_rejected() {
    let json = br#"[{"T":1000,"L":[0,0],"data":{}}]"#;
    assert!(matches!(
        MsfEventTimeline::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn location_wrong_length_rejected() {
    let json = br#"[{"L":[0],"data":{}}]"#;
    assert!(matches!(
        MsfEventTimeline::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
    let json = br#"[{"L":[0,0,0],"data":{}}]"#;
    assert!(matches!(
        MsfEventTimeline::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn invalid_utf8_rejected() {
    let bytes = vec![0x80, 0x81];
    assert!(matches!(
        MsfEventTimeline::decode(&bytes),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn data_any_json_value_accepted() {
    // draft-ietf-moq-msf-02 §8.2 (Event Timeline data format): data は任意の JSON 値
    for (json, expected) in [
        (
            br#"[{"T":1000,"data":[1,2,3]}]"#.as_slice(),
            &b"[1,2,3]"[..],
        ),
        (
            br#"[{"T":1000,"data":"hello"}]"#.as_slice(),
            &b"\"hello\""[..],
        ),
        (br#"[{"T":1000,"data":42}]"#.as_slice(), &b"42"[..]),
        (br#"[{"T":1000,"data":null}]"#.as_slice(), &b"null"[..]),
        (br#"[{"T":1000,"data":true}]"#.as_slice(), &b"true"[..]),
    ] {
        let timeline = MsfEventTimeline::decode(json).expect("任意の JSON 値を受理すること");
        assert_eq!(timeline.0[0].data_raw, expected);
    }
}

/// 小文字の index key だけを持つ record は index 欠如として拒否されること
#[test]
fn lowercase_index_keys_rejected() {
    for json in [
        br#"[{"t":1000,"data":{}}]"#.as_slice(),
        br#"[{"l":[0,0],"data":{}}]"#.as_slice(),
        br#"[{"m":1000,"data":{}}]"#.as_slice(),
    ] {
        assert!(matches!(
            MsfEventTimeline::decode(json),
            Err(MessageError::InvalidCatalog(_))
        ));
    }
}

#[test]
fn encode_rejects_non_utf8_data_raw() {
    // 非 UTF-8 の data_raw は panic せずエラーを返す
    let tl = MsfEventTimeline(vec![MsfEventTimelineEntry {
        index: MsfEventIndex::WallclockMs(1000),
        data_raw: vec![0x80, 0x81],
    }]);
    assert!(matches!(tl.encode(), Err(MessageError::InvalidCatalog(_))));
}

#[test]
fn encode_rejects_invalid_json_data_raw() {
    // draft-ietf-moq-msf-02 §8.2 (Event Timeline data format): data は任意の JSON 値。
    // JSON として構文不正なバイト列だけを拒否する。
    let tl = MsfEventTimeline(vec![MsfEventTimelineEntry {
        index: MsfEventIndex::WallclockMs(1000),
        data_raw: b"abc".to_vec(),
    }]);
    assert!(matches!(tl.encode(), Err(MessageError::InvalidCatalog(_))));
}

#[test]
fn encode_rejects_empty_data_raw() {
    // 空バイト列は JSON として成立しないためエラーを返す
    let tl = MsfEventTimeline(vec![MsfEventTimelineEntry {
        index: MsfEventIndex::WallclockMs(1000),
        data_raw: Vec::new(),
    }]);
    assert!(matches!(tl.encode(), Err(MessageError::InvalidCatalog(_))));
}

#[test]
fn encode_rejects_trailing_garbage_after_object() {
    // data_raw 全体が単一の JSON 値でなければならない。末尾の余剰文字は拒否される
    let tl = MsfEventTimeline(vec![MsfEventTimelineEntry {
        index: MsfEventIndex::WallclockMs(1000),
        data_raw: br#"{"v":1} extra"#.to_vec(),
    }]);
    assert!(matches!(tl.encode(), Err(MessageError::InvalidCatalog(_))));
}

#[test]
fn encode_rejects_invalid_entry_in_multiple_entries() {
    // 検証は全エントリを検査する。1 エントリ目が正しくても 2 エントリ目が不正ならエラーになる
    let tl = MsfEventTimeline(vec![
        MsfEventTimelineEntry {
            index: MsfEventIndex::WallclockMs(1000),
            data_raw: br#"{"v":1}"#.to_vec(),
        },
        MsfEventTimelineEntry {
            index: MsfEventIndex::WallclockMs(2000),
            data_raw: vec![0x80, 0x81],
        },
    ]);
    assert!(matches!(tl.encode(), Err(MessageError::InvalidCatalog(_))));
}

#[test]
fn encode_preserves_raw_data_bytes() {
    // 検証のみ行い出力は再正規化しない。内部空白を含む data_raw がバイト単位で維持されることを確認する
    let tl = MsfEventTimeline(vec![MsfEventTimelineEntry {
        index: MsfEventIndex::WallclockMs(1000),
        data_raw: br#"{"v": 1}"#.to_vec(),
    }]);
    let encoded = tl.encode().expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(encoded, br#"[{"T":1000,"data":{"v": 1}}]"#);
}

#[test]
fn encode_preserves_non_ascii_raw_data_bytes() {
    // UTF-8 マルチバイトを含む data_raw もエスケープせずバイト単位で維持される
    // こんにちは は UTF-8 で \xe3\x81\x93\xe3\x82\x93\xe3\x81\xab\xe3\x81\xa1\xe3\x81\xaf
    let tl = MsfEventTimeline(vec![MsfEventTimelineEntry {
        index: MsfEventIndex::Location(1, 2),
        data_raw: b"{\"msg\":\"\xe3\x81\x93\xe3\x82\x93\xe3\x81\xab\xe3\x81\xa1\xe3\x81\xaf\"}"
            .to_vec(),
    }]);
    let encoded = tl.encode().expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        encoded,
        b"[{\"L\":[1,2],\"data\":{\"msg\":\"\xe3\x81\x93\xe3\x82\x93\xe3\x81\xab\xe3\x81\xa1\xe3\x81\xaf\"}}]"
    );
}

// ─── 深ネスト JSON の深さ制限 (nojson の MAX_NESTING_DEPTH) ─────────────────
//
// このテスト群は nojson 0.3.13 以降の深さ制限 (MAX_NESTING_DEPTH = 128) を前提とする。
// 0.3.12 以前には深さ制限が無く、深ネスト JSON でスタックオーバーフロー (SIGABRT) する。

/// 深さ `depth` のネストした JSON object を構築する (最内は `{}`、深さ 1。depth は 1 以上)
fn nested_object_json(depth: usize) -> String {
    let mut s = String::new();
    for _ in 1..depth {
        s.push_str("{\"a\":");
    }
    s.push_str("{}");
    for _ in 1..depth {
        s.push('}');
    }
    s
}

/// 深さ `depth` のネストした JSON array を構築する (最内は `0`、深さ 1。depth は 1 以上)
fn nested_array_json(depth: usize) -> String {
    let mut s = String::new();
    for _ in 1..depth {
        s.push('[');
    }
    s.push('0');
    for _ in 1..depth {
        s.push(']');
    }
    s
}

/// 深ネスト JSON の decode が panic / abort せず `Err(MessageError::InvalidCatalog)` を返すこと
///
/// nojson パーサは深さ制限 (`MAX_NESTING_DEPTH`) を持ち、超過時はエラーを返す
/// (制限が無いと深さ約 200,000 の入力でスタックオーバーフローによりプロセスが abort する)。
/// data 単体の深さが制限を超える JSON を `MsfEventTimeline::decode` に渡しても abort しない
/// ことを検証する。同一の `RawJson::parse` を経由するため、このテストで decode 系 3 関数
/// (`MsfEventTimeline::decode` / `MsfMediaTimeline::decode` / `MsfCatalogDocument::decode`)
/// を代表できる。
#[test]
fn decode_rejects_deeply_nested_document() {
    let data = nested_object_json(nojson::MAX_NESTING_DEPTH + 1);
    let json = format!("[{{\"t\":1000,\"data\":{data}}}]");
    assert!(matches!(
        MsfEventTimeline::decode(json.as_bytes()),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// 深ネスト配列を含む JSON の decode が panic / abort せず `Err(MessageError::InvalidCatalog)` を
/// 返すこと
///
/// nojson の深さ制限は `parse_object` と `parse_array` の両方に適用される。
/// ここでは「配列も累積深さ制限に参加する」ことを検証する (制限値そのものは
/// `parse_object` と同じ実装定数 `MAX_NESTING_DEPTH` に依存する)。
/// data を Object で包んでいるのは、data が素の配列だと「event data MUST be a JSON object」
/// の構造検証で先に落ち、深さ制限の有無に関係なくエラーになるため
/// (深さ制限を検証するには、制限なし実装ではパース成功 → decode 成功となる入力を
/// 使わなければならない)。
#[test]
fn decode_rejects_deeply_nested_array_document() {
    let inner = nested_array_json(nojson::MAX_NESTING_DEPTH + 1);
    let data = format!("{{\"a\":{inner}}}");
    let json = format!("[{{\"t\":1000,\"data\":{data}}}]");
    assert!(matches!(
        MsfEventTimeline::decode(json.as_bytes()),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// 深ネスト `data_raw` の encode が panic / abort せず `Err(MessageError::InvalidCatalog)` を
/// 返すこと
///
/// `validate_event_data_raw` も同一の `RawJson::parse` を経由するため、
/// 制限超過の `data_raw` は encode 時にエラーになる。
#[test]
fn encode_rejects_deeply_nested_data_raw() {
    let data = nested_object_json(nojson::MAX_NESTING_DEPTH + 1);
    let tl = MsfEventTimeline(vec![MsfEventTimelineEntry {
        index: MsfEventIndex::WallclockMs(1000),
        data_raw: data.into_bytes(),
    }]);
    assert!(matches!(tl.encode(), Err(MessageError::InvalidCatalog(_))));
}

/// data_raw 深さ `MAX_NESTING_DEPTH - 1` は encode が成功するが decode はエラーになる
/// (既知の非対称) こと
///
/// `validate_event_data_raw` は data 単独の深さしか検証しないため、`MAX_NESTING_DEPTH - 1`
/// の `data_raw` は encode できる。一方 decode 時の文書全体の深さはトップレベル配列 +
/// エントリ object で +2 され、制限を超えるため `Err(MessageError::InvalidCatalog)` になる。
/// encode/decode の文書構造差による既知の非対称をテストで固定する (送信側は成功するため、
/// 受信側で初めてエラーになる障害パターン)。
#[test]
fn encode_succeeds_but_decode_rejects_at_document_boundary() {
    let data = nested_object_json(nojson::MAX_NESTING_DEPTH - 1);
    let tl = MsfEventTimeline(vec![MsfEventTimelineEntry {
        index: MsfEventIndex::WallclockMs(1000),
        data_raw: data.into_bytes(),
    }]);
    let encoded = tl
        .encode()
        .expect("data 単独では制限内のため encode は成功すること");
    assert!(matches!(
        MsfEventTimeline::decode(&encoded),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// data_raw 深さ `MAX_NESTING_DEPTH` は encode が成功するが decode はエラーになること
///
/// encode 側の成功境界 (data 単独でちょうど制限内) を固定する。
/// decode は文書全体で +2 されるためエラーになる (`encode_succeeds_but_decode_rejects_at_document_boundary`
/// と同型の非対称)。
#[test]
fn encode_succeeds_but_decode_rejects_at_data_max_depth() {
    let data = nested_object_json(nojson::MAX_NESTING_DEPTH);
    let tl = MsfEventTimeline(vec![MsfEventTimelineEntry {
        index: MsfEventIndex::WallclockMs(1000),
        data_raw: data.into_bytes(),
    }]);
    let encoded = tl
        .encode()
        .expect("data 単独で制限ちょうどのため encode は成功すること");
    assert!(matches!(
        MsfEventTimeline::decode(&encoded),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// `data_raw` 深さ `MAX_NESTING_DEPTH - 2` の roundtrip が成功すること
///
/// decode 時の文書全体の深さはトップレベル配列 + エントリ object で +2 されるため、
/// `data_raw` 深さ `MAX_NESTING_DEPTH - 2` がラウンドトリップ保証の境界になる
/// (`encode_succeeds_but_decode_rejects_at_document_boundary` の非対称との境界)。
/// なお +2 は event timeline の文書構造に依存する値であり、media timeline は
/// トップレベル + tracks 配列 + エントリで +3、catalog はトップレベル object で +1 になる。
/// 境界値は `nojson::MAX_NESTING_DEPTH` から導出し、ハードコードしない
/// (nojson のドキュメントは制限値の将来変更を明示している)。
#[test]
fn deep_data_raw_roundtrip_at_boundary() {
    let data = nested_object_json(nojson::MAX_NESTING_DEPTH - 2);
    let tl = MsfEventTimeline(vec![MsfEventTimelineEntry {
        index: MsfEventIndex::WallclockMs(1000),
        data_raw: data.into_bytes(),
    }]);
    let encoded = tl.encode().expect("境界内の data_raw は encode できること");
    let decoded =
        MsfEventTimeline::decode(&encoded).expect("境界内の data_raw は decode できること");
    assert_eq!(decoded, tl);
}

/// event timeline の index が MediaPtsMs を参照する場合の depends 検証
#[test]
fn event_timeline_depends_validation_requires_media_timeline_dependency() {
    let mut catalog = MsfCatalog::new();
    let media = MsfTrack::new("media".to_string(), MsfPackaging::MediaTimeline, true);
    let mut event = MsfTrack::new("event".to_string(), MsfPackaging::EventTimeline, true);
    event.event_type = Some("com.example.cue".to_string());
    catalog.tracks.push(media);
    catalog.tracks.push(event);

    // MediaPtsMs を持たない timeline は depends なしでよい
    let no_media_pts = MsfEventTimeline(vec![MsfEventTimelineEntry {
        index: MsfEventIndex::WallclockMs(1),
        data_raw: b"{}".to_vec(),
    }]);
    assert!(
        validate_event_timeline_depends(&catalog, &no_media_pts).is_ok(),
        "MediaPtsMs が無ければ depends を要求しないこと"
    );

    // MediaPtsMs を持つのに media timeline への依存が無ければ拒否する
    let media_pts = MsfEventTimeline(vec![MsfEventTimelineEntry {
        index: MsfEventIndex::MediaPtsMs(1),
        data_raw: b"{}".to_vec(),
    }]);
    assert!(
        matches!(
            validate_event_timeline_depends(&catalog, &media_pts),
            Err(MessageError::InvalidCatalog(_))
        ),
        "MediaPtsMs を持つ場合は media timeline への依存を要求すること"
    );

    // media timeline track を depends に列挙すれば受理する
    catalog.tracks[1].depends = vec![MsfDependency::TrackName("media".to_string())];
    assert!(
        validate_event_timeline_depends(&catalog, &media_pts).is_ok(),
        "media timeline を depends に持つ場合は受理すること"
    );
}

/// v02 の大文字 index key と任意 JSON 値の data が往復すること
#[test]
fn v02_index_keys_and_any_data_roundtrip() {
    let json = br#"[{"T":1000,"data":{"status":"ok"}},{"L":[0,0],"data":"text"},{"M":2000,"data":[1,2,3]}]"#;
    let timeline = MsfEventTimeline::decode(json).expect("v02 の index key を受理すること");
    assert_eq!(timeline.0[0].index, MsfEventIndex::WallclockMs(1000));
    assert_eq!(timeline.0[1].index, MsfEventIndex::Location(0, 0));
    assert_eq!(timeline.0[2].index, MsfEventIndex::MediaPtsMs(2000));

    let encoded = timeline.encode().expect("encode に成功すること");
    let decoded = MsfEventTimeline::decode(&encoded).expect("roundtrip できること");
    assert_eq!(decoded, timeline);
    let text = String::from_utf8(encoded).expect("JSON は UTF-8 であること");
    assert!(text.contains("\"T\":1000"));
    assert!(text.contains("\"L\":[0,0]"));
    assert!(text.contains("\"M\":2000"));
    assert!(!text.contains("\"t\":"));
    assert!(!text.contains("\"l\":"));
    assert!(!text.contains("\"m\":"));
}
