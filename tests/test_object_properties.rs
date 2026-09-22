/// Object-scoped Properties の単体テスト
///
/// draft-ietf-moq-transport-21 §10.7 (Immutable Properties) / draft-ietf-moq-transport-21 §10.8 (Prior Group ID Gap) / draft-ietf-moq-transport-21 §10.9 (Prior Object ID Gap) の境界条件・エラーパス
/// および IMMUTABLE_PROPERTIES の入れ子検出など PBT で書きにくいケースと、
/// `ObjectFieldMismatch` が `core::error::Error` を実装していることを検証する。
use shiguredo_moqt::error::MessageError;
use shiguredo_moqt::object_properties::{
    ObjectFieldTracker, ObjectProperties, ObjectProperty, ObjectPropertyTracker,
    ObjectPropertyValue, PROP_PRIOR_GROUP_ID_GAP, PROP_PRIOR_OBJECT_ID_GAP,
};
use shiguredo_moqt::track_properties::PROP_IMMUTABLE_PROPERTIES;
use shiguredo_moqt::varint;

#[test]
fn empty_encode_decode() {
    let props = ObjectProperties::new();
    let mut buf = Vec::new();
    props
        .encode(&mut buf)
        .expect("正当なテスト入力の encode は成功する");
    assert_eq!(buf, vec![0x00]);

    let (decoded, consumed) =
        ObjectProperties::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
    assert!(decoded.is_empty());
    assert_eq!(consumed, 1);
}

#[test]
fn prior_group_id_gap_roundtrip() {
    let mut props = ObjectProperties::new();
    props.push(ObjectProperty {
        prop_type: PROP_PRIOR_GROUP_ID_GAP,
        value: ObjectPropertyValue::VarInt(7),
    });
    let mut buf = Vec::new();
    props
        .encode(&mut buf)
        .expect("正当なテスト入力の encode は成功する");

    let (decoded, consumed) =
        ObjectProperties::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(consumed, buf.len());
    assert_eq!(decoded.prior_group_id_gap(), Some(7));
    assert_eq!(decoded.prior_object_id_gap(), None);
}

#[test]
fn prior_object_id_gap_roundtrip() {
    let mut props = ObjectProperties::new();
    props.push(ObjectProperty {
        prop_type: PROP_PRIOR_OBJECT_ID_GAP,
        value: ObjectPropertyValue::VarInt(3),
    });
    let mut buf = Vec::new();
    props
        .encode(&mut buf)
        .expect("正当なテスト入力の encode は成功する");

    let (decoded, _consumed) =
        ObjectProperties::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded.prior_object_id_gap(), Some(3));
}

#[test]
fn multiple_properties_sorted_delta_encoded() {
    let mut props = ObjectProperties::new();
    // 意図的に昇順で push しない
    props.push(ObjectProperty {
        prop_type: PROP_PRIOR_OBJECT_ID_GAP,
        value: ObjectPropertyValue::VarInt(2),
    });
    props.push(ObjectProperty {
        prop_type: PROP_PRIOR_GROUP_ID_GAP,
        value: ObjectPropertyValue::VarInt(1),
    });

    let mut buf = Vec::new();
    props
        .encode(&mut buf)
        .expect("正当なテスト入力の encode は成功する");

    let (decoded, _consumed) =
        ObjectProperties::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded.as_slice().len(), 2);
    // デコード結果は prop_type 昇順で並ぶ
    assert_eq!(decoded.as_slice()[0].prop_type, PROP_PRIOR_GROUP_ID_GAP);
    assert_eq!(decoded.as_slice()[1].prop_type, PROP_PRIOR_OBJECT_ID_GAP);
}

#[test]
fn immutable_properties_with_inner_pairs() {
    // IMMUTABLE_PROPERTIES の内側に PRIOR_GROUP_ID_GAP / PRIOR_OBJECT_ID_GAP を詰める
    let mut inner = ObjectProperties::new();
    inner.push(ObjectProperty {
        prop_type: PROP_PRIOR_GROUP_ID_GAP,
        value: ObjectPropertyValue::VarInt(10),
    });
    inner.push(ObjectProperty {
        prop_type: PROP_PRIOR_OBJECT_ID_GAP,
        value: ObjectPropertyValue::VarInt(20),
    });
    let mut inner_buf = Vec::new();
    inner
        .encode(&mut inner_buf)
        .expect("正当なテスト入力の encode は成功する");
    // encode() は Properties Length varint を先頭に書くので、中身だけを取り出す
    // varint 1 バイトで len を表現できる場合の前提 (inner_buf 長 < 128)
    let len = inner_buf[0] as usize;
    assert_eq!(inner_buf.len(), 1 + len);
    let raw_inner = inner_buf[1..].to_vec();

    let mut outer = ObjectProperties::new();
    outer.push(ObjectProperty {
        prop_type: PROP_IMMUTABLE_PROPERTIES,
        value: ObjectPropertyValue::Bytes(raw_inner.clone()),
    });
    let mut buf = Vec::new();
    outer
        .encode(&mut buf)
        .expect("正当なテスト入力の encode は成功する");

    let (decoded, _consumed) =
        ObjectProperties::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
    let inner_bytes = decoded
        .immutable_properties()
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(inner_bytes, raw_inner.as_slice());
    assert_eq!(decoded.prior_group_id_gap(), Some(10));
}

#[test]
fn nested_immutable_properties_rejected_on_encode() {
    // IMMUTABLE_PROPERTIES の中に IMMUTABLE_PROPERTIES を入れる
    // 内側用の K-V を手動で組み立てる: delta=0x0B, len=0
    let nested_inner = vec![0x0B, 0x00];

    let mut outer = ObjectProperties::new();
    outer.push(ObjectProperty {
        prop_type: PROP_IMMUTABLE_PROPERTIES,
        value: ObjectPropertyValue::Bytes(nested_inner),
    });
    let mut buf = Vec::new();
    let err = outer.encode(&mut buf).unwrap_err();
    assert!(matches!(err, MessageError::ProtocolViolation(_)));
}

#[test]
fn nested_immutable_properties_rejected_on_decode() {
    // 外側: IMMUTABLE_PROPERTIES の中に IMMUTABLE_PROPERTIES
    //   Outer: Length=4, [delta=0x0B, inner_len=2, delta=0x0B, inner_len=0]
    let buf = vec![
        0x04, // Outer Properties Length
        0x0B, // delta to 0x0B (outer IMMUTABLE_PROPERTIES)
        0x02, // value length = 2
        0x0B, // inner delta to 0x0B (nested IMMUTABLE_PROPERTIES)
        0x00, // inner value length = 0
    ];
    let err = ObjectProperties::decode(&buf).unwrap_err();
    assert!(matches!(err, MessageError::ProtocolViolation(_)));
}

#[test]
fn parity_mismatch_rejected() {
    // 偶数型に Bytes
    let mut props = ObjectProperties::new();
    props.push(ObjectProperty {
        prop_type: PROP_PRIOR_GROUP_ID_GAP,
        value: ObjectPropertyValue::Bytes(vec![1, 2, 3]),
    });
    let mut buf = Vec::new();
    assert!(props.encode(&mut buf).is_err());

    // 奇数型に VarInt
    let mut props = ObjectProperties::new();
    props.push(ObjectProperty {
        prop_type: 0x0B,
        value: ObjectPropertyValue::VarInt(0),
    });
    buf.clear();
    assert!(props.encode(&mut buf).is_err());
}

#[test]
fn duplicate_prop_type_rejected_on_encode() {
    let mut props = ObjectProperties::new();
    props.push(ObjectProperty {
        prop_type: PROP_PRIOR_GROUP_ID_GAP,
        value: ObjectPropertyValue::VarInt(1),
    });
    props.push(ObjectProperty {
        prop_type: PROP_PRIOR_GROUP_ID_GAP,
        value: ObjectPropertyValue::VarInt(2),
    });
    let mut buf = Vec::new();
    let err = props.encode(&mut buf).unwrap_err();
    assert!(matches!(err, MessageError::ProtocolViolation(_)));
}

#[test]
fn duplicate_prop_type_rejected_on_decode() {
    // 2 個の PRIOR_GROUP_ID_GAP=1 を delta=0x3C, delta=0 で手動エンコード
    //   Length=4, [delta=0x3C, value=1, delta=0, value=2]
    // 0x3C は 1 バイト varint = 0x3C
    let buf = vec![
        0x04, // Properties Length
        0x3C, // delta
        0x01, // value
        0x00, // delta (duplicate)
        0x02, // value
    ];
    let err = ObjectProperties::decode(&buf).unwrap_err();
    assert!(matches!(err, MessageError::ProtocolViolation(_)));
}

/// 不正入力に対する decode エラーが型付けされていることを検証する
///
/// PBT の `decode_never_panics` が任意バイト列で確認していた「パニックせず
/// 型付きエラーを返す」性質を、代表フィクスチャ 5 入力で退避する。
/// 各入力で `MessageError::UnexpectedEof` / `ProtocolViolation`
/// のいずれかが返ることを `matches!` で検証する。
#[test]
fn decode_error_variants_are_typed() {
    // 代表フィクスチャ 5 入力
    let fixtures: &[(&str, &[u8])] = &[
        // 空入力: Properties Length varint が無い
        ("empty input", &[]),
        // 切断 varint: type (delta=0x3C, 偶数型) のみで value varint が欠落
        ("truncated varint after type", &[0x01, 0x3C]),
        // 不正 length: Properties Length=16 が残りバイト数 (1) を超過
        ("length exceeds remaining", &[0x10, 0x00]),
        // 奇数 type に非 varint バイト: 0x0B (IMMUTABLE_PROPERTIES, 奇数型) の
        // value length varint が 0x80 (2 バイト varint 開始) で切断
        ("odd type with truncated length varint", &[0x02, 0x0B, 0x80]),
        // truncate した immutable inner: IMMUTABLE_PROPERTIES の内側 KVP が
        // 偶数型 0x3C の value varint 0x80 で切断
        ("truncated immutable inner", &[0x04, 0x0B, 0x02, 0x3C, 0x80]),
    ];

    for (name, input) in fixtures {
        let result = ObjectProperties::decode(input);
        assert!(
            matches!(
                result,
                Err(MessageError::UnexpectedEof) | Err(MessageError::MalformedTrack(_))
            ),
            "フィクスチャ '{name}' は型付きデコードエラーを返すべき: {result:?}"
        );
    }
}

#[test]
fn decode_rejects_truncated_value_length() {
    // Properties Length = 3, inner = [delta=0x3C, value varint cut off]
    // delta varint で使う 0xC0..0xFF は multi-byte だが単純に 0x80 指定
    let buf = vec![0x03, 0x3C, 0x80];
    // 0x80 は 2 バイト varint の開始、次が欠けている
    let err = ObjectProperties::decode(&buf).unwrap_err();
    assert!(matches!(err, MessageError::UnexpectedEof));
}

// ─── ObjectPropertyTracker の状態追跡テスト ──────────────────

#[test]
fn tracker_rejects_prior_group_gap_covering_received_group() {
    let mut tracker = ObjectPropertyTracker::new();
    tracker
        .observe_object(8, 0, None)
        .expect("テストフィクスチャの前提条件を満たす");

    let mut props = ObjectProperties::new();
    props.push(ObjectProperty {
        prop_type: PROP_PRIOR_GROUP_ID_GAP,
        value: ObjectPropertyValue::VarInt(2),
    });
    let mut encoded = Vec::new();
    props
        .encode(&mut encoded)
        .expect("正当なテスト入力の encode は成功する");

    assert!(matches!(
        tracker.observe_object(10, 0, Some(&encoded)),
        Err(MessageError::MalformedTrack(_))
    ));
}

#[test]
fn tracker_rejects_group_inside_previously_announced_gap() {
    let mut tracker = ObjectPropertyTracker::new();

    let mut props = ObjectProperties::new();
    props.push(ObjectProperty {
        prop_type: PROP_PRIOR_GROUP_ID_GAP,
        value: ObjectPropertyValue::VarInt(2),
    });
    let mut encoded = Vec::new();
    props
        .encode(&mut encoded)
        .expect("正当なテスト入力の encode は成功する");
    tracker
        .observe_object(10, 0, Some(&encoded))
        .expect("テストフィクスチャの前提条件を満たす");

    assert!(matches!(
        tracker.observe_object(9, 0, None),
        Err(MessageError::MalformedTrack(_))
    ));
}

#[test]
fn tracker_rejects_prior_object_gap_covering_received_object() {
    let mut tracker = ObjectPropertyTracker::new();
    tracker
        .observe_object(3, 8, None)
        .expect("テストフィクスチャの前提条件を満たす");

    let mut props = ObjectProperties::new();
    props.push(ObjectProperty {
        prop_type: PROP_PRIOR_OBJECT_ID_GAP,
        value: ObjectPropertyValue::VarInt(2),
    });
    let mut encoded = Vec::new();
    props
        .encode(&mut encoded)
        .expect("正当なテスト入力の encode は成功する");

    assert!(matches!(
        tracker.observe_object(3, 10, Some(&encoded)),
        Err(MessageError::MalformedTrack(_))
    ));
}

#[test]
fn tracker_rejects_object_inside_previously_announced_gap() {
    let mut tracker = ObjectPropertyTracker::new();

    let mut props = ObjectProperties::new();
    props.push(ObjectProperty {
        prop_type: PROP_PRIOR_OBJECT_ID_GAP,
        value: ObjectPropertyValue::VarInt(2),
    });
    let mut encoded = Vec::new();
    props
        .encode(&mut encoded)
        .expect("正当なテスト入力の encode は成功する");
    tracker
        .observe_object(3, 10, Some(&encoded))
        .expect("テストフィクスチャの前提条件を満たす");

    assert!(matches!(
        tracker.observe_object(3, 9, None),
        Err(MessageError::MalformedTrack(_))
    ));
}

#[test]
fn tracker_rejects_inconsistent_prior_group_gap_within_group() {
    let mut tracker = ObjectPropertyTracker::new();

    let mut props_a = ObjectProperties::new();
    props_a.push(ObjectProperty {
        prop_type: PROP_PRIOR_GROUP_ID_GAP,
        value: ObjectPropertyValue::VarInt(2),
    });
    let mut encoded_a = Vec::new();
    props_a
        .encode(&mut encoded_a)
        .expect("正当なテスト入力の encode は成功する");
    tracker
        .observe_object(10, 0, Some(&encoded_a))
        .expect("テストフィクスチャの前提条件を満たす");

    let mut props_b = ObjectProperties::new();
    props_b.push(ObjectProperty {
        prop_type: PROP_PRIOR_GROUP_ID_GAP,
        value: ObjectPropertyValue::VarInt(1),
    });
    let mut encoded_b = Vec::new();
    props_b
        .encode(&mut encoded_b)
        .expect("正当なテスト入力の encode は成功する");

    assert!(matches!(
        tracker.observe_object(10, 1, Some(&encoded_b)),
        Err(MessageError::MalformedTrack(_))
    ));
}

// ─── IMMUTABLE_PROPERTIES 内側探索テスト ─────────────────────

/// 内側 KVP 列のバイト列を構築するヘルパー
///
/// 指定した偶数型プロパティ 1 つを `ObjectProperties` としてエンコードし、
/// 先頭の Properties Length varint を除去して内側 KVP 列だけを取り出す。
/// IMMUTABLE_PROPERTIES (0x0B) の値 (入れ子 KVP 列) のテストフィクスチャを作るために使う。
fn encode_inner_varint(prop_type: u64, value: u64) -> Vec<u8> {
    let mut props = ObjectProperties::new();
    props.push(ObjectProperty {
        prop_type,
        value: ObjectPropertyValue::VarInt(value),
    });
    let mut buf = Vec::new();
    props
        .encode(&mut buf)
        .expect("正当なテスト入力の encode は成功する");
    // 先頭の Properties Length varint を除去する
    let (_, n) =
        shiguredo_moqt::varint::decode(&buf).expect("エンコードされたバッファは varint で始まる");
    buf[n..].to_vec()
}

#[test]
fn prior_group_id_gap_found_inside_immutable_properties() {
    // IMMUTABLE_PROPERTIES の内側に PRIOR_GROUP_ID_GAP を持つフィクスチャ
    let inner = encode_inner_varint(PROP_PRIOR_GROUP_ID_GAP, 7);
    let mut props = ObjectProperties::new();
    props.push(ObjectProperty {
        prop_type: PROP_IMMUTABLE_PROPERTIES,
        value: ObjectPropertyValue::Bytes(inner),
    });

    // mutable リストには無くても内側を探索して値を返す (MUST search both)
    assert_eq!(props.prior_group_id_gap(), Some(7));
}

#[test]
fn prior_object_id_gap_found_inside_immutable_properties() {
    // IMMUTABLE_PROPERTIES の内側に PRIOR_OBJECT_ID_GAP を持つフィクスチャ
    let inner = encode_inner_varint(PROP_PRIOR_OBJECT_ID_GAP, 3);
    let mut props = ObjectProperties::new();
    props.push(ObjectProperty {
        prop_type: PROP_IMMUTABLE_PROPERTIES,
        value: ObjectPropertyValue::Bytes(inner),
    });

    // mutable リストには無くても内側を探索して値を返す (MUST search both)
    assert_eq!(props.prior_object_id_gap(), Some(3));
}

#[test]
fn mutable_list_takes_precedence_over_immutable_inner() {
    // 内側にも同じプロパティがあるが、mutable 側の値が優先される
    let inner = encode_inner_varint(PROP_PRIOR_GROUP_ID_GAP, 99);
    let mut props = ObjectProperties::new();
    props.push(ObjectProperty {
        prop_type: PROP_PRIOR_GROUP_ID_GAP,
        value: ObjectPropertyValue::VarInt(1),
    });
    props.push(ObjectProperty {
        prop_type: PROP_IMMUTABLE_PROPERTIES,
        value: ObjectPropertyValue::Bytes(inner),
    });

    // mutable リスト優先
    assert_eq!(props.prior_group_id_gap(), Some(1));
}

#[test]
fn find_varint_returns_none_for_empty_immutable_inner() {
    // IMMUTABLE_PROPERTIES の内側が空バイト列のケース (非回帰)
    let mut props = ObjectProperties::new();
    props.push(ObjectProperty {
        prop_type: PROP_IMMUTABLE_PROPERTIES,
        value: ObjectPropertyValue::Bytes(Vec::new()),
    });

    assert_eq!(props.prior_group_id_gap(), None);
}

#[test]
fn find_varint_returns_none_for_corrupted_immutable_inner() {
    // IMMUTABLE_PROPERTIES の内側が破損しているケース (非回帰)
    // 0x3C (PRIOR_GROUP_ID_GAP, 偶数型) の後に varint 値が無く、デコードが失敗する
    let mut props = ObjectProperties::new();
    props.push(ObjectProperty {
        prop_type: PROP_IMMUTABLE_PROPERTIES,
        value: ObjectPropertyValue::Bytes(vec![0x3C]),
    });

    // 破損した内側は空 Vec として扱われ、探索は None を返す
    assert_eq!(props.prior_group_id_gap(), None);
}

// ─── ObjectFieldTracker の重複 Object フィールド検証テスト ───

/// `ObjectFieldMismatch` を `Box<dyn std::error::Error + Send + Sync>` へ `?` で変換できる
///
/// Object のフィールドを追跡する公開 API のエラーを、利用側が自身のエラー型に包んで
/// 伝播させるために `?` を使えることを確認する。
#[test]
fn object_field_mismatch_converts_into_boxed_error() {
    /// 不一致の検出を `?` で伝播させる
    fn observe_mismatch() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut tracker = ObjectFieldTracker::new();
        // 初回受信は記録される
        tracker.observe_object_fields(3, 7, true, Some(1), 0)?;
        // 同じ (group_id, object_id) を datagram 経由で再受信する
        // (datagram は Subgroup ID を持たないため None を渡す)
        tracker.observe_object_fields(3, 7, false, None, 0)?;
        Ok(())
    }

    let err = observe_mismatch()
        .expect_err("Forwarding Preference が異なる重複 Object はエラーになること");
    assert_eq!(
        err.to_string(),
        "object field mismatch (group_id=3, object_id=7): malformed track: duplicate Object with different Forwarding Preference",
        "Display が不一致の内容を表すこと"
    );
    assert!(err.source().is_none(), "内包するエラーは無いこと");
}

/// GREASE の Property Type は 0x4000-0x7FFF に入っていても Object scope で malformed にならない
///
/// draft-ietf-moq-transport-21 §16.8 (Properties) の Table 14 は GREASE の Property Type
/// (`0x7f * N + 0x9D`) を Scope Any として予約しており、その一部 (N = 128 の 0x401D から
/// N = 256 の 0x7F9D まで) は §3.6 (Mandatory Track Properties) の 0x4000-0x7FFF に入る。
/// GREASE 値は IANA に登録された Property ではないため malformed としない (§13 (Grease) の
/// "Endpoints MUST NOT close the session solely because they received an unknown value.")。
#[test]
fn grease_property_in_mandatory_range_is_accepted() {
    // 奇数型 (長さ付きバイト列) の GREASE 値: N = 128 -> 0x401D
    // 偶数型 (varint) の GREASE 値: N = 129 -> 0x409C
    // 重なり範囲の上限: N = 256 -> 0x7F9D (奇数型)
    for (prop_type, value) in [
        (0x401Du64, ObjectPropertyValue::Bytes(vec![0xAB])),
        (0x409C, ObjectPropertyValue::VarInt(7)),
        (0x7F9D, ObjectPropertyValue::Bytes(vec![0xCD])),
    ] {
        assert!(
            shiguredo_moqt::grease::is_grease(prop_type),
            "テスト前提として GREASE 値であること: {prop_type:#x}"
        );
        let mut props = ObjectProperties::new();
        props.push(ObjectProperty {
            prop_type,
            value: value.clone(),
        });
        let mut buf = Vec::new();
        props
            .encode(&mut buf)
            .unwrap_or_else(|e| panic!("GREASE 値の encode は成功すること: {prop_type:#x}: {e:?}"));
        let (decoded, _) = ObjectProperties::decode(&buf)
            .unwrap_or_else(|e| panic!("GREASE 値の decode は成功すること: {prop_type:#x}: {e:?}"));
        assert_eq!(
            decoded.as_slice().len(),
            1,
            "GREASE 値が保持されること: {prop_type:#x}"
        );
        assert_eq!(decoded.as_slice()[0].prop_type, prop_type);
        assert_eq!(decoded.as_slice()[0].value, value);
    }
}

/// GREASE 値でない 0x4000-0x7FFF は Object scope で malformed のままである
///
/// draft-ietf-moq-transport-21 §3.6 (Mandatory Track Properties): "An Object received with a
/// Mandatory Track Property as an Object Property is malformed".
#[test]
fn non_grease_mandatory_property_in_object_scope_is_malformed() {
    for prop_type in [0x4000u64, 0x7FFF] {
        assert!(
            !shiguredo_moqt::grease::is_grease(prop_type),
            "テスト前提として GREASE 値でないこと: {prop_type:#x}"
        );
        // Object scope の wire 表現 (Properties Length + delta エンコードされた KVP) を
        // 手組みして decode させる。malformed の判定は decode 側の `decode_kv_pairs` にある。
        let mut inner = Vec::new();
        varint::encode(prop_type, &mut inner); // prev_type = 0 からの delta
        varint::encode(1, &mut inner); // 偶数型は varint 値
        let mut buf = Vec::new();
        varint::encode(inner.len() as u64, &mut buf);
        buf.extend_from_slice(&inner);
        assert!(
            matches!(
                ObjectProperties::decode(&buf),
                Err(MessageError::MalformedTrack(_))
            ),
            "GREASE 値でない必須プロパティは decode で malformed になること: {prop_type:#x}"
        );
    }
}

/// IMMUTABLE_PROPERTIES の内側に 1 プロパティだけを持つ ObjectProperties を作る
///
/// `inner_prop` を内側に持つ。エンコードした内側の生バイト列 (Properties Length を除く) を
/// IMMUTABLE_PROPERTIES の値として載せる。
fn properties_with_single_inner(inner_prop: ObjectProperty) -> ObjectProperties {
    let mut inner = ObjectProperties::new();
    inner.push(inner_prop);
    let mut inner_buf = Vec::new();
    inner
        .encode(&mut inner_buf)
        .expect("正当なテスト入力の encode は成功する");
    let len = inner_buf[0] as usize;
    assert_eq!(inner_buf.len(), 1 + len);
    let raw_inner = inner_buf[1..].to_vec();

    let mut outer = ObjectProperties::new();
    outer.push(ObjectProperty {
        prop_type: PROP_IMMUTABLE_PROPERTIES,
        value: ObjectPropertyValue::Bytes(raw_inner),
    });
    outer
}

/// PRIOR_GROUP_ID_GAP / PRIOR_OBJECT_ID_GAP の二重インスタンスは malformed になる
///
/// draft-ietf-moq-transport-21 §10.8 (Prior Group ID Gap) / §10.9 (Prior Object ID Gap):
/// "An Object MUST NOT contain more than one instance of this property." 同じ型を mutable
/// リストと IMMUTABLE_PROPERTIES 内側の両方に置いた場合も 2 インスタンスと数える (§10.7)。
#[test]
fn tracker_rejects_duplicate_prior_gap_instances() {
    for prop_type in [PROP_PRIOR_GROUP_ID_GAP, PROP_PRIOR_OBJECT_ID_GAP] {
        let mut tracker = ObjectPropertyTracker::new();
        // mutable リスト側に 1 個 + IMMUTABLE_PROPERTIES 内側に同じ型を 1 個
        let mut mutable = properties_with_single_inner(ObjectProperty {
            prop_type,
            value: ObjectPropertyValue::VarInt(2),
        });
        mutable.push(ObjectProperty {
            prop_type,
            value: ObjectPropertyValue::VarInt(1),
        });

        let err = tracker
            .observe_decoded_object(10, 0, Some(&mutable))
            .unwrap_err();
        assert!(
            matches!(err, MessageError::MalformedTrack(_)),
            "二重インスタンスは malformed になること: {prop_type:#x}: {err:?}"
        );
    }
}

/// どちらか片方に 1 個だけ置いた場合は受理される (非退行)
#[test]
fn tracker_accepts_single_prior_gap_instance() {
    for prop_type in [PROP_PRIOR_GROUP_ID_GAP, PROP_PRIOR_OBJECT_ID_GAP] {
        // mutable リストのみ
        let mut tracker = ObjectPropertyTracker::new();
        let mut mutable = ObjectProperties::new();
        mutable.push(ObjectProperty {
            prop_type,
            value: ObjectPropertyValue::VarInt(1),
        });
        // PRIOR_OBJECT_ID_GAP は object_id 以下でなければ別条件で拒否されるため object_id=10 を使う
        tracker
            .observe_decoded_object(10, 10, Some(&mutable))
            .unwrap_or_else(|e| {
                panic!("mutable のみの 1 個は受理されること: {prop_type:#x}: {e:?}")
            });

        // IMMUTABLE_PROPERTIES 内側のみ
        let mut tracker = ObjectPropertyTracker::new();
        let only_inner = properties_with_single_inner(ObjectProperty {
            prop_type,
            value: ObjectPropertyValue::VarInt(1),
        });
        tracker
            .observe_decoded_object(10, 10, Some(&only_inner))
            .unwrap_or_else(|e| panic!("内側のみの 1 個は受理されること: {prop_type:#x}: {e:?}"));
    }
}

/// PRIOR_GROUP_ID_GAP / PRIOR_OBJECT_ID_GAP を持たない Object は従来どおり受理される
#[test]
fn tracker_accepts_object_without_prior_gap() {
    let mut tracker = ObjectPropertyTracker::new();
    let mut props = ObjectProperties::new();
    props.push(ObjectProperty {
        prop_type: PROP_PRIOR_OBJECT_ID_GAP,
        value: ObjectPropertyValue::VarInt(0),
    });
    tracker
        .observe_decoded_object(10, 0, Some(&props))
        .expect("gap を持たない Object は受理されること");
}
