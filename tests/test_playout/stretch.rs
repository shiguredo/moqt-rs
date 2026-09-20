//! 音声の波形の周期による時間圧縮・伸長のテスト
//!
//! 公開 API (`compress` / `expand`) の契約を確認する。

use shiguredo_moqt::playout::stretch::{
    TIME_STRETCH_MAX_LAG, TIME_STRETCH_MIN_LAG, compress, expand,
};

/// 周期 `period` の矩形波を `length` サンプル作る
fn square_wave(length: usize, period: usize, amplitude: f32) -> Vec<f32> {
    let mut samples = vec![0.0f32; length];
    for (index, sample) in samples.iter_mut().enumerate() {
        *sample = if index % period < period / 2 {
            amplitude
        } else {
            -amplitude
        };
    }
    samples
}

/// 周期ごとに振幅が 1 % ずつ違う矩形波を作る
///
/// 隣り合う周期の値がわずかに違うため、クロスフェードが恒等にならず、フェードの
/// 適用位置と重みを検証できる (前後の周期の相関は 0.9 以上に保たれる)。
fn varied_square_wave(length: usize, period: usize, amplitude: f32) -> Vec<f32> {
    let mut samples = vec![0.0f32; length];
    for (index, sample) in samples.iter_mut().enumerate() {
        let block = index / period;
        let scale = 1.0 + 0.01 * ((block % 4) as f32 - 1.5);
        let value = if index % period < period / 2 {
            amplitude
        } else {
            -amplitude
        };
        *sample = value * scale;
    }
    samples
}

/// 決定的な擬似乱数でノイズを作る (振幅は [-0.5, 0.5))
fn noise(length: usize) -> Vec<f32> {
    let mut state = 0x1234_5678_9abc_def0u64;
    let mut samples = vec![0.0f32; length];
    for sample in samples.iter_mut() {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *sample = (state >> 40) as f32 / (1u64 << 24) as f32 - 0.5;
    }
    samples
}

#[test]
fn compress_removes_one_pitch_period() {
    // 48 kHz、周期 240 サンプル (5 ms) の矩形波 (4 kHz では 20 サンプル)
    let original = square_wave(960, 240, 0.5);
    let mut channel = original.clone();
    let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
    let change = compress(&mut channels, 48_000);
    assert_eq!(change, -240);
    // 周期と同じ長さを削るため、使用する範囲 (720 サンプル) は元の波形のまま
    for index in 0..720 {
        assert!((channel[index] - original[index + 240]).abs() < 1e-6);
    }
}

#[test]
fn compress_mixes_splice_with_cross_fade() {
    // 周期ごとに値が違う波形で、クロスフェードの適用位置と重みを固定する
    let original = varied_square_wave(960, 240, 0.5);
    let mut channel = original.clone();
    let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
    let change = compress(&mut channels, 48_000);
    assert_eq!(change, -240);

    let removed = 240usize;
    let splice = 480usize;
    // 削る窓より手前は変わらない
    assert_eq!(&channel[..splice - removed], &original[..splice - removed]);
    // 切れ目の前後は重み (index + 1) / (removed + 1) で混ざる
    for index in 0..removed {
        let weight = (index as f32 + 1.0) / (removed as f32 + 1.0);
        let expected =
            (1.0 - weight) * original[splice - removed + index] + weight * original[splice + index];
        assert!(
            (channel[splice - removed + index] - expected).abs() < 1e-5,
            "index={index}"
        );
    }
    // 切れ目より後ろは削った分だけ前へ詰まる
    assert_eq!(
        &channel[splice..960 - removed],
        &original[splice + removed..960]
    );
}

#[test]
fn compress_with_mismatched_channel_lengths_is_ignored() {
    let mut left = square_wave(960, 240, 0.5);
    let mut right = square_wave(480, 240, 0.5);
    let original_left = left.clone();
    let original_right = right.clone();
    let mut channels: [&mut [f32]; 2] = [left.as_mut_slice(), right.as_mut_slice()];
    assert_eq!(compress(&mut channels, 48_000), 0);
    assert_eq!(left, original_left);
    assert_eq!(right, original_right);
}

#[test]
fn compress_with_short_input_is_ignored() {
    let mut channel = square_wave(100, 20, 0.5);
    let original = channel.clone();
    let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
    assert_eq!(compress(&mut channels, 48_000), 0);
    assert_eq!(channel, original);
}

#[test]
fn compress_with_period_longer_than_the_search_range_is_ignored() {
    // 20 ms (960 サンプル) のフレームでは 4 kHz の 29 サンプル (7.25 ms) までしか
    // 探索しないため、周期 480 サンプル (10 ms) の音は操作しない
    let mut channel = square_wave(960, 480, 0.5);
    let original = channel.clone();
    let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
    assert_eq!(compress(&mut channels, 48_000), 0);
    assert_eq!(channel, original);
}

#[test]
fn compress_with_unsupported_sample_rate_is_ignored() {
    let original = square_wave(960, 240, 0.5);
    let mut channel = original.clone();
    let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
    assert_eq!(compress(&mut channels, 44_100), 0);
    assert_eq!(channel, original);
}

#[test]
fn compress_with_empty_input_is_ignored() {
    let mut no_channels: [&mut [f32]; 0] = [];
    assert_eq!(compress(&mut no_channels, 48_000), 0);

    let mut channel: Vec<f32> = Vec::new();
    let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
    assert_eq!(compress(&mut channels, 48_000), 0);
}

#[test]
fn stereo_channels_are_stretched_together() {
    let original = square_wave(960, 240, 0.5);
    let mut left = original.clone();
    let mut right = original.clone();
    let mut channels: [&mut [f32]; 2] = [left.as_mut_slice(), right.as_mut_slice()];
    assert_eq!(compress(&mut channels, 48_000), -240);
    assert_eq!(left, right);
}

#[test]
fn pitch_period_must_fit_in_half_of_the_frame() {
    // 周期 720 サンプル (15 ms) が音の半分 (720) にちょうど収まるときは操作する
    let mut channel = square_wave(1_440, 720, 0.5);
    let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
    assert_eq!(compress(&mut channels, 48_000), -720);

    // 音の半分 (665) に収まらないときは操作しない
    let mut channel = square_wave(1_330, 720, 0.5);
    let original = channel.clone();
    let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
    assert_eq!(compress(&mut channels, 48_000), 0);
    assert_eq!(channel, original);
}

#[test]
fn analysis_boundary_of_the_downsampled_length_is_inclusive() {
    // 8 kHz: 間引き後 59 サンプルでは周期を求められない
    let mut channel = vec![0.0f32; 123];
    let original = channel.clone();
    let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
    assert_eq!(compress(&mut channels, 8_000), 0);
    assert_eq!(channel, original);

    // 8 kHz: 間引き後 60 サンプル (ずらし幅の上限が下限と同じ 10) では操作する
    let mut channel = vec![0.0f32; 124];
    let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
    assert_eq!(
        compress(&mut channels, 8_000),
        -(TIME_STRETCH_MIN_LAG as isize * 2)
    );
}

#[test]
fn noise_is_not_stretched() {
    let original = noise(960);
    let mut channel = original.clone();
    let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
    assert_eq!(compress(&mut channels, 48_000), 0);
    // 操作しなかったのでバッファは変わらない
    assert_eq!(channel, original);

    let mut output = vec![7.0f32; 960 + TIME_STRETCH_MAX_LAG * 12];
    let channels: [&[f32]; 1] = [original.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(expand(&channels, &mut outputs, 48_000), 0);
    // 操作しなかったので出力へは何も書かない
    assert!(output.iter().all(|sample| *sample == 7.0));
}

#[test]
fn silence_is_stretched() {
    // 無音は相関に関係なく操作される。ピッチは探索範囲の下限 (4 kHz の 10 サンプル)
    let mut channel = vec![0.0f32; 960];
    let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
    assert_eq!(
        compress(&mut channels, 48_000),
        -(TIME_STRETCH_MIN_LAG as isize * 12)
    );

    let input = vec![0.0f32; 960];
    let mut output = vec![0.0f32; 960 + TIME_STRETCH_MAX_LAG * 12];
    let channels: [&[f32]; 1] = [input.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(
        expand(&channels, &mut outputs, 48_000),
        TIME_STRETCH_MIN_LAG as isize * 12
    );
}

#[test]
fn expand_inserts_one_pitch_period() {
    let original = square_wave(960, 240, 0.5);
    let mut output = vec![0.0f32; 960 + TIME_STRETCH_MAX_LAG * 12];
    let channels: [&[f32]; 1] = [original.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    let change = expand(&channels, &mut outputs, 48_000);
    assert_eq!(change, 240);
    // 周期と同じ長さを挿すため、使用する範囲は元の波形のまま
    for index in 0..960 + 240 {
        assert!((output[index] - original[index % 240]).abs() < 1e-6);
    }
}

#[test]
fn expand_mixes_splice_with_cross_fade() {
    // 周期ごとに値が違う波形で、クロスフェードの適用位置と重みを固定する
    let original = varied_square_wave(960, 240, 0.5);
    let mut output = vec![0.0f32; 960 + TIME_STRETCH_MAX_LAG * 12];
    let channels: [&[f32]; 1] = [original.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    let change = expand(&channels, &mut outputs, 48_000);
    assert_eq!(change, 240);

    let added = 240usize;
    let splice = 480usize;
    // 切れ目までは変わらない
    assert_eq!(&output[..splice], &original[..splice]);
    // 挿した分の先頭は、切れ目の直前の音へ重み (index + 1) / (added + 1) で寄る
    for index in 0..added {
        let weight = (index as f32 + 1.0) / (added as f32 + 1.0);
        let expected =
            (1.0 - weight) * original[splice + index] + weight * original[splice - added + index];
        assert!(
            (output[splice + index] - expected).abs() < 1e-5,
            "index={index}"
        );
    }
    // 切れ目より後ろは挿した分だけ後ろへずれる
    assert_eq!(&output[splice + added..960 + added], &original[splice..960]);
}

#[test]
fn expand_with_mismatched_channel_lengths_is_ignored() {
    let left = square_wave(960, 240, 0.5);
    let right = square_wave(480, 240, 0.5);
    let mut output_left = vec![0.0f32; 960 + TIME_STRETCH_MAX_LAG * 12];
    let mut output_right = vec![0.0f32; 480 + TIME_STRETCH_MAX_LAG * 12];
    let channels: [&[f32]; 2] = [left.as_slice(), right.as_slice()];
    let mut outputs: [&mut [f32]; 2] = [output_left.as_mut_slice(), output_right.as_mut_slice()];
    assert_eq!(expand(&channels, &mut outputs, 48_000), 0);
    assert!(output_left.iter().all(|sample| *sample == 0.0));
    assert!(output_right.iter().all(|sample| *sample == 0.0));
}

#[test]
fn expand_with_different_output_channel_count_is_ignored() {
    let channel = square_wave(960, 240, 0.5);
    let mut output_left = vec![0.0f32; 960 + TIME_STRETCH_MAX_LAG * 12];
    let mut output_right = vec![0.0f32; 960 + TIME_STRETCH_MAX_LAG * 12];
    let channels: [&[f32]; 1] = [channel.as_slice()];
    let mut outputs: [&mut [f32]; 2] = [output_left.as_mut_slice(), output_right.as_mut_slice()];
    assert_eq!(expand(&channels, &mut outputs, 48_000), 0);
    assert!(output_left.iter().all(|sample| *sample == 0.0));
    assert!(output_right.iter().all(|sample| *sample == 0.0));
}

#[test]
fn expand_with_empty_channel_is_ignored() {
    let channels: [&[f32]; 0] = [];
    let mut outputs: [&mut [f32]; 0] = [];
    assert_eq!(expand(&channels, &mut outputs, 48_000), 0);

    let channel: Vec<f32> = Vec::new();
    let mut output: Vec<f32> = Vec::new();
    let channels: [&[f32]; 1] = [channel.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(expand(&channels, &mut outputs, 48_000), 0);
}

#[test]
fn expand_with_unsupported_sample_rate_is_ignored() {
    let channel = square_wave(960, 240, 0.5);
    let mut output = vec![0.0f32; 960 + TIME_STRETCH_MAX_LAG * 12];
    let channels: [&[f32]; 1] = [channel.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(expand(&channels, &mut outputs, 44_100), 0);
    assert!(output.iter().all(|sample| *sample == 0.0));
}

#[test]
fn expand_without_enough_output_is_ignored() {
    let original = square_wave(960, 240, 0.5);
    // 挿入される長さ (240) に足りない出力では操作しない
    let mut output = vec![0.0f32; 960 + 240 - 1];
    let channels: [&[f32]; 1] = [original.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(expand(&channels, &mut outputs, 48_000), 0);
    assert!(output.iter().all(|sample| *sample == 0.0));
}

#[test]
fn supported_sample_rates_are_stretched() {
    // (サンプルレート, 元の長さ, 周期, ピッチのサンプル数)
    let cases = [
        (8_000u32, 160usize, 40usize, 40isize),
        (16_000, 320, 80, 80),
        (32_000, 640, 160, 160),
        (48_000, 960, 240, 240),
    ];
    for (sample_rate, length, period, expected) in cases {
        let original = square_wave(length, period, 0.5);
        let mut channel = original.clone();
        let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
        assert_eq!(
            compress(&mut channels, sample_rate),
            -expected,
            "sample_rate={sample_rate}"
        );

        let mut output = vec![0.0f32; length + TIME_STRETCH_MAX_LAG * 12];
        let channels: [&[f32]; 1] = [original.as_slice()];
        let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
        assert_eq!(
            expand(&channels, &mut outputs, sample_rate),
            expected,
            "sample_rate={sample_rate}"
        );
    }
}
