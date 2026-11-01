//! 実際の未着時に出力側で生成する、最大 100 ms の続き音
//!
//! 音声が欠落したとき、周期が確かな音では既存の周期補間 ([`conceal`]) を使い、それ以外は
//! 線形予測フィルターへ非周期の励振を入れて続きの音を合成する。NetEq Expand の有声 / 無声の
//! 分離を参考にしているが、上流とビット一致する移植ではない (上流の処理は変更され得る)。
//!
//! 生成する長さは [`AUDIO_EXPANSION_SECONDS`] (100 ms) で、補間の長さの上限として
//! moqt-js が使う値と同じである。生成した音は必ず最新の実音の RMS とピークを超えないよう
//! に抑える (過去の大きな発声を補間で持ち上げない)。時刻や出力デバイスには触れない。

use alloc::vec;
use alloc::vec::Vec;

use crate::playout::predictor::{
    AUDIO_PREDICTOR_HISTORY_SIZE, AUDIO_PREDICTOR_ORDER, generate_unvoiced,
};
use crate::playout::stretch::{conceal, round_positive, sqrt};

/// 生成する続き音の長さ (秒)
///
/// moqt-js の `AUDIO_PLAYOUT_MAX_CONCEAL_SECONDS` (補間する隙間の上限) と同じ 100 ms で
/// ある。
pub const AUDIO_EXPANSION_SECONDS: f64 = 0.1;

/// サンプルレートの下限 (Hz)
///
/// これ未満のレートは補間の対応範囲外であり、素の PCM をそのまま鳴らす。
const AUDIO_EXPANSION_MIN_SAMPLE_RATE: u32 = 8_000;

/// サンプルレートの上限 (Hz)
///
/// これより高いレートも補間の対応範囲外であり、素の PCM をそのまま鳴らす。
const AUDIO_EXPANSION_MAX_SAMPLE_RATE: u32 = 96_000;

/// 続き音を生成できなかった理由
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpandAudioError {
    /// サンプルレートが 0、チャンネルの長さが揃っていない、または有限でないサンプルがある
    InvalidInput,
}

impl core::fmt::Display for ExpandAudioError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidInput => write!(
                f,
                "expand audio: expected finite audio channels with equal lengths and a positive sample rate"
            ),
        }
    }
}

impl core::error::Error for ExpandAudioError {}

/// 信号の RMS とピークを求める
///
/// コールバックや一時配列を使わず、元の順序で集計する。空の入力は (0, 0) を返す。
fn signal_level(samples: &[f32]) -> (f64, f64) {
    if samples.is_empty() {
        return (0.0, 0.0);
    }
    let mut energy = 0.0f64;
    let mut peak = 0.0f64;
    for sample in samples {
        let value = f64::from(*sample);
        energy += value * value;
        peak = peak.max(value.abs());
    }
    (sqrt(energy / samples.len() as f64), peak)
}

/// 自己相関から、安定性を制約した線形予測係数と残差の大きさを求める
///
/// Levinson-Durbin 法で 12 次の係数を求める。反射係数は ±0.98 に制限し、フィルターが発散
/// しないようにする。戻り値は (係数, 励振の大きさ, 入力の RMS) である。
fn predictor(samples: &[f32]) -> ([f64; AUDIO_PREDICTOR_ORDER], f64, f64) {
    let mut correlations = [0.0f64; AUDIO_PREDICTOR_ORDER + 1];
    for lag in 0..=AUDIO_PREDICTOR_ORDER {
        let mut sum = 0.0f64;
        for index in lag..samples.len() {
            sum += f64::from(samples[index]) * f64::from(samples[index - lag]);
        }
        correlations[lag] = sum;
    }
    let mut error = correlations[0] * 1.0001;
    let rms = sqrt(correlations[0] / samples.len() as f64);
    let mut coefficients = [0.0f64; AUDIO_PREDICTOR_ORDER];
    for order in 0..AUDIO_PREDICTOR_ORDER {
        if error.is_nan() || error <= 1e-20 {
            break;
        }
        let mut residual = correlations[order + 1];
        for index in 0..order {
            residual -= coefficients[index] * correlations[order - index];
        }
        let reflection = (residual / error).clamp(-0.98, 0.98);
        let previous = coefficients;
        for index in 0..order {
            coefficients[index] = previous[index] - reflection * previous[order - index - 1];
        }
        coefficients[order] = reflection;
        error *= 1.0 - reflection * reflection;
    }
    (
        coefficients,
        sqrt(error.max(0.0) / samples.len() as f64),
        rms,
    )
}

/// 有限の倍精度値を、JavaScript のビット演算と同じ 32 bit の整数へ変換する
///
/// moqt-js は `state ^ Math.round(Math.abs(value) * 1e7)` を計算する。ビット演算は値を
/// ToInt32 (0 方向へ切り捨てて 2^32 で割った余り) に直すため、同じ変換をここでも行う。
/// 0 と NaN、負の値は 0 にする (呼び出し側は絶対値を渡す)。
fn to_uint32(value: f64) -> u32 {
    if value.is_nan() || value <= 0.0 {
        return 0;
    }
    let rounded = round_positive(value) as f64;
    // 2^32 で割った余りを取ってから整数へ直す (値が 2^32 以上でも折り返す)
    (rounded % 4_294_967_296.0) as u32
}

/// 無声成分を周期的に繰り返さず、過去のスペクトルを使って合成する
///
/// `samples` から線形予測係数を求め、`current` (最新の実音) の RMS とピークに合わせて
/// 励振の大きさを決める。生成した音は音量を増やさないよう正規化し、長い欠落ほど徐々に
/// 減衰させる。
fn unvoiced(samples: &[f32], frames: usize, current: &[f32]) -> Vec<f32> {
    let (coefficients, reference_innovation, reference_rms) = predictor(samples);
    let (rms, limit) = signal_level(current);
    let innovation = if reference_rms > 0.0 {
        (reference_innovation * rms) / reference_rms
    } else {
        0.0
    };
    if rms < 1e-8 {
        return vec![0.0f32; frames];
    }
    // 予測の積和の順番を保ち、各サンプルでの履歴のコピーだけを省く (末尾が最新になるよう並べる)
    let mut history = [0.0f64; AUDIO_PREDICTOR_HISTORY_SIZE];
    for index in 0..AUDIO_PREDICTOR_ORDER {
        history[AUDIO_PREDICTOR_HISTORY_SIZE - 1 - index] = samples
            .len()
            .checked_sub(1 + index)
            .map_or(0.0, |position| f64::from(samples[position]));
    }
    // 同一チャンネルは同じ励振、逆相チャンネルは逆の励振にして定位を変えない
    let polarity = samples
        .iter()
        .find(|value| value.abs() > 1e-8)
        .map_or(1.0, |value| if *value < 0.0 { -1.0 } else { 1.0 });
    // 励振の乱数の種は、入力の絶対値の並びから決める (同じ音には同じ励振)
    let mut state = 2_166_136_261u32;
    for value in samples {
        state = (state ^ to_uint32(f64::from(value.abs()) * 1e7)).wrapping_mul(16_777_619);
    }
    let mut result = generate_unvoiced(
        &coefficients,
        &history,
        frames,
        state,
        innovation,
        limit,
        polarity,
    )
    .expect("100 ms の長さはカーネルの上限内である");
    let (generated_rms, _) = signal_level(&result);
    // フィルターの有限区間の過渡で音量を増やさない。長い欠落ほど徐々に減衰する
    let scale = if generated_rms > rms {
        rms / generated_rms
    } else {
        1.0
    };
    for (index, sample) in result.iter_mut().enumerate() {
        let decay = 1.0 - (0.5 * (index as f64 + 1.0)) / frames as f64;
        *sample = (f64::from(*sample) * scale * decay) as f32;
    }
    result
}

/// 解析に使う過去が大きな音でも、最新の実音より音量やピークを増やさない
///
/// `expansion` は生成した続き音、`channels` は最新の実音である。最新の実音の RMS とピークの
/// 両方を超えない倍率を掛ける。超えていなければ生成した音をそのまま返す。
fn constrain_level(expansion: Vec<Vec<f32>>, channels: &[&[f32]]) -> Vec<Vec<f32>> {
    let mut output = Vec::new();
    for (index, samples) in expansion.into_iter().enumerate() {
        let Some(current) = channels.get(index) else {
            output.push(vec![0.0f32; samples.len()]);
            continue;
        };
        let (current_rms, current_peak) = signal_level(current);
        let (expanded_rms, expanded_peak) = signal_level(&samples);
        let scale = 1.0f64
            .min(if expanded_rms > 0.0 {
                current_rms / expanded_rms
            } else {
                1.0
            })
            .min(if expanded_peak > 0.0 {
                current_peak / expanded_peak
            } else {
                1.0
            });
        if scale >= 1.0 {
            output.push(samples);
            continue;
        }
        let scaled = samples
            .iter()
            .map(|sample| (f64::from(*sample) * scale) as f32)
            .collect();
        output.push(scaled);
    }
    output
}

/// 周期補間で続きの音を作る (作れないときは None)
///
/// 補間の長さは [`AUDIO_EXPANSION_SECONDS`] であり、末尾の振幅は 0.5 まで下げる
/// (moqt-js の `concealmentEndGain` の 100 ms の値)。`channels` はすべて同じ長さであること。
fn periodic_expansion(channels: &[&[f32]], sample_rate: u32) -> Option<Vec<Vec<f32>>> {
    let target = round_positive(f64::from(sample_rate) * AUDIO_EXPANSION_SECONDS);
    if target == 0 {
        return None;
    }
    let mut output = vec![vec![0.0f32; target]; channels.len()];
    let mut destinations: Vec<&mut [f32]> = output
        .iter_mut()
        .map(|channel| channel.as_mut_slice())
        .collect();
    let conceal_us = (AUDIO_EXPANSION_SECONDS * 1_000_000.0) as i64;
    let filled = conceal(channels, &mut destinations, sample_rate, conceal_us);
    (filled > 0).then_some(output)
}

/// 実際の未着時に出力側で生成する、最大 100 ms の続き音を作る
///
/// 周期が確かな音では [`conceal`] の周期補間を使い、それ以外は `unvoiced` の線形予測で
/// 合成する。`history` は直前の音の履歴であり、今回の音より長いときは解析に使う
/// (省略したときは `channels` を履歴として使う)。生成した音は最新の実音の RMS とピークを
/// 超えないように抑える。
///
/// # 戻り値
///
/// 生成した続き音 (チャンネルごと、長さは `sample_rate * 0.1` を四捨五入したサンプル数)。
/// 補間を作らない場合は空になる。空になるのは、チャンネルが無いか空のとき、サンプルレートが
/// 8 kHz 未満か 96 kHz を超えるとき、今回の音が 10 ms 未満のとき、補間の対象が無いとき。
///
/// # エラー
///
/// サンプルレートが 0、チャンネルの長さが揃っていない、有限でないサンプルがあるときは
/// [`ExpandAudioError::InvalidInput`] を返す。
pub fn expand_audio(
    channels: &[&[f32]],
    sample_rate: u32,
    history: Option<&[&[f32]]>,
) -> Result<Vec<Vec<f32>>, ExpandAudioError> {
    if channels.is_empty() || channels[0].is_empty() {
        return Ok(Vec::new());
    }
    if sample_rate == 0
        || channels.iter().any(|channel| {
            channel.len() != channels[0].len() || channel.iter().any(|sample| !sample.is_finite())
        })
    {
        return Err(ExpandAudioError::InvalidInput);
    }
    // 実音の再生は補間の対応範囲で制限しない。範囲外は従来どおり素の PCM を鳴らす
    if !(AUDIO_EXPANSION_MIN_SAMPLE_RATE..=AUDIO_EXPANSION_MAX_SAMPLE_RATE).contains(&sample_rate) {
        return Ok(Vec::new());
    }
    if (channels[0].len() as f64) < f64::from(sample_rate) * 0.01 {
        return Ok(Vec::new());
    }
    let mut periodic = periodic_expansion(channels, sample_rate);
    // 解析には長い方の履歴を使う。今回の音が短いときも過去のスペクトルを使える
    let reference = history
        .filter(|history| {
            !history.is_empty()
                && history.len() == channels.len()
                && history[0].len() > channels[0].len()
        })
        .unwrap_or(channels);
    // 同じ配列を指しているかは先頭要素のアドレスで判定する (JavaScript の配列の同一性と同じ)
    let reference_is_history = if reference.is_empty() {
        false
    } else {
        !core::ptr::eq(reference[0].as_ptr(), channels[0].as_ptr())
    };
    if periodic.is_none() && reference_is_history {
        periodic = periodic_expansion(reference, sample_rate);
    }
    if let Some(expansion) = periodic {
        return Ok(constrain_level(expansion, channels));
    }
    let frames = round_positive(f64::from(sample_rate) * AUDIO_EXPANSION_SECONDS);
    let expansion: Vec<Vec<f32>> = reference
        .iter()
        .enumerate()
        .map(|(index, channel)| {
            let current = channels.get(index).map_or(*channel, |current| *current);
            unvoiced(channel, frames, current)
        })
        .collect();
    Ok(constrain_level(expansion, channels))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 0.5 で埋めた音声を作る
    fn constant(length: usize, value: f32) -> Vec<f32> {
        vec![value; length]
    }

    #[test]
    fn signal_level_of_a_constant_signal_is_its_absolute_value() {
        let (rms, peak) = signal_level(&constant(100, -0.5));
        assert!((rms - 0.5).abs() < 1e-6);
        assert!((peak - 0.5).abs() < 1e-6);
        assert_eq!(signal_level(&[]), (0.0, 0.0));
    }

    #[test]
    fn to_uint32_wraps_like_javascript_bit_operations() {
        // 2^32 以上は折り返す (JavaScript の ToInt32 と同じ)
        assert_eq!(to_uint32(4_294_967_296.0), 0);
        assert_eq!(to_uint32(4_294_967_297.0), 1);
        // 負の値と 0 は 0 にする (呼び出し側は絶対値を渡す)
        assert_eq!(to_uint32(0.0), 0);
        assert_eq!(to_uint32(-1.0), 0);
        // 端数は四捨五入する
        assert_eq!(to_uint32(1.5), 2);
    }
}
