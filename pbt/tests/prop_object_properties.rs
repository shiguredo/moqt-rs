use pbt::common::test_runner;
use shiguredo_moqt::object_properties::{ObjectProperties, ObjectProperty, ObjectPropertyValue};
use shiguredo_moqt::track_properties::{
    PROP_DEFAULT_PUBLISHER_GROUP_ORDER, PROP_DEFAULT_PUBLISHER_PRIORITY, PROP_DYNAMIC_GROUPS,
    PROP_IMMUTABLE_PROPERTIES, PROP_MAX_CACHE_DURATION,
};

/// Track 専用の既知 Property の型
///
/// Object scope で受信すると malformed になるため
/// (draft-ietf-moq-transport-23 §8.4 (Track and Object Properties))、正常系のサンプラーでは
/// 生成しない。
const TRACK_ONLY_TYPES: &[u64] = &[
    PROP_MAX_CACHE_DURATION,
    PROP_DEFAULT_PUBLISHER_PRIORITY,
    PROP_DEFAULT_PUBLISHER_GROUP_ORDER,
    PROP_DYNAMIC_GROUPS,
];

/// ユニークな prop_type と対応する値のペアを生成する
///
/// IMMUTABLE_PROPERTIES (0x0B) はネストした中身まで生成するのが煩雑なため除外し、
/// 単純な varint/bytes 値のみを扱う。prop_type は 0..=255 の範囲で生成 (vi64 1 バイト内)。
/// 重複や IMMUTABLE_PROPERTIES は生成から除外する (valid-by-construction)。
fn sample_unique_non_immutable_properties(
    ctx: &mut noprop::TestCaseContext,
) -> Vec<ObjectProperty> {
    let count = noprop::sample_usize_in(ctx, 0..=8);
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for _ in 0..count {
        let id = noprop::sample_u64_in(ctx, 0..=255);
        if id == PROP_IMMUTABLE_PROPERTIES || TRACK_ONLY_TYPES.contains(&id) {
            continue;
        }
        if !seen.insert(id) {
            continue;
        }
        let value = if id.is_multiple_of(2) {
            ObjectPropertyValue::VarInt(id.wrapping_mul(3))
        } else {
            ObjectPropertyValue::Bytes(vec![(id & 0xff) as u8; (id % 8) as usize])
        };
        out.push(ObjectProperty {
            prop_type: id,
            value,
        });
    }
    out
}

/// 任意の (重複なし、parity 整合、非 IMMUTABLE) プロパティ集合は encode → decode で
/// 等価なプロパティ集合 (prop_type 昇順) に戻る
#[test]
fn roundtrip_non_immutable() -> noprop::TestResult {
    // 非空のプロパティ集合の観測をカバレッジゲートで検証する
    let non_empty_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let entries = sample_unique_non_immutable_properties(ctx);
        if !entries.is_empty() {
            non_empty_seen.set(true);
        }
        let mut props = ObjectProperties::new();
        for e in entries.iter().cloned() {
            props.push(e);
        }

        let mut buf = Vec::new();
        props
            .encode(&mut buf)
            .expect("正当なテスト入力の encode は成功する");

        let (decoded, consumed) =
            ObjectProperties::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, buf.len());

        let mut expected = entries.clone();
        expected.sort_by_key(|e| e.prop_type);
        assert_eq!(decoded.as_slice(), expected.as_slice());
        Ok(())
    })?;
    assert!(
        non_empty_seen.get(),
        "非空のプロパティ集合のケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

/// IMMUTABLE_PROPERTIES (0x0B) に入れ子 KVP を入れた roundtrip と `find_varint` の統合検索
///
/// draft-ietf-moq-transport-23 §10.7 (Immutable Properties): 値は入れ子の KVP リスト。
/// `find_varint` は mutable なプロパティを優先し、無ければ IMMUTABLE_PROPERTIES の内側を探索する。
#[test]
fn roundtrip_immutable_and_find_varint() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        // 内側: 一意な偶数キー (IMMUTABLE は奇数型なので衝突しない) と varint 値
        let count = noprop::sample_usize_in(ctx, 0..=4);
        let mut seen = std::collections::BTreeSet::new();
        let mut inner = ObjectProperties::new();
        let mut expected = Vec::new();
        for _ in 0..count {
            let key = noprop::sample_u64_in(ctx, 0..=255) & !1u64; // 偶数型
            if TRACK_ONLY_TYPES.contains(&key) || !seen.insert(key) {
                continue;
            }
            let value = noprop::sample_u64(ctx);
            inner.push(ObjectProperty {
                prop_type: key,
                value: ObjectPropertyValue::VarInt(value),
            });
            expected.push((key, value));
        }
        let mut encoded_inner = Vec::new();
        inner
            .encode(&mut encoded_inner)
            .expect("正当なテスト入力の encode は成功する");
        // ObjectProperties::encode は Properties Length (varint) を前置するが、
        // IMMUTABLE_PROPERTIES の値は長さ前置なしの KVP 列 (draft-ietf-moq-transport-23 §10.7 (Immutable Properties))
        let (inner_len, prefix_len) =
            shiguredo_moqt::varint::decode(&encoded_inner).expect("length prefix は varint");
        assert_eq!(inner_len as usize, encoded_inner.len() - prefix_len);
        let inner_kvp = encoded_inner[prefix_len..].to_vec();

        let mut outer = ObjectProperties::new();
        outer.push(ObjectProperty {
            prop_type: PROP_IMMUTABLE_PROPERTIES,
            value: ObjectPropertyValue::Bytes(inner_kvp.clone()),
        });
        let mut outer_bytes = Vec::new();
        outer
            .encode(&mut outer_bytes)
            .expect("正当なテスト入力の encode は成功する");

        let (decoded, consumed) =
            ObjectProperties::decode(&outer_bytes).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, outer_bytes.len());
        assert_eq!(
            decoded.immutable_properties(),
            Some(inner_kvp.as_slice()),
            "IMMUTABLE_PROPERTIES の内側バイト列が保存されること"
        );
        for (key, value) in &expected {
            assert_eq!(
                decoded.find_varint(*key),
                Some(*value),
                "find_varint が IMMUTABLE 内側を探索すること"
            );
        }
        Ok(())
    })?;
    Ok(())
}

// ─── Property のスコープ検証 ─────────────────────────────

/// `ObjectProperties::decode` が Track 専用の既知 Property だけを malformed にすること
///
/// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties): スコープ外で受信した
/// 既知 Property は malformed track (§12.1 (Malformed Tracks) 条件 8)。未知の型はスコープを
/// 推測せず受理する (§16.8 の "Endpoints MUST ignore unknown Property types")。
/// スコープ違反の Object Properties は `encode` が拒否するため、wire 表現を手組みする。
#[test]
fn object_scope_rejects_track_only_property() -> noprop::TestResult {
    use shiguredo_moqt::error::MessageError;
    let malformed_seen = std::cell::Cell::new(false);
    let accepted_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let prop_type = noprop::sample_u64_in(ctx, 0..=0x40);
        let mut kvps = Vec::new();
        shiguredo_moqt::varint::encode(prop_type, &mut kvps);
        if prop_type.is_multiple_of(2) {
            shiguredo_moqt::varint::encode(1, &mut kvps);
        } else {
            // 奇数型は長さ 0 の値にする (IMMUTABLE_PROPERTIES の内側も空になり構造検証を通る)
            shiguredo_moqt::varint::encode(0, &mut kvps);
        }
        let mut block = Vec::new();
        shiguredo_moqt::varint::encode(kvps.len() as u64, &mut block);
        block.extend_from_slice(&kvps);

        let result = ObjectProperties::decode(&block).map(|_| ());
        if TRACK_ONLY_TYPES.contains(&prop_type) {
            malformed_seen.set(true);
            assert_eq!(
                result,
                Err(MessageError::MalformedTrack(
                    "Track-only property in Object scope"
                )),
                "{prop_type:#x} は Track 専用 Property として検出すること"
            );
        } else {
            accepted_seen.set(true);
            assert_eq!(
                result,
                Ok(()),
                "{prop_type:#x} は Object scope で受理すること"
            );
        }
        Ok(())
    })?;
    assert!(
        malformed_seen.get(),
        "Track 専用型のケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        accepted_seen.get(),
        "受理される型のケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

// ─── Property の多重可否 ─────────────────────────────────────

/// この実装が意味を理解しない未知の Object Property Type
///
/// draft-ietf-moq-transport-23 §16.8 (Properties) Table 15 / Table 16 の既知型と
/// 0x4000-0x7FFF の Mandatory Track Property 範囲を含まない値だけを使う。未知の型は
/// 複数インスタンスを許す (§8.4 の "MUST allow multiple instances of the Property")。
/// この PBT は varint 値で生成するため偶数型だけを使う。
const UNKNOWN_OBJECT_TYPES: &[u64] = &[0x1000, 0x1002, 0x1004, 0x1006];

/// 偶数型 Property の KVP 生バイト列
fn even_kvp(prop_type: u64, value: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    shiguredo_moqt::varint::encode(prop_type, &mut bytes);
    shiguredo_moqt::varint::encode(value, &mut bytes);
    bytes
}

/// KVP 列を Properties Length 付きのブロックにする
fn wrap(kvps: &[u8]) -> Vec<u8> {
    let mut block = Vec::new();
    shiguredo_moqt::varint::encode(kvps.len() as u64, &mut block);
    block.extend_from_slice(kvps);
    block
}

/// 未知の Type の同値・異値の複数インスタンスが lossless に往復し、多重違反にならないこと
///
/// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) は、理解できない Property に
/// ついて "MUST allow multiple instances of the Property" と定める。
#[test]
fn unknown_duplicate_types_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let type_count = noprop::sample_usize_in(ctx, 1..=3);
        let mut entries = Vec::new();
        for index in 0..type_count {
            let prop_type = UNKNOWN_OBJECT_TYPES[index % UNKNOWN_OBJECT_TYPES.len()];
            let instance_count = noprop::sample_usize_in(ctx, 1..=3);
            let shared = noprop::sample_u64_in(ctx, 0..=1_000);
            for _ in 0..instance_count {
                let value = match noprop::sample_weighted_index(ctx, &[1, 1]) {
                    0 => shared,
                    _ => noprop::sample_u64_in(ctx, 0..=1_000),
                };
                entries.push(ObjectProperty {
                    prop_type,
                    value: ObjectPropertyValue::VarInt(value),
                });
            }
        }
        // 昇順に並べる (encode は型の昇順に安定ソートする)
        entries.sort_by_key(|p| p.prop_type);
        let mut props = ObjectProperties::new();
        for entry in entries {
            props.push(entry);
        }
        let mut buf = Vec::new();
        props
            .encode(&mut buf)
            .expect("未知 Type の重複は encode できること");
        let (decoded, consumed) =
            ObjectProperties::decode(&buf).expect("未知 Type の重複は decode できること");
        assert_eq!(consumed, buf.len());
        assert_eq!(
            decoded, props,
            "未知 Type の同値・異値の複数インスタンスが往復すること"
        );
        assert_eq!(
            decoded.validate(),
            Ok(()),
            "未知 Type の重複は多重違反にならないこと"
        );
        Ok(())
    })?;
    Ok(())
}

/// 単一に限られる既知 Property の重複が malformed Track として報告されること
///
/// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1 (Malformed Tracks)
/// 条件 9。Object scope で正当な偶数型だけを使い、スコープ違反と混ざらないようにする。
#[test]
fn known_duplicate_property_is_malformed() -> noprop::TestResult {
    use shiguredo_moqt::error::MessageError;
    const SINGLE_TYPES: &[u64] = &[0x02, 0x06, 0x08, 0x0C, 0x10, 0x3C, 0x3E];
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        let prop_type = noprop::sample_choice(ctx, SINGLE_TYPES);
        // 既知型の値域をすべて満たす値にする (AUDIO_LEVEL 0x0C は 0-255)
        let value = 1u64;
        // 同一型を delta=0 で 2 回繰り返す wire 表現を手組みする
        let mut kvps = even_kvp(prop_type, value);
        shiguredo_moqt::varint::encode(0, &mut kvps);
        shiguredo_moqt::varint::encode(value, &mut kvps);
        let block = wrap(&kvps);
        assert_eq!(
            ObjectProperties::decode(&block),
            Err(MessageError::MalformedTrack("duplicate object property")),
            "{prop_type:#x} の重複を malformed Track とすること"
        );

        let mut local = ObjectProperties::new();
        for _ in 0..2 {
            local.push(ObjectProperty {
                prop_type,
                value: ObjectPropertyValue::VarInt(value),
            });
        }
        let mut buf = Vec::new();
        assert_eq!(
            local.encode(&mut buf),
            Err(MessageError::ProtocolViolation("duplicate object property")),
            "{prop_type:#x} の重複 encode を入力検証エラーにすること"
        );
        Ok(())
    })?;
    Ok(())
}
