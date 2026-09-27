//! CBOR (RFC 8949) の property テスト

use pbt::common::{sample_bytes, test_runner};
use shiguredo_moqt::c4m::cbor::{Value, decode, encode};

/// 葉のデータ項目を生成する
fn sample_leaf(ctx: &mut noprop::TestCaseContext) -> Value {
    match noprop::sample_weighted_index(ctx, &[3, 2, 2, 2, 2, 1, 1, 1]) {
        0 => Value::Unsigned(noprop::sample_u64(ctx)),
        1 => Value::Negative(noprop::sample_u64(ctx)),
        2 => Value::ByteString(sample_bytes(ctx, 8)),
        3 => {
            let len = noprop::sample_usize_in(ctx, 0..=8);
            Value::TextString(noprop::sample_ascii_printable_string(ctx, len))
        }
        4 => Value::Float(noprop::sample_f64(ctx)),
        5 => Value::Bool(noprop::sample_bool(ctx)),
        6 => Value::Null,
        _ => {
            // 単純値の予約域 (20 〜 31) は生成しない
            if noprop::sample_bool(ctx) {
                Value::Simple(noprop::sample_usize_in(ctx, 0..=19) as u8)
            } else {
                Value::Simple(noprop::sample_usize_in(ctx, 32..=255) as u8)
            }
        }
    }
}

/// データ項目を生成する (`remaining` は残りのネスト深度)
fn sample_value(ctx: &mut noprop::TestCaseContext, remaining: usize) -> Value {
    if remaining == 0 {
        return sample_leaf(ctx);
    }
    match noprop::sample_weighted_index(ctx, &[5, 3, 3, 1]) {
        0 => sample_leaf(ctx),
        1 => {
            let len = noprop::sample_usize_in(ctx, 0..=3);
            Value::Array((0..len).map(|_| sample_value(ctx, remaining - 1)).collect())
        }
        2 => {
            // キーは重複しないように生成し、決定論的エンコードと同じ昇順に並べる
            // (重複キーはエンコードできず、順序が違うとデコード結果と一致しない)
            let len = noprop::sample_usize_in(ctx, 0..=3);
            let mut keys = Vec::new();
            for _ in 0..len {
                let key = noprop::sample_u64(ctx);
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
            keys.sort_unstable();
            let entries = keys
                .into_iter()
                .map(|key| (Value::Unsigned(key), sample_value(ctx, remaining - 1)))
                .collect();
            Value::Map(entries)
        }
        _ => Value::Tag(
            noprop::sample_u64(ctx),
            Box::new(sample_value(ctx, remaining - 1)),
        ),
    }
}

/// 生成した値の encode -> decode -> encode が一致する
///
/// デコード結果が元の値と等しく、再エンコードが同じバイト列になること
/// (決定論的エンコードの安定性) を検証する。
#[test]
fn value_roundtrip() -> noprop::TestResult {
    let structured_seen = std::cell::Cell::new(0usize);
    let empty_collection_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let value = sample_value(ctx, 3);
        match &value {
            Value::Array(items) if items.is_empty() => {
                empty_collection_seen.set(empty_collection_seen.get() + 1);
            }
            Value::Map(entries) if entries.is_empty() => {
                empty_collection_seen.set(empty_collection_seen.get() + 1);
            }
            Value::Array(_) | Value::Map(_) | Value::Tag(_, _) => {
                structured_seen.set(structured_seen.get() + 1);
            }
            _ => {}
        }
        let encoded = encode(&value).expect("生成した値はエンコードできる");
        let decoded = decode(&encoded).expect("エンコードした値はデコードできる");
        assert_eq!(decoded, value, "ラウンドトリップで値が変わる");
        let reencoded = encode(&decoded).expect("デコードした値はエンコードできる");
        assert_eq!(reencoded, encoded, "再エンコードでバイト列が変わる");
        Ok(())
    })?;
    assert!(
        structured_seen.get() > 0,
        "配列 / マップ / タグのケースが生成されなかった\n{runner}"
    );
    assert!(
        empty_collection_seen.get() > 0,
        "空の配列 / マップのケースが生成されなかった\n{runner}"
    );
    Ok(())
}
