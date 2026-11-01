//! 非周期の励振と線形予測の積和を行う数値カーネル
//!
//! moqt-js の `audioPredictor.c` (WebAssembly) を Rust へ移したものである。固定 12 次の
//! 線形予測フィルターへ非周期の励振を入れ、続きの音を合成する。JavaScript の演算順序と
//! Float32 への丸めを保ち、音量の制約と減衰は呼び出し側 ([`expand_audio`](
//! crate::playout::expansion::expand_audio)) が行う。
//!
//! 動的確保は結果のサンプル列を作るときだけであり、作業領域は固定長の配列を使う。
//! 時刻や出力デバイスには触れない。

use alloc::vec;
use alloc::vec::Vec;

/// 線形予測の次数
pub const AUDIO_PREDICTOR_ORDER: usize = 12;

/// 線形予測の履歴の長さ (2 のべき乗)
pub const AUDIO_PREDICTOR_HISTORY_SIZE: usize = 16;

/// 1 回で生成できるサンプル数の上限
///
/// 96000 Hz の 100 ms が 9600 サンプルであり、moqt-js のカーネルの固定領域と同じ大きさで
/// ある。これを超える要求は拒否する。
pub const AUDIO_PREDICTOR_MAX_FRAMES: usize = 9_600;

/// 履歴の位置を 16 で折り返すためのマスク
const AUDIO_PREDICTOR_HISTORY_MASK: usize = AUDIO_PREDICTOR_HISTORY_SIZE - 1;

/// 非周期の励振を線形予測フィルターへ通して `frames` サンプルを生成する
///
/// `coefficients` は線形予測係数 (12 次)、`history` は直前のサンプル (16 個の遅延線)、
/// `state` は励振を作る乱数の状態、`innovation` は励振の大きさ、`limit` は出力の絶対値の
/// 上限、`polarity` は励振の極性である。`history` は末尾が最新のサンプルになるように
/// 並べる。
///
/// 励振は `state = state * 1664525 + 1013904223` で進め、`[-1, 1]` の一様乱数を
/// `innovation * polarity` 倍して作る。予測の積和は係数の順に加算し、再結合や積和融合を
/// 使わない (moqt-js のカーネルと同じ計算順序である)。出力は 1 サンプルごとに Float32 へ
/// 丸める。
///
/// # 戻り値
///
/// 生成したサンプル列。`frames` が [`AUDIO_PREDICTOR_MAX_FRAMES`] を超えるときは None を
/// 返す。
pub fn generate_unvoiced(
    coefficients: &[f64; AUDIO_PREDICTOR_ORDER],
    history: &[f64; AUDIO_PREDICTOR_HISTORY_SIZE],
    frames: usize,
    state: u32,
    innovation: f64,
    limit: f64,
    polarity: f64,
) -> Option<Vec<f32>> {
    if frames > AUDIO_PREDICTOR_MAX_FRAMES {
        return None;
    }
    // 履歴は呼び出し側の配列を変更しない (moqt-js は作業領域へ写してから使う)
    let mut history = *history;
    let mut state = state;
    let mut samples = vec![0.0f32; frames];
    let mut head = 0usize;
    for sample in samples.iter_mut() {
        // C の `state * 1664525u + 1013904223u` と同じく 32 bit で折り返す
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        // `(((double)state / 4294967295.0) * 2.0 - 1.0) * 1.7320508075688772 * innovation *
        // polarity` と同じ順に掛ける
        let random = (f64::from(state) / 4_294_967_295.0) * 2.0 - 1.0;
        let mut value = random * 1.732_050_807_568_877_2 * innovation * polarity;
        // 再結合や積和融合を使わず、JavaScript と同じ順に加算する
        for (order, coefficient) in coefficients.iter().enumerate() {
            let position = head.wrapping_sub(1).wrapping_sub(order) & AUDIO_PREDICTOR_HISTORY_MASK;
            value += coefficient * history[position];
        }
        if value > limit {
            value = limit;
        }
        if value < -limit {
            value = -limit;
        }
        history[head] = value;
        head = (head + 1) & AUDIO_PREDICTOR_HISTORY_MASK;
        *sample = value as f32;
    }
    Some(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_unvoiced_follows_the_first_order_decay() {
        // 1 次の係数 0.5 だけなら、履歴の最新値が 1 サンプルごとに半分になる解析解になる
        let mut coefficients = [0.0f64; AUDIO_PREDICTOR_ORDER];
        coefficients[0] = 0.5;
        let mut history = [0.0f64; AUDIO_PREDICTOR_HISTORY_SIZE];
        history[AUDIO_PREDICTOR_HISTORY_SIZE - 1] = 0.25;
        let output = generate_unvoiced(&coefficients, &history, 64, 731, 0.0, 1.0, 1.0)
            .expect("上限内の生成は成功する");
        // 解析解は 0.25 * 0.5^(index + 1) である (no_std に powi が無いため掛け算で作る)
        let mut expected = 0.25 * 0.5;
        for (index, sample) in output.iter().enumerate() {
            assert!(
                (f64::from(*sample) - expected).abs() < 1e-9,
                "index={index} sample={sample} expected={expected}"
            );
            expected *= 0.5;
        }
    }

    #[test]
    fn generate_unvoiced_rejects_frames_over_the_capacity() {
        let coefficients = [0.0f64; AUDIO_PREDICTOR_ORDER];
        let history = [0.0f64; AUDIO_PREDICTOR_HISTORY_SIZE];
        assert!(
            generate_unvoiced(
                &coefficients,
                &history,
                AUDIO_PREDICTOR_MAX_FRAMES + 1,
                731,
                0.1,
                0.3,
                1.0,
            )
            .is_none(),
            "固定領域を超える要求は拒否すること"
        );
        // 拒否のあとでも正常な生成ができる
        let output = generate_unvoiced(&coefficients, &history, 128, 731, 0.1, 0.3, 1.0)
            .expect("上限内の生成は成功する");
        assert!(output.iter().all(|sample| sample.is_finite()));
        assert!(
            output.iter().all(|sample| sample.abs() <= 0.3),
            "出力は上限を超えないこと"
        );
        assert!(
            output.iter().any(|sample| *sample != 0.0),
            "失敗を無音で隠さないこと"
        );
    }
}
