//! 波形の周期による時間圧縮・時間伸長と、欠落した区間の補間のプロパティテスト
//!
//! 任意の音声サンプル列に対して、時間圧縮・時間伸長・補間の長さの契約とバッファ操作の
//! 構造が保たれることを検証する。時間圧縮は削る窓の外側のサンプルを変えず、窓の中も
//! 「元の位置」と「周期ぶん後ろの位置」の間の値にしかならない (クロスフェードの重みが
//! 0 から 1 の間にあるため)。時間伸長は実音の先頭と末尾を変えず、元の振幅を増やさない。

use pbt::common::test_runner;
use shiguredo_moqt::playout::stretch::{
    TIME_STRETCH_CONCEAL_END_GAIN, TIME_STRETCH_MAX_LAG, TIME_STRETCH_MIN_LAG, compress, conceal,
    concealment_end_gain, is_time_stretch_available, preemptively_expand,
};

/// 対応しているサンプルレートと、4 kHz の 1 サンプルに対応する入力のサンプル数
///
/// 44.1 kHz は解析用の信号だけを 48 kHz へ換算するため、倍率が非整数 (11.025) になる。
const SUPPORTED_RATES: [(u32, f64); 5] = [
    (8_000, 2.0),
    (16_000, 4.0),
    (32_000, 8.0),
    (44_100, 11.025),
    (48_000, 12.0),
];

/// 対応しているサンプルレートと格子の倍率の組を 1 つ選ぶ
fn sample_supported_rate(ctx: &mut noprop::TestCaseContext) -> (u32, f64) {
    SUPPORTED_RATES[noprop::sample_usize_in(ctx, 0..SUPPORTED_RATES.len())]
}

/// 解析の受理境界を含む入力長を選ぶ
///
/// 4 kHz へ間引いた長さが自己相関の窓 (50) + 周期の下限 (10) + 1 に届くかどうかの境界を
/// レートごとに混ぜる。
fn sample_length(ctx: &mut noprop::TestCaseContext, sample_rate: u32) -> usize {
    let boundaries: &[usize] = match sample_rate {
        // 8 kHz: 間引き後 59 / 60 / 61 サンプル
        8_000 => &[0, 1, 123, 124, 126, 160],
        // 44.1 kHz: 解析用の 48 kHz 変換を含む境界
        44_100 => &[0, 1, 800, 882, 441, 1_323],
        // 16 / 32 / 48 kHz
        _ => &[0, 1, 729, 730, 742, 960],
    };
    match noprop::sample_usize_in(ctx, 0..4) {
        0..=2 => boundaries[noprop::sample_usize_in(ctx, 0..boundaries.len())],
        _ => noprop::sample_usize_in(ctx, 0..=2_000),
    }
}

/// 削ってよい長さの上限 (サンプル数) を選ぶ
///
/// 無制限、0、周期の下限の前後、音の長さの半分、音の長さの範囲を混ぜて、上限の分岐を
/// ひととおり踏む。
fn sample_max_length_change_samples(
    ctx: &mut noprop::TestCaseContext,
    length: usize,
    grid: f64,
) -> usize {
    let period_floor = (TIME_STRETCH_MIN_LAG as f64 * grid) as usize;
    match noprop::sample_usize_in(ctx, 0..6) {
        0 => usize::MAX,
        1 => 0,
        2 => period_floor - 1,
        3 => period_floor,
        4 => length / 2,
        _ => noprop::sample_usize_in(ctx, 0..=length),
    }
}

/// 任意の音声のサンプル列を 1 チャンネル分生成する
///
/// ノイズ (操作されない経路)、周期的な矩形波 (操作される経路)、無音 (相関に関係なく
/// 操作される経路) を混ぜて、時間圧縮の分岐をひととおり踏む。
fn sample_signal(ctx: &mut noprop::TestCaseContext, length: usize, grid: f64) -> Vec<f32> {
    match noprop::sample_usize_in(ctx, 0..3) {
        0 => (0..length)
            .map(|_| noprop::sample_f32_in(ctx, -1.0, 1.0))
            .collect(),
        1 => {
            // 4 kHz の格子に乗る周期 (端数補間を掛けない経路) と乗らない周期を混ぜる
            let period = match noprop::sample_usize_in(ctx, 0..3) {
                0 => (10.0 * grid) as usize,
                1 => (20.0 * grid) as usize,
                _ => noprop::sample_usize_in(ctx, 2..=2_000),
            }
            .max(2);
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
    grid: f64,
) -> Vec<Vec<f32>> {
    (0..channel_count)
        .map(|_| sample_signal(ctx, length, grid))
        .collect()
}

/// 補間する長さ (マイクロ秒) を選ぶ
///
/// 0 (埋めない)、1 サンプルに満たない長さ、隙間の下限の前後、上限、上限を超える長さを
/// 混ぜて、補間の分岐をひととおり踏む。
fn sample_conceal_us(ctx: &mut noprop::TestCaseContext) -> i64 {
    match noprop::sample_usize_in(ctx, 0..6) {
        0 => 0,
        1 => 1,
        2 => 4_999,
        3 => 5_000,
        4 => 100_000,
        _ => noprop::sample_usize_in(ctx, 0..=200_000) as i64,
    }
}

/// 時間伸長で増やせる長さ (サンプル数) を選ぶ
fn sample_expansion_limit(ctx: &mut noprop::TestCaseContext, length: usize) -> usize {
    match noprop::sample_usize_in(ctx, 0..4) {
        0 => 0,
        1 => 1,
        _ => noprop::sample_usize_in(ctx, 0..=length),
    }
}

/// compress は 0 または負の長さの変化を返し、削った長さは上限を超えず、削る窓の外側の
/// サンプルを変えず、窓の中も「元の位置」と「周期ぶん後ろの位置」の間の値にしかならない
#[test]
fn compress_reports_length_change_and_keeps_buffers_consistent() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let (sample_rate, grid) = sample_supported_rate(ctx);
        let length = sample_length(ctx, sample_rate);
        let channel_count = noprop::sample_usize_in(ctx, 1..=2);
        let mut channels = sample_channels(ctx, length, channel_count, grid);
        let original = channels.clone();
        let max_length_change_samples = sample_max_length_change_samples(ctx, length, grid);
        let mut references: Vec<&mut [f32]> = channels
            .iter_mut()
            .map(|channel| channel.as_mut_slice())
            .collect();
        let change = compress(&mut references, sample_rate, max_length_change_samples);
        assert!(change <= 0);
        if change == 0 {
            // 操作しなかったときはバッファを変えない
            assert_eq!(channels, original);
        } else {
            let removed = (-change) as usize;
            assert!(
                removed <= max_length_change_samples,
                "上限を超えて削った: removed={removed} max={max_length_change_samples}"
            );
            assert!(removed <= length, "音より長く削った");
            // 4 kHz の格子の下限から上限までのずらし幅に端数補間を掛けた範囲に収まる
            assert!(
                removed >= ((TIME_STRETCH_MIN_LAG as f64 - 1.0) * grid) as usize,
                "削る長さが下限より短い: removed={removed} grid={grid}"
            );
            assert!(
                removed <= ((TIME_STRETCH_MAX_LAG as f64 + 1.0) * grid).ceil() as usize,
                "削る長さが上限より長い: removed={removed} grid={grid}"
            );
            let used = length - removed;
            for (channel, original_channel) in channels.iter().zip(original.iter()) {
                // 先頭と末尾は隣接フレームへ繋がるため、削る位置をどこへ移しても変わらない
                assert_eq!(channel[0], original_channel[0], "先頭のサンプルを変えない");
                assert_eq!(
                    channel[used - 1],
                    original_channel[length - 1],
                    "末尾のサンプルを変えない"
                );
                // クロスフェードの重みが 0 から 1 の間にあるため、使用する範囲の各サンプルは
                // 「元の位置」と「周期ぶん後ろの位置」の間の値にしかならない
                for index in 0..used {
                    let before = f64::from(original_channel[index]);
                    let after = f64::from(original_channel[index + removed]);
                    let value = f64::from(channel[index]);
                    assert!(
                        value >= before.min(after) - 1e-5 && value <= before.max(after) + 1e-5,
                        "index={index} の値が元の 2 位置の外: value={value} before={before} after={after}"
                    );
                }
            }
        }
        Ok(())
    })?;
    Ok(())
}

/// conceal は 0 または要求した長さを返し、埋めた範囲は元の音の振幅に収まり、埋めなかった
/// 範囲と埋めなかったときの出力は変えない
#[test]
fn conceal_reports_length_and_keeps_buffers_consistent() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let (sample_rate, grid) = sample_supported_rate(ctx);
        let length = sample_length(ctx, sample_rate);
        let channel_count = noprop::sample_usize_in(ctx, 1..=2);
        let channels = sample_channels(ctx, length, channel_count, grid);
        let conceal_us = sample_conceal_us(ctx);
        // 要求する長さ (サンプル数、四捨五入)
        let requested =
            ((conceal_us * i64::from(sample_rate) + 500_000) / 1_000_000).max(0) as usize;
        // 要求より 8 サンプル長い出力を用意し、書かなかった範囲が変わらないことも見る
        let mut output = vec![vec![0.0f32; requested + 8]; channel_count];
        let references: Vec<&[f32]> = channels.iter().map(|channel| channel.as_slice()).collect();
        let mut outputs: Vec<&mut [f32]> = output
            .iter_mut()
            .map(|channel| channel.as_mut_slice())
            .collect();
        let change = conceal(&references, &mut outputs, sample_rate, conceal_us);
        assert!(change >= 0);
        if change == 0 {
            // 埋めなかったときは出力へ何も書かない
            assert!(
                output
                    .iter()
                    .all(|channel| channel.iter().all(|sample| *sample == 0.0))
            );
        } else {
            let generated = change as usize;
            // 要求した長さちょうどを埋める
            assert_eq!(generated, requested);
            for (output_channel, channel) in output.iter().zip(channels.iter()) {
                // 埋めた音は元の音の振幅の範囲に収まり、有限である
                let peak = channel
                    .iter()
                    .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
                for sample in &output_channel[..generated] {
                    assert!(sample.is_finite());
                    assert!(sample.abs() <= peak);
                }
                // 埋めた長さを超える範囲は変えない
                assert!(
                    output_channel[generated..]
                        .iter()
                        .all(|sample| *sample == 0.0)
                );
            }
        }
        Ok(())
    })?;
    Ok(())
}

/// 補間した音の末尾の振幅は、補間が長いほど下がり、下限と 1 の間に収まる
#[test]
fn concealment_end_gain_is_monotone_and_bounded() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let left = noprop::sample_usize_in(ctx, 0..=200_000) as i64;
        let right = noprop::sample_usize_in(ctx, 0..=200_000) as i64;
        // 短い方と長い方の組にする
        let (shorter, longer) = if left <= right {
            (left, right)
        } else {
            (right, left)
        };
        let end_gain_of_shorter = concealment_end_gain(shorter);
        let end_gain_of_longer = concealment_end_gain(longer);
        assert!(
            end_gain_of_shorter >= end_gain_of_longer,
            "shorter={shorter} longer={longer}"
        );
        assert!(
            (TIME_STRETCH_CONCEAL_END_GAIN..=1.0).contains(&end_gain_of_longer),
            "end_gain={end_gain_of_longer}"
        );
        Ok(())
    })?;
    Ok(())
}

/// 隣り合うサンプルの差の最大値 (波形の連続性)
fn max_step(samples: &[f32]) -> f32 {
    let mut maximum = 0.0f32;
    for (before, after) in samples.iter().zip(&samples[1..]) {
        maximum = maximum.max((after - before).abs());
    }
    maximum
}

/// 周期が探索の範囲にある音は補間され、生成した音は元の振幅と段差の範囲に収まる
///
/// 端数まで周期を求めて線形補間で繰り返すため、格子に乗らない周期 (4 kHz の間引き率 12 の
/// 倍数でない周期) でも位相が合い、継ぎ目で波形が飛ばない。振幅は元の音のピークを超えず、
/// 段差は元の音の最大段差の 2 倍以下になる (継ぎ目の段差は `is_seam_smooth` がこの倍率で
/// 制限する)。
#[test]
fn conceal_of_a_periodic_wave_keeps_the_amplitude_and_the_step() -> noprop::TestResult {
    // 48 kHz、20 ms のフレーム。探索するずらし幅は 4 kHz の 10〜40 (120〜480 サンプル)
    const SAMPLE_RATE: u32 = 48_000;
    const LENGTH: usize = 960;
    const HOLE_US: i64 = 40_000;
    // 探索の範囲にある周期 (120 = 下限、480 = 上限、160 / 320 = 4 kHz の格子に乗らない端数)
    const PERIODS: [usize; 5] = [120, 160, 240, 320, 480];
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        let period = PERIODS[noprop::sample_usize_in(ctx, 0..PERIODS.len())];
        let amplitude = noprop::sample_f32_in(ctx, 0.1, 1.0);
        let channel: Vec<f32> = (0..LENGTH)
            .map(|index| {
                if index % period < period / 2 {
                    amplitude
                } else {
                    -amplitude
                }
            })
            .collect();
        let requested = ((HOLE_US * i64::from(SAMPLE_RATE) + 500_000) / 1_000_000) as usize;
        let mut output = vec![0.0f32; requested];
        let channels: [&[f32]; 1] = [channel.as_slice()];
        let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
        let change = conceal(&channels, &mut outputs, SAMPLE_RATE, HOLE_US);
        assert_eq!(
            change, requested as isize,
            "周期 {period} の矩形波は補間すること"
        );
        // 生成した音は元の音の振幅を超えない (線形補間とゲインは振幅を増やさない)
        let peak = channel
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        for sample in &output[..requested] {
            assert!(sample.is_finite());
            assert!(
                sample.abs() <= peak,
                "振幅を超えた: sample={sample} peak={peak} period={period}"
            );
        }
        // 継ぎ目で波形が飛ばない (元の最大段差の 2 倍以下)
        let source_step = max_step(&channel);
        let generated_step = max_step(&output[..requested]);
        assert!(
            generated_step <= source_step * 2.0 + 1e-6,
            "段差が大きい: generated={generated_step} source={source_step} period={period}"
        );
        Ok(())
    })?;
    Ok(())
}

/// 時間伸長は実音の先頭と末尾を変えず、元の振幅を増やさず、増やした長さとバッファの契約を
/// 守る
#[test]
fn preemptive_expand_keeps_the_edges_and_the_peak() -> noprop::TestResult {
    const SAMPLE_RATE: u32 = 48_000;
    const LENGTH: usize = 960;
    const OLD_LENGTH: usize = 480;
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        // 周期のある音と無い音を混ぜる (どちらも非無音)
        let periodic = noprop::sample_bool(ctx);
        let amplitude = noprop::sample_f32_in(ctx, 0.1, 1.0);
        let period = if periodic {
            (TIME_STRETCH_MIN_LAG as f64 * 12.0) as usize
        } else {
            0
        };
        let channel: Vec<f32> = (0..LENGTH)
            .map(|index| {
                if periodic {
                    if index % period < period / 2 {
                        amplitude
                    } else {
                        -amplitude
                    }
                } else {
                    noprop::sample_f32_in(ctx, -amplitude, amplitude)
                }
            })
            .collect();
        let history: Vec<f32> = channel[..OLD_LENGTH].to_vec();
        let limit = sample_expansion_limit(ctx, LENGTH);
        let expansion = preemptively_expand(
            core::slice::from_ref(&channel),
            core::slice::from_ref(&history),
            SAMPLE_RATE,
            limit,
        );
        if expansion.added_samples == 0 {
            assert!(
                expansion.channels.is_empty(),
                "操作しなかったときは音を返さない"
            );
            Ok(())
        } else {
            let added = expansion.added_samples;
            assert!(added <= limit, "上限を超えて伸ばした");
            let output = &expansion.channels[0];
            assert_eq!(output.len(), LENGTH + added);
            assert_eq!(output[0], channel[0], "実音の先頭を変えない");
            assert_eq!(
                output[LENGTH + added - 1],
                channel[LENGTH - 1],
                "実音の末尾を変えない"
            );
            let peak = channel
                .iter()
                .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
            for sample in output {
                assert!(sample.is_finite());
                assert!(
                    sample.abs() <= peak + 1e-6,
                    "元の振幅を超えた: sample={sample} peak={peak}"
                );
            }
            Ok(())
        }
    })?;
    Ok(())
}

/// 対応外のサンプルレートと、増やせる長さが 0 のときは時間伸長しない
#[test]
fn preemptive_expand_does_nothing_when_unavailable() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(64, |ctx| {
        let length = noprop::sample_usize_in(ctx, 1..=960);
        let channel: Vec<f32> = (0..length)
            .map(|_| noprop::sample_f32_in(ctx, -1.0, 1.0))
            .collect();
        let history = vec![channel.clone()];
        // 対応外のレート (4 kHz の格子へ丸めてもフィルタの表に無い)
        let unavailable = if noprop::sample_bool(ctx) {
            96_000
        } else {
            22_050
        };
        assert!(!is_time_stretch_available(unavailable));
        let expansion = preemptively_expand(
            core::slice::from_ref(&channel),
            &history,
            unavailable,
            length,
        );
        assert_eq!(expansion.added_samples, 0, "対応外のレートでは伸ばさない");
        let expansion = preemptively_expand(&[channel], &history, 48_000, 0);
        assert_eq!(expansion.added_samples, 0, "上限が 0 なら伸ばさない");
        Ok(())
    })?;
    Ok(())
}
