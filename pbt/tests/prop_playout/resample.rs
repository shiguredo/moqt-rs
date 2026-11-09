//! 音声の出力レートへの変換のプロパティテスト
//!
//! 任意の PCM、サンプルレート、出力位置で、結果の形が合い、値が有限であり、同じレートの
//! 整数位置では入力をそのまま保ち、定数入力は帯域制限の有無にかかわらず定数のままである
//! ことを検証する。

use noprop::sample_usize_in;
use pbt::common::test_runner;
use shiguredo_moqt::playout::resample::{
    RESAMPLE_MAX_RADIUS, RESAMPLE_OUTPUT_RADIUS, ResampleKernel, linear_sample, resample_block,
};

const RATES: [u32; 6] = [8_000, 16_000, 24_000, 44_100, 48_000, 96_000];

fn sample_rate(ctx: &mut noprop::TestCaseContext) -> u32 {
    RATES[sample_usize_in(ctx, 0..RATES.len())]
}

#[test]
fn resampled_output_has_the_requested_shape_and_finite_values() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let channels = sample_usize_in(ctx, 1..5);
        let length = sample_usize_in(ctx, 1..256);
        let input_rate = sample_rate(ctx);
        let output_rate = sample_rate(ctx);
        let output_frames = sample_usize_in(ctx, 0..512);
        let start_output_frame = sample_usize_in(ctx, 0..2_000) as f64 / 1_000.0 - 1.0;
        let mut input = Vec::with_capacity(channels);
        for _ in 0..channels {
            input.push(
                (0..length)
                    .map(|index| noprop::sample_f64_in(ctx, -1.0, 1.0) as f32 + index as f32 * 1e-6)
                    .collect::<Vec<f32>>(),
            );
        }
        let output = resample_block(
            &input,
            input_rate,
            output_rate,
            start_output_frame,
            output_frames,
        );
        assert_eq!(output.len(), channels, "チャンネル数が変わった");
        for channel in &output {
            assert_eq!(channel.len(), output_frames, "出力のサンプル数が要求と違う");
            assert!(
                channel.iter().all(|sample| sample.is_finite()),
                "出力に有限でない値が混じった"
            );
        }
        Ok(())
    })?;
    Ok(())
}

#[test]
fn constant_input_stays_constant_for_any_rate_ratio() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let value = noprop::sample_f64_in(ctx, -1.0, 1.0) as f32;
        let length = sample_usize_in(ctx, 64..2_048);
        let input_rate = sample_rate(ctx);
        let output_rate = sample_rate(ctx);
        let output_frames = sample_usize_in(ctx, 1..256);
        let output = resample_block(
            &[vec![value; length]],
            input_rate,
            output_rate,
            0.0,
            output_frames,
        );
        for sample in &output[0] {
            assert!(
                (f64::from(*sample) - f64::from(value)).abs() < 1e-3,
                "定数入力の値が変わった: value={value} sample={sample} input_rate={input_rate} output_rate={output_rate}"
            );
        }
        Ok(())
    })?;
    Ok(())
}

#[test]
fn same_rate_with_an_integer_start_is_the_identity() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let length = sample_usize_in(ctx, 1..512);
        let output_frames = sample_usize_in(ctx, 0..=length);
        let input: Vec<f32> = (0..length)
            .map(|_| noprop::sample_f64_in(ctx, -1.0, 1.0) as f32)
            .collect();
        let output = resample_block(
            std::slice::from_ref(&input),
            48_000,
            48_000,
            0.0,
            output_frames,
        );
        assert_eq!(&output[0][..], &input[..output_frames]);
        Ok(())
    })?;
    Ok(())
}

#[test]
fn linear_sample_clamps_positions_outside_the_input() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let length = sample_usize_in(ctx, 1..512);
        let input: Vec<f32> = (0..length)
            .map(|_| noprop::sample_f64_in(ctx, -1.0, 1.0) as f32)
            .collect();
        assert_eq!(linear_sample(&input, -1.0), input[0]);
        assert_eq!(
            linear_sample(&input, length as f64 + 1.0),
            input[length - 1]
        );
        Ok(())
    })?;
    Ok(())
}

#[test]
fn resample_kernel_exists_exactly_for_finite_downsampling_ratios() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let ratio = noprop::sample_f64_in(ctx, 0.0, 2.0);
        let kernel = ResampleKernel::new(ratio);
        let radius = (RESAMPLE_OUTPUT_RADIUS as f64 / ratio).ceil() as usize;
        if ratio > 0.0 && ratio < 1.0 && radius <= RESAMPLE_MAX_RADIUS {
            let kernel = kernel.expect("ダウンサンプリングの比なら係数を作ること");
            assert!((kernel.ratio() - ratio).abs() < 1e-12);
            assert!(kernel.radius() > 0);
            assert_eq!(kernel.taps(), kernel.radius() * 2 + 1);
        } else {
            assert!(kernel.is_none(), "ダウンサンプリング以外では作らないこと");
        }
        Ok(())
    })?;
    Ok(())
}
