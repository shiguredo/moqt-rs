use shiguredo_moqt::track_properties::{
    MANDATORY_TRACK_PROPERTY_MAX, MANDATORY_TRACK_PROPERTY_MIN, PROP_IMMUTABLE_PROPERTIES,
    PROP_MAX_CACHE_DURATION, PROP_OBJECT_DELIVERY_TIMEOUT, PROP_SUBGROUP_DELIVERY_TIMEOUT,
    TrackProperties, TrackProperty, TrackPropertyValue,
};

use pbt::common::{sample_track_properties, test_runner};

#[test]
fn roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let props = sample_track_properties(ctx);
        let mut buf = Vec::new();
        props
            .encode(&mut buf)
            .expect("正当なテスト入力の encode は成功する");
        let decoded = TrackProperties::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        // デコード後も内容が一致すること (件数・再 encode のみでは値の破壊を検出できない)
        assert_eq!(decoded, props);

        // decode 後のプロパティが prop_type の昇順でソートされていること
        let slice = decoded.as_slice();
        for w in slice.windows(2) {
            assert!(w[0].prop_type < w[1].prop_type);
        }
        Ok(())
    })?;
    Ok(())
}

/// MAX_CACHE_DURATION (0x04) を含む TrackProperties が往復し、
/// デコード後も同じ値を返すこと (draft-ietf-moq-transport-22 §10.3 (MAX CACHE DURATION))。
/// 値域制約は無いため full u64 域 (varint は 9 バイトまでで全域対応) を生成する。
#[test]
fn max_cache_duration_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let v = noprop::sample_u64(ctx);
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: PROP_MAX_CACHE_DURATION,
            value: TrackPropertyValue::VarInt(v),
        });
        let mut buf = Vec::new();
        props
            .encode(&mut buf)
            .expect("正当なテスト入力の encode は成功する");
        let decoded = TrackProperties::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(decoded.find_varint(PROP_MAX_CACHE_DURATION), Some(v));
        Ok(())
    })?;
    Ok(())
}

// ─── 未知の Mandatory Track Property の列挙 ────────────────────

/// 既知の Track Property 型 (この PBT が値域の制約なしに生成できるもの)
///
/// 0x02 / 0x04 / 0x06 は値域制約のない Track Property である
/// (draft-ietf-moq-transport-23 §10.1 (SUBGROUP_DELIVERY_TIMEOUT) /
/// §10.2 (OBJECT_DELIVERY_TIMEOUT) / §10.3 (MAX CACHE DURATION))。
const KNOWN_TYPES: &[u64] = &[
    PROP_OBJECT_DELIVERY_TIMEOUT,
    PROP_MAX_CACHE_DURATION,
    PROP_SUBGROUP_DELIVERY_TIMEOUT,
];

/// 0x4000-0x7FFF に入る GREASE 予約値 (draft-ietf-moq-transport-23 §13 (Grease))
const GREASE_IN_MANDATORY_RANGE: &[u64] = &[0x401D, 0x409C, 0x7F9D];

/// 0x4000-0x7FFF のうち GREASE でない未知の必須プロパティ型を生成する
fn sample_unknown_mandatory_type(ctx: &mut noprop::TestCaseContext) -> u64 {
    let property_type = noprop::sample_u64_in(
        ctx,
        MANDATORY_TRACK_PROPERTY_MIN..=MANDATORY_TRACK_PROPERTY_MAX,
    );
    if shiguredo_moqt::grease::is_grease(property_type) {
        // GREASE 値は 0x7F 間隔で孤立しているため、+1 すれば範囲内かつ GREASE でない値になる
        property_type + 1
    } else {
        property_type
    }
}

/// 型と値形式の整合した Track Property を生成する
fn property_of_type(ctx: &mut noprop::TestCaseContext, prop_type: u64) -> TrackProperty {
    if prop_type.is_multiple_of(2) {
        // 偶数型は varint (draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure))
        TrackProperty {
            prop_type,
            value: TrackPropertyValue::VarInt(noprop::sample_u64_in(ctx, 0..=1000)),
        }
    } else {
        // 奇数型は長さ付きバイト列
        let len = noprop::sample_usize_in(ctx, 0..=8);
        TrackProperty {
            prop_type,
            value: TrackPropertyValue::Bytes(noprop::sample_bytes_vec(ctx, len)),
        }
    }
}

/// 候補となる Track Property 型を 1 つ生成する
///
/// 未知の必須型・GREASE 予約値・既知型・必須範囲外の型を混ぜ、
/// 列挙の対象と対象外が同じ入力に現れるようにする。
fn sample_candidate_type(ctx: &mut noprop::TestCaseContext) -> u64 {
    match noprop::sample_weighted_index(ctx, &[3, 1, 1, 1]) {
        // 未知の必須プロパティ (列挙の対象)
        0 => sample_unknown_mandatory_type(ctx),
        // GREASE 予約値 (0x4000-0x7FFF に入るが列挙の対象外)
        1 => noprop::sample_choice(ctx, GREASE_IN_MANDATORY_RANGE),
        // 既知プロパティ (列挙の対象外)
        2 => noprop::sample_choice(ctx, KNOWN_TYPES),
        // 必須範囲外の型 (列挙の対象外)。Track scope で正当な型だけを使う
        // (0x09 は Object 専用のため、encode がスコープ違反として拒否する)
        _ => noprop::sample_choice(ctx, &[0x08u64, 0x0Au64, 0x1000u64]),
    }
}

/// 仕様の条件をそのまま書き下した未知の Mandatory Track Property 判定
///
/// 実装 (`TrackProperties::is_unknown_mandatory`) とは独立に、§3.7 (Mandatory to Understand
/// Track Properties) の範囲・§13 (Grease) の GREASE 予約値・§16.8 (Properties) の既知型という
/// 3 条件だけで判定する。
fn is_unknown_mandatory_property(prop_type: u64) -> bool {
    (MANDATORY_TRACK_PROPERTY_MIN..=MANDATORY_TRACK_PROPERTY_MAX).contains(&prop_type)
        && !shiguredo_moqt::grease::is_grease(prop_type)
        && !KNOWN_TYPES.contains(&prop_type)
}

/// `unknown_mandatory_types` が mutable と IMMUTABLE 内側を合わせた未知の必須型だけを返すこと
///
/// draft-ietf-moq-transport-23 §3.7 (Mandatory to Understand Track Properties) の範囲、
/// §10.7 (Immutable Properties) の "MUST search both"、§13 (Grease) の GREASE 除外、
/// §16.8 (Properties) の既知型を独立に判定した期待値と比較する。同じ型が mutable と
/// IMMUTABLE の両方に現れないよう候補を重複除去してから分割するため、列挙の重複除去と
/// 昇順は別途固定する (単体テスト側)。
#[test]
fn unknown_mandatory_types_matches_independent_filter() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let count = noprop::sample_usize_in(ctx, 0..=6);
        let mut candidates: Vec<u64> = Vec::new();
        for _ in 0..count {
            candidates.push(sample_candidate_type(ctx));
        }
        candidates.sort_unstable();
        candidates.dedup();

        // 半分を IMMUTABLE_PROPERTIES の内側に置き、残りを mutable リストに置く
        let split = candidates.len() / 2;
        let (inner_types, mutable_types) = candidates.split_at(split);
        let mut props = TrackProperties::new();
        for &prop_type in mutable_types {
            props.push(property_of_type(ctx, prop_type));
        }
        if !inner_types.is_empty() {
            let mut inner = TrackProperties::new();
            for &prop_type in inner_types {
                inner.push(property_of_type(ctx, prop_type));
            }
            let mut inner_bytes = Vec::new();
            inner
                .encode(&mut inner_bytes)
                .expect("内側の KVP 列は encode 可能");
            props.push(TrackProperty {
                prop_type: PROP_IMMUTABLE_PROPERTIES,
                value: TrackPropertyValue::Bytes(inner_bytes),
            });
        }

        let expected: Vec<u64> = candidates
            .iter()
            .copied()
            .filter(|prop_type| is_unknown_mandatory_property(*prop_type))
            .collect();
        assert_eq!(
            props.unknown_mandatory_types(),
            expected,
            "未知の必須プロパティ型の列挙が仕様の条件と一致すること"
        );
        assert_eq!(
            props.has_unknown_mandatory(),
            !expected.is_empty(),
            "bool 判定が列挙と一致すること"
        );

        // wire を往復しても同じ列挙になること
        let mut buf = Vec::new();
        props
            .encode(&mut buf)
            .expect("正当なテスト入力の encode は成功する");
        let decoded = TrackProperties::decode(&buf).expect("encode 直後の decode は成功する");
        assert_eq!(
            decoded.unknown_mandatory_types(),
            expected,
            "wire 経由でも同じ列挙になること"
        );
        assert_eq!(
            decoded.has_unknown_mandatory(),
            !expected.is_empty(),
            "wire 経由でも bool 判定が列挙と一致すること"
        );
        Ok(())
    })?;
    Ok(())
}

// ─── Property のスコープ検証 ─────────────────────────────

/// Object 専用の既知 Property 型
///
/// draft-ietf-moq-transport-23 §16.8 (Properties) Table 15 の PRIOR_GROUP_ID_GAP /
/// PRIOR_OBJECT_ID_GAP と、Table 16 (draft-ietf-moq-loc-05 §6.1) の VIDEO_FRAME_MARKING /
/// AUDIO_LEVEL / TIMESTAMP。仕様の表を独立に書き下し、実装の表と突き合わせる。
const OBJECT_ONLY_TYPES: &[u64] = &[0x09, 0x0C, 0x10, 0x3C, 0x3E];

/// `TrackProperties::validate_scope` の判定が、仕様の表から独立に書いた期待値と一致すること
///
/// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties): スコープ外で受信した
/// 既知 Property は malformed track (§12.1 (Malformed Tracks) 条件 8)。未知の型はスコープを
/// 推測せず受理する (§16.8 の "Endpoints MUST ignore unknown Property types")。
#[test]
fn validate_scope_matches_independent_scope_table() -> noprop::TestResult {
    use shiguredo_moqt::error::MessageError;
    let malformed_seen = std::cell::Cell::new(false);
    let accepted_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let prop_type = noprop::sample_u64_in(ctx, 0..=0x50);
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type,
            value: if prop_type % 2 == 0 {
                TrackPropertyValue::VarInt(1)
            } else {
                TrackPropertyValue::Bytes(vec![0xAB])
            },
        });
        let result = props.validate_scope();
        if OBJECT_ONLY_TYPES.contains(&prop_type) {
            malformed_seen.set(true);
            assert_eq!(
                result,
                Err(MessageError::MalformedTrack(
                    "Object-only property in Track scope"
                )),
                "{prop_type:#x} は Object 専用 Property として検出すること"
            );
        } else {
            accepted_seen.set(true);
            assert_eq!(
                result,
                Ok(()),
                "{prop_type:#x} は Track scope で受理すること"
            );
        }
        Ok(())
    })?;
    assert!(
        malformed_seen.get(),
        "Object 専用型のケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        accepted_seen.get(),
        "受理される型のケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

// ─── Property の多重可否 ─────────────────────────────────────

/// この実装が意味を理解しない未知の Track Property Type
///
/// draft-ietf-moq-transport-23 §16.8 (Properties) Table 15 / Table 16 の既知型と
/// 0x4000-0x7FFF の Mandatory Track Property 範囲を含まない値だけを使う。未知の型は
/// 複数インスタンスを許す (§8.4 の "MUST allow multiple instances of the Property")。
/// この PBT は varint 値で生成するため偶数型だけを使う。
const UNKNOWN_TYPES: &[u64] = &[0x1000, 0x1002, 0x1004, 0x1006];

/// 偶数型 Property の KVP 生バイト列
fn even_kvp(prop_type: u64, value: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    shiguredo_moqt::varint::encode(prop_type, &mut bytes);
    shiguredo_moqt::varint::encode(value, &mut bytes);
    bytes
}

/// 未知の Type の同値・異値の複数インスタンスが lossless に往復し、多重違反にならないこと
///
/// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) は、理解できない Property に
/// ついて "MUST allow multiple instances of the Property" と定める。既知の単一 Property が
/// 混ざっても、それぞれ 1 インスタンスなら多重違反にならない。
#[test]
fn unknown_duplicate_types_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        // 未知の型を高々 3 種類、それぞれ 1-3 インスタンス (同値と異値を混ぜる)
        let type_count = noprop::sample_usize_in(ctx, 1..=3);
        let mut entries = Vec::new();
        for index in 0..type_count {
            let prop_type = UNKNOWN_TYPES[index % UNKNOWN_TYPES.len()];
            let instance_count = noprop::sample_usize_in(ctx, 1..=3);
            let shared = noprop::sample_u64_in(ctx, 0..=1_000);
            for _ in 0..instance_count {
                let value = match noprop::sample_weighted_index(ctx, &[1, 1]) {
                    0 => shared,
                    _ => noprop::sample_u64_in(ctx, 0..=1_000),
                };
                entries.push(TrackProperty {
                    prop_type,
                    value: TrackPropertyValue::VarInt(value),
                });
            }
        }
        // 昇順に並べる (encode は型の昇順に安定ソートする)
        entries.sort_by_key(|p| p.prop_type);
        let mut props = TrackProperties::new();
        for entry in entries {
            props.push(entry);
        }
        let mut buf = Vec::new();
        props
            .encode(&mut buf)
            .expect("未知 Type の重複は encode できること");
        let decoded = TrackProperties::decode(&buf).expect("未知 Type の重複は decode できること");
        assert_eq!(
            decoded, props,
            "未知 Type の同値・異値の複数インスタンスが往復すること"
        );
        assert_eq!(
            decoded.validate(),
            Ok(()),
            "未知 Type の重複は多重違反にならないこと"
        );
        assert_eq!(
            decoded.validate_multiplicity(),
            Ok(()),
            "未知 Type の重複は多重検証で受理されること"
        );
        Ok(())
    })?;
    Ok(())
}

/// 単一に限られる既知 Property の重複が malformed Track として報告されること
///
/// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1 (Malformed Tracks)
/// 条件 9。構造 decode は重複を保持し、`validate_multiplicity` が検出する。ローカル入力の
/// `encode` は `ProtocolViolation` で拒否する。
#[test]
fn known_duplicate_property_is_malformed() -> noprop::TestResult {
    use shiguredo_moqt::error::MessageError;
    const SINGLE_TYPES: &[u64] = &[0x02, 0x04, 0x06, 0x08, 0x0E, 0x22, 0x30];
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        let prop_type = noprop::sample_choice(ctx, SINGLE_TYPES);
        // 既知型の値域をすべて満たす値にする (0x22 は 1/2、0x30 は 0/1、0x0E は 0-255)
        let value = 1u64;
        // 同一型を delta=0 で 2 回繰り返す wire 表現を手組みする
        // (ローカル入力の encode は重複を拒否するため、wire 入力を作れない)
        let mut bytes = even_kvp(prop_type, value);
        shiguredo_moqt::varint::encode(0, &mut bytes);
        shiguredo_moqt::varint::encode(value, &mut bytes);
        let decoded =
            TrackProperties::decode(&bytes).expect("重複した既知型は構造 decode で保持すること");
        assert_eq!(decoded.len(), 2, "重複したインスタンスを保持すること");
        assert_eq!(
            decoded.validate_multiplicity(),
            Err(MessageError::MalformedTrack("duplicate track property")),
            "{prop_type:#x} の重複を malformed Track とすること"
        );

        let mut local = TrackProperties::new();
        for _ in 0..2 {
            local.push(TrackProperty {
                prop_type,
                value: TrackPropertyValue::VarInt(value),
            });
        }
        let mut buf = Vec::new();
        assert_eq!(
            local.encode(&mut buf),
            Err(MessageError::ProtocolViolation("duplicate track property")),
            "{prop_type:#x} の重複 encode を入力検証エラーにすること"
        );
        Ok(())
    })?;
    Ok(())
}
