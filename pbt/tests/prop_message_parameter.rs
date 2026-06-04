use shiguredo_moqt::{
    message::common::Location, message_parameter::LocationFilter,
    message_parameter::LocationFilterContext, message_parameter::MessageParameter,
    message_parameter::MessageParameterValue, message_parameter::MessageParameters, varint,
};

use pbt::common::{RANGE_FILTER_TYPES, has_property_type_field, sample_small_varint, test_runner};

fn sample_location(ctx: &mut noprop::TestCaseContext) -> Location {
    Location {
        group_id: sample_small_varint(ctx),
        object_id: sample_small_varint(ctx),
    }
}

/// Range Filter インスタンス 1 件の生成指示
///
/// 2 要素目が `None` なら Length=0 (REQUEST_UPDATE での削除指示)、
/// `Some((set_id, property_type))` なら SetID と Property Type を持つ 1 Range のインスタンス。
/// Property Type は 0x28 / 0x29 でのみワイヤに載る。
type RangeFilterSpec = (u64, Option<(u8, u64)>);

/// Range Filter インスタンスの中身 (SetID と Property Type) を生成する
///
/// SetID と Property Type の候補を狭く取り、重複キーが十分な確率で生成されるようにする。
/// draft-ietf-moq-transport-21 §9.20.14 / §9.20.15: Property Type は偶数でなければならない。
fn sample_range_filter_inner(ctx: &mut noprop::TestCaseContext) -> Option<(u8, u64)> {
    if noprop::sample_bool(ctx) {
        let set_id = noprop::sample_usize_in(ctx, 0..=2) as u8;
        let property_type = noprop::sample_choice(ctx, &[0u64, 2u64]);
        Some((set_id, property_type))
    } else {
        None
    }
}

fn sample_range_filter_spec(ctx: &mut noprop::TestCaseContext) -> RangeFilterSpec {
    (
        noprop::sample_choice(ctx, RANGE_FILTER_TYPES),
        sample_range_filter_inner(ctx),
    )
}

/// 生成指示から Range Filter のワイヤバイト列を組み立てる
///
/// ワイヤフォーマット: SetID (1 byte) | [Property Type (vi64)] | Range...
/// Start=1 の open-ended な 1 Range に固定する。デルタ溢出を起こさず、
/// PRIORITY_FILTER (0x27) の 0..=255 制約 (§9.20.13) も満たすため、
/// `validate_range_filters()` が返すエラーの原因を重複キーだけに絞れる。
fn range_filter_bytes_from_spec(spec: RangeFilterSpec) -> Vec<u8> {
    let (param_type, inner) = spec;
    let Some((set_id, property_type)) = inner else {
        return Vec::new();
    };
    let mut bytes = vec![set_id];
    if has_property_type_field(param_type) {
        varint::encode(property_type, &mut bytes);
    }
    varint::encode(1, &mut bytes);
    bytes
}

/// 生成指示が §3.3.2 の重複判定キーに寄与する場合のキーを返す
///
/// draft-ietf-moq-transport-21 §3.3.2 の重複判定キーは
/// (Parameter Type, SetID, Property Type) で、Property Type は 0x28 / 0x29 のみ。
/// Length=0 は SetID フィールド自体を持たないため対象外 (`None`)。
fn duplicate_key_from_spec(spec: RangeFilterSpec) -> Option<(u64, u8, Option<u64>)> {
    let (param_type, inner) = spec;
    let (set_id, property_type) = inner?;
    Some((
        param_type,
        set_id,
        has_property_type_field(param_type).then_some(property_type),
    ))
}

/// 生成指示の列から `MessageParameters` を組み立てる
fn params_from_specs(specs: &[RangeFilterSpec]) -> MessageParameters {
    let mut params = MessageParameters::new();
    for &spec in specs {
        params.push(MessageParameter {
            param_type: spec.0,
            value: MessageParameterValue::LengthPrefixed(range_filter_bytes_from_spec(spec)),
        });
    }
    params
}

/// draft-ietf-moq-transport-21 §3.3.2 (Range Filters): `validate_range_filters()` は
/// (Parameter Type, SetID, Property Type) の重複があるとき、かつそのときに限りエラーを返す。
///
/// 生成される Range は Start=1 の 1 Range 固定でデルタ溢出も PRIORITY_FILTER の値域超過も
/// 起こさないため、エラー要因は重複キーだけに絞られる。Length=0 は SetID を持たず
/// 重複判定の対象外なので、いくつ並んでもエラーにならないことも同時に検証される。
/// 判定キーに Property Type が入るのは 0x28 / 0x29 のみで、0x25-0x27 は
/// SetID だけが異なれば別キーになる点も含めて確認する。
#[test]
fn validate_range_filters_errs_exactly_on_duplicate_keys() -> noprop::TestResult {
    // 重複キーあり/なしの両方の観測をカバレッジゲートで検証する
    let duplicate_seen = std::cell::Cell::new(false);
    let no_duplicate_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let spec_count = noprop::sample_usize_in(ctx, 0..=6);
        let specs = (0..spec_count)
            .map(|_| sample_range_filter_spec(ctx))
            .collect::<Vec<_>>();
        let params = params_from_specs(&specs);

        let mut keys = Vec::new();
        let mut has_duplicate = false;
        for &spec in &specs {
            if let Some(key) = duplicate_key_from_spec(spec) {
                if keys.contains(&key) {
                    has_duplicate = true;
                }
                keys.push(key);
            }
        }
        if has_duplicate {
            duplicate_seen.set(true);
        } else {
            no_duplicate_seen.set(true);
        }

        assert_eq!(params.validate_range_filters().is_err(), has_duplicate);
        Ok(())
    })?;
    assert!(
        duplicate_seen.get(),
        "重複キーが存在するケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        no_duplicate_seen.get(),
        "重複キーが存在しないケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

/// draft-ietf-moq-transport-21 §3.3.2 (Range Filters): 同一 Parameter Type の複数インスタンスが
/// encode → decode を経ても件数・内容・出現順ごと保たれる。
///
/// §3.3.2 の意味論としてはインスタンスの順序に意味はない (SetID は各インスタンスの
/// ペイロード先頭に載り、AND と OR は可換)。ここで順序まで固定するのは実装契約の回帰防止:
/// `range_filters()` は「出現順に返す」ことを doc で約束しており、`merge_from()` の
/// 置換結果もその順序に依存している。encode は `sort_by_key` の安定性でこれを保っているが、
/// 安定でないソートに変えると同一型内の順序が崩れて契約が破れる。
///
/// `count_range_filters()` は decode 結果との一致ではなく生成指示から独立に数える。
/// 両辺を decode 前後で比較すると、実装が壊れても同じ値になり検証にならないため。
#[test]
fn range_filter_instances_survive_roundtrip() -> noprop::TestResult {
    // Length=0 のインスタンス (削除指示) の観測をカバレッジゲートで検証する
    let length_zero_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let spec_count = noprop::sample_usize_in(ctx, 0..=6);
        let specs = (0..spec_count)
            .map(|_| sample_range_filter_spec(ctx))
            .collect::<Vec<_>>();
        if specs.iter().any(|s| s.1.is_none()) {
            length_zero_seen.set(true);
        }
        let params = params_from_specs(&specs);

        let mut buf = Vec::new();
        params
            .encode(&mut buf)
            .expect("Range Filter の複数出現は encode できなければならない");
        let (decoded, consumed) = MessageParameters::decode(&buf)
            .expect("Range Filter の複数出現は decode できなければならない");
        assert_eq!(consumed, buf.len());

        for &param_type in RANGE_FILTER_TYPES {
            assert_eq!(
                decoded.range_filters(param_type),
                params.range_filters(param_type)
            );
        }

        // 生成指示から独立に期待値を導く。1 件の spec につき Length=0 なら Range 0 個、
        // それ以外は Start=1 の 1 Range だけを載せている。
        let expected_ranges = specs.iter().filter(|s| s.1.is_some()).count() as u64;
        assert_eq!(decoded.count_range_filters(), expected_ranges);
        assert_eq!(decoded.has_range_filters(), !specs.is_empty());
        Ok(())
    })?;
    assert!(
        length_zero_seen.get(),
        "Length=0 の Range Filter インスタンスが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

/// draft-ietf-moq-transport-21 §3.3.2 (Range Filters): REQUEST_UPDATE のマージは
/// Range Filter を型単位で全置換する。
///
/// "In REQUEST_UPDATE, Length can be 0 to remove a filter parameter or non-zero to replace
/// that entire filter parameter including all sets and Property Types."
/// "If a filter parameter is omitted from REQUEST_UPDATE, the value is unchanged."
///
/// 期待値は生成指示から独立に導く: `other` に現れた型は `other` の非ゼロインスタンスだけに
/// なり、現れなかった型は `base` のまま。削除パスと追加パスの分離が壊れると落ちる
/// (分離の根拠は `MessageParameters::merge_from` のコメント参照)。
#[test]
fn merge_from_replaces_range_filters_per_type() -> noprop::TestResult {
    // 置換 (other に型が現れる) と維持 (other に型が現れない) の両方の観測を
    // カバレッジゲートで検証する
    let replaced_seen = std::cell::Cell::new(false);
    let preserved_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let base_count = noprop::sample_usize_in(ctx, 0..=5);
        let base_specs = (0..base_count)
            .map(|_| sample_range_filter_spec(ctx))
            .collect::<Vec<_>>();
        let other_count = noprop::sample_usize_in(ctx, 0..=5);
        let other_specs = (0..other_count)
            .map(|_| sample_range_filter_spec(ctx))
            .collect::<Vec<_>>();
        let base = params_from_specs(&base_specs);
        let other = params_from_specs(&other_specs);

        let mut merged = base.clone();
        merged.merge_from(&other);

        for &param_type in RANGE_FILTER_TYPES {
            let appears_in_other = other_specs.iter().any(|s| s.0 == param_type);
            if appears_in_other {
                replaced_seen.set(true);
            } else {
                preserved_seen.set(true);
            }
            let expected: Vec<Vec<u8>> = if appears_in_other {
                // 型が現れたら other の非ゼロインスタンスだけが残る
                other_specs
                    .iter()
                    .filter(|s| s.0 == param_type && s.1.is_some())
                    .map(|&s| range_filter_bytes_from_spec(s))
                    .collect()
            } else {
                // 現れなかった型は base のまま (Length=0 も含めて不変)
                base_specs
                    .iter()
                    .filter(|s| s.0 == param_type)
                    .map(|&s| range_filter_bytes_from_spec(s))
                    .collect()
            };
            let got: Vec<Vec<u8>> = merged
                .range_filters(param_type)
                .into_iter()
                .map(<[u8]>::to_vec)
                .collect();
            assert_eq!(got, expected);
        }
        Ok(())
    })?;
    assert!(
        replaced_seen.get(),
        "other に現れて置換される型のケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        preserved_seen.get(),
        "other に現れず base のまま維持される型のケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

/// draft-ietf-moq-transport-21 §3.3.1 (Location Filters): NextObject の実効 Start は {group, object + 1} (飽和付き)。
#[test]
fn next_object_start_derivation() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let largest = sample_location(ctx);
        let got = LocationFilter::NextObject.effective_start_location(Some(&largest));
        assert_eq!(
            got,
            Some(Location {
                group_id: largest.group_id,
                object_id: largest.object_id.saturating_add(1),
            })
        );
        Ok(())
    })?;
    Ok(())
}

/// draft-ietf-moq-transport-21 §3.3.1 (Location Filters): RelativeGroup の実効 Start は {Largest Group + 1 - StartGroup, 0}。
/// sample は小さい varint 同士なので Largest Group + 1 はオーバーフローせず、StartGroup が大きければ 0 にクランプされる。
#[test]
fn relative_group_start_derivation() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let largest = sample_location(ctx);
        let start_group = sample_small_varint(ctx);
        let got =
            LocationFilter::RelativeGroup { start_group }.effective_start_location(Some(&largest));
        let expected_group = (largest.group_id + 1).saturating_sub(start_group);
        assert_eq!(
            got,
            Some(Location {
                group_id: expected_group,
                object_id: 0,
            })
        );
        Ok(())
    })?;
    Ok(())
}

/// draft-ietf-moq-transport-21 §3.3.1 (Location Filters): Absolute 系は largest によらず start を恒等で返す。
/// AbsoluteRange の End は {start.group + delta, u64::MAX} (End Group の全 Object)、
/// AbsoluteRangeWithEnd の End は {start.group + delta, end_object}。
#[test]
fn absolute_filters_identity() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let start = sample_location(ctx);
        let delta = sample_small_varint(ctx);
        let end_object = sample_small_varint(ctx);
        let largest = sample_location(ctx);
        let abs_start = LocationFilter::AbsoluteStart { start };
        assert_eq!(
            abs_start.effective_start_location(Some(&largest)),
            Some(start)
        );
        assert_eq!(abs_start.effective_start_location(None), Some(start));
        assert_eq!(
            abs_start.effective_end_location(Some(&largest), LocationFilterContext::Subscription),
            None
        );

        let abs_range = LocationFilter::AbsoluteRange {
            start,
            end_group_delta: delta,
        };
        assert_eq!(
            abs_range.effective_start_location(Some(&largest)),
            Some(start)
        );
        // sample_small_varint 同士なので start.group + delta はオーバーフローしない
        assert_eq!(
            abs_range.effective_end_location(None, LocationFilterContext::Subscription),
            Some(Location {
                group_id: start.group_id + delta,
                object_id: u64::MAX,
            })
        );

        let abs_range_end = LocationFilter::AbsoluteRangeWithEnd {
            start,
            end_group_delta: delta,
            end_object,
        };
        assert_eq!(
            abs_range_end.effective_end_location(None, LocationFilterContext::Subscription),
            Some(Location {
                group_id: start.group_id + delta,
                object_id: end_object,
            })
        );
        Ok(())
    })?;
    Ok(())
}

/// draft-ietf-moq-transport-21 §3.3.1 (Location Filters): 同一 largest に対し RelativeGroup{0} の Start は NextObject の Start 以上。
/// Location は group 優先の辞書順なので {G+1, 0} > {G, O+1} が object 値によらず常に成立する。
#[test]
fn relative_group_zero_ge_next_object() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let largest = sample_location(ctx);
        let next_object = LocationFilter::NextObject
            .effective_start_location(Some(&largest))
            .expect("sample_location は小さい varint なのでオーバーフローしない");
        let next_group = LocationFilter::RelativeGroup { start_group: 0 }
            .effective_start_location(Some(&largest))
            .expect("相対 StartGroup はクランプするため常時 Some を返す");
        assert!(next_group >= next_object);
        Ok(())
    })?;
    Ok(())
}
