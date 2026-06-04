//! pbt 共通サンプラー
//!
//! 複数の prop_*.rs から使う生成ヘルパーを置く。各テストターゲットは
//! `use pbt::common::...;` で取り込む (pbt クレートの lib ターゲット経由)。
//! `#[path] mod` による取り込みはターゲット毎コンパイルで未使用ヘルパーが
//! dead_code になるため使わない。

use shiguredo_moqt::message::common::TrackNamespace;
use shiguredo_moqt::message_parameter::{
    PARAM_OBJECT_PROPERTY_FILTER, PARAM_OBJECTID_FILTER, PARAM_PRIORITY_FILTER,
    PARAM_SUBGROUP_FILTER, PARAM_TRACK_PROPERTY_FILTER,
};
use shiguredo_moqt::parameter::{SetupOption, SetupOptionValue, SetupOptions};
use shiguredo_moqt::track_properties::{
    PROP_DEFAULT_PUBLISHER_GROUP_ORDER, PROP_DEFAULT_PUBLISHER_PRIORITY, PROP_DYNAMIC_GROUPS,
    PROP_IMMUTABLE_PROPERTIES, PROP_MAX_CACHE_DURATION, TrackProperties, TrackProperty,
    TrackPropertyValue,
};

/// PBT シードの環境変数名 (全 property テストで共通)
///
/// 文字列リテラルの散在はタイポが時刻シードへの静かなフォールバックになり
/// 再現性喪失でしか表面化しないため、定数で 1 箇所管理する。
pub const PBT_SEED_ENV_VAR: &str = "PBT_SEED";

/// 環境変数または時刻からシードを得て `Runner` を作る
pub fn test_runner() -> noprop::TestResult<noprop::Runner> {
    let seed = noprop::seed_from_env_or_time(PBT_SEED_ENV_VAR)?;
    Ok(noprop::Runner::new(seed))
}

/// 空でないバイト列を生成する
pub fn sample_nonempty_bytes(ctx: &mut noprop::TestCaseContext, max_len: usize) -> Vec<u8> {
    let len = noprop::sample_usize_in(ctx, 1..=max_len);
    noprop::sample_bytes_vec(ctx, len)
}

/// バイト列を生成する (空を許す)
pub fn sample_bytes(ctx: &mut noprop::TestCaseContext, max_len: usize) -> Vec<u8> {
    let len = noprop::sample_usize_in(ctx, 0..=max_len);
    noprop::sample_bytes_vec(ctx, len)
}

/// 名前空間プレフィックスを生成する
pub fn sample_namespace_prefix(ctx: &mut noprop::TestCaseContext) -> TrackNamespace {
    let field_count = noprop::sample_usize_in(ctx, 0..=4);
    let fields = (0..field_count)
        .map(|_| sample_nonempty_bytes(ctx, 20))
        .collect();
    TrackNamespace::new(fields).expect("テストフィクスチャの前提条件を満たす")
}

/// varint (vi64) フィールド用のサンプラー。
///
/// `sample_u64` の一様分布は 1〜数バイトの短い varint をほぼ生成しないため、
/// `src/varint.rs` の各バイト長分岐を確実に踏むよう、本実装の vi64 (7 bit グループの
/// leading-ones 形式) のバイト長境界に揃えた境界値と全域を混ぜて生成する。境界値は
/// `src/varint.rs` の THRESHOLD_n (1 バイト=2^7-1=127, 2 バイト=2^14-1, 3 バイト=2^21-1,
/// 4 バイト=2^28-1, 5 バイト=2^35-1, 6 バイト=2^42-1, 7 バイト=2^49-1 (7 バイト長は
/// draft-ietf-moq-transport-21 Appendix A.3 (Since draft-ietf-moq-transport-17) #1595 由来),
/// 8 バイト=2^56-1) に対応する。0 も含むため `ObjectDatagram` の
/// ZERO_OBJECT_ID (object_id == 0) 経路も踏める。9 バイト (64 bit 全域,
/// draft-ietf-moq-transport-21 §8.1 (Variable-Length Integers)) は
/// 境界値 `u64::MAX` と内部サンプラーの一様分布でカバーする。
/// 5〜7 バイト域は一様分布ではほぼ生成されない (2^-15 以下) ため境界値を明示する。
pub fn sample_varint(ctx: &mut noprop::TestCaseContext) -> u64 {
    noprop::sample_with_boundaries(
        ctx,
        &[
            0u64,
            127,                    // 1 バイト最大 (2^7-1)
            128,                    // 2 バイト最小
            16_383,                 // 2 バイト最大 (2^14-1)
            16_384,                 // 3 バイト最小
            2_097_151,              // 3 バイト最大 (2^21-1)
            2_097_152,              // 4 バイト最小
            268_435_455,            // 4 バイト最大 (2^28-1)
            268_435_456,            // 5 バイト最小
            34_359_738_367,         // 5 バイト最大 (2^35-1)
            34_359_738_368,         // 6 バイト最小
            4_398_046_511_103,      // 6 バイト最大 (2^42-1)
            4_398_046_511_104,      // 7 バイト最小
            562_949_953_421_311,    // 7 バイト最大 (2^49-1)
            562_949_953_421_312,    // 8 バイト最小
            72_057_594_037_927_935, // 8 バイト最大 (2^56-1)
            u64::MAX,               // 9 バイト最大
        ],
        noprop::Ratio::one_nth(4),
        |ctx| noprop::sample_u64(ctx),
    )
}

/// 小さな varint 値を生成する (メッセージ全体の肥大防止用)
pub fn sample_small_varint(ctx: &mut noprop::TestCaseContext) -> u64 {
    noprop::sample_u64_in(ctx, 0..1000)
}

/// reg-name ラベルに使える 1 バイトを生成する (draft-ietf-moq-transport-21 §9.1 (SETUP) の PATH / AUTHORITY)
pub fn sample_reg_name_char(ctx: &mut noprop::TestCaseContext) -> u8 {
    noprop::sample_choice(
        ctx,
        b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ-",
    )
}

pub fn sample_reg_name_label(ctx: &mut noprop::TestCaseContext, max_len: usize) -> Vec<u8> {
    let len = noprop::sample_usize_in(ctx, 1..=max_len);
    (0..len).map(|_| sample_reg_name_char(ctx)).collect()
}

pub fn sample_setup_authority(ctx: &mut noprop::TestCaseContext) -> Vec<u8> {
    let label_count = noprop::sample_usize_in(ctx, 1..=4);
    let mut iter = (0..label_count)
        .map(|_| sample_reg_name_label(ctx, 12))
        .collect::<Vec<_>>()
        .into_iter();
    let mut bytes = iter.next().unwrap_or_default();
    for label in iter {
        bytes.push(b'.');
        bytes.extend_from_slice(&label);
    }
    if noprop::sample_bool(ctx) {
        let port = noprop::sample_u16(ctx);
        bytes.push(b':');
        bytes.extend_from_slice(port.to_string().as_bytes());
    }
    bytes
}

/// RFC 3986 pchar + ":" + "@" の文字セット (path segment 用)
pub const PATH_CHARS: &[u8] =
    b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ-._~!$&'()*+,;=:@";
/// RFC 3986 pchar + "/" + "?" の文字セット (query component 用)
pub const QUERY_CHARS: &[u8] =
    b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ-._~!$&'()*+,;=:@/?";

pub fn sample_path_segment(ctx: &mut noprop::TestCaseContext, max_len: usize) -> Vec<u8> {
    let len = noprop::sample_usize_in(ctx, 0..=max_len);
    (0..len)
        .map(|_| noprop::sample_choice(ctx, PATH_CHARS))
        .collect()
}

pub fn sample_query_component(ctx: &mut noprop::TestCaseContext, max_len: usize) -> Vec<u8> {
    let len = noprop::sample_usize_in(ctx, 0..=max_len);
    (0..len)
        .map(|_| noprop::sample_choice(ctx, QUERY_CHARS))
        .collect()
}

pub fn sample_setup_path(ctx: &mut noprop::TestCaseContext) -> Vec<u8> {
    let segment_count = noprop::sample_usize_in(ctx, 0..=4);
    let mut bytes = Vec::new();
    for _ in 0..segment_count {
        bytes.push(b'/');
        bytes.extend_from_slice(&sample_path_segment(ctx, 12));
    }
    if noprop::sample_bool(ctx) {
        bytes.push(b'?');
        bytes.extend_from_slice(&sample_query_component(ctx, 20));
    }
    bytes
}

/// SETUP 系生成器の値域指定
///
/// 呼び出し側の意図で値域が異なるため、共通ロジックに値域だけを渡す。
/// - 全域版: parameter 単体の検証用。型番号・varint 値・バイト長を広く取る
/// - 縮小版: message 全体の検証用。エンコード済みメッセージの肥大を抑える
pub struct SetupSampling {
    /// 偶数型の上限 (排他的。option_type = sample(0..bound) * 2)
    pub even_type_bound: usize,
    /// 奇数型バイト値の最大長
    pub odd_bytes_max: usize,
    /// 偶数型 varint 値を small (0..1000) に抑えるか
    pub small_varint_values: bool,
    /// 生成件数の上限 (0..=count_max)
    pub count_max: usize,
}

/// SETUP 系生成の全域版 (parameter 単体の検証用)
pub const SETUP_SAMPLING_FULL: SetupSampling = SetupSampling {
    even_type_bound: 1000,
    odd_bytes_max: 100,
    small_varint_values: false,
    count_max: 10,
};

/// SETUP 系生成の縮小版 (message 全体の検証用。肥大防止)
pub const SETUP_SAMPLING_SMALL: SetupSampling = SetupSampling {
    even_type_bound: 100,
    odd_bytes_max: 50,
    small_varint_values: true,
    count_max: 5,
};

/// テスト用の任意 Setup Option を生成する
///
/// 偶数型は varint 値、奇数型はバイト列。
/// PATH (0x01) / AUTHORITY (0x05) は encode 時に URI バリデーションがあるため、
/// 生成側でも RFC 3986 の必要 subset を満たす値だけを作る。AUTHORIZATION_TOKEN (0x03) は
/// Token 構造なので除外する。
pub fn sample_setup_option_with(
    ctx: &mut noprop::TestCaseContext,
    sampling: &SetupSampling,
) -> SetupOption {
    match noprop::sample_weighted_index(ctx, &[2, 1, 1, 6]) {
        // 偶数型: varint 値
        0 => {
            let option_type = noprop::sample_usize_in(ctx, 0..sampling.even_type_bound) as u64 * 2;
            let v = if sampling.small_varint_values {
                sample_small_varint(ctx)
            } else {
                noprop::sample_u64(ctx)
            };
            SetupOption {
                option_type,
                value: SetupOptionValue::VarInt(v),
            }
        }
        // 奇数型 0x01 (PATH)
        1 => SetupOption {
            option_type: shiguredo_moqt::parameter::SETUP_OPTION_PATH,
            value: SetupOptionValue::Bytes(sample_setup_path(ctx)),
        },
        // 奇数型 0x05 (AUTHORITY)
        2 => SetupOption {
            option_type: shiguredo_moqt::parameter::SETUP_OPTION_AUTHORITY,
            value: SetupOptionValue::Bytes(sample_setup_authority(ctx)),
        },
        // その他の奇数型 (AUTHORIZATION_TOKEN (0x03) は除外)
        _ => {
            let t = noprop::sample_usize_in(ctx, 0..sampling.even_type_bound) as u64;
            // t == 1 (0x03) を除外する (1 を飛ばして 2.. に射影)
            let t = if t >= 1 { t + 1 } else { 0 };
            let option_type = t * 2 + 1;
            let len = noprop::sample_usize_in(ctx, 0..=sampling.odd_bytes_max);
            SetupOption {
                option_type,
                value: SetupOptionValue::Bytes(noprop::sample_bytes_vec(ctx, len)),
            }
        }
    }
}

/// 重複のない昇順 Setup Options リストを生成する
pub fn sample_setup_options_with(
    ctx: &mut noprop::TestCaseContext,
    sampling: &SetupSampling,
) -> SetupOptions {
    let count = noprop::sample_usize_in(ctx, 0..=sampling.count_max);
    let mut options = (0..count)
        .map(|_| sample_setup_option_with(ctx, sampling))
        .collect::<Vec<_>>();
    // 型番号の重複を除去しつつ昇順に並べる
    options.sort_by_key(|p| p.option_type);
    options.dedup_by_key(|p| p.option_type);
    let mut result = SetupOptions::new();
    for p in options {
        result.push(p);
    }
    result
}

/// IMMUTABLE_PROPERTIES (0x0B) を含まない Track Property を生成する。
/// IMMUTABLE の内側 KVP 列の生成にも使う (入れ子は禁止のため内側に 0x0B は入れない)。
pub fn sample_non_immutable_property(ctx: &mut noprop::TestCaseContext) -> TrackProperty {
    match noprop::sample_weighted_index(ctx, &[1, 1, 1, 1, 1, 1]) {
        // DEFAULT_PUBLISHER_PRIORITY (0x0E): 0-255
        0 => TrackProperty {
            prop_type: PROP_DEFAULT_PUBLISHER_PRIORITY,
            value: TrackPropertyValue::VarInt(noprop::sample_u64_in(ctx, 0..=255)),
        },
        // DEFAULT_PUBLISHER_GROUP_ORDER (0x22): 1 または 2
        1 => TrackProperty {
            prop_type: PROP_DEFAULT_PUBLISHER_GROUP_ORDER,
            value: TrackPropertyValue::VarInt(noprop::sample_choice(ctx, &[1u64, 2u64])),
        },
        // DYNAMIC_GROUPS (0x30): 0 または 1
        2 => TrackProperty {
            prop_type: PROP_DYNAMIC_GROUPS,
            value: TrackPropertyValue::VarInt(noprop::sample_choice(ctx, &[0u64, 1u64])),
        },
        // MAX_CACHE_DURATION (0x04): 値域制約なしの varint (draft-ietf-moq-transport-21 §10.3 (MAX CACHE DURATION))
        3 => TrackProperty {
            prop_type: PROP_MAX_CACHE_DURATION,
            value: TrackPropertyValue::VarInt(sample_small_varint(ctx)),
        },
        // その他の偶数型 (既知型 0x02 / 0x04 / 0x06 / 0x0E / 0x22 / 0x30 と重複しない)
        4 => TrackProperty {
            prop_type: noprop::sample_choice(ctx, &[0x08u64, 0x0Au64, 0x0Cu64]),
            value: TrackPropertyValue::VarInt(sample_small_varint(ctx)),
        },
        // 奇数型 (IMMUTABLE_PROPERTIES 0x0B は除外する。0x0B は専用の生成関数で生成する)
        _ => {
            let t = noprop::sample_usize_in(ctx, 0..100) as u64;
            // t == 5 (0x0B) を除外する (0..=99 のうち 5 を飛ばして 6..=100 に射影)
            let t = if t >= 5 { t + 1 } else { t };
            let prop_type = t * 2 + 1;
            let len = noprop::sample_usize_in(ctx, 0..=50);
            TrackProperty {
                prop_type,
                value: TrackPropertyValue::Bytes(noprop::sample_bytes_vec(ctx, len)),
            }
        }
    }
}

/// IMMUTABLE_PROPERTIES (0x0B) を生成する。内側は非 IMMUTABLE な正規の Track Property KVP 列。
pub fn sample_immutable_property(ctx: &mut noprop::TestCaseContext) -> TrackProperty {
    let inner_count = noprop::sample_usize_in(ctx, 0..=4);
    let mut inner_props = (0..inner_count)
        .map(|_| sample_non_immutable_property(ctx))
        .collect::<Vec<_>>();
    inner_props.sort_by_key(|p| p.prop_type);
    inner_props.dedup_by_key(|p| p.prop_type);
    let mut inner = TrackProperties::new();
    for p in inner_props {
        inner.push(p);
    }
    let mut inner_bytes = Vec::new();
    inner
        .encode(&mut inner_bytes)
        .expect("非 IMMUTABLE な内側 KVP 列は encode 可能");
    TrackProperty {
        prop_type: PROP_IMMUTABLE_PROPERTIES,
        value: TrackPropertyValue::Bytes(inner_bytes),
    }
}

/// Track Property を生成する (非 IMMUTABLE : IMMUTABLE = 4 : 1)
pub fn sample_track_property(ctx: &mut noprop::TestCaseContext) -> TrackProperty {
    match noprop::sample_weighted_index(ctx, &[4, 1]) {
        0 => sample_non_immutable_property(ctx),
        _ => sample_immutable_property(ctx),
    }
}

pub fn sample_track_properties(ctx: &mut noprop::TestCaseContext) -> TrackProperties {
    let count = noprop::sample_usize_in(ctx, 0..=8);
    let mut props = (0..count)
        .map(|_| sample_track_property(ctx))
        .collect::<Vec<_>>();
    props.sort_by_key(|p| p.prop_type);
    props.dedup_by_key(|p| p.prop_type);
    let mut result = TrackProperties::new();
    for p in props {
        result.push(p);
    }
    result
}

/// Range Filter パラメータ型 (0x25-0x29) の一覧
///
/// draft-ietf-moq-transport-21 §3.3.2 (Range Filters)。
/// `src/message_parameter.rs` の `is_range_filter_type` と同期して更新すること。
pub const RANGE_FILTER_TYPES: &[u64] = &[
    PARAM_SUBGROUP_FILTER,
    PARAM_OBJECTID_FILTER,
    PARAM_PRIORITY_FILTER,
    PARAM_OBJECT_PROPERTY_FILTER,
    PARAM_TRACK_PROPERTY_FILTER,
];

/// Range Filter パラメータ型 (0x25-0x29) かどうかを返す
///
/// draft-ietf-moq-transport-21 §3.3.2 (Range Filters)
pub fn is_range_filter_type(param_type: u64) -> bool {
    RANGE_FILTER_TYPES.contains(&param_type)
}

/// 0x28 / 0x29 のみ Property Type フィールドを持つ
pub fn has_property_type_field(param_type: u64) -> bool {
    matches!(
        param_type,
        PARAM_OBJECT_PROPERTY_FILTER | PARAM_TRACK_PROPERTY_FILTER
    )
}
