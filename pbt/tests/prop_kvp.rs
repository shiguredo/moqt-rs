//! `src/kvp.rs` の delta-key 骨格 (encode_delta_key / decode_delta_key) を検証する PBT
//!
//! draft-ietf-moq-transport-21 §8.3 (Key-Value-Pair Structure)
//!
//! `kvp` のヘルパーは `pub(crate)` で外部公開していないため、delta-key を素直に経由する公開型
//! `LocProperties` の encode/decode をドライバとして用いる (偶数キー = varint 値で値形式が最も単純なため)。
//! ここでは「キー昇順 → delta varint 読み書きのラウンドトリップ」と「絶対キー復元時の overflow guard」という
//! 全 5 KVP モジュール共通の骨格性質のみを検証する。値域や値形式といった型固有性質は各 prop_*.rs が担う。

use pbt::common::test_runner;
use shiguredo_moqt::{
    error::MessageError,
    loc::{LocProperties, LocProperty, LocPropertyValue},
    varint,
};

/// 昇順かつ重複のない偶数キーと任意の varint 値の列を生成する
///
/// LOC の既知偶数プロパティ ID (0x08 / 0x0C / 0x10) のうち Audio Level (0x0C) は値域バリデーションを伴う。
/// 型固有の意味論を持ち込まず delta-key 骨格のみを純粋に検証するため、14 以上の偶数キー (= 未知プロパティ) に限定する。
/// 未知偶数キーは値域検証なしで任意の varint 値を許容するため、delta varint のラウンドトリップを
/// u64 全域 (large gap を含む) で素直に観測できる。
fn sample_even_key_entries(ctx: &mut noprop::TestCaseContext) -> Vec<(u64, u64)> {
    let count = noprop::sample_usize_in(ctx, 0..=16);
    // キーを偶数化し、既知 ID 回避のため 14 以上のみ採用。BTreeMap で重複除去かつ昇順整列する。
    let mut map = std::collections::BTreeMap::new();
    for _ in 0..count {
        let k = noprop::sample_u64(ctx);
        let v = noprop::sample_u64(ctx);
        let even_key = k & !1u64;
        if even_key >= 14 {
            map.insert(even_key, v);
        }
    }
    map.into_iter().collect()
}

/// delta-key のラウンドトリップ: 昇順の偶数キーと値が encode → decode で完全に保存される
///
/// encode_delta_key (checked_sub による delta 計算) と decode_delta_key (checked_add による
/// 絶対キー復元) が任意のキー間隔で一致することを検証する。
#[test]
fn delta_key_roundtrip() -> noprop::TestResult {
    // 非空のエントリ列でしか意味を持たないため、非空ケースの観測をカバレッジゲートで検証する
    let non_empty_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let entries = sample_even_key_entries(ctx);
        if !entries.is_empty() {
            non_empty_seen.set(true);
        }
        let mut props = LocProperties::new();
        for &(key, val) in &entries {
            props.push(LocProperty {
                prop_id: key,
                value: LocPropertyValue::VarInt(val),
            });
        }
        let encoded = props
            .encode()
            .expect("未知偶数キーと任意 varint 値の組は常にエンコードできる");
        let (decoded, consumed) =
            LocProperties::decode(&encoded).expect("自前で生成した正当な KVP は常にデコードできる");
        assert_eq!(consumed, encoded.len());
        let got: Vec<(u64, u64)> = decoded
            .iter()
            .map(|p| match &p.value {
                LocPropertyValue::VarInt(v) => (p.prop_id, *v),
                LocPropertyValue::Bytes(_) => {
                    unreachable!("偶数キーは必ず VarInt にデコードされる")
                }
            })
            .collect();
        assert_eq!(got, entries);
        Ok(())
    })?;
    assert!(
        non_empty_seen.get(),
        "非空のエントリ列のケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

/// overflow guard: 絶対キー復元 (prev + delta) が u64 を超える wire は PROTOCOL_VIOLATION で弾かれる
///
/// 先頭キーを u64::MAX - 1 (偶数) に置き、続く delta >= 2 を与えると 2 個目の絶対キー復元で
/// checked_add が overflow する。decode_delta_key がこれを ProtocolViolation にすることを検証する。
#[test]
fn delta_key_overflow_rejected() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let second_delta = noprop::sample_u64_in(ctx, 2..=u64::MAX);
        let first_key = u64::MAX - 1; // 偶数 → varint 値を取る
        let mut inner = Vec::new();
        varint::encode(first_key, &mut inner); // delta1 = first_key (prev=0)
        varint::encode(0, &mut inner); // first_key (偶数) の varint 値
        varint::encode(second_delta, &mut inner); // first_key + second_delta は u64 を超える
        let mut buf = Vec::new();
        varint::encode(inner.len() as u64, &mut buf);
        buf.extend_from_slice(&inner);
        assert!(matches!(
            LocProperties::decode(&buf),
            Err(MessageError::ProtocolViolation(_))
        ));
        Ok(())
    })?;
    Ok(())
}
