//! 音声の出力レートへの変換のテスト
//!
//! 公開 API (`ResampleKernel` / `linear_sample` / `resample_block`) の契約を確認する。
//! 同じレートとアップサンプリングは線形補間、ダウンサンプリングは Blackman 窓付き sinc で
//! 帯域を制限する。

use shiguredo_moqt::playout::resample::{
    RESAMPLE_MAX_RADIUS, RESAMPLE_OUTPUT_RADIUS, RESAMPLE_PHASES, ResampleKernel, linear_sample,
    resample_block,
};

/// 正弦波を作る (振幅 `amplitude`)
fn sine(length: usize, frequency_hz: f64, sample_rate: u32, amplitude: f64) -> Vec<f32> {
    (0..length)
        .map(|index| {
            (amplitude
                * (2.0 * std::f64::consts::PI * frequency_hz * index as f64
                    / f64::from(sample_rate))
                .sin()) as f32
        })
        .collect()
}

/// 中央の区間の RMS を求める
fn rms(samples: &[f32]) -> f64 {
    let sum = samples
        .iter()
        .map(|sample| f64::from(*sample) * f64::from(*sample))
        .sum::<f64>();
    (sum / samples.len() as f64).sqrt()
}

#[test]
fn resample_kernel_is_created_only_for_downsampling() {
    assert!(ResampleKernel::new(0.5).is_some());
    assert!(ResampleKernel::new(0.91875).is_some());
    assert!(
        ResampleKernel::new(1.0).is_none(),
        "同じレートでは作らないこと"
    );
    assert!(
        ResampleKernel::new(2.0).is_none(),
        "アップサンプリングでは作らないこと"
    );
    assert!(ResampleKernel::new(0.0).is_none());
    assert!(ResampleKernel::new(-0.5).is_none());
    assert!(ResampleKernel::new(f64::NAN).is_none());
    assert!(ResampleKernel::new(f64::INFINITY).is_none());
    // 半径が上限を超える比では作らない (係数の配列が現実的でない大きさになる)
    assert!(
        ResampleKernel::new(0.001).is_none(),
        "極端に低いレート比では作らないこと"
    );
    assert_eq!(RESAMPLE_MAX_RADIUS, 4_096);
}

#[test]
fn resample_kernel_has_the_expected_radius_and_taps() {
    let kernel = ResampleKernel::new(0.5).expect("半分のレート比の係数を作れること");
    assert!((kernel.ratio() - 0.5).abs() < 1e-12);
    assert_eq!(kernel.radius(), RESAMPLE_OUTPUT_RADIUS * 2);
    assert_eq!(kernel.taps(), RESAMPLE_OUTPUT_RADIUS * 2 * 2 + 1);
    assert_eq!(RESAMPLE_PHASES, 128);
}

#[test]
fn downsampling_preserves_dc() {
    let input = vec![0.25f32; 512];
    let output = resample_block(&[input], 96_000, 48_000, 0.0, 256);
    assert_eq!(output.len(), 1);
    for (index, sample) in output[0].iter().enumerate() {
        assert!(
            (sample - 0.25).abs() < 1e-4,
            "DC が変わった: index={index} sample={sample}"
        );
    }
}

#[test]
fn downsampling_suppresses_an_out_of_band_tone() {
    // 96 kHz の 30 kHz は、48 kHz へ変換すると 18 kHz へ折り返す。Blackman 窓付き sinc で
    // 阻止帯域まで減衰させる
    let input = sine(4_096, 30_000.0, 96_000, 0.5);
    let output = resample_block(&[input], 96_000, 48_000, 0.0, 2_048);
    let middle = &output[0][512..1_536];
    let alias_rms = rms(middle);
    assert!(
        alias_rms < 0.005,
        "折り返した成分が減衰していない: rms={alias_rms}"
    );
}

#[test]
fn downsampling_passes_the_audio_band() {
    // 1 kHz は通過帯域であり、振幅 0.5 の正弦波の RMS (0.3536) を保つ
    let input = sine(4_096, 1_000.0, 96_000, 0.5);
    let output = resample_block(&[input], 96_000, 48_000, 0.0, 2_048);
    let middle = &output[0][512..1_536];
    let pass_rms = rms(middle);
    assert!(
        (pass_rms - 0.353_553).abs() < 0.01,
        "通過帯域の振幅が変わった: rms={pass_rms}"
    );
}

#[test]
fn same_rate_keeps_the_fractional_offset() {
    let input = vec![0.0f32, 1.0, 2.0, 3.0];
    // 出力の 0 番目が入力の 0.5 番目に対応する
    let output = resample_block(std::slice::from_ref(&input), 48_000, 48_000, 0.5, 2);
    assert!((output[0][0] - 0.5).abs() < 1e-6);
    assert!((output[0][1] - 1.5).abs() < 1e-6);
    assert!((linear_sample(&input, 2.5) - 2.5).abs() < 1e-6);
}

#[test]
fn upsampling_uses_linear_interpolation() {
    let input = vec![0.0f32, 1.0];
    let output = resample_block(&[input], 24_000, 48_000, 0.0, 3);
    assert!((output[0][0] - 0.0).abs() < 1e-6);
    assert!((output[0][1] - 0.5).abs() < 1e-6);
    assert!((output[0][2] - 1.0).abs() < 1e-6);
}

#[test]
fn channels_are_resampled_independently() {
    let left = vec![0.0f32; 512];
    let right = vec![1.0f32; 512];
    let output = resample_block(&[left, right], 96_000, 48_000, 0.0, 128);
    assert_eq!(output.len(), 2);
    assert!(output[0].iter().all(|sample| sample.abs() < 1e-6));
    assert!(output[1].iter().all(|sample| (sample - 1.0).abs() < 1e-4));
}

#[test]
fn mismatched_channel_lengths_return_silence() {
    let output = resample_block(
        &[vec![1.0f32; 128], vec![1.0f32; 64]],
        96_000,
        48_000,
        0.0,
        32,
    );
    assert_eq!(output.len(), 2);
    assert!(output.iter().all(|channel| channel.len() == 32));
    assert!(
        output
            .iter()
            .all(|channel| channel.iter().all(|sample| *sample == 0.0))
    );
}

#[test]
fn zero_rates_and_empty_inputs_return_silence() {
    let output = resample_block(&[vec![1.0f32; 64]], 0, 48_000, 0.0, 16);
    assert!(output[0].iter().all(|sample| *sample == 0.0));
    let output = resample_block(&[vec![1.0f32; 64]], 48_000, 0, 0.0, 16);
    assert!(output[0].iter().all(|sample| *sample == 0.0));
    let output = resample_block(&[], 48_000, 48_000, 0.0, 16);
    assert!(output.is_empty());
    assert!(linear_sample(&[], 0.0) == 0.0);
}

#[test]
fn kernel_sample_extends_the_endpoints() {
    let kernel = ResampleKernel::new(0.5).expect("係数を作れること");
    let input = vec![0.5f32; 64];
    assert!((kernel.sample(&input, -100.0) - 0.5).abs() < 1e-4);
    assert!((kernel.sample(&input, 1_000.0) - 0.5).abs() < 1e-4);
    assert!(kernel.sample(&[], 0.0) == 0.0);
    assert!(
        kernel.sample(&input, f64::NAN) == 0.0,
        "位置が有限でなければ値を伝播させないこと"
    );
}

#[test]
fn a_non_finite_start_frame_falls_back_to_the_beginning() {
    let input = vec![0.0f32, 1.0];
    let output = resample_block(&[input], 48_000, 48_000, f64::NAN, 1);
    assert_eq!(output[0][0], 0.0);
}
