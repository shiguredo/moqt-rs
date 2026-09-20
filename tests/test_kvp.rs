//! delta-key 骨格の境界値テスト
//!
//! draft-ietf-moq-transport-21 §8.3 (Key-Value-Pair Structure)
//!
//! `encode_delta_key` / `decode_delta_key` は `pub(crate)` のため、delta-key を
//! 素直に経由する公開型 `LocProperties` の encode/decode をドライバとして用いる
//! (方針は `pbt/tests/prop_kvp.rs` と同じ)。PBT が生成しない境界・特異値
//! (Delta 0、小さな Delta、u64::MAX 付近、overflow) を固定値で検証する。

use shiguredo_moqt::{
    error::MessageError,
    loc::{LocProperties, LocProperty, LocPropertyValue},
    varint,
};

/// 偶数キーと varint 値でプロパティ列を作る
fn props_of(entries: &[(u64, u64)]) -> LocProperties {
    let mut props = LocProperties::new();
    for &(key, val) in entries {
        props.push(LocProperty {
            prop_id: key,
            value: LocPropertyValue::VarInt(val),
        });
    }
    props
}

/// デコード結果を (キー, 値) 列に戻す (偶数キーは必ず VarInt)
fn to_pairs(props: &LocProperties) -> Vec<(u64, u64)> {
    props
        .iter()
        .map(|p| match &p.value {
            LocPropertyValue::VarInt(v) => (p.prop_id, *v),
            LocPropertyValue::Bytes(_) => {
                panic!("偶数キーは必ず VarInt にデコードされること")
            }
        })
        .collect()
}

/// Delta 0 と小さな Delta のラウンドトリップ
#[test]
fn delta_zero_and_small_roundtrip() {
    let entries = vec![(14, 0), (16, 1), (18, 42)];
    let encoded = props_of(&entries)
        .encode()
        .expect("テストフィクスチャの前提条件を満たす");
    let (decoded, consumed) =
        LocProperties::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(consumed, encoded.len());
    assert_eq!(to_pairs(&decoded), entries);
}

/// u64::MAX 付近の偶数キーのラウンドトリップ
#[test]
fn near_max_keys_roundtrip() {
    // MAX - 1 (偶数) 単独と大きな gap を含む列
    for entries in [vec![(u64::MAX - 1, 7)], vec![(14, 1), (u64::MAX - 1, 2)]] {
        let encoded = props_of(&entries)
            .encode()
            .expect("テストフィクスチャの前提条件を満たす");
        let (decoded, consumed) =
            LocProperties::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, encoded.len());
        assert_eq!(to_pairs(&decoded), entries);
    }
}

/// 絶対キー復元 (prev + delta) の overflow は PROTOCOL_VIOLATION で弾かれる
#[test]
fn decode_overflow_rejected() {
    // 内側: キー MAX - 1・値 0 の 1 件目に続き、delta 2 で MAX + 1 溢出する 2 件目
    let mut inner = Vec::new();
    varint::encode(u64::MAX - 1, &mut inner);
    varint::encode(0, &mut inner);
    varint::encode(2, &mut inner);
    // Properties Length プレフィックス付き (値は空でもキー走査で溢出が先に検出される)
    let mut buf = Vec::new();
    varint::encode(inner.len() as u64, &mut buf);
    buf.extend_from_slice(&inner);
    let err = LocProperties::decode(&buf).expect_err("溢出する delta は拒否されること");
    assert!(
        matches!(err, MessageError::ProtocolViolation(_)),
        "KVP type delta overflow で PROTOCOL_VIOLATION になること: {err:?}"
    );
}
