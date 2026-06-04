use pbt::common::test_runner;
use shiguredo_moqt::object_properties::{ObjectProperties, ObjectProperty, ObjectPropertyValue};
use shiguredo_moqt::track_properties::PROP_IMMUTABLE_PROPERTIES;

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
        if id == PROP_IMMUTABLE_PROPERTIES {
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
/// draft-ietf-moq-transport-21 §10.7 (Immutable Properties): 値は入れ子の KVP リスト。
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
            if !seen.insert(key) {
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
        // IMMUTABLE_PROPERTIES の値は長さ前置なしの KVP 列 (draft-ietf-moq-transport-21 §10.7 (Immutable Properties))
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
