use shiguredo_moqt::{
    message::common::Location, message_parameter::AuthorizationToken,
    message_parameter::LocationFilter, message_parameter::LocationFilterContext,
    message_parameter::MessageParameter, message_parameter::MessageParameterValue,
    message_parameter::MessageParameters, message_parameter::PARAM_AUTHORIZATION_TOKEN,
    message_parameter::PARAM_EXPIRES, message_parameter::PARAM_FILL_PARAMETERS,
    message_parameter::PARAM_FILL_TIMEOUT, message_parameter::PARAM_GROUP_ORDER,
    message_parameter::PARAM_INCLUDE_PROPERTIES, message_parameter::PARAM_LARGEST_OBJECT,
    message_parameter::PARAM_LOCATION_FILTER, message_parameter::PARAM_NEW_GROUP_REQUEST,
    message_parameter::PARAM_OBJECT_DELIVERY_TIMEOUT,
    message_parameter::PARAM_OBJECT_PROPERTY_FILTER, message_parameter::PARAM_OBJECTID_FILTER,
    message_parameter::PARAM_PAUSED, message_parameter::PARAM_PRIORITY_FILTER,
    message_parameter::PARAM_SUBGROUP_DELIVERY_TIMEOUT, message_parameter::PARAM_SUBGROUP_FILTER,
    message_parameter::PARAM_SUBSCRIBER_PRIORITY, varint,
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
/// draft-ietf-moq-transport-22 §9.20.13 / §9.20.14: Property Type は偶数でなければならない。
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
/// PRIORITY_FILTER (0x27) の 0..=255 制約 (§9.20.12) も満たすため、
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
/// draft-ietf-moq-transport-22 §3.3.2 の重複判定キーは
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

/// draft-ietf-moq-transport-22 §3.3.2 (Range Filters): `validate_range_filters()` は
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

/// draft-ietf-moq-transport-22 §3.3.2 (Range Filters): 同一 Parameter Type の複数インスタンスが
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

/// draft-ietf-moq-transport-22 §3.3.2 (Range Filters): REQUEST_UPDATE のマージは
/// Range Filter を型単位で全置換する。
///
/// "In REQUEST_UPDATE, Length of 0 removes the filter; non-zero replaces it entirely."
/// "If a filter parameter is omitted from REQUEST_UPDATE, it is unchanged."
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

/// draft-ietf-moq-transport-22 §3.3.1 (Location Filters): NextObject の実効 Start は {group, object + 1} (飽和付き)。
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

/// draft-ietf-moq-transport-22 §3.3.1 (Location Filters): RelativeGroup の実効 Start は {Largest Group + 1 - StartGroup, 0}。
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

/// draft-ietf-moq-transport-22 §3.3.1 (Location Filters): Absolute 系は largest によらず start を恒等で返す。
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

/// draft-ietf-moq-transport-22 §3.3.1 (Location Filters): 同一 largest に対し RelativeGroup{0} の Start は NextObject の Start 以上。
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

/// AUTHORIZATION_TOKEN 1 件の生成指示
///
/// REGISTER / USE_VALUE は (Token Type, Token Value) を、USE_ALIAS / DELETE は alias を持つ。
/// 候補を狭く取ることで (Token Type, Token Value) の重複が十分な確率で生成されるようにする。
#[derive(Debug, Clone, Copy)]
enum AuthTokenSpec {
    /// REGISTER (alias, token_type, token_value)
    Register(u64, u64, u8),
    /// USE_VALUE (token_type, token_value)
    UseValue(u64, u8),
    /// USE_ALIAS (alias)
    UseAlias(u64),
    /// DELETE (alias)
    Delete(u64),
}

fn sample_auth_token_spec(ctx: &mut noprop::TestCaseContext) -> AuthTokenSpec {
    let alias = noprop::sample_usize_in(ctx, 1..=2) as u64;
    let token_type = noprop::sample_usize_in(ctx, 0..=1) as u64;
    let token_value = noprop::sample_usize_in(ctx, 0..=1) as u8;
    match noprop::sample_usize_in(ctx, 0..=3) {
        0 => AuthTokenSpec::Register(alias, token_type, token_value),
        1 => AuthTokenSpec::UseValue(token_type, token_value),
        2 => AuthTokenSpec::UseAlias(alias),
        _ => AuthTokenSpec::Delete(alias),
    }
}

/// alias 解決なしで (Token Type, Token Value) を決められる Token の値を返す
///
/// §8.9 の一意性検証は alias 解決後の値を対象とするが、USE_ALIAS / DELETE は
/// セッション状態を要するため codec 層の `validate_auth_token_uniqueness()` の対象外である
/// (alias 解決後まで含めた検証は peer の token cache を持つセッション層が行う)。
fn auth_spec_type_value(spec: AuthTokenSpec) -> Option<(u64, u8)> {
    match spec {
        AuthTokenSpec::Register(_, token_type, token_value)
        | AuthTokenSpec::UseValue(token_type, token_value) => Some((token_type, token_value)),
        AuthTokenSpec::UseAlias(_) | AuthTokenSpec::Delete(_) => None,
    }
}

/// 生成指示の列から `MessageParameters` を組み立てる
fn params_from_auth_specs(specs: &[AuthTokenSpec]) -> MessageParameters {
    let mut params = MessageParameters::new();
    for &spec in specs {
        let token = match spec {
            AuthTokenSpec::Register(alias, token_type, token_value) => {
                AuthorizationToken::Register {
                    alias,
                    token_type,
                    token_value: vec![token_value],
                }
            }
            AuthTokenSpec::UseValue(token_type, token_value) => AuthorizationToken::UseValue {
                token_type,
                token_value: vec![token_value],
            },
            AuthTokenSpec::UseAlias(alias) => AuthorizationToken::UseAlias { alias },
            AuthTokenSpec::Delete(alias) => AuthorizationToken::Delete { alias },
        };
        params.push(MessageParameter {
            param_type: PARAM_AUTHORIZATION_TOKEN,
            value: MessageParameterValue::AuthorizationToken(token),
        });
    }
    params
}

/// 生成指示から独立に (Token Type, Token Value) の重複を判定する参照モデル
///
/// codec 層の `validate_auth_token_uniqueness()` の対象は alias 解決を要さない
/// REGISTER / USE_VALUE のみである (USE_ALIAS / DELETE はセッション状態を要するため対象外。
/// alias 解決後まで含む検証は peer の token cache を持つセッション層が行う)。
fn has_duplicate_type_value(specs: &[AuthTokenSpec]) -> bool {
    let mut seen = Vec::new();
    for &spec in specs {
        if let Some(key) = auth_spec_type_value(spec) {
            if seen.contains(&key) {
                return true;
            }
            seen.push(key);
        }
    }
    false
}

/// draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression):
/// `validate_auth_token_uniqueness()` は alias 解決を要さない Token (REGISTER / USE_VALUE) の
/// (Token Type, Token Value) が重複するとき、かつそのときに限り `MalformedAuthToken` を返す。
///
/// USE_ALIAS / DELETE は codec 層の検証対象外であり、いくつ並んでも結果に影響しないことも
/// あわせて確認する。重複あり / 重複なし / USE_ALIAS・DELETE を含む入力の 3 つを
/// カバレッジゲートで検証する。
#[test]
fn validate_auth_token_uniqueness_errs_exactly_on_duplicate_type_value() -> noprop::TestResult {
    let duplicate_seen = std::cell::Cell::new(false);
    let no_duplicate_seen = std::cell::Cell::new(false);
    let alias_or_delete_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let spec_count = noprop::sample_usize_in(ctx, 0..=6);
        let specs = (0..spec_count)
            .map(|_| sample_auth_token_spec(ctx))
            .collect::<Vec<_>>();
        let params = params_from_auth_specs(&specs);

        let has_duplicate = has_duplicate_type_value(&specs);
        if has_duplicate {
            duplicate_seen.set(true);
        } else {
            no_duplicate_seen.set(true);
        }
        if specs
            .iter()
            .any(|spec| matches!(spec, AuthTokenSpec::UseAlias(_) | AuthTokenSpec::Delete(_)))
        {
            alias_or_delete_seen.set(true);
        }

        assert_eq!(
            params.validate_auth_token_uniqueness().is_err(),
            has_duplicate
        );
        Ok(())
    })?;
    assert!(
        duplicate_seen.get(),
        "同一 (Token Type, Token Value) が生成されたケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        no_duplicate_seen.get(),
        "重複が無いケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        alias_or_delete_seen.get(),
        "USE_ALIAS / DELETE を含むケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

/// 生成指示 1 件の Token 構造のワイヤバイト列を組み立てる
///
/// draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression):
/// Alias Type (vi64) と、REGISTER は Alias + Token Type + Token Value、USE_VALUE は
/// Token Type + Token Value、USE_ALIAS / DELETE は Alias を続ける。
fn auth_token_wire_bytes(spec: AuthTokenSpec) -> Vec<u8> {
    let mut buf = Vec::new();
    match spec {
        AuthTokenSpec::Delete(alias) => {
            varint::encode(0x0, &mut buf);
            varint::encode(alias, &mut buf);
        }
        AuthTokenSpec::Register(alias, token_type, token_value) => {
            varint::encode(0x1, &mut buf);
            varint::encode(alias, &mut buf);
            varint::encode(token_type, &mut buf);
            buf.push(token_value);
        }
        AuthTokenSpec::UseAlias(alias) => {
            varint::encode(0x2, &mut buf);
            varint::encode(alias, &mut buf);
        }
        AuthTokenSpec::UseValue(token_type, token_value) => {
            varint::encode(0x3, &mut buf);
            varint::encode(token_type, &mut buf);
            buf.push(token_value);
        }
    }
    buf
}

/// 生成指示の列から Message Parameters のワイヤバイト列を組み立てる
///
/// encode は重複した (Token Type, Token Value) を拒否するため、重複を含む入力の decode を
/// 検証するにはテスト側でバイト列を組み立てる必要がある。すべて AUTHORIZATION_TOKEN
/// (0x03) なので、delta は先頭のみ 0x03、以降は 0 になる。
fn auth_params_wire_bytes(specs: &[AuthTokenSpec]) -> Vec<u8> {
    let mut buf = Vec::new();
    varint::encode(specs.len() as u64, &mut buf);
    for (index, &spec) in specs.iter().enumerate() {
        varint::encode(
            if index == 0 {
                PARAM_AUTHORIZATION_TOKEN
            } else {
                0
            },
            &mut buf,
        );
        let token = auth_token_wire_bytes(spec);
        varint::encode(token.len() as u64, &mut buf);
        buf.extend_from_slice(&token);
    }
    buf
}

/// draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression):
/// decode は重複した (Token Type, Token Value) を拒否せず、encode は拒否する。
///
/// decode が重複を拒否すると Request ID が失われ、メッセージ単位の
/// REQUEST_ERROR (MALFORMED_AUTH_TOKEN) を送れなくなる。重複の reject は Request ID を
/// 得たセッション層の責務であり、decode は受理して
/// `validate_auth_token_uniqueness()` に判定を委ねる。encode は自側で重複を組み立てた
/// 時点で拒否する。ワイヤバイト列はテスト側で組み立て、重複の有無にかかわらず decode が
/// 受理して件数と検証結果を保つことを確認する。
#[test]
fn auth_token_encode_rejects_duplicates_but_decode_accepts_them() -> noprop::TestResult {
    let duplicate_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let spec_count = noprop::sample_usize_in(ctx, 0..=6);
        let specs = (0..spec_count)
            .map(|_| sample_auth_token_spec(ctx))
            .collect::<Vec<_>>();
        let params = params_from_auth_specs(&specs);
        // encode の結果は実装内の検証を経由するため、参照モデルで独立に期待値を求める
        let has_duplicate = has_duplicate_type_value(&specs);
        if has_duplicate {
            duplicate_seen.set(true);
        }

        // encode は自側で重複を組み立てた時点で拒否する
        let mut encoded = Vec::new();
        assert_eq!(
            params.encode(&mut encoded).is_err(),
            has_duplicate,
            "encode の重複拒否が参照モデルと一致すること"
        );

        // decode は重複を受理する (reject は Request ID を得たセッション層の責務)
        let wire = auth_params_wire_bytes(&specs);
        let (decoded, consumed) = MessageParameters::decode(&wire)
            .expect("(Token Type, Token Value) の重複は decode では検出しない");
        assert_eq!(consumed, wire.len(), "生成したバイト列をすべて消費すること");
        assert_eq!(decoded.authorization_tokens().len(), specs.len());
        assert_eq!(
            decoded.validate_auth_token_uniqueness().is_err(),
            has_duplicate,
            "decode 後も一意性検証の結果が参照モデルと一致すること"
        );
        Ok(())
    })?;
    assert!(
        duplicate_seen.get(),
        "重複した (Token Type, Token Value) の decode が 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

// ─── FILL_PARAMETERS の個数符号化 (draft-ietf-moq-transport-23 §9.19.15) ────────

/// FILL_PARAMETERS の内側に出現できる型 (Table 7)
///
/// `src/message_parameter.rs` の `FILL_PARAMETERS_ALLOWED_PARAMS` と一致させること。
const FILL_INNER_TYPES: &[u64] = &[
    PARAM_FILL_TIMEOUT,
    PARAM_SUBSCRIBER_PRIORITY,
    PARAM_LOCATION_FILTER,
    PARAM_GROUP_ORDER,
    PARAM_SUBGROUP_FILTER,
    PARAM_OBJECTID_FILTER,
    PARAM_PRIORITY_FILTER,
    PARAM_OBJECT_PROPERTY_FILTER,
];

/// FILL_PARAMETERS (0x23) より小さい型の外側パラメータ候補
///
/// 外側のデルタの基準が内側へ漏れないことを確かめるため、FILL_PARAMETERS の前に置く。
const OUTER_LOWER_TYPES: &[u64] = &[
    PARAM_OBJECT_DELIVERY_TIMEOUT,
    PARAM_SUBGROUP_DELIVERY_TIMEOUT,
    PARAM_EXPIRES,
    PARAM_LARGEST_OBJECT,
    PARAM_FILL_TIMEOUT,
    PARAM_PAUSED,
    PARAM_SUBSCRIBER_PRIORITY,
    PARAM_LOCATION_FILTER,
    PARAM_GROUP_ORDER,
];

/// FILL_PARAMETERS (0x23) より大きい型の外側パラメータ候補
///
/// 内側の個数で読み終えたあとに残る外側パラメータとして置き、内側へ巻き込まれないことを
/// 確かめる。
const OUTER_HIGHER_TYPES: &[u64] = &[
    PARAM_SUBGROUP_FILTER,
    PARAM_OBJECTID_FILTER,
    PARAM_PRIORITY_FILTER,
    PARAM_OBJECT_PROPERTY_FILTER,
    PARAM_NEW_GROUP_REQUEST,
    PARAM_INCLUDE_PROPERTIES,
];

/// 型の候補から重複なしで `count` 個を選び、昇順に並べて返す
///
/// draft-ietf-moq-transport-23 §9.19 (Control Message Parameters) は、パラメータ定義が
/// 明示的に複数出現を許可していない型の繰り返しを送信側の MUST NOT と定める。
/// 同一メッセージ内で型が重複しないよう、Fisher-Yates で候補から選ぶ。
fn sample_distinct_types(
    ctx: &mut noprop::TestCaseContext,
    types: &[u64],
    count: usize,
) -> Vec<u64> {
    let mut pool = types.to_vec();
    for i in 0..count {
        let j = i + noprop::sample_usize_in(ctx, 0..(pool.len() - i));
        pool.swap(i, j);
    }
    pool.truncate(count);
    pool.sort_unstable();
    pool
}

/// Table 7 の LOCATION_FILTER 用に、§9.19.9 の値域を満たす typed filter を生成する
///
/// AbsoluteRange 系の `StartGroup + EndGroupDelta` が 2^64 - 1 を超えないよう、
/// 各値は小さな varint に限定する。
fn sample_fill_location_filter(ctx: &mut noprop::TestCaseContext) -> LocationFilter {
    let start = Location {
        group_id: sample_small_varint(ctx),
        object_id: sample_small_varint(ctx),
    };
    match noprop::sample_usize_in(ctx, 0..=5) {
        0 => LocationFilter::NoFilter,
        1 => LocationFilter::RelativeGroup {
            start_group: sample_small_varint(ctx),
        },
        2 => LocationFilter::AbsoluteStart { start },
        3 => LocationFilter::AbsoluteRange {
            start,
            end_group_delta: sample_small_varint(ctx),
        },
        4 => LocationFilter::AbsoluteRangeWithEnd {
            start,
            end_group_delta: sample_small_varint(ctx),
            end_object: sample_small_varint(ctx),
        },
        _ => LocationFilter::NextObject,
    }
}

/// Range Filter 1 インスタンスのバイト列を生成する
///
/// draft-ietf-moq-transport-22 §8.6 (Range Filter Structure): SetID (8 bits) と、
/// 0x28 系のみが持つ Property Type (偶数) のあとに、開始位置 1 の open-ended な Range を
/// 1 つ置く。PRIORITY_FILTER (0x27) の 0..=255 制約も満たす。
fn sample_range_filter_value(ctx: &mut noprop::TestCaseContext, param_type: u64) -> Vec<u8> {
    let mut bytes = vec![noprop::sample_u8(ctx)];
    if has_property_type_field(param_type) {
        // draft-ietf-moq-transport-22 §9.20.13 / §9.20.14: Property Type MUST be even
        varint::encode(noprop::sample_choice(ctx, &[0u64, 2u64]), &mut bytes);
    }
    varint::encode(1, &mut bytes);
    bytes
}

/// Table 7 の範囲で 0 個以上の内側パラメータ群を生成する
fn sample_inner_parameters(ctx: &mut noprop::TestCaseContext) -> MessageParameters {
    let max_count = FILL_INNER_TYPES.len().min(4);
    let count = noprop::sample_usize_in(ctx, 0..=max_count);
    let mut params = MessageParameters::new();
    for param_type in sample_distinct_types(ctx, FILL_INNER_TYPES, count) {
        let value = match param_type {
            PARAM_FILL_TIMEOUT => MessageParameterValue::VarInt(sample_small_varint(ctx)),
            PARAM_SUBSCRIBER_PRIORITY => MessageParameterValue::Uint8(noprop::sample_u8(ctx)),
            PARAM_GROUP_ORDER => {
                MessageParameterValue::Uint8(noprop::sample_choice(ctx, &[1u8, 2u8]))
            }
            PARAM_LOCATION_FILTER => {
                MessageParameterValue::LocationFilter(sample_fill_location_filter(ctx))
            }
            PARAM_SUBGROUP_FILTER
            | PARAM_OBJECTID_FILTER
            | PARAM_PRIORITY_FILTER
            | PARAM_OBJECT_PROPERTY_FILTER => {
                MessageParameterValue::LengthPrefixed(sample_range_filter_value(ctx, param_type))
            }
            _ => panic!("FILL_PARAMETERS の内側に置けない型: {param_type}"),
        };
        params.push(MessageParameter { param_type, value });
    }
    params
}

/// 外側のパラメータ群を生成する (型は昇順、重複なし)
fn sample_outer_parameters(ctx: &mut noprop::TestCaseContext, types: &[u64]) -> MessageParameters {
    let count = noprop::sample_usize_in(ctx, 0..=types.len());
    let mut params = MessageParameters::new();
    for param_type in sample_distinct_types(ctx, types, count) {
        let value = match param_type {
            PARAM_OBJECT_DELIVERY_TIMEOUT
            | PARAM_SUBGROUP_DELIVERY_TIMEOUT
            | PARAM_EXPIRES
            | PARAM_FILL_TIMEOUT
            | PARAM_NEW_GROUP_REQUEST => MessageParameterValue::VarInt(sample_small_varint(ctx)),
            PARAM_LARGEST_OBJECT => MessageParameterValue::Location {
                group: sample_small_varint(ctx),
                object: sample_small_varint(ctx),
            },
            PARAM_PAUSED | PARAM_INCLUDE_PROPERTIES => {
                MessageParameterValue::Uint8(noprop::sample_choice(ctx, &[0u8, 1u8]))
            }
            PARAM_SUBSCRIBER_PRIORITY => MessageParameterValue::Uint8(noprop::sample_u8(ctx)),
            PARAM_GROUP_ORDER => {
                MessageParameterValue::Uint8(noprop::sample_choice(ctx, &[1u8, 2u8]))
            }
            PARAM_LOCATION_FILTER => {
                MessageParameterValue::LocationFilter(sample_fill_location_filter(ctx))
            }
            PARAM_SUBGROUP_FILTER
            | PARAM_OBJECTID_FILTER
            | PARAM_PRIORITY_FILTER
            | PARAM_OBJECT_PROPERTY_FILTER => {
                MessageParameterValue::LengthPrefixed(sample_range_filter_value(ctx, param_type))
            }
            _ => panic!("外側に置く型として未定義: {param_type}"),
        };
        params.push(MessageParameter { param_type, value });
    }
    params
}

/// draft-ietf-moq-transport-23 §9.19.15 (FILL PARAMETERS Parameter):
/// FILL_PARAMETERS の値は `Number of Parameters (vi64)` とその個数の Parameters であり、
/// 外側の byte length を持たない。
///
/// 内側の個数ぶんだけをデコードし、外側の後続パラメータを内側へ巻き込まないことを
/// 任意の内側パラメータ数で検証する。外側には FILL_PARAMETERS より小さい型のパラメータを
/// 前、大きい型のパラメータを後ろに置く。内側の delta の基準は 0 から取り直すため、
/// 前段の型が何であっても内側の先頭の型はその delta のまま解決される。
///
/// 期待値は decode 結果と encode 入力を突き合わせて求める。外側の並びが変われば
/// 後続パラメータが内側へ吸われたことになり、内側が変われば個数の読み進め方が壊れている。
#[test]
fn fill_parameters_count_framing_roundtrip() -> noprop::TestResult {
    // 内側 0 個 / 内側 2 個以上 / 外側に後続パラメータあり の観測をカバレッジゲートで検証する
    let empty_inner_seen = std::cell::Cell::new(false);
    let multiple_inner_seen = std::cell::Cell::new(false);
    let following_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let inner = sample_inner_parameters(ctx);
        let mut params = sample_outer_parameters(ctx, OUTER_LOWER_TYPES);
        params.push(MessageParameter {
            param_type: PARAM_FILL_PARAMETERS,
            value: MessageParameterValue::FillParameters(inner.clone()),
        });
        let following = sample_outer_parameters(ctx, OUTER_HIGHER_TYPES);
        let has_following = !following.is_empty();
        for param in following.as_slice() {
            params.push(param.clone());
        }

        if inner.is_empty() {
            empty_inner_seen.set(true);
        }
        if inner.len() > 1 {
            multiple_inner_seen.set(true);
        }
        if has_following {
            following_seen.set(true);
        }

        let mut buf = Vec::new();
        params
            .encode(&mut buf)
            .expect("正当なパラメータ群は encode できなければならない");
        let (decoded, consumed) = MessageParameters::decode(&buf)
            .expect("encode したパラメータ群は decode できなければならない");
        assert_eq!(consumed, buf.len(), "生成したバイト列をすべて消費すること");

        // 外側は型の並びだけでなく値まで含めて一致することを確認する。型が同じでも
        // 値が壊れていれば検出できる
        assert_eq!(
            decoded, params,
            "外側の型・値と内側のパラメータ群がすべて保たれること"
        );
        assert_eq!(
            decoded.fill_parameters(),
            Some(&inner),
            "内側のパラメータ群が件数・内容ともに保たれること"
        );
        Ok(())
    })?;
    assert!(
        empty_inner_seen.get(),
        "内側 0 個の FILL_PARAMETERS が 1 つも観測されなかった\n{runner}"
    );
    assert!(
        multiple_inner_seen.get(),
        "内側 2 個以上の FILL_PARAMETERS が 1 つも観測されなかった\n{runner}"
    );
    assert!(
        following_seen.get(),
        "FILL_PARAMETERS の後続する外側パラメータが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

/// PAUSED (0x11) の省略・0・1 が encode → decode で区別されたまま保たれること
///
/// draft-ietf-moq-transport-23 §9.19.18 (PAUSED Parameter): PAUSED は uint8 で
/// 0 は unpaused、1 は paused、省略時の既定値は 0 である。省略と 0 の明示は
/// REQUEST_UPDATE / PUBLISH_STATE_NOTIFY で意味が異なる (前者は現在値の維持、
/// 後者は unpaused への変更) ため、codec は両者を区別して保持しなければならない。
///
/// 3 状態 (省略 / 0 / 1) を等しく生成し、`paused()` が `None` / `Some(0)` / `Some(1)` を
/// そのまま返すこと、およびワイヤ上の型が 0x11 で値が 1 バイトであることを検証する。
/// 旧 FORWARD (0x10) のワイヤ値が使われていないことも、このバイト列の固定で検出できる。
#[test]
fn paused_parameter_roundtrip_preserves_absence_and_values() -> noprop::TestResult {
    // 3 状態すべてが観測されたことをカバレッジゲートで検証する
    let absent_seen = std::cell::Cell::new(false);
    let unpaused_seen = std::cell::Cell::new(false);
    let paused_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        // 0: 省略、1: 0 (unpaused) の明示、2: 1 (paused) の明示
        let (sampled, expected) = match noprop::sample_usize_in(ctx, 0..=2) {
            0 => {
                absent_seen.set(true);
                (None, None)
            }
            1 => {
                unpaused_seen.set(true);
                (Some(0u8), Some(0u8))
            }
            _ => {
                paused_seen.set(true);
                (Some(1u8), Some(1u8))
            }
        };
        let mut params = MessageParameters::new();
        if let Some(value) = sampled {
            params.push(MessageParameter {
                param_type: PARAM_PAUSED,
                value: MessageParameterValue::Uint8(value),
            });
        }

        let mut buf = Vec::new();
        params
            .encode(&mut buf)
            .expect("正当な PAUSED の値は encode できなければならない");
        // ワイヤは count (vi64) + delta 0x11 + 値 1 バイト。省略時は count 0 の 1 バイトのみ
        let expected_wire: Vec<u8> = match sampled {
            None => vec![0x00],
            Some(value) => vec![0x01, PARAM_PAUSED as u8, value],
        };
        assert_eq!(
            buf, expected_wire,
            "PAUSED のワイヤは count + 型 0x11 + uint8 値であること"
        );

        let (decoded, consumed) = MessageParameters::decode(&buf)
            .expect("encode した PAUSED は decode できなければならない");
        assert_eq!(consumed, buf.len(), "生成したバイト列をすべて消費すること");
        assert_eq!(
            decoded.paused(),
            expected,
            "省略・0・1 がそのまま保たれること"
        );
        Ok(())
    })?;
    assert!(
        absent_seen.get(),
        "PAUSED を省略したケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        unpaused_seen.get(),
        "PAUSED=0 (unpaused) のケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        paused_seen.get(),
        "PAUSED=1 (paused) のケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}
