//! 出力側で生成する続き音 (expandAudio) と、その数値カーネルのテスト
//!
//! moqt-js の `audioExpansion.test.ts` と `audioPredictorKernel.test.ts` が固定する規則を
//! 移植する。周期が確かな音では周期補間を使い、それ以外は線形予測フィルターへ非周期の
//! 励振を入れて合成する。生成した音は必ず最新の実音の RMS とピークを超えない。
//! カーネルは固定 12 次の積和を JavaScript と同じ計算順序で行い、Float32 へ丸める。

use shiguredo_moqt::playout::expansion::{ExpandAudioError, expand_audio};
use shiguredo_moqt::playout::predictor::{
    AUDIO_PREDICTOR_HISTORY_SIZE, AUDIO_PREDICTOR_MAX_FRAMES, AUDIO_PREDICTOR_ORDER,
    generate_unvoiced,
};
use shiguredo_moqt::playout::stretch::conceal;

/// 1 極フィルタ (係数 0.6) を通した有色雑音を作る (moqt-js の `noise` と同じ合同式)
fn noise(length: usize) -> Vec<f32> {
    let mut state = 731u32;
    let mut previous = 0.0f64;
    (0..length)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            previous = previous * 0.6 + (f64::from(state) / 4_294_967_295.0 - 0.5) * 0.1;
            previous as f32
        })
        .collect()
}

/// 正弦波を作る
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

/// RMS を求める
fn rms(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples
        .iter()
        .map(|sample| f64::from(*sample) * f64::from(*sample))
        .sum::<f64>()
        / samples.len() as f64)
        .sqrt()
}

/// ピークを求める
fn peak(samples: &[f32]) -> f64 {
    samples.iter().fold(0.0f64, |maximum, sample| {
        maximum.max(f64::from(sample.abs()))
    })
}

/// 補間できるか (moqt-js の `concealSamples(...).generatedSamples` に当たる)
fn conceal_generated(channels: &[&[f32]], sample_rate: u32, conceal_us: i64) -> isize {
    let target = ((conceal_us * i64::from(sample_rate) + 500_000) / 1_000_000).max(0) as usize;
    let mut output = vec![vec![0.0f32; target]; channels.len()];
    let mut outputs: Vec<&mut [f32]> = output
        .iter_mut()
        .map(|channel| channel.as_mut_slice())
        .collect();
    conceal(channels, &mut outputs, sample_rate, conceal_us)
}

#[test]
fn expand_audio_conceals_a_low_voice_from_the_history() {
    // 80 Hz の低い有声音を、連続する 30 ms の履歴から周期補間する
    for sample_rate in [44_100u32, 48_000] {
        let history = sine(
            (f64::from(sample_rate) * 0.03).round() as usize,
            80.0,
            sample_rate,
            0.2,
        );
        let result =
            expand_audio(&[history.as_slice()], sample_rate, None).expect("有限の音声は受け付ける");
        let expected_length = (f64::from(sample_rate) * 0.1).round() as usize;
        assert_eq!(
            result[0].len(),
            expected_length,
            "100 ms ぶんの続き音を生成すること: sample_rate={sample_rate}"
        );
        assert!(
            rms(&result[0]) > 0.04,
            "生成した音の音量が残ること: sample_rate={sample_rate} rms={}",
            rms(&result[0])
        );
        let last = history[history.len() - 1];
        assert!(
            (f64::from(result[0][0]) - f64::from(last)).abs() < 0.01,
            "補間の先頭が急に跳ばないこと: sample_rate={sample_rate} first={} last={last}",
            result[0][0]
        );
    }
}

#[test]
fn expand_audio_generates_unvoiced_sound_when_the_period_is_unknown() {
    // 周期を推定できない音でも、元のスペクトルと振幅を使う非周期の補間を生成する
    let input = noise(1_440);
    assert_eq!(
        conceal_generated(&[input.as_slice()], 48_000, 100_000),
        0,
        "周期反復では補間できない入力を使うこと"
    );
    let result = expand_audio(&[input.as_slice()], 48_000, None).expect("有限の音声は受け付ける");
    let expanded = &result[0];
    assert_eq!(expanded.len(), 4_800, "100 ms ぶんを生成すること");
    assert!(
        expanded.iter().all(|sample| sample.is_finite()),
        "生成した音は有限であること"
    );
    let input_rms = rms(&input);
    let expanded_rms = rms(expanded);
    assert!(
        expanded_rms > input_rms * 0.2,
        "無音にせず元の音量を残すこと: {} 対 {}",
        expanded_rms,
        input_rms
    );
    assert!(
        expanded_rms < input_rms * 1.2,
        "元の音量より大きくしないこと: {} 対 {}",
        expanded_rms,
        input_rms
    );
    assert!(
        peak(expanded) <= peak(&input) + 1e-6,
        "予測音で過去のピークを超えないこと: {} 対 {}",
        peak(expanded),
        peak(&input)
    );
    // 最後の 20 ms で元の 20 ms を周期的に繰り返していないこと
    let mut product = 0.0f64;
    let mut energy = 0.0f64;
    for index in 960..expanded.len() {
        product += f64::from(expanded[index]) * f64::from(expanded[index - 960]);
        energy += f64::from(expanded[index]) * f64::from(expanded[index]);
    }
    assert!(
        (product / energy).abs() < 0.2,
        "無声成分を周期的に繰り返さないこと: {}",
        product / energy
    );
}

#[test]
fn expand_audio_keeps_channel_relations() {
    // 同一と逆相のチャンネルを、異なる雑音へばらさない
    let input = noise(1_440);
    let opposite: Vec<f32> = input.iter().map(|sample| -*sample).collect();
    let result = expand_audio(
        &[input.as_slice(), input.as_slice(), opposite.as_slice()],
        48_000,
        None,
    )
    .expect("有限の音声は受け付ける");
    assert_eq!(result[0], result[1], "同一のチャンネルは同じ音になること");
    for (left, right) in result[0].iter().zip(&result[2]) {
        assert!(
            (f64::from(*left) + f64::from(*right)).abs() < 1e-6,
            "逆相のチャンネルは逆相のままであること"
        );
    }
}

#[test]
fn expand_audio_does_not_raise_the_recent_level() {
    // 過去が大きな音でも、静かになった末尾の音量を再び持ち上げない
    let older: Vec<f32> = noise(480).iter().map(|sample| sample * 5.0).collect();
    let current: Vec<f32> = noise(960).iter().map(|sample| sample * 0.1).collect();
    let mut history = vec![0.0f32; 1_440];
    history[..480].copy_from_slice(&older);
    history[480..].copy_from_slice(&current);
    let result = expand_audio(&[current.as_slice()], 48_000, Some(&[history.as_slice()]))
        .expect("有限の音声は受け付ける");
    let output = &result[0];
    assert!(
        rms(output) < rms(&current) * 1.1,
        "過去の強い発声を補間へ戻さないこと: {} 対 {}",
        rms(output),
        rms(&current)
    );
    assert!(
        peak(output) <= peak(&current) + 1e-6,
        "静かになった末尾のピークを超えないこと: {} 対 {}",
        peak(output),
        peak(&current)
    );
}

#[test]
fn expand_audio_does_not_create_sound_from_silence() {
    // 無音から勝手に音を作らない
    let silent_input = vec![0.0f32; 1_440];
    let silent =
        expand_audio(&[silent_input.as_slice()], 48_000, None).expect("有限の音声は受け付ける");
    assert_eq!(silent.len(), 1);
    assert!(silent[0].iter().all(|sample| *sample == 0.0));
    // 空の入力は空を返す
    assert!(
        expand_audio(&[], 48_000, None)
            .expect("空の入力は受け付ける")
            .is_empty()
    );
    // 壊れた入力 (有限でないサンプル) は拒否する
    let broken: Vec<f32> = noise(1_440).into_iter().map(|_| f32::NAN).collect();
    assert_eq!(
        expand_audio(&[broken.as_slice()], 48_000, None),
        Err(ExpandAudioError::InvalidInput)
    );
    // サンプルレートが 0 のときも拒否する
    let invalid_rate_input = noise(1_440);
    assert_eq!(
        expand_audio(&[invalid_rate_input.as_slice()], 0, None),
        Err(ExpandAudioError::InvalidInput)
    );
}

#[test]
fn expand_audio_skips_unsupported_rates() {
    // 補間に未対応の有効なレートは、実音を再生できるよう補間だけを省く
    for sample_rate in [3_000u32, 192_000] {
        let input = noise(960);
        assert_eq!(
            expand_audio(&[input.as_slice()], sample_rate, None).expect("有効なレートは受け付ける"),
            Vec::<Vec<f32>>::new(),
            "sample_rate={sample_rate} では補間しないこと"
        );
    }
}

#[test]
fn generate_unvoiced_follows_the_first_order_decay() {
    // 1 次の減衰の解析解と一致し、次の呼び出しで既存の結果を上書きしない
    let mut coefficients = [0.0f64; AUDIO_PREDICTOR_ORDER];
    coefficients[0] = 0.5;
    let mut history = [0.0f64; AUDIO_PREDICTOR_HISTORY_SIZE];
    history[AUDIO_PREDICTOR_HISTORY_SIZE - 1] = 0.25;
    let first =
        generate_unvoiced(&coefficients, &history, 64, 731, 0.0, 1.0, 1.0).expect("生成できる");
    // 解析解は 0.25 * 0.5^(index + 1) である
    let mut expected = 0.125f32;
    for (index, sample) in first.iter().enumerate() {
        assert_eq!(
            *sample, expected,
            "index={index} が解析解と一致すること: {sample} 対 {expected}"
        );
        expected *= 0.5;
    }
    // 履歴を変えて呼び直すと、結果は変わるが前回の結果は所有権ごと残る
    history[AUDIO_PREDICTOR_HISTORY_SIZE - 1] = 0.5;
    let second =
        generate_unvoiced(&coefficients, &history, 64, 731, 0.0, 1.0, 1.0).expect("生成できる");
    for (index, sample) in second.iter().enumerate() {
        let first_value = first[index];
        assert_eq!(
            *sample,
            first_value * 2.0,
            "index={index} が 2 倍になること"
        );
    }
    assert_eq!(first[0], 0.125, "前回の結果を上書きしないこと");
}

#[test]
fn generate_unvoiced_rejects_inputs_over_the_fixed_capacity() {
    // 固定領域を超える入力を拒否し、失敗後も正常な演算を行える
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
        "固定領域を超える要求を拒否すること"
    );
    let output = generate_unvoiced(&coefficients, &history, 128, 731, 0.1, 0.3, 1.0)
        .expect("上限内の生成は成功する");
    assert!(output.iter().all(|sample| sample.is_finite()));
    assert!(
        output.iter().all(|sample| sample.abs() <= 0.3),
        "上限を超えないこと"
    );
    assert!(
        output.iter().any(|sample| *sample != 0.0),
        "失敗を無音で隠さないこと"
    );
}
