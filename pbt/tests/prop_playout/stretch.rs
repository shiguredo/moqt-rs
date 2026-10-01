//! 波形の周期による時間圧縮・伸長のプロパティテスト
//!
//! 任意の音声サンプル列に対して、長さの変化の契約とバッファ操作の構造が
//! 保たれることを検証する。

use pbt::common::test_runner;
use shiguredo_moqt::playout::stretch::{
    TIME_STRETCH_MAX_LAG, TIME_STRETCH_MIN_LAG, compress, expand,
};

/// 対応しているサンプルレートと間引き率の組
///
/// 間引き率は公開 API に無いため、テスト側で対応表を持つ。
const SUPPORTED_RATES: [(u32, usize); 4] = [(8_000, 2), (16_000, 4), (32_000, 8), (48_000, 12)];

/// 対応しているサンプルレートと間引き率の組を 1 つ選ぶ
fn sample_supported_rate(ctx: &mut noprop::TestCaseContext) -> (u32, usize) {
    SUPPORTED_RATES[noprop::sample_usize_in(ctx, 0..SUPPORTED_RATES.len())]
}

/// 解析の受理境界を含む入力長を選ぶ
fn sample_length(ctx: &mut noprop::TestCaseContext) -> usize {
    match noprop::sample_usize_in(ctx, 0..8) {
        0 => 0,
        1 => 1,
        2 => 123,   // 8 kHz: 間引き後 59 サンプル (操作しない)
        3 => 124,   // 8 kHz: 間引き後 60 サンプル (操作する境界)
        4 => 729,   // 48 kHz: 間引き後 59 サンプル (操作しない)
        5 => 730,   // 48 kHz: 間引き後 60 サンプル (操作する境界)
        6 => 1_440, // 48 kHz: ピッチ周期が音の半分にちょうど収まる長さ
        _ => noprop::sample_usize_in(ctx, 0..=2_000),
    }
}

/// 任意の音声のサンプル列を 1 チャンネル分生成する
///
/// ノイズ (操作されない経路)、周期的な矩形波 (操作される経路)、無音 (相関に関係なく
/// 操作される経路) を混ぜて、時間伸縮の分岐をひととおり踏む。
fn sample_signal(ctx: &mut noprop::TestCaseContext, length: usize) -> Vec<f32> {
    match noprop::sample_usize_in(ctx, 0..3) {
        0 => (0..length)
            .map(|_| noprop::sample_f32_in(ctx, -1.0, 1.0))
            .collect(),
        1 => {
            let period = match noprop::sample_usize_in(ctx, 0..3) {
                0 => 240,
                1 => 720,
                _ => noprop::sample_usize_in(ctx, 2..=2_000),
            };
            let amplitude = noprop::sample_f32_in(ctx, 0.1, 1.0);
            (0..length)
                .map(|index| {
                    if index % period < period / 2 {
                        amplitude
                    } else {
                        -amplitude
                    }
                })
                .collect()
        }
        _ => vec![0.0f32; length],
    }
}

/// 任意のチャンネル数の音声を生成する
fn sample_channels(
    ctx: &mut noprop::TestCaseContext,
    length: usize,
    channel_count: usize,
) -> Vec<Vec<f32>> {
    (0..channel_count)
        .map(|_| sample_signal(ctx, length))
        .collect()
}

/// compress は 0 または負の長さの変化を返し、変更しなかった範囲と詰めた範囲が
/// 元のサンプルと一致する
#[test]
fn compress_reports_length_change_and_keeps_buffers_consistent() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let (sample_rate, factor) = sample_supported_rate(ctx);
        let length = sample_length(ctx);
        let channel_count = noprop::sample_usize_in(ctx, 1..=2);
        let mut channels = sample_channels(ctx, length, channel_count);
        let original = channels.clone();
        let mut references: Vec<&mut [f32]> = channels
            .iter_mut()
            .map(|channel| channel.as_mut_slice())
            .collect();
        let change = compress(&mut references, sample_rate);
        assert!(change <= 0);
        if change == 0 {
            // 操作しなかったときはバッファを変えない
            assert_eq!(channels, original);
        } else {
            let removed = (-change) as usize;
            assert_eq!(removed % factor, 0);
            assert!(
                (TIME_STRETCH_MIN_LAG * factor..=TIME_STRETCH_MAX_LAG * factor).contains(&removed)
            );
            let splice = length / 2;
            // 削る長さは音の中央までである
            assert!(removed <= splice);
            for (channel, original_channel) in channels.iter().zip(original.iter()) {
                // 削る窓より手前は変わらない
                assert_eq!(
                    &channel[..splice - removed],
                    &original_channel[..splice - removed]
                );
                // 切れ目より後ろは削った分だけ前へ詰まる
                assert_eq!(
                    &channel[splice..length - removed],
                    &original_channel[splice + removed..length]
                );
                for sample in channel {
                    assert!(sample.is_finite());
                }
            }
        }
        Ok(())
    })?;
    Ok(())
}

/// expand は 0 または正の長さの変化を返し、挿した範囲の後ろが元のサンプルと一致する
#[test]
fn expand_reports_length_change_and_keeps_buffers_consistent() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let (sample_rate, factor) = sample_supported_rate(ctx);
        let length = sample_length(ctx);
        let channel_count = noprop::sample_usize_in(ctx, 1..=2);
        let channels = sample_channels(ctx, length, channel_count);
        // 挿入しうる最大長 (60 × 間引き率) を足した出力を用意する
        let mut output = vec![vec![0.0f32; length + TIME_STRETCH_MAX_LAG * factor]; channel_count];
        let references: Vec<&[f32]> = channels.iter().map(|channel| channel.as_slice()).collect();
        let mut outputs: Vec<&mut [f32]> = output
            .iter_mut()
            .map(|channel| channel.as_mut_slice())
            .collect();
        let change = expand(&references, &mut outputs, sample_rate);
        assert!(change >= 0);
        if change == 0 {
            // 操作しなかったときは出力へ何も書かない
            assert!(
                output
                    .iter()
                    .all(|channel| channel.iter().all(|sample| *sample == 0.0))
            );
        } else {
            let added = change as usize;
            assert_eq!(added % factor, 0);
            assert!(
                (TIME_STRETCH_MIN_LAG * factor..=TIME_STRETCH_MAX_LAG * factor).contains(&added)
            );
            let splice = length / 2;
            // 挿す長さは音の中央までである
            assert!(added <= splice);
            for (output_channel, channel) in output.iter().zip(channels.iter()) {
                // 切れ目までは元のまま (フェードする範囲を除く)
                assert_eq!(&output_channel[..splice], &channel[..splice]);
                // 切れ目より後ろは挿した分だけ後ろへずれる
                assert_eq!(
                    &output_channel[splice + added..length + added],
                    &channel[splice..length]
                );
                for sample in output_channel {
                    assert!(sample.is_finite());
                }
            }
        }
        Ok(())
    })?;
    Ok(())
}
