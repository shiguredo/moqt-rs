//! 音声の波形の周期による時間圧縮と、欠落した区間の補間のテスト
//!
//! 公開 API (`compress` / `conceal` / `concealment_end_gain` / `is_time_stretch_available`)
//! の契約を確認する。補間では、4 kHz へ間引いた粗い格子の周辺を元のサンプルレートの相関で
//! 評価して端数まで周期を求めるため、格子に乗らない周期でも穴の終わりで位相がずれない。
//! 整数ラグが丸め誤差の範囲で完全相関のときは放物線の頂点へずらさず整数の周期を維持し、
//! 細探索の端で最大になったときは評価していない点で外挿しない。周期の推定が確かでない音では、
//! 生成した音の末尾を短くフェードして段差を消す。

use shiguredo_moqt::playout::stretch::{
    TIME_STRETCH_CONCEAL_FADE_MS, TIME_STRETCH_MAX_CONCEAL_US, TIME_STRETCH_MIN_LAG, compress,
    conceal, concealment_end_gain, is_time_stretch_available,
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

/// 正弦波の `index` 番目のサンプル (振幅 `amplitude`)
fn sine_sample(index: usize, frequency_hz: f64, sample_rate: u32, amplitude: f64) -> f32 {
    (amplitude
        * (2.0 * std::f64::consts::PI * frequency_hz * index as f64 / f64::from(sample_rate)).sin())
        as f32
}

/// 位相 `phase` (1 周期を 1 とした割合) を持つ正弦波の `index` 番目のサンプル (振幅 `amplitude`)
///
/// フレームの先頭の位相を変えても、周期を求める経路が位相に依存しないことを確かめるために使う。
fn sine_sample_with_phase(
    index: usize,
    frequency_hz: f64,
    sample_rate: u32,
    amplitude: f64,
    phase: f64,
) -> f32 {
    (amplitude
        * (2.0
            * std::f64::consts::PI
            * ((frequency_hz * index as f64) / f64::from(sample_rate) + phase))
            .sin()) as f32
}

/// 周波数 `frequency_hz` の正弦波を `length` サンプル作る
fn sine_wave(length: usize, frequency_hz: f64, sample_rate: u32, amplitude: f64) -> Vec<f32> {
    (0..length)
        .map(|index| sine_sample(index, frequency_hz, sample_rate, amplitude))
        .collect()
}

/// 基本周波数 `frequency_hz` の有声 (倍音を 1 / 次数で重ねた周期波) を 48 kHz の 20 ms で作る
///
/// 実際の声に近づけるため、整数次の倍音を 4 kHz (音声の帯域) まで重ねる。振幅は 0.5 に揃える。
fn voiced_wave(frequency_hz: f64) -> Vec<f32> {
    const MAX_HARMONIC_HZ: f64 = 4_000.0;
    let mut norm = 0.0;
    let mut harmonic = 1;
    while harmonic as f64 * frequency_hz < MAX_HARMONIC_HZ {
        norm += 1.0 / harmonic as f64;
        harmonic += 1;
    }
    (0..FRAME_SAMPLES)
        .map(|index| {
            let mut value = 0.0;
            let mut harmonic = 1;
            while harmonic as f64 * frequency_hz < MAX_HARMONIC_HZ {
                value += (1.0 / harmonic as f64)
                    * (2.0 * std::f64::consts::PI * harmonic as f64 * frequency_hz * index as f64
                        / f64::from(SAMPLE_RATE))
                    .sin();
                harmonic += 1;
            }
            ((0.5 * value) / norm) as f32
        })
        .collect()
}

/// 1 極フィルタ (係数 `coefficient`) を通した有色雑音のフレームを作る
///
/// 白色雑音を `y[n] = a * y[n-1] + (1 - a) * x[n]` でならしたもの。相関は `a` のラグ乗で
/// 減るため、周期の探索が短いずらし幅を選ぶと 400 Hz のブザーになる。moqt-js の実測と
/// 同じ音にするため、乱数は moqt-js と同じ合同式 (`state = state * 1103515245 + 12345` を
/// 2^31 で割った余り) を f64 で計算する。
fn colored_noise_frame(length: usize, coefficient: f64, seed: f64) -> Vec<f32> {
    let mut state = seed;
    let mut previous = 0.0f64;
    let mut samples = vec![0.0f32; length];
    for sample in samples.iter_mut() {
        state = (state * 1_103_515_245.0 + 12_345.0) % 2_147_483_648.0;
        let white = (state / 2_147_483_648.0) * 2.0 - 1.0;
        previous = coefficient * previous + (1.0 - coefficient) * white;
        *sample = previous as f32;
    }
    samples
}

/// 純音に白色雑音を混ぜたフレームを作る (周期の推定が確かでない音)
fn noisy_sine_wave(frequency_hz: f64, noise_amplitude: f64) -> Vec<f32> {
    let mut samples = sine_wave(FRAME_SAMPLES, frequency_hz, SAMPLE_RATE, 0.5);
    let mut state = 987_654_321.0f64;
    for sample in samples.iter_mut() {
        state = (state * 1_103_515_245.0 + 12_345.0) % 2_147_483_648.0;
        *sample =
            (*sample as f64 + ((state / 2_147_483_648.0) * 2.0 - 1.0) * noise_amplitude) as f32;
    }
    samples
}

/// 隣り合うサンプルの差の最大値 (波形の連続性)
fn max_step(samples: &[f32]) -> f32 {
    let mut maximum = 0.0f32;
    for (before, after) in samples.iter().zip(&samples[1..]) {
        maximum = maximum.max((after - before).abs());
    }
    maximum
}

/// 生成した音が `period` サンプルの周期で繰り返しているか
///
/// 先頭から半分の範囲だけで比べる (末尾は短くフェードすることがあり、位相の検査には
/// 使えないため)。補間した音には長さに応じて振幅を下げるゲインが掛かるため、ゲインを
/// 戻してから比べる。
fn repeats_at_period(generated: &[f32], period: usize, conceal_us: i64, tolerance: f32) -> bool {
    let target = generated.len();
    // 周期が生成した長さを超えると比較する位置が無い (`target - period` が負になる)。呼び出し
    // 側の取り違えを、比較を飛ばして黙って通さないよう、ここで落とす
    assert!(
        period <= target,
        "周期が生成した長さを超えている: period={period} len={target}"
    );
    let end_gain = concealment_end_gain(conceal_us);
    for index in 0..(target / 2).min(target - period) {
        let gain_before = 1.0 - (1.0 - end_gain) * ((index as f32 + 1.0) / target as f32);
        let gain_after = 1.0 - (1.0 - end_gain) * (((index + period) as f32 + 1.0) / target as f32);
        let before = generated[index] / gain_before;
        let after = generated[index + period] / gain_after;
        if (after - before).abs() > tolerance {
            return false;
        }
    }
    true
}

#[test]
fn compress_removes_one_pitch_period() {
    // 48 kHz、周期 240 サンプル (5 ms) の矩形波 (4 kHz では 20 サンプル)
    let original = square_wave(960, 240, 0.5);
    let mut channel = original.clone();
    let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
    let change = compress(&mut channels, 48_000, usize::MAX);
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
    let change = compress(&mut channels, 48_000, usize::MAX);
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
    assert_eq!(compress(&mut channels, 48_000, usize::MAX), 0);
    assert_eq!(left, original_left);
    assert_eq!(right, original_right);
}

#[test]
fn compress_with_short_input_is_ignored() {
    let mut channel = square_wave(100, 20, 0.5);
    let original = channel.clone();
    let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
    assert_eq!(compress(&mut channels, 48_000, usize::MAX), 0);
    assert_eq!(channel, original);
}

#[test]
fn compress_with_period_longer_than_the_search_range_is_ignored() {
    // 20 ms (960 サンプル) のフレームでは 4 kHz の 29 サンプル (7.25 ms) までしか
    // 探索しないため、周期 480 サンプル (10 ms) の音は操作しない
    let mut channel = square_wave(960, 480, 0.5);
    let original = channel.clone();
    let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
    assert_eq!(compress(&mut channels, 48_000, usize::MAX), 0);
    assert_eq!(channel, original);
}

#[test]
fn compress_with_unsupported_sample_rate_is_ignored() {
    let original = square_wave(960, 240, 0.5);
    let mut channel = original.clone();
    let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
    assert_eq!(compress(&mut channels, 44_100, usize::MAX), 0);
    assert_eq!(channel, original);
}

#[test]
fn compress_with_empty_input_is_ignored() {
    let mut no_channels: [&mut [f32]; 0] = [];
    assert_eq!(compress(&mut no_channels, 48_000, usize::MAX), 0);

    let mut channel: Vec<f32> = Vec::new();
    let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
    assert_eq!(compress(&mut channels, 48_000, usize::MAX), 0);
}

#[test]
fn stereo_channels_are_stretched_together() {
    let original = square_wave(960, 240, 0.5);
    let mut left = original.clone();
    let mut right = original.clone();
    let mut channels: [&mut [f32]; 2] = [left.as_mut_slice(), right.as_mut_slice()];
    assert_eq!(compress(&mut channels, 48_000, usize::MAX), -240);
    assert_eq!(left, right);
}

#[test]
fn pitch_period_must_fit_in_half_of_the_frame() {
    // 周期 720 サンプル (15 ms) が音の半分 (720) にちょうど収まるときは操作する
    let mut channel = square_wave(1_440, 720, 0.5);
    let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
    assert_eq!(compress(&mut channels, 48_000, usize::MAX), -720);

    // 音の半分 (665) に収まらないときは操作しない
    let mut channel = square_wave(1_330, 720, 0.5);
    let original = channel.clone();
    let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
    assert_eq!(compress(&mut channels, 48_000, usize::MAX), 0);
    assert_eq!(channel, original);
}

#[test]
fn analysis_boundary_of_the_downsampled_length_is_inclusive() {
    // 8 kHz: 間引き後 59 サンプルでは周期を求められない
    let mut channel = vec![0.0f32; 123];
    let original = channel.clone();
    let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
    assert_eq!(compress(&mut channels, 8_000, usize::MAX), 0);
    assert_eq!(channel, original);

    // 8 kHz: 間引き後 60 サンプル (ずらし幅の上限が下限と同じ 10) では操作する
    let mut channel = vec![0.0f32; 124];
    let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
    assert_eq!(
        compress(&mut channels, 8_000, usize::MAX),
        -(TIME_STRETCH_MIN_LAG as isize * 2)
    );
}

#[test]
fn noise_is_not_stretched() {
    let original = noise(960);
    let mut channel = original.clone();
    let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
    assert_eq!(compress(&mut channels, 48_000, usize::MAX), 0);
    // 操作しなかったのでバッファは変わらない
    assert_eq!(channel, original);
}

#[test]
fn silence_is_stretched() {
    // 無音は相関に関係なく操作される。ピッチは探索範囲の下限 (4 kHz の 10 サンプル)
    let mut channel = vec![0.0f32; 960];
    let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
    assert_eq!(
        compress(&mut channels, 48_000, usize::MAX),
        -(TIME_STRETCH_MIN_LAG as isize * 12)
    );
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
        let mut channel = square_wave(length, period, 0.5);
        let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
        assert_eq!(
            compress(&mut channels, sample_rate, usize::MAX),
            -expected,
            "sample_rate={sample_rate}"
        );
    }
}

/// 実リレーの E2E が使う音と同じ条件 (48 kHz、20 ms のフレーム)
const SAMPLE_RATE: u32 = 48_000;
/// 20 ms のフレームのサンプル数
const FRAME_SAMPLES: usize = 960;
/// 補間する穴の長さ (20 ms)
const HOLE_US: i64 = 20_000;

/// 周波数 `frequency_hz` の正弦波 (20 ms、48 kHz、振幅 0.5) から `hole_us` の穴を補間した
/// サンプル数
///
/// 実リレーの E2E が使う音は正弦波であるため、補間の可否を周波数ごとに見るために使う。
fn conceal_for_frequency(frequency_hz: f64, hole_us: i64) -> isize {
    let frame = sine_wave(FRAME_SAMPLES, frequency_hz, SAMPLE_RATE, 0.5);
    let target = ((hole_us * i64::from(SAMPLE_RATE) + 500_000) / 1_000_000) as usize;
    let mut output = vec![0.0f32; target];
    let channels: [&[f32]; 1] = [frame.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    conceal(&channels, &mut outputs, SAMPLE_RATE, hole_us)
}

/// 穴の終わりの段差を、振幅で正規化して返す
///
/// 穴の終わりでは、次に鳴る音が本来の続き (穴の分だけ進んだ音) であるとして、生成した音の
/// 最後のサンプルとの差を測る。段差が大きいとクリックとして聞こえる。
///
/// 生成した音には、長さに応じて末尾の振幅を [`concealment_end_gain`] まで下げるゲインが
/// 掛かる (これは周期のずれとは別の、意図した減衰である)。段差の原因が周期のずれかどうかを
/// 見るため、ゲインを戻してから比べる。最後のサンプルに掛かるゲインはちょうど
/// [`concealment_end_gain`] になる。
fn end_step_ratio(frequency_hz: f64, hole_us: i64) -> f64 {
    const AMPLITUDE: f64 = 0.5;
    let frame = sine_wave(FRAME_SAMPLES, frequency_hz, SAMPLE_RATE, AMPLITUDE);
    let target = ((hole_us * i64::from(SAMPLE_RATE) + 500_000) / 1_000_000) as usize;
    let mut output = vec![0.0f32; target];
    let channels: [&[f32]; 1] = [frame.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(
        conceal(&channels, &mut outputs, SAMPLE_RATE, hole_us),
        target as isize,
        "{frequency_hz} Hz、穴 {hole_us} µs を補間すること"
    );
    let last = f64::from(output[target - 1]) / f64::from(concealment_end_gain(hole_us));
    let next = f64::from(sine_sample(
        FRAME_SAMPLES + target,
        frequency_hz,
        SAMPLE_RATE,
        AMPLITUDE,
    ));
    (last - next).abs() / AMPLITUDE
}

#[test]
fn conceal_finds_the_tail_period_at_the_full_sample_rate() {
    // 補間の周期は、4 kHz へ間引いた粗い格子で探したあと、その周辺 (±1 格子) を元の
    // サンプルレートの相関で評価し、ピークを放物線補間して端数まで求める。そのため格子に
    // 乗らない純音でも、格子の倍数で繰り返すのではなく本当の周期で繰り返し、穴の終わりで
    // 位相が合う。
    //
    // 探索するずらし幅の上限は、窓の長さ (4 kHz の 39 サンプル) を引いた範囲で最大化する。
    // 20 ms のフレームは 48 kHz では 4 kHz へ間引くと 79 サンプルであり、ずらし幅は
    // 10〜40 (2.5 ms〜10 ms = 400 Hz〜100 Hz) になる。
    //
    // 実測 (48 kHz、20 ms のフレーム、振幅 0.5、20 ms の穴): 1000 / 440 / 300 / 220 / 150 /
    // 100 Hz のすべてで 960 サンプルを生成する。格子のままで末尾の 2 周期分だけを探して
    // いたときは 1000 Hz と 440 Hz だけが 960 で、300 / 220 / 150 / 100 Hz は 0 だった
    // (求めた周期が真の周期とずれ、継ぎ目の段差が自然な段差の 2 倍を超えていたため)。
    for frequency_hz in [1_000.0, 440.0, 300.0, 220.0, 150.0, 100.0] {
        assert_eq!(
            conceal_for_frequency(frequency_hz, HOLE_US),
            960,
            "{frequency_hz} Hz の純音を補間すること"
        );
    }
}

#[test]
fn conceal_keeps_the_exact_integer_period_of_the_lowest_pitch() {
    // 探索の下限 (4 kHz の 10 サンプル = 400 Hz、48 kHz で周期 120 サンプル) は 4 kHz の
    // 格子にちょうど乗る。細探索の範囲は下限の 1 サンプル外側 (119) まで広げるため、最大の
    // 120 は範囲の内側になり、前後の 119 / 121 も実際に評価される。整数ラグが丸め誤差の
    // 範囲で完全相関のときは放物線の頂点へずらさない。有限窓の前後の相関は位相によって
    // 非対称になり得るため、正しい整数周期を頂点へずらすと長い補間で位相がずれる。
    //
    // 実測 (48 kHz、20 ms のフレーム、振幅 0.5、20 ms の穴): 周期は 120 サンプルのままで、
    // 生成の先頭は末尾の 1 周期前のサンプルと一致する (差 6.1e-19)。120 サンプルで繰り返す。
    let frame = sine_wave(FRAME_SAMPLES, 400.0, SAMPLE_RATE, 0.5);
    let mut output = vec![0.0f32; 960];
    let channels: [&[f32]; 1] = [frame.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(conceal(&channels, &mut outputs, SAMPLE_RATE, HOLE_US), 960);
    assert!(
        (output[0] - frame[FRAME_SAMPLES - 120]).abs() < 1e-6,
        "先頭は末尾の 1 周期前のサンプルであること: {} 対 {}",
        output[0],
        frame[FRAME_SAMPLES - 120]
    );
    assert!(
        repeats_at_period(&output, 120, HOLE_US, 1e-4),
        "120 サンプルの周期で繰り返すこと"
    );
}

#[test]
fn conceal_preserves_an_exact_integer_period_for_any_phase() {
    // 400 Hz は 4 kHz の格子にちょうど乗り、対応するどのレートでも周期が整数になる
    // (8 kHz で 20、16 kHz で 40、32 kHz で 80、48 kHz で 120 サンプル)。整数ラグが丸め
    // 誤差の範囲で完全相関のときは、放物線の頂点へずらさず整数の周期を維持する。有限窓の
    // 前後の相関は位相によって非対称になり得るため、頂点へずらすと長い補間で位相がずれる。
    //
    // moqt-js の PBT と同じく、フレーム先頭の位相を 0〜99 の 100 通り変えて確かめる。実測
    // (各レート、20 ms のフレーム、振幅 0.5、100 ms の穴): 生成する長さは 8 / 16 / 32 / 48 kHz
    // で 800 / 1600 / 3200 / 4800 サンプルであり、理想の続き (フレームの続きの正弦波に末尾の
    // ゲインを掛けたもの) との最大差は 1.1e-14 である。
    for sample_rate in [8_000u32, 16_000, 32_000, 48_000] {
        let length = (sample_rate / 50) as usize;
        let target = (sample_rate / 10) as usize;
        for phase_percent in 0..100 {
            let sample = |index: usize| {
                sine_sample_with_phase(index, 400.0, sample_rate, 0.5, phase_percent as f64 / 100.0)
            };
            let reference: Vec<f32> = (0..length).map(sample).collect();
            let mut output = vec![0.0f32; target];
            let channels: [&[f32]; 1] = [reference.as_slice()];
            let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
            assert_eq!(
                conceal(&channels, &mut outputs, sample_rate, 100_000),
                target as isize,
                "sample_rate={sample_rate} phase={phase_percent} の純音を補間すること"
            );
            // 生成した音は、末尾のゲインを掛けた理想の続きと一致する
            let end_gain = concealment_end_gain(100_000);
            let mut maximum_error = 0.0f32;
            for (index, generated) in output.iter().enumerate() {
                let gain = 1.0 - (1.0 - end_gain) * ((index as f32 + 1.0) / target as f32);
                let expected = sample(length + index) * gain;
                maximum_error = maximum_error.max((generated - expected).abs());
            }
            assert!(
                maximum_error < 1e-5,
                "sample_rate={sample_rate} phase={phase_percent} で整数周期をずらさないこと: 最大差 {maximum_error}"
            );
        }
    }
}

#[test]
fn conceal_does_not_fabricate_a_vertex_at_the_search_boundary() {
    // 4 kHz の下限 (400 Hz = 120 サンプル) のすぐ上にピッチがある音では、細探索の最大が
    // 探索範囲の下端に来る。真の周期 119 サンプル (403.4 Hz) の純音は、細探索を 1 サンプル
    // 外側へ広げた 119 サンプルで相関が完全になる。広げていなければ探索の下限は 120 なので、
    // 周期は 120 サンプルへ丸められていた。
    //
    // 周期をさらに短い 118.6 サンプル (404.7 Hz) にすると、119 サンプルの相関が最大だが
    // 完全相関ではない。このとき、評価していない 118 サンプルの相関を最大値で代用して
    // 放物線の頂点を求めると、架空の頂点 118.5 サンプルを返してしまう。実際に評価した点
    // だけで補間する (両隣が無いときは補間しない) ため、周期は整数の 119 サンプルになる。
    //
    // 実測 (48 kHz、20 ms のフレーム、振幅 0.5、20 ms の穴): 生成の先頭は末尾の 119 サンプル
    // 前のサンプルと一致する (差 0)。架空の頂点 118.5 サンプルなら先頭は 841 と 842 の
    // 中間 (差 0.011) になる。119 サンプルで繰り返し、120 サンプルでは繰り返さない。
    for period in [119.0f64, 118.6] {
        let frequency_hz = f64::from(SAMPLE_RATE) / period;
        let frame = sine_wave(FRAME_SAMPLES, frequency_hz, SAMPLE_RATE, 0.5);
        let mut output = vec![0.0f32; 960];
        let channels: [&[f32]; 1] = [frame.as_slice()];
        let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
        assert_eq!(
            conceal(&channels, &mut outputs, SAMPLE_RATE, HOLE_US),
            960,
            "周期 {period} サンプルの純音を補間すること"
        );
        let end_gain = concealment_end_gain(HOLE_US);
        let gain = 1.0 - (1.0 - end_gain) * (1.0 / 960.0);
        let expected = frame[FRAME_SAMPLES - 119] * gain;
        assert!(
            (output[0] - expected).abs() < 1e-6,
            "周期 {period} サンプルでは先頭が末尾の 119 サンプル前のサンプルであること: {} 対 {expected}",
            output[0]
        );
        // 架空の頂点 (118.5 サンプル) を返していたら、先頭は 841 と 842 の中間になる
        let fabricated = 0.5 * (frame[FRAME_SAMPLES - 119] + frame[FRAME_SAMPLES - 118]) * gain;
        assert!(
            (output[0] - fabricated).abs() > 1e-3,
            "架空の頂点を作らないこと: {} 対 {fabricated}",
            output[0]
        );
        assert!(
            repeats_at_period(&output, 119, HOLE_US, 1e-6),
            "周期 {period} サンプルでは 119 サンプルで繰り返すこと"
        );
        assert!(
            !repeats_at_period(&output, 120, HOLE_US, 1e-6),
            "周期 {period} サンプルでは 120 サンプルで繰り返さないこと"
        );
    }
}

#[test]
fn conceal_removes_the_step_at_the_end_of_a_long_hole() {
    // 格子に乗らない 440 Hz (周期 109.09 サンプル) では、求めた周期が真の周期とずれると
    // 穴が長いほど位相がずれ、次の音との段差が大きくなる。端数まで周期を求めると段差は
    // 振幅の 10 % 以下に収まる。
    //
    // 実測 (48 kHz、20 ms のフレーム、振幅 0.5、穴の終わりの段差 ÷ 振幅): 4 kHz の格子の
    // 周期 (108 サンプル) で繰り返していたときは 20 ms で 34.4 %、40 ms で 101.8 %、
    // 100 ms で 195.1 % だった (moqt-js の実測)。端数まで求めると 4.7 / 4.5 / 1.6 % に
    // なる。4 kHz の格子に乗る 200 Hz (周期 240 サンプル) は 2.6 % である。
    for (frequency_hz, hole_us) in [
        (440.0, 20_000),
        (440.0, 40_000),
        (440.0, 100_000),
        (200.0, 100_000),
    ] {
        let ratio = end_step_ratio(frequency_hz, hole_us);
        assert!(
            ratio <= 0.1,
            "{frequency_hz} Hz、穴 {hole_us} µs の段差が振幅の 10 % 以下であること: {ratio}"
        );
    }
}

#[test]
fn conceal_finds_the_fractional_period_of_a_low_pitch() {
    // 有声の低いピッチでは、窓の長さ + ずらし幅がフレームに収まる範囲までずらし幅を広げ
    // (20 ms のフレームでは 4 kHz の 40 サンプル = 100 Hz まで)、端数まで周期を求める。
    //
    // 末尾の周期は端数を含む。100 Hz の有声では真の 480 サンプルに対して 479.9 サンプルに
    // なる (moqt-js の実測)。120 Hz は 400.0 サンプルである。4 kHz の格子だけで探して
    // いたときは 120 Hz が 396 であり、100 Hz は探索の上限 (137.9 Hz) に届かず補間そのものを
    // しなかった。
    for (frequency_hz, period) in [(100.0, 480usize), (120.0, 400)] {
        let frame = voiced_wave(frequency_hz);
        let mut output = vec![0.0f32; 1_920];
        let channels: [&[f32]; 1] = [frame.as_slice()];
        let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
        assert_eq!(
            conceal(&channels, &mut outputs, SAMPLE_RATE, 40_000),
            1_920,
            "{frequency_hz} Hz の有声を補間すること"
        );
        assert!(
            repeats_at_period(&output, period, 40_000, 0.05),
            "{frequency_hz} Hz が本当の周期 {period} サンプルで繰り返すこと"
        );
        assert!(
            !repeats_at_period(&output, 396, 40_000, 0.05),
            "{frequency_hz} Hz が 4 kHz の格子の周期 396 サンプルで繰り返さないこと"
        );
    }
}

#[test]
fn conceal_repeats_the_tail_period_with_the_fractional_phase() {
    // 48 kHz、200 Hz (周期 240 サンプル) の正弦波。40 ms の隙間を 1920 サンプルで埋める。
    // 求めた周期は端数 (240.0002 サンプル) を含むため、生成した音は末尾の周期 1 つ分を
    // 線形補間で繰り返したものになる。位相が合っていることを、穴の続きになる理想の音
    // (フレームの続きの正弦波) と比べて確かめる (実測の最大差は 1.6e-5)。
    let original = sine_wave(FRAME_SAMPLES, 200.0, SAMPLE_RATE, 0.5);
    let mut output = vec![0.0f32; 1_928];
    let channels: [&[f32]; 1] = [original.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(conceal(&channels, &mut outputs, 48_000, 40_000), 1_920);
    let end_gain = concealment_end_gain(40_000);
    for (index, sample) in output[..1_920].iter().enumerate() {
        let gain = 1.0 - (1.0 - end_gain) * ((index as f32 + 1.0) / 1_920.0);
        let expected = sine_sample(FRAME_SAMPLES + index, 200.0, SAMPLE_RATE, 0.5) * gain;
        assert!((sample - expected).abs() < 1e-4, "index={index}");
    }
    // 埋める長さを超える範囲には書かない
    assert!(output[1_920..].iter().all(|sample| *sample == 0.0));
}

#[test]
fn conceal_uses_the_tail_period_when_the_earlier_part_differs() {
    // 前半だけ周期が違っても、末尾の周期 (240 サンプル) で繰り返す。ただし周期の変わる位置が
    // 末尾から「相関の窓 (4 kHz の 39 サンプル = 48 kHz で 468 サンプル) + 1 周期」より近いと、
    // 末尾が繰り返しているかを確かめられないため補間しない。
    //
    // 実測 (48 kHz、40 ms の穴): 先頭 240 サンプルを周期 260 へ変えた音は補間し、480 サンプルを
    // 変えた音は相関が 0.83 になり補間しない。
    let mut original = sine_wave(FRAME_SAMPLES, 200.0, SAMPLE_RATE, 0.5);
    for (index, sample) in original.iter_mut().take(240).enumerate() {
        *sample = (0.5 * (2.0 * std::f64::consts::PI * index as f64 / 260.0).sin()) as f32;
    }
    let mut output = vec![0.0f32; 1_920];
    let channels: [&[f32]; 1] = [original.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(
        conceal(&channels, &mut outputs, SAMPLE_RATE, 40_000),
        1_920,
        "末尾の周期で補間すること"
    );
    // 継ぎ目は末尾の周期の自然な段差と同程度になる
    assert!(
        max_step(&output[..1_920]) <= max_step(&original[720..]) * 1.2,
        "波形が飛ばないこと"
    );

    // 周期の変わる位置が末尾に近すぎると、末尾が繰り返しているかを確かめられない
    let mut late = sine_wave(FRAME_SAMPLES, 200.0, SAMPLE_RATE, 0.5);
    for (index, sample) in late.iter_mut().take(480).enumerate() {
        *sample = (0.5 * (2.0 * std::f64::consts::PI * index as f64 / 260.0).sin()) as f32;
    }
    let mut output = vec![0.0f32; 1_920];
    let channels: [&[f32]; 1] = [late.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(
        conceal(&channels, &mut outputs, SAMPLE_RATE, 40_000),
        0,
        "繰り返しを確かめられない音では補間しないこと"
    );
    assert!(output.iter().all(|sample| *sample == 0.0));
}

#[test]
fn conceal_fills_every_channel_from_its_own_tail() {
    // 各チャンネルは自分の末尾の周期 1 つ分から埋める (周期は第 1 チャンネルだけで決める)
    let left = sine_wave(FRAME_SAMPLES, 200.0, SAMPLE_RATE, 0.5);
    let right = sine_wave(FRAME_SAMPLES, 200.0, SAMPLE_RATE, 0.25);
    let mut output_left = vec![0.0f32; 960];
    let mut output_right = vec![0.0f32; 960];
    let channels: [&[f32]; 2] = [left.as_slice(), right.as_slice()];
    let mut outputs: [&mut [f32]; 2] = [output_left.as_mut_slice(), output_right.as_mut_slice()];
    assert_eq!(conceal(&channels, &mut outputs, 48_000, 20_000), 960);
    // 生成の先頭は、末尾の周期 1 つ分の先頭 (端数を含む位置) から始まる
    let end_gain = concealment_end_gain(20_000);
    let gain = 1.0 - (1.0 - end_gain) * (1.0 / 960.0);
    assert!((output_left[0] - left[720] * gain).abs() < 1e-4);
    assert!((output_right[0] - right[720] * gain).abs() < 1e-4);
}

#[test]
fn conceal_checks_the_seam_of_every_channel() {
    // 周期は第 1 チャンネルだけで決めるが、繰り返しの継ぎ目の段差はどのチャンネルでも
    // クリックになるため、全チャンネルで確かめる。
    //
    // 実測 (48 kHz、40 ms の穴): 第 1 チャンネルが 5 ms の正弦で第 2 チャンネルがランプの
    // とき、第 1 チャンネルだけを見ていたときは 1920 サンプル生成し、全チャンネルを見ると
    // 生成しない。
    let sine = sine_wave(FRAME_SAMPLES, 200.0, SAMPLE_RATE, 0.5);
    let ramp: Vec<f32> = (0..FRAME_SAMPLES)
        .map(|index| (index as f32 / FRAME_SAMPLES as f32) * 0.9 - 0.45)
        .collect();
    // 第 1 チャンネルだけなら補間する
    let mut output = vec![0.0f32; 1_920];
    let channels: [&[f32]; 1] = [sine.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(conceal(&channels, &mut outputs, SAMPLE_RATE, 40_000), 1_920);
    // 第 2 チャンネルの継ぎ目の段差が大きいと補間しない
    let mut output_left = vec![0.0f32; 1_920];
    let mut output_right = vec![0.0f32; 1_920];
    let channels: [&[f32]; 2] = [sine.as_slice(), ramp.as_slice()];
    let mut outputs: [&mut [f32]; 2] = [output_left.as_mut_slice(), output_right.as_mut_slice()];
    assert_eq!(
        conceal(&channels, &mut outputs, SAMPLE_RATE, 40_000),
        0,
        "第 2 チャンネルの段差で補間しないこと"
    );
    assert!(output_left.iter().all(|sample| *sample == 0.0));
    assert!(output_right.iter().all(|sample| *sample == 0.0));
}

#[test]
fn conceal_does_not_choose_the_shortest_lag_for_colored_noise() {
    // 相関の窓を候補のずらし幅に比例させると、窓が短い (= ずらし幅が短い) 候補ほど相関が
    // 高く出て、有色雑音で最短のずらし幅 (4 kHz の 10 サンプル = 120 サンプル = 400 Hz) を
    // 選び、ブザーになる。窓を固定長 (4 kHz の 39 サンプル) にすると誤検出が消える。
    //
    // 実測 (48 kHz、20 ms のフレーム、40 ms の穴、40 フレームずつ): 補間が働いたフレームは
    // 係数 0.99 が 0 件、0.999 が 4 件であり、いずれも最短のずらし幅では繰り返していない。
    // 窓を比例させていたときの moqt-js の実測では、補間が 5.5 % / 19.5 % のフレームで働き、
    // 最短のずらし幅を選んだフレームが 6 件あった。
    for coefficient in [0.99f64, 0.999] {
        let mut concealed = 0;
        for frame in 0..40 {
            let samples = colored_noise_frame(
                FRAME_SAMPLES,
                coefficient,
                12_345.0 + frame as f64 * 7_919.0,
            );
            let mut output = vec![0.0f32; 1_920];
            let channels: [&[f32]; 1] = [samples.as_slice()];
            let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
            if conceal(&channels, &mut outputs, SAMPLE_RATE, 40_000) == 0 {
                continue;
            }
            concealed += 1;
            assert!(
                !repeats_at_period(&output, 120, 40_000, 1e-6),
                "係数 {coefficient} の {frame} 番目が最短のずらし幅で繰り返している"
            );
        }
        // 補間が働くのは稀である。実測 (0.99 が 0 件、0.999 が 4 件) に 1 件の余裕を足した
        // 上限で固定する。窓を比例させていた旧挙動 (5.5 % / 19.5 % = 2 件 / 8 件) に戻れば
        // どちらも超える
        let limit = if coefficient >= 0.999 { 5 } else { 1 };
        assert!(
            concealed <= limit,
            "係数 {coefficient} で補間が働きすぎている: {concealed}/40 (上限 {limit})"
        );
    }
}

#[test]
fn conceal_fades_the_tail_when_the_period_is_uncertain() {
    // 周期の推定が確かでない音では、穴の終わりで次に鳴る音の位相が分からず段差が残る。
    // 補間をやめるのではなく、生成した音の末尾を 2 ms で 0 へ落とす。
    //
    // 実測 (48 kHz、40 ms の穴、振幅 0.5 の 440 Hz に振幅 0.15 の白色雑音を混ぜた音):
    // 末尾のサンプルは 0 になり、その直前の振幅も下がる。きれいな 440 Hz では相関が 1.0 の
    // ためフェードせず、末尾は次の音の続き (差は 3.3e-4 以下) になる。
    let frame = noisy_sine_wave(440.0, 0.15);
    let mut output = vec![0.0f32; 1_920];
    let channels: [&[f32]; 1] = [frame.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(
        conceal(&channels, &mut outputs, SAMPLE_RATE, 40_000),
        1_920,
        "補間すること"
    );
    assert_eq!(output[1_919], 0.0, "末尾のサンプルが 0 になること");
    // フェードは末尾の 2 ms だけに掛ける (その前は振幅が残る)
    let fade = ((f64::from(TIME_STRETCH_CONCEAL_FADE_MS) / 1_000.0) * f64::from(SAMPLE_RATE))
        .round() as usize;
    let faded_peak = output[1_920 - fade / 4..]
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    let full_peak = output[..100]
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    assert!(
        faded_peak < full_peak * 0.3,
        "フェードした末尾の振幅が下がること: faded={faded_peak} full={full_peak}"
    );

    // きれいな純音ではフェードしない (末尾が次の音の続きのまま)
    let frame = sine_wave(FRAME_SAMPLES, 440.0, SAMPLE_RATE, 0.5);
    let target = 4_800usize;
    let mut output = vec![0.0f32; target];
    let channels: [&[f32]; 1] = [frame.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(
        conceal(&channels, &mut outputs, SAMPLE_RATE, 100_000),
        target as isize
    );
    // 100 ms の穴では末尾の振幅を [concealment_end_gain] まで下げるため、続きの音もその
    // ゲインで比べる
    let end_gain = concealment_end_gain(100_000);
    let next = sine_sample(FRAME_SAMPLES + target - 1, 440.0, SAMPLE_RATE, 0.5) * end_gain;
    assert!(
        (output[target - 1] - next).abs() <= 1e-3,
        "相関が高い音ではフェードしないこと: {} 対 {next}",
        output[target - 1]
    );
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
    // 埋めた長さは要求どおりにする (時間圧縮が無音でも操作するのと同じ扱い)
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
    // 8 kHz: 4 kHz へ間引くと 10 サンプルであり、窓の長さが 0 になるため末尾の周期を探せない
    let original = square_wave(24, 20, 0.5);
    let mut output = vec![0.0f32; 160];
    let channels: [&[f32]; 1] = [original.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(conceal(&channels, &mut outputs, 8_000, 20_000), 0);
    assert!(output.iter().all(|sample| *sample == 0.0));

    // 8 kHz: 間引いて 11 サンプルあれば、窓の長さが 1 サンプルでも繰り返しが見えれば埋める
    let original = square_wave(26, 20, 0.5);
    let mut output = vec![0.0f32; 160];
    let channels: [&[f32]; 1] = [original.as_slice()];
    let mut outputs: [&mut [f32]; 1] = [output.as_mut_slice()];
    assert_eq!(conceal(&channels, &mut outputs, 8_000, 20_000), 160);
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
    // (サンプルレート, 元の長さ, 周期 (5 ms), 埋めるサンプル数)
    let cases = [
        (8_000u32, 160usize, 40usize, 160usize),
        (16_000, 320, 80, 320),
        (32_000, 640, 160, 640),
        (48_000, 960, 240, 960),
    ];
    for (sample_rate, length, period, expected) in cases {
        // 5 ms (200 Hz) の正弦波。端数まで周期を求めるため、生成の先頭は末尾の周期 1 つ分の
        // 先頭 (端数を含む位置) になる
        let original = sine_wave(length, 200.0, sample_rate, 0.5);
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
            (output[0] - original[length - period] * gain).abs() < 1e-4,
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

#[test]
fn is_time_stretch_available_accepts_only_the_downsample_table_rates() {
    // 4 kHz へ間引くフィルタの表にある 8 / 16 / 32 / 48 kHz だけが使える
    for sample_rate in [8_000u32, 16_000, 32_000, 48_000] {
        assert!(
            is_time_stretch_available(sample_rate),
            "sample_rate={sample_rate} は時間圧縮と補間を使えること"
        );
    }
    // 表に無いレートは使えない。44.1 kHz (AAC など) とその半分の 22.05 kHz、レートとして
    // 成り立たない 0 がこれにあたる
    for sample_rate in [44_100u32, 22_050, 0] {
        assert!(
            !is_time_stretch_available(sample_rate),
            "sample_rate={sample_rate} は時間圧縮と補間を使えないこと"
        );
    }
    // 判定は表の完全一致にする。4 kHz の倍数へ丸めると、8.2 kHz のような表に無いレートを
    // 8 kHz とみなして「使える」と答えてしまうが、compress はサンプルレートそのもので
    // フィルタを引くため実際には動かない
    assert!(
        !is_time_stretch_available(8_200),
        "表に無いレートは表の近くでも使えないこと"
    );
    // 判定と実際の操作が一致することを固定する。丸めと完全一致で結論が分かれるレート
    // (8.2 kHz は丸めると 8 kHz、47.999 kHz は丸めると 48 kHz) では、判定が false である
    // だけでなく compress / conceal がどれも何もしないことを確かめる
    for sample_rate in [8_200u32, 47_999] {
        assert!(
            !is_time_stretch_available(sample_rate),
            "sample_rate={sample_rate} は時間圧縮と補間を使えないこと"
        );
        let original = square_wave(960, 240, 0.5);
        let mut channel = original.clone();
        let mut channels: [&mut [f32]; 1] = [channel.as_mut_slice()];
        assert_eq!(
            compress(&mut channels, sample_rate, usize::MAX),
            0,
            "sample_rate={sample_rate} では時間圧縮をしないこと"
        );
        assert_eq!(
            channel, original,
            "sample_rate={sample_rate} では音を変えないこと"
        );
        let mut concealed = vec![0.0f32; 1_764];
        let channels: [&[f32]; 1] = [original.as_slice()];
        let mut outputs: [&mut [f32]; 1] = [concealed.as_mut_slice()];
        assert_eq!(
            conceal(&channels, &mut outputs, sample_rate, 40_000),
            0,
            "sample_rate={sample_rate} では補間をしないこと"
        );
        assert!(concealed.iter().all(|sample| *sample == 0.0));
    }
}
