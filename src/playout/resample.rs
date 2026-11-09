//! 音声の出力レートへの変換 (ダウンサンプリングの帯域制限補間)
//!
//! 復号した PCM を別のサンプルレートの出力格子へ写す。同じレートとアップサンプリングは
//! 線形補間を使い、ダウンサンプリングは出力のナイキスト周波数を超える成分が折り返さない
//! よう、Blackman 窓付き sinc で帯域を制限する。フィルタの係数はレート比ごとに一度だけ
//! 作り、描画中に三角関数や配列の確保を行わない。
//!
//! 根拠: Julius O. Smith, Physical Audio Signal Processing,
//! "Windowed Sinc Interpolation"。係数の数と遮断周波数は仕様の要求値ではなく、本実装の
//! 品質と負荷の選択である。通過帯域と阻止帯域は実際の出力で検証する。
//!
//! moqt-js の `src/playout/audioResampleKernel.ts` と `audioRenderQueue.ts` のうち、
//! 補間の数値カーネルと 1 ブロックの変換を移植した。フレームをまたぐ連続した PCM の
//! キュー (`AudioRenderQueue`) は、出力時計のサンプル位置を持つ適用層が前提であり、
//! moqt-rs にはその層が無いため移植していない。呼び出し側は、前後のブロックを含めた
//! 連続した PCM を渡すこと。
//!
//! 時刻や出力デバイスには触れない。

use alloc::vec;
use alloc::vec::Vec;
use core::f64::consts::PI;

/// ダウンサンプリングのフィルタの半径 (出力サンプル数)
///
/// 入出力のレート比が変わっても、参照する時間の幅を一定に保つ。
pub const RESAMPLE_OUTPUT_RADIUS: usize = 64;

/// 位相を分ける数
///
/// サンプル位置の端数をこの数で量子化し、位相ごとの係数を補間して使う。
pub const RESAMPLE_PHASES: usize = 128;

/// フィルタの半径の上限 (入力サンプル数)
///
/// 極端に低いレート比 (入力のサンプルレートが出力より 2 桁以上高い場合) では、係数の
/// 配列が現実的でない大きさになる。この上限を超える比では [`ResampleKernel::new`] は
/// `None` を返し、[`resample_block`] は線形補間へ退避する。8 kHz 出力に対する 96 kHz
/// 入力 (半径 768) や 192 kHz 入力 (半径 1536) は上限の内側である。
pub const RESAMPLE_MAX_RADIUS: usize = 4_096;

/// ダウンサンプリング用の Blackman 窓付き sinc の係数
///
/// レート比 (出力 / 入力) ごとに作る。比が 1 以上 (同じレートとアップサンプリング) では
/// 帯域制限が要らないため、[`ResampleKernel::new`] は `None` を返す。
#[derive(Debug, Clone, PartialEq)]
pub struct ResampleKernel {
    /// 出力 / 入力のレート比 (0 より大きく 1 未満)
    ratio: f64,
    /// 参照する入出力サンプルの半径 (入力サンプル数)
    radius: usize,
    /// 1 つの位相で使うタップ数 (`radius * 2 + 1`)
    taps: usize,
    /// 位相ごとの係数。`(RESAMPLE_PHASES + 1) * taps` 個
    weights: Vec<f32>,
}

impl ResampleKernel {
    /// レート比から係数を作る (ダウンサンプリングのときだけ)
    ///
    /// `ratio` は出力のサンプルレート / 入力のサンプルレートである。`0 < ratio < 1` の
    /// 有限値で、かつフィルタの半径が [`RESAMPLE_MAX_RADIUS`] 以下のときだけ作り、それ以外
    /// (同じレート、アップサンプリング、不正な値、極端に低いレート比) は `None` を返す。
    pub fn new(ratio: f64) -> Option<Self> {
        if !ratio.is_finite() || ratio <= 0.0 || ratio >= 1.0 {
            return None;
        }
        let radius = libm::ceil(RESAMPLE_OUTPUT_RADIUS as f64 / ratio) as usize;
        if radius > RESAMPLE_MAX_RADIUS {
            return None;
        }
        let taps = radius.saturating_mul(2).saturating_add(1);
        let mut weights = vec![0.0f32; (RESAMPLE_PHASES + 1).saturating_mul(taps)];
        let cutoff = ratio * 0.95;
        for phase in 0..=RESAMPLE_PHASES {
            let fraction = phase as f64 / RESAMPLE_PHASES as f64;
            let offset = phase * taps;
            let mut sum = 0.0f64;
            for tap in 0..taps {
                let distance = tap as f64 - radius as f64 - fraction;
                let angle = PI * cutoff * distance;
                let window = if distance.abs() > radius as f64 {
                    0.0
                } else {
                    0.42 + 0.5 * libm::cos(PI * distance / radius as f64)
                        + 0.08 * libm::cos(2.0 * PI * distance / radius as f64)
                };
                let sinc = if angle == 0.0 {
                    1.0
                } else {
                    libm::sin(angle) / angle
                };
                let weight = cutoff * sinc * window;
                weights[offset + tap] = weight as f32;
                sum += weight;
            }
            // DC の振幅を位相に依存させない
            if sum != 0.0 {
                for tap in 0..taps {
                    weights[offset + tap] = (f64::from(weights[offset + tap]) / sum) as f32;
                }
            }
        }
        Some(Self {
            ratio,
            radius,
            taps,
            weights,
        })
    }

    /// 出力 / 入力のレート比
    pub fn ratio(&self) -> f64 {
        self.ratio
    }

    /// 参照する入力サンプルの半径
    pub fn radius(&self) -> usize {
        self.radius
    }

    /// 1 つの位相で使うタップ数
    pub fn taps(&self) -> usize {
        self.taps
    }

    /// 入力の `position` (サンプル位置、端数を含む) の値を補間する
    ///
    /// 範囲外は先頭と末尾のサンプルで延長する (moqt-js の `Math.max(0, Math.min(...))` と
    /// 同じ)。ブロックの境界で前後の実サンプルを使うには、呼び出し側が連続した PCM を
    /// 渡すこと。`input` が空のときは 0 を返す。
    pub fn sample(&self, input: &[f32], position: f64) -> f32 {
        if input.is_empty() || !position.is_finite() {
            return 0.0;
        }
        let source_index = libm::floor(position);
        let fraction = position - source_index;
        let base = source_index as isize - self.radius as isize;
        // 端数は 0 以上 1 未満であるため、位相の添字は RESAMPLE_PHASES 未満になる。
        // 丸め誤差で境界を超えたときは最後の位相へ切る
        let phase = (fraction * RESAMPLE_PHASES as f64).clamp(0.0, RESAMPLE_PHASES as f64);
        let phase_index = (libm::floor(phase) as usize).min(RESAMPLE_PHASES - 1);
        let phase_fraction = phase - phase_index as f64;
        let offset = phase_index * self.taps;
        let last_index = input.len() - 1;
        let mut value = 0.0f64;
        for tap in 0..self.taps {
            let index = (base + tap as isize).clamp(0, last_index as isize) as usize;
            let weight = f64::from(self.weights[offset + tap]);
            let next_weight = f64::from(self.weights[offset + self.taps + tap]);
            value += f64::from(input[index]) * (weight + (next_weight - weight) * phase_fraction);
        }
        value as f32
    }
}

/// 線形補間で入力の `position` の値を求める
///
/// 範囲外は先頭と末尾のサンプルで延長する。同じレートとアップサンプリングで使う。
pub fn linear_sample(input: &[f32], position: f64) -> f32 {
    if input.is_empty() || !position.is_finite() {
        return 0.0;
    }
    let index = libm::floor(position);
    let fraction = (position - index).clamp(0.0, 1.0) as f32;
    let before_index = (index as isize).clamp(0, input.len() as isize - 1) as usize;
    let after_index = (before_index + 1).min(input.len() - 1);
    let before = input[before_index];
    let after = input[after_index];
    before + (after - before) * fraction
}

/// 1 ブロックの PCM を出力のサンプルレートへ変換する
///
/// `channels` はチャンネルごとの入力 PCM で、すべて同じ長さであること。
/// `start_output_frame` は、入力の先頭 (`position = 0`) に対応する出力のサンプル位置
/// (端数を含む) である。出力の `frame` 番目のサンプルは、入力の
/// `start_output_frame + frame` を `input_rate / output_rate` 倍した位置から取る。
/// ブロックを連続して変換するときは、前のブロックの終端に合わせてこの値を進める。
///
/// 出力のレートが入力より低いときは [`ResampleKernel`] で帯域を制限し、同じレートと
/// アップサンプリングは [`linear_sample`] を使う。レート比が極端に低くてフィルタを
/// 作れない ([`RESAMPLE_MAX_RADIUS`] を超える) ときは、帯域を制限せず線形補間へ退避する。
/// チャンネル数か長さが合わないとき、サンプルレートが 0 のときは、`output_frames` 個の
/// 無音を返す。
pub fn resample_block(
    channels: &[Vec<f32>],
    input_rate: u32,
    output_rate: u32,
    start_output_frame: f64,
    output_frames: usize,
) -> Vec<Vec<f32>> {
    let silent = || -> Vec<Vec<f32>> {
        channels
            .iter()
            .map(|_| vec![0.0f32; output_frames])
            .collect()
    };
    if input_rate == 0 || output_rate == 0 || channels.is_empty() {
        return silent();
    }
    let length = channels[0].len();
    if channels.iter().any(|channel| channel.len() != length) {
        return silent();
    }
    if output_frames == 0 {
        return channels.iter().map(|_| Vec::new()).collect();
    }
    // 位置が有限でなければ先頭から変換する (呼び出し側の時計の異常で NaN を伝播させない)
    let start_output_frame = if start_output_frame.is_finite() {
        start_output_frame
    } else {
        0.0
    };
    let ratio = f64::from(output_rate) / f64::from(input_rate);
    let kernel = if ratio < 1.0 {
        ResampleKernel::new(ratio)
    } else {
        None
    };
    let step = f64::from(input_rate) / f64::from(output_rate);
    channels
        .iter()
        .map(|input| {
            (0..output_frames)
                .map(|frame| {
                    let position = (start_output_frame + frame as f64) * step;
                    match &kernel {
                        Some(kernel) => kernel.sample(input, position),
                        None => linear_sample(input, position),
                    }
                })
                .collect()
        })
        .collect()
}
