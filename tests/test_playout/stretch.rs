//! 音声の波形の周期による時間圧縮・伸長と、欠落した区間の補間のテスト
//!
//! 公開 API (`compress` / `expand` / `conceal` / `concealment_end_gain`) の契約を確認する。

use shiguredo_moqt::playout::stretch::{
    TIME_STRETCH_MAX_CONCEAL_US, TIME_STRETCH_MAX_LAG, TIME_STRETCH_MIN_LAG, compress, conceal,
    concealment_end_gain, expand,
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

#[test]
fn conceal_repeats_the_tail_period() {
    // 48 kHz、周期 240 サンプル (5 ms) の矩形波。40 ms の隙間を 1920 サンプルで埋める
    let original = square_wave(960, 240, 0.5);
    let mut output = vec![0.0f32; 1_928];
    let channels: [&[f32]; 1] = [original.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(conceal(&channels, &mut outputs, 48_000, 40_000), 1_920);
    // 生成の先頭は、末尾の周期 1 つ分の先頭 (960 - 240) から始まる
    let end_gain = concealment_end_gain(40_000);
    for (index, sample) in output[..1_920].iter().enumerate() {
        let gain = 1.0 - (1.0 - end_gain) * ((index as f32 + 1.0) / 1_920.0);
        let expected = original[720 + index % 240] * gain;
        assert!((sample - expected).abs() < 1e-6, "index={index}");
    }
    // 埋める長さを超える範囲には書かない
    assert!(output[1_920..].iter().all(|sample| *sample == 0.0));
}

#[test]
fn conceal_uses_the_tail_period_when_the_earlier_part_differs() {
    // 前半だけ周期が違っても、末尾の周期 (240 サンプル) で繰り返す
    let mut original = square_wave(960, 240, 0.5);
    // 置き換えるのは 400 サンプルまでにする。末尾の周期を求める範囲 (最後の 2 周期分) に
    // 掛からない位置であり、間引きのフィルタの窓も越えない
    let first_part = square_wave(400, 260, 0.5);
    original[..400].copy_from_slice(&first_part);

    let mut output = vec![0.0f32; 1_920];
    let channels: [&[f32]; 1] = [original.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(conceal(&channels, &mut outputs, 48_000, 40_000), 1_920);
    let end_gain = concealment_end_gain(40_000);
    for (index, sample) in output.iter().enumerate() {
        let gain = 1.0 - (1.0 - end_gain) * ((index as f32 + 1.0) / 1_920.0);
        let expected = original[720 + index % 240] * gain;
        assert!((sample - expected).abs() < 1e-6, "index={index}");
    }
}

#[test]
fn conceal_fills_every_channel_from_its_own_tail() {
    let left = square_wave(960, 240, 0.5);
    let right = square_wave(960, 240, 0.25);
    let mut output_left = vec![0.0f32; 960];
    let mut output_right = vec![0.0f32; 960];
    let channels: [&[f32]; 2] = [left.as_slice(), right.as_slice()];
    let mut outputs: [&mut [f32]; 2] = [output_left.as_mut_slice(), output_right.as_mut_slice()];
    assert_eq!(conceal(&channels, &mut outputs, 48_000, 20_000), 960);
    // どちらのチャンネルも、自分の末尾の周期 1 つ分から埋める
    let end_gain = concealment_end_gain(20_000);
    let gain = 1.0 - (1.0 - end_gain) * (1.0 / 960.0);
    assert!((output_left[0] - left[720] * gain).abs() < 1e-6);
    assert!((output_right[0] - right[720] * gain).abs() < 1e-6);
}

#[test]
fn conceal_lowers_the_amplitude_towards_the_end() {
    let original = square_wave(960, 240, 0.5);
    let mut output = vec![0.0f32; 4_800];
    let channels: [&[f32]; 1] = [original.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    // 上限 (100 ms) まで埋めると、埋めた音の末尾の振幅は下限 (0.5) まで下がる
    assert_eq!(
        conceal(&channels, &mut outputs, 48_000, TIME_STRETCH_MAX_CONCEAL_US),
        4_800
    );
    let peak = |samples: &[f32]| {
        samples
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()))
    };
    let first = peak(&output[..100]);
    let last = peak(&output[4_700..]);
    assert!(last < first * 0.6, "first={first} last={last}");
}

#[test]
fn conceal_fills_silence_with_zeros() {
    // 無音に近い入力では相関が常に 0 になり周期が求まらない。周期の代わりに 0 を書き、
    // 埋めた長さは要求どおりにする (時間圧縮・時間伸長は無音でも操作するのと同じ扱い)
    let silence = vec![0.0f32; 960];
    let mut output = vec![7.0f32; 1_920];
    let channels: [&[f32]; 1] = [silence.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    // 48 kHz で 40 ms は 1_920 サンプル
    assert_eq!(
        conceal(&channels, &mut outputs, 48_000, 40_000),
        1_920,
        "無音でも要求ぶんを埋めること"
    );
    assert!(
        output.iter().all(|sample| *sample == 0.0),
        "無音の隙間は 0 で埋まること"
    );
}

#[test]
fn conceal_is_ignored_for_silence_with_a_short_output() {
    // 出力の長さが足りないときは、無音でも書かない
    let silence = vec![0.0f32; 960];
    let mut output = vec![7.0f32; 1_919];
    let channels: [&[f32]; 1] = [silence.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(
        conceal(&channels, &mut outputs, 48_000, 40_000),
        0,
        "出力が足りなければ埋めないこと"
    );
    assert!(output.iter().all(|sample| *sample == 7.0));
}

#[test]
fn conceal_is_ignored_for_noise() {
    // 波形が繰り返していない音 (相関が足りない音) では埋めない
    let original = noise(960);
    let mut output = vec![0.0f32; 1_920];
    let channels: [&[f32]; 1] = [original.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(conceal(&channels, &mut outputs, 48_000, 40_000), 0);
    assert!(output.iter().all(|sample| *sample == 0.0));
}

#[test]
fn conceal_is_ignored_for_an_inverted_tail() {
    // 末尾の 1 周期だけ位相を反転した音では、末尾の相関が足りないため埋めない
    let mut original = square_wave(960, 240, 0.5);
    for sample in &mut original[720..] {
        *sample = -*sample;
    }
    let mut output = vec![0.0f32; 1_920];
    let channels: [&[f32]; 1] = [original.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(conceal(&channels, &mut outputs, 48_000, 40_000), 0);
    assert!(output.iter().all(|sample| *sample == 0.0));
}

#[test]
fn conceal_is_ignored_when_the_seam_step_is_too_large() {
    // 単調なランプは末尾の相関が高いが、繰り返しの継ぎ目の段差が自然な段差より大きい
    let mut original = vec![0.0f32; 960];
    for (index, sample) in original.iter_mut().enumerate() {
        *sample = (index as f32 / 960.0) * 0.9 - 0.45;
    }
    let mut output = vec![0.0f32; 1_920];
    let channels: [&[f32]; 1] = [original.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(conceal(&channels, &mut outputs, 48_000, 40_000), 0);
    assert!(output.iter().all(|sample| *sample == 0.0));
}

#[test]
fn conceal_with_a_short_input_is_ignored() {
    // 8 kHz: 間引き後 18 サンプルでは末尾の 2 周期分 (下限の 2 倍) が取れない
    let original = square_wave(40, 20, 0.5);
    let mut output = vec![0.0f32; 160];
    let channels: [&[f32]; 1] = [original.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(conceal(&channels, &mut outputs, 8_000, 20_000), 0);
    assert!(output.iter().all(|sample| *sample == 0.0));
}

#[test]
fn conceal_with_unsupported_sample_rate_is_ignored() {
    let original = square_wave(960, 240, 0.5);
    let mut output = vec![0.0f32; 1_764];
    let channels: [&[f32]; 1] = [original.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(conceal(&channels, &mut outputs, 44_100, 40_000), 0);
    assert!(output.iter().all(|sample| *sample == 0.0));
}

#[test]
fn conceal_without_enough_output_is_ignored() {
    let original = square_wave(960, 240, 0.5);
    // 埋める長さ (1920) に 1 サンプル足りない出力では埋めない
    let mut output = vec![0.0f32; 1_919];
    let channels: [&[f32]; 1] = [original.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(conceal(&channels, &mut outputs, 48_000, 40_000), 0);
    assert!(output.iter().all(|sample| *sample == 0.0));
}

#[test]
fn conceal_with_empty_or_mismatched_input_is_ignored() {
    let no_channels: [&[f32]; 0] = [];
    let mut no_outputs: [&mut [f32]; 0] = [];
    assert_eq!(conceal(&no_channels, &mut no_outputs, 48_000, 40_000), 0);

    let empty: Vec<f32> = Vec::new();
    let mut output: Vec<f32> = Vec::new();
    let channels: [&[f32]; 1] = [empty.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(conceal(&channels, &mut outputs, 48_000, 40_000), 0);

    // 埋める長さが 0 以下では埋めない
    let original = square_wave(960, 240, 0.5);
    let mut output = vec![5.0f32; 1_920];
    let channels: [&[f32]; 1] = [original.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(conceal(&channels, &mut outputs, 48_000, 0), 0);
    assert_eq!(conceal(&channels, &mut outputs, 48_000, -1), 0);
    assert!(output.iter().all(|sample| *sample == 5.0));

    // チャンネルの長さが揃っていない
    let short = square_wave(480, 240, 0.5);
    let mut output_left = vec![0.0f32; 1_920];
    let mut output_right = vec![0.0f32; 960];
    let channels: [&[f32]; 2] = [original.as_slice(), short.as_slice()];
    let mut outputs: [&mut [f32]; 2] = [output_left.as_mut_slice(), output_right.as_mut_slice()];
    assert_eq!(conceal(&channels, &mut outputs, 48_000, 40_000), 0);
    assert!(output_left.iter().all(|sample| *sample == 0.0));
    assert!(output_right.iter().all(|sample| *sample == 0.0));

    // 出力の数が合わない
    let mut output_left = vec![0.0f32; 1_920];
    let mut output_right = vec![0.0f32; 1_920];
    let channels: [&[f32]; 1] = [original.as_slice()];
    let mut outputs: [&mut [f32]; 2] = [output_left.as_mut_slice(), output_right.as_mut_slice()];
    assert_eq!(conceal(&channels, &mut outputs, 48_000, 40_000), 0);
    assert!(output_left.iter().all(|sample| *sample == 0.0));
    assert!(output_right.iter().all(|sample| *sample == 0.0));
}

#[test]
fn supported_sample_rates_are_concealed() {
    // (サンプルレート, 元の長さ, 周期, 埋めるサンプル数)
    let cases = [
        (8_000u32, 160usize, 40usize, 160usize),
        (16_000, 320, 80, 320),
        (32_000, 640, 160, 640),
        (48_000, 960, 240, 960),
    ];
    for (sample_rate, length, period, expected) in cases {
        let original = square_wave(length, period, 0.5);
        let mut output = vec![0.0f32; expected];
        let channels: [&[f32]; 1] = [original.as_slice()];
        let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
        // 20 ms の隙間を埋める
        assert_eq!(
            conceal(&channels, &mut outputs, sample_rate, 20_000),
            expected as isize,
            "sample_rate={sample_rate}"
        );
        // 生成の先頭は、末尾の周期 1 つ分の先頭
        let end_gain = concealment_end_gain(20_000);
        let gain = 1.0 - (1.0 - end_gain) * (1.0 / expected as f32);
        assert!(
            (output[0] - original[length - period] * gain).abs() < 1e-6,
            "sample_rate={sample_rate}"
        );
    }
}

#[test]
fn concealment_end_gain_ramps_to_the_end_gain() {
    // 補間が長いほど末尾の振幅を下げる (上限の 100 ms で下限の 0.5 になる)
    assert!((concealment_end_gain(0) - 1.0).abs() < 1e-6);
    assert!((concealment_end_gain(-1) - 1.0).abs() < 1e-6);
    assert!((concealment_end_gain(TIME_STRETCH_MAX_CONCEAL_US / 2) - 0.75).abs() < 1e-6);
    assert!((concealment_end_gain(TIME_STRETCH_MAX_CONCEAL_US) - 0.5).abs() < 1e-6);
    // 上限を超える長さでも下限より下げない
    assert!((concealment_end_gain(TIME_STRETCH_MAX_CONCEAL_US * 2) - 0.5).abs() < 1e-6);
}
