//! PBT の共通ヘルパー
//!
//! 任意の CBOR データ項目 ([`Value`]) を生成する `sample_value` を提供する。
//! `sample_value` は値として妥当な CBOR しか生成しないため、ケース拒否は行わない。

use noprop::{Ratio, TestCaseContext};
use shiguredo_moqt::cbor::value::Value;

/// 生成する [`Value`] の入れ子の最大の深さ
const MAX_DEPTH: usize = 4;

/// 分岐の重み
///
/// 0: 符号なし整数, 1: 負の整数, 2: バイト文字列, 3: テキスト文字列,
/// 4: 配列, 5: マップ, 6: タグ, 7: simple value, 8: 真偽値, 9: null,
/// 10: undefined, 11: 浮動小数点値, 12: 入れ子のコンテナを強制する配列
const WEIGHTS: [u32; 13] = [4, 3, 3, 3, 3, 3, 2, 2, 2, 1, 1, 4, 2];

/// 任意の CBOR データ項目を生成する
///
/// `depth` は現在の入れ子の深さで、[`MAX_DEPTH`] に達するとコンテナの分岐を
/// 無効化して再帰を止める。
pub fn sample_value(ctx: &mut TestCaseContext, depth: usize) -> Value {
    let mut weights = WEIGHTS;
    if depth >= MAX_DEPTH {
        // コンテナ系の分岐を無効化する
        weights[4] = 0;
        weights[5] = 0;
        weights[6] = 0;
        weights[12] = 0;
    }
    match noprop::sample_weighted_index(ctx, &weights) {
        0 => Value::Unsigned(sample_integer(ctx)),
        1 => Value::Negative(sample_integer(ctx)),
        2 => {
            let len = sample_len(ctx);
            Value::ByteString(noprop::sample_bytes_vec(ctx, len))
        }
        3 => {
            let len = sample_len(ctx);
            Value::TextString(noprop::sample_string(ctx, len))
        }
        4 => Value::Array(sample_items(ctx, depth + 1)),
        5 => Value::Map(sample_pairs(ctx, depth + 1)),
        6 => Value::Tag(sample_integer(ctx), Box::new(sample_value(ctx, depth + 1))),
        7 => Value::Simple(sample_simple(ctx)),
        8 => Value::Bool(noprop::sample_bool(ctx)),
        9 => Value::Null,
        10 => Value::Undefined,
        11 => Value::Float(sample_float(ctx)),
        12 => {
            // 入れ子のコンテナが必ず生成されるようにする分岐
            Value::Array(vec![
                Value::Array(sample_items(ctx, depth + 2)),
                Value::Map(sample_pairs(ctx, depth + 2)),
                Value::Tag(
                    sample_integer(ctx),
                    Box::new(Value::Array(sample_items(ctx, depth + 2))),
                ),
            ])
        }
        _ => unreachable!("分岐の重みは 13 個なので 0..=12 に収まる"),
    }
}

/// 整数の引数を生成する
///
/// 境界値 (引数のエンコード長が変わる値) と一様乱数を混ぜる。
fn sample_integer(ctx: &mut TestCaseContext) -> u64 {
    noprop::sample_with_boundaries(
        ctx,
        &[
            0,
            23,
            24,
            255,
            256,
            65535,
            65536,
            u32::MAX as u64,
            u32::MAX as u64 + 1,
            u64::MAX,
        ],
        Ratio::one_nth(4),
        |ctx| noprop::sample_u64(ctx),
    )
}

/// 文字列とコンテナの長さを生成する
///
/// 境界値 (長さのエンコード長が変わる値) と小さな一様乱数を混ぜる。
fn sample_len(ctx: &mut TestCaseContext) -> usize {
    noprop::sample_with_boundaries(ctx, &[0, 1, 23, 24, 255, 256], Ratio::one_nth(3), |ctx| {
        noprop::sample_usize_in(ctx, 0..=32)
    })
}

/// 配列の要素を生成する
fn sample_items(ctx: &mut TestCaseContext, depth: usize) -> Vec<Value> {
    let len = noprop::sample_usize_in(ctx, 0..=4);
    let mut items = Vec::new();
    for _ in 0..len {
        items.push(sample_value(ctx, depth));
    }
    items
}

/// マップのエントリを生成する
fn sample_pairs(ctx: &mut TestCaseContext, depth: usize) -> Vec<(Value, Value)> {
    let len = noprop::sample_usize_in(ctx, 0..=4);
    let mut pairs = Vec::new();
    for _ in 0..len {
        pairs.push((sample_value(ctx, depth), sample_value(ctx, depth)));
    }
    pairs
}

/// 割り当てられていない simple value を生成する
///
/// 20..=23 は真偽値・null・undefined に対応し、24..=31 は予約されているため、
/// 0..=19 と 32..=255 だけを生成する。
fn sample_simple(ctx: &mut TestCaseContext) -> u8 {
    noprop::sample_with_boundaries(ctx, &[0, 19, 32, 255], Ratio::one_nth(3), |ctx| {
        if noprop::sample_bool(ctx) {
            noprop::sample_u64_in(ctx, 0..=19) as u8
        } else {
            noprop::sample_u64_in(ctx, 32..=255) as u8
        }
    })
}

/// 浮動小数点値を生成する
///
/// 特殊値 (符号付きゼロ・無限大・NaN) と、ビット列全体からの値と、
/// 有限値の一様乱数を混ぜる。
fn sample_float(ctx: &mut TestCaseContext) -> f64 {
    if noprop::sample_ratio(ctx, Ratio::one_nth(4)) {
        noprop::sample_choice(
            ctx,
            &[
                0.0,
                -0.0,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NAN,
                1.0,
                -1.0,
                65504.0,
                5.960464477539063e-8,
                1.0e300,
            ],
        )
    } else if noprop::sample_ratio(ctx, Ratio::one_nth(4)) {
        // NaN のペイロード違いや非正規化数も含めて網羅する
        f64::from_bits(noprop::sample_u64(ctx))
    } else {
        noprop::sample_f64(ctx)
    }
}
