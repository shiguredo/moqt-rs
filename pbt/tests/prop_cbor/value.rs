//! Value の正規化に関する Property-Based Testing
//!
//! 通常エンコードと決定論的エンコードの関係を検証する。

use super::helpers;

use std::cell::Cell;

use shiguredo_moqt::cbor::value::Value;

/// このファイルの PBT ケース数
const CASES: usize = 512;

#[test]
fn canonical_encoding_matches_normal_encoding_without_maps_and_floats() -> noprop::TestResult {
    let seed = noprop::seed_from_env_or_time("CBOR_RS_PBT_SEED")?;
    let without_maps_and_floats = Cell::new(0usize);
    let mut runner = noprop::Runner::new(seed);

    runner.run(CASES, |ctx| {
        let value = helpers::sample_value(ctx, 0);
        if contains_map_or_float(&value) {
            return Ok(());
        }
        without_maps_and_floats.set(without_maps_and_floats.get() + 1);
        assert_eq!(
            value.to_canonical_bytes(),
            value.to_bytes(),
            "マップと浮動小数点値を含まない値の決定論的エンコードが通常エンコードと違う: {value:?}"
        );
        Ok(())
    })?;

    assert!(
        without_maps_and_floats.get() > 0,
        "マップも浮動小数点値も含まない値が 1 件も生成されなかった\n{runner}"
    );
    Ok(())
}

#[test]
fn canonical_encoding_ignores_map_entry_order() -> noprop::TestResult {
    let seed = noprop::seed_from_env_or_time("CBOR_RS_PBT_SEED")?;
    let shuffled_maps = Cell::new(0usize);
    let mut runner = noprop::Runner::new(seed);

    runner.run(CASES, |ctx| {
        let value = helpers::sample_value(ctx, 0);
        let pairs = match &value {
            Value::Map(pairs) => pairs.clone(),
            _ => return Ok(()),
        };
        if pairs.len() < 2 {
            return Ok(());
        }

        // キーの決定論的エンコードが重複するマップは対象外にする
        // (重複キーは invalid であり、決定論的エンコードの対象ではない)
        let mut encoded_keys: Vec<Vec<u8>> = pairs
            .iter()
            .map(|(key, _)| key.to_canonical_bytes())
            .collect();
        encoded_keys.sort();
        if encoded_keys.windows(2).any(|window| window[0] == window[1]) {
            return Ok(());
        }
        shuffled_maps.set(shuffled_maps.get() + 1);

        // Fisher-Yates でエントリを並べ替えても決定論的エンコードは同じになること
        let expected = value.to_canonical_bytes();
        let mut shuffled = pairs;
        for index in (1..shuffled.len()).rev() {
            let swap_index = noprop::sample_usize_in(ctx, 0..=index);
            shuffled.swap(index, swap_index);
        }
        assert_eq!(
            Value::Map(shuffled).to_canonical_bytes(),
            expected,
            "マップのエントリ順によって決定論的エンコード結果が変わった"
        );
        Ok(())
    })?;

    assert!(
        shuffled_maps.get() > 0,
        "キーが重複しない 2 エントリ以上のマップが 1 件も生成されなかった\n{runner}"
    );
    Ok(())
}

/// 値のどこかにマップまたは浮動小数点値を含むかどうかを返す
fn contains_map_or_float(value: &Value) -> bool {
    match value {
        Value::Map(_) | Value::Float(_) => true,
        Value::Array(items) => items.iter().any(contains_map_or_float),
        Value::Tag(_, content) => contains_map_or_float(content),
        _ => false,
    }
}
