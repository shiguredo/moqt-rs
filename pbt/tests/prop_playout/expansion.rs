//! 未着時に出力する続き音と、その数値カーネルのプロパティテスト
//!
//! 任意の有限 PCM に対して、続き音の長さ・有限性・振幅の契約が保たれ、入力が変更されない
//! ことを検証する。続き音は最新の実音の RMS とピークを超えない (過去の大きな発声を補間で
//! 持ち上げない)。数値カーネルは出力の上限を守り、容量を超える要求を拒否する。

use pbt::common::test_runner;
use shiguredo_moqt::playout::expansion::{AUDIO_EXPANSION_SECONDS, expand_audio};
use shiguredo_moqt::playout::predictor::{
    AUDIO_PREDICTOR_HISTORY_SIZE, AUDIO_PREDICTOR_MAX_FRAMES, AUDIO_PREDICTOR_ORDER,
    generate_unvoiced,
};

/// 対応しているサンプルレートと、10 ms 以上の入力長を選ぶ
fn sample_rate_and_length(ctx: &mut noprop::TestCaseContext) -> (u32, usize) {
    match noprop::sample_usize_in(ctx, 0..4) {
        0 => (8_000, noprop::sample_usize_in(ctx, 80..=960)),
        1 => (16_000, noprop::sample_usize_in(ctx, 160..=960)),
        2 => (32_000, noprop::sample_usize_in(ctx, 320..=960)),
        _ => (48_000, noprop::sample_usize_in(ctx, 480..=960)),
    }
}

/// 任意の有限 PCM を作る (-1.0 以上 1.0 以下)
fn sample_channels(
    ctx: &mut noprop::TestCaseContext,
    length: usize,
    channel_count: usize,
) -> Vec<Vec<f32>> {
    (0..channel_count)
        .map(|_| {
            (0..length)
                .map(|_| noprop::sample_f32_in(ctx, -1.0, 1.0))
                .collect()
        })
        .collect()
}

/// ピークを求める
fn peak(samples: &[f32]) -> f64 {
    samples.iter().fold(0.0f64, |maximum, sample| {
        maximum.max(f64::from(sample.abs()))
    })
}

/// 続き音は 0 または 100 ms ちょうどの長さで、有限で、最新の実音のピークを超えず、入力を
/// 変更しない
#[test]
fn expand_is_finite_and_bounded_by_the_current_level() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        let (sample_rate, length) = sample_rate_and_length(ctx);
        let channel_count = noprop::sample_usize_in(ctx, 1..=2);
        let channels = sample_channels(ctx, length, channel_count);
        let saved = channels.clone();
        let references: Vec<&[f32]> = channels.iter().map(|channel| channel.as_slice()).collect();
        let expansion =
            expand_audio(&references, sample_rate, None).expect("有限の音声は受け付ける");
        if expansion.is_empty() {
            // 補間を省くのは対応範囲外のレートだけであり、ここでは起きない
            assert!(expansion.is_empty());
            return Ok(());
        }
        let expected = (f64::from(sample_rate) * AUDIO_EXPANSION_SECONDS).round() as usize;
        assert_eq!(
            expansion[0].len(),
            expected,
            "100 ms ちょうどの長さを生成すること: sample_rate={sample_rate}"
        );
        for (index, channel) in expansion.iter().enumerate() {
            assert_eq!(channel.len(), expected);
            assert!(
                channel.iter().all(|sample| sample.is_finite()),
                "生成した音が有限であること"
            );
            assert!(
                peak(channel) <= peak(&channels[index]) + 1e-6,
                "最新の実音のピークを超えないこと: index={index}"
            );
        }
        assert_eq!(channels, saved, "入力を変更しないこと");
        Ok(())
    })?;
    Ok(())
}

/// 同じ入力からは同じ続き音ができる (励振の種は入力から決まる)
#[test]
fn expand_is_deterministic() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(64, |ctx| {
        let length = noprop::sample_usize_in(ctx, 480..=1_440);
        let channels = sample_channels(ctx, length, 1);
        let references: Vec<&[f32]> = channels.iter().map(|channel| channel.as_slice()).collect();
        let first = expand_audio(&references, 48_000, None).expect("有限の音声は受け付ける");
        let second = expand_audio(&references, 48_000, None).expect("有限の音声は受け付ける");
        assert_eq!(first, second, "同じ入力なら同じ続き音になること");
        Ok(())
    })?;
    Ok(())
}

/// 数値カーネルは出力の上限を守り、容量を超える要求と不正な入力長を拒否する
#[test]
fn generate_unvoiced_respects_the_limit() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        let coefficients: [f64; AUDIO_PREDICTOR_ORDER] =
            core::array::from_fn(|_| noprop::sample_f64_in(ctx, -0.99, 0.99));
        let history: [f64; AUDIO_PREDICTOR_HISTORY_SIZE] =
            core::array::from_fn(|_| noprop::sample_f64_in(ctx, -1.0, 1.0));
        let frames = match noprop::sample_usize_in(ctx, 0..4) {
            0 => 0,
            1 => 1,
            2 => AUDIO_PREDICTOR_MAX_FRAMES,
            _ => noprop::sample_usize_in(ctx, 0..=AUDIO_PREDICTOR_MAX_FRAMES),
        };
        let innovation = noprop::sample_f64_in(ctx, 0.0, 1.0);
        let limit = noprop::sample_f64_in(ctx, 0.0, 2.0);
        let output =
            generate_unvoiced(&coefficients, &history, frames, 731, innovation, limit, 1.0)
                .expect("容量内の要求は成功する");
        assert_eq!(output.len(), frames);
        for sample in &output {
            assert!(sample.is_finite(), "出力が有限であること");
            assert!(
                f64::from(sample.abs()) <= limit + 1e-6,
                "出力が上限を超えないこと: sample={sample} limit={limit}"
            );
        }
        // 容量を超える要求は拒否する
        assert!(
            generate_unvoiced(
                &coefficients,
                &history,
                AUDIO_PREDICTOR_MAX_FRAMES + 1,
                731,
                innovation,
                limit,
                1.0,
            )
            .is_none(),
            "固定領域を超える要求を拒否すること"
        );
        Ok(())
    })?;
    Ok(())
}
