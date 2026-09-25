//! IEEE 754 の半精度 (binary16) / 単精度 (binary32) と倍精度 (binary64) の変換を行うモジュール
//!
//! Rust の `f16` は未安定なため、半精度浮動小数点はビット列 ([`u16`]) として扱い、
//! 自前で変換する。変換結果はプラットフォームに依存しない。
//! RFC 8949 Appendix D および IEEE 754-2019 を参照。

/// 倍精度浮動小数点の仮数部 (52 ビット) を取り出すマスク
const MANTISSA_MASK: u64 = 0x000f_ffff_ffff_ffff;

/// 半精度浮動小数点のビット列を倍精度浮動小数点に変換する
///
/// 変換は正確で、値が変わることはない。NaN は仮数部のペイロードを
/// 上位ビットから引き継ぐ。
pub(crate) fn half_to_f64(bits: u16) -> f64 {
    let sign = u64::from(bits >> 15) << 63;
    let exponent = u64::from((bits >> 10) & 0x1f);
    let fraction = u64::from(bits & 0x3ff);

    let out = if exponent == 0 {
        if fraction == 0 {
            // ±0
            sign
        } else {
            // サブノーマル: fraction * 2^-24 を正規化する
            let msb = 63 - fraction.leading_zeros();
            let mantissa = (fraction << (52 - msb)) & MANTISSA_MASK;
            let exponent = 1023 + u64::from(msb) - 24;
            sign | (exponent << 52) | mantissa
        }
    } else if exponent == 31 {
        if fraction == 0 {
            // ±Infinity
            sign | 0x7ff0_0000_0000_0000
        } else {
            // NaN: 仮数部のペイロードを 42 ビット左シフトして引き継ぐ
            sign | 0x7ff0_0000_0000_0000 | (fraction << 42)
        }
    } else {
        // 正規化数
        sign | ((exponent + (1023 - 15)) << 52) | (fraction << 42)
    };
    f64::from_bits(out)
}

/// 倍精度浮動小数点を最近接偶数丸めで半精度浮動小数点のビット列に変換する
///
/// オーバーフローは ±Infinity、アンダーフローは ±0 になる。
/// NaN は符号を保ったまま quiet NaN に変換する (ペイロードは保持しない)。
pub(crate) fn f64_to_half_bits(value: f64) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 63) as u16) << 15;
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & MANTISSA_MASK;

    if exponent == 0x7ff {
        if fraction == 0 {
            // ±Infinity
            return sign | 0x7c00;
        }
        // NaN
        return sign | 0x7e00;
    }

    let unbiased = exponent - 1023;
    if unbiased > 15 {
        // 半精度の最大値を超えるため ±Infinity に丸められる
        return sign | 0x7c00;
    }

    if unbiased >= -14 {
        // 正規化数: 仮数部 52 ビットを 10 ビットに丸める
        let mut fraction_field = (fraction >> 42) as u16;
        let round_bit = ((fraction >> 41) & 1) as u16;
        let sticky = fraction & ((1u64 << 41) - 1) != 0;
        let mut exponent_field = (unbiased + 15) as u16;
        if round_bit == 1 && (sticky || fraction_field & 1 == 1) {
            fraction_field += 1;
        }
        if fraction_field == 0x400 {
            // 仮数部の繰り上がり
            fraction_field = 0;
            exponent_field += 1;
            if exponent_field == 31 {
                return sign | 0x7c00;
            }
        }
        return sign | (exponent_field << 10) | fraction_field;
    }

    // サブノーマル / ゼロ: 仮数部を (28 - unbiased) ビット右シフトして丸める
    let shift = 28 - unbiased;
    if shift >= 64 {
        // 2^-38 未満は 0 に丸められる
        return sign;
    }
    let shift = shift as u32;
    let mantissa = fraction | (1u64 << 52);
    let mut fraction_field = (mantissa >> shift) as u16;
    let round_bit = ((mantissa >> (shift - 1)) & 1) as u16;
    let sticky = mantissa & ((1u64 << (shift - 1)) - 1) != 0;
    if round_bit == 1 && (sticky || fraction_field & 1 == 1) {
        fraction_field += 1;
    }
    sign | fraction_field
}

/// 倍精度浮動小数点が半精度浮動小数点で正確に表現できる場合にそのビット列を返す
///
/// 丸めた結果を再度展開して元のビット列と一致する場合だけ `Some` を返す。
/// `-0.0` と `0.0` はビット列が異なるため区別される。
pub(crate) fn f64_to_half_exact(value: f64) -> Option<u16> {
    let bits = f64_to_half_bits(value);
    if half_to_f64(bits).to_bits() == value.to_bits() {
        Some(bits)
    } else {
        None
    }
}

/// 単精度浮動小数点のビット列を倍精度浮動小数点に変換する
///
/// 変換は正確で、値が変わることはない。NaN は仮数部のペイロードを
/// 上位ビットから引き継ぐ。
pub(crate) fn f32_to_f64(bits: u32) -> f64 {
    let sign = u64::from(bits >> 31) << 63;
    let exponent = u64::from((bits >> 23) & 0xff);
    let fraction = u64::from(bits & 0x7f_ffff);

    let out = if exponent == 0 {
        if fraction == 0 {
            // ±0
            sign
        } else {
            // サブノーマル: fraction * 2^-149 を正規化する
            let msb = 63 - fraction.leading_zeros();
            let mantissa = (fraction << (52 - msb)) & MANTISSA_MASK;
            let exponent = 1023 + u64::from(msb) - 149;
            sign | (exponent << 52) | mantissa
        }
    } else if exponent == 0xff {
        if fraction == 0 {
            // ±Infinity
            sign | 0x7ff0_0000_0000_0000
        } else {
            // NaN: 仮数部のペイロードを 29 ビット左シフトして引き継ぐ
            sign | 0x7ff0_0000_0000_0000 | (fraction << 29)
        }
    } else {
        // 正規化数
        sign | ((exponent + (1023 - 127)) << 52) | (fraction << 29)
    };
    f64::from_bits(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_to_f64_matches_rfc8949_appendix_a() {
        // RFC 8949 Appendix A の半精度の例
        assert_eq!(half_to_f64(0x0000), 0.0);
        assert_eq!(half_to_f64(0x8000).to_bits(), (-0.0f64).to_bits());
        assert_eq!(half_to_f64(0x3c00), 1.0);
        assert_eq!(half_to_f64(0x3e00), 1.5);
        assert_eq!(half_to_f64(0x7bff), 65504.0);
        assert_eq!(half_to_f64(0x0001), 5.960464477539063e-8);
        assert_eq!(half_to_f64(0x0400), 0.00006103515625);
        assert_eq!(half_to_f64(0xc400), -4.0);
        assert_eq!(half_to_f64(0x7c00), f64::INFINITY);
        assert_eq!(half_to_f64(0xfc00), f64::NEG_INFINITY);
        assert!(half_to_f64(0x7e00).is_nan());
    }

    #[test]
    fn half_all_non_nan_patterns_round_trip() {
        // NaN 以外の全てのビット列で、半精度 → 倍精度 → 半精度の変換が一致すること
        for bits in 0..=u16::MAX {
            let value = half_to_f64(bits);
            if value.is_nan() {
                continue;
            }
            assert_eq!(
                f64_to_half_exact(value),
                Some(bits),
                "bits = {bits:#06x} で往復変換が一致しない"
            );
        }
    }

    #[test]
    fn f64_to_half_bits_rounds_to_nearest_even() {
        // 2^-25 はちょうど 0 と 2^-24 の中間なので、偶数側の 0 に丸められる
        assert_eq!(f64_to_half_bits(2.0f64.powi(-25)), 0x0000);
        // 2^-25 の 1.5 倍は 2^-24 に丸められる
        assert_eq!(f64_to_half_bits(1.5 * 2.0f64.powi(-25)), 0x0001);
        // 2^-24 ちょうど
        assert_eq!(f64_to_half_bits(2.0f64.powi(-24)), 0x0001);
        // 最大値とオーバーフロー境界
        assert_eq!(f64_to_half_bits(65504.0), 0x7bff);
        assert_eq!(f64_to_half_bits(65519.996), 0x7bff);
        // 65520.0 は 65504 と 65536 (半精度では Infinity) の中間なので、
        // 仮数部が偶数になる Infinity 側に丸められる
        assert_eq!(f64_to_half_bits(65520.0), 0x7c00);
        assert_eq!(f64_to_half_bits(65520.5), 0x7c00);
        assert_eq!(f64_to_half_bits(f64::INFINITY), 0x7c00);
        assert_eq!(f64_to_half_bits(f64::NEG_INFINITY), 0xfc00);
        // NaN は符号を保ったまま quiet NaN になる
        assert_eq!(f64_to_half_bits(f64::NAN), 0x7e00);
        assert_eq!(
            f64_to_half_bits(f64::from_bits(0xfff8_0000_0000_0000)),
            0xfe00
        );
        // アンダーフロー
        assert_eq!(f64_to_half_bits(0.0), 0x0000);
        assert_eq!(f64_to_half_bits(-0.0), 0x8000);
        assert_eq!(f64_to_half_bits(1.0e-10), 0x0000);
    }

    #[test]
    fn f64_to_half_exact_rejects_inexact_values() {
        // 倍精度でしか表現できない値
        assert_eq!(f64_to_half_exact(1.1), None);
        assert_eq!(f64_to_half_exact(100000.0), None);
        // 半精度で正確に表現できる値
        assert_eq!(f64_to_half_exact(1.5), Some(0x3e00));
        assert_eq!(f64_to_half_exact(5.5), Some(0x4580));
        assert_eq!(f64_to_half_exact(-0.0), Some(0x8000));
    }

    #[test]
    fn f32_to_f64_matches_reference_conversion() {
        // 通常値・ゼロ・無限大・サブノーマル・NaN が f32 からの `as f64` と一致すること
        for bits in [
            0x0000_0000u32,
            0x8000_0000,
            0x3f80_0000,
            0x7f7f_ffff,
            0x0000_0001,
            0x007f_ffff,
            0x7f80_0000,
            0xff80_0000,
            0x7fc0_0000,
        ] {
            let value = f32::from_bits(bits);
            let expected = f64::from(value);
            if value.is_nan() {
                assert!(f32_to_f64(bits).is_nan());
            } else {
                assert_eq!(
                    f32_to_f64(bits).to_bits(),
                    expected.to_bits(),
                    "bits = {bits:#010x}"
                );
            }
        }
    }
}
