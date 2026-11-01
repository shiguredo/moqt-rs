//! 棄却で飛んだ境目のフェードのテスト
//!
//! 公開 API (`fade_splice_edge` / `splice_fade_samples` / 出力側の接続部品) の契約を確認する。
//! moqt-js の `src/audioSpliceFade.ts` が固定する規則 (落とす長さ・端の落とし方・外側を
//! 変えないこと・隙間の補間・継ぎ目のクロスフェード) を移植する。

use shiguredo_moqt::playout::splice_fade::{
    AudioSpliceEdge, AudioSpliceEdges, cross_fade_head, fade_splice_edge, fade_splice_edge_i16,
    interpolate_splice_gap, merge_faded_planes, smooth_splice_head, splice_fade_samples,
};

/// 0.5 で埋めた音声を作る
fn samples(length: usize) -> Vec<f32> {
    vec![0.5; length]
}

#[test]
fn the_fade_length_follows_the_sample_rate() {
    // 48 kHz では 96 サンプルになる (2 ms。8 ms のフレームの半分より短い)
    assert_eq!(
        splice_fade_samples(960, 48_000),
        96,
        "48 kHz の落とす長さは 96 サンプルであること"
    );
    assert_eq!(
        splice_fade_samples(480, 24_000),
        48,
        "24 kHz の落とす長さは 48 サンプルであること"
    );
    // 端数は四捨五入する (44.1 kHz では 88.2 サンプル)
    assert_eq!(
        splice_fade_samples(441, 44_100),
        88,
        "端数を四捨五入すること"
    );
    // 長さが 0 のときは落とす長さも 0
    assert_eq!(splice_fade_samples(0, 48_000), 0, "空では 0 であること");
}

#[test]
fn the_fade_length_never_exceeds_the_buffer() {
    // バッファが落とす長さより短いときは、全体をフェードする
    assert_eq!(
        splice_fade_samples(10, 48_000),
        10,
        "短いバッファでは全体をフェードすること"
    );
    // サンプルレートが 0 でも 1 サンプルは落とす (端だけは 0 にする)
    assert_eq!(splice_fade_samples(100, 0), 1, "1 以上であること");
}

#[test]
fn fading_the_head_makes_the_edge_silent() {
    let mut buffer = samples(960);
    let original = 0.5;
    fade_splice_edge(&mut [buffer.as_mut_slice()], 48_000, AudioSpliceEdge::Head);
    assert_eq!(buffer[0], 0.0, "端が 0 になること");
    // 落とす長さだけ離れた位置は元の値のまま (0.5 * 96 / 96)
    assert_eq!(
        buffer[96], original,
        "落とす長さだけ離れた位置は元の値のままであること"
    );
    assert_eq!(buffer[959], original, "落とす長さの外は変わらないこと");
    // 途中は端へ向けて小さくなる
    assert!(
        buffer[1] < buffer[2] && buffer[2] < buffer[94],
        "端へ向けて小さくなること: 1={}, 2={}, 94={}",
        buffer[1],
        buffer[2],
        buffer[94]
    );
    assert!(
        buffer[1] > 0.0,
        "端の次は 0 より大きいこと (完全な無音にしない): {}",
        buffer[1]
    );
}

#[test]
fn fading_the_tail_makes_the_edge_silent() {
    let mut buffer = samples(960);
    let original = 0.5;
    fade_splice_edge(&mut [buffer.as_mut_slice()], 48_000, AudioSpliceEdge::Tail);
    assert_eq!(buffer[959], 0.0, "末尾が 0 になること");
    assert_eq!(
        buffer[863], original,
        "落とす長さだけ離れた位置は元の値のままであること"
    );
    assert_eq!(buffer[0], original, "落とす長さの外は変わらないこと");
    assert!(
        buffer[958] < buffer[957] && buffer[957] < buffer[865],
        "末尾へ向けて小さくなること"
    );
}

#[test]
fn every_channel_is_faded_the_same_way() {
    let mut left = samples(960);
    let mut right = vec![0.25; 960];
    fade_splice_edge(
        &mut [left.as_mut_slice(), right.as_mut_slice()],
        48_000,
        AudioSpliceEdge::Head,
    );
    assert_eq!(left[0], 0.0, "左チャンネルの端が 0 になること");
    assert_eq!(right[0], 0.0, "右チャンネルの端が 0 になること");
    assert_eq!(left[96], 0.5, "左チャンネルの外側は変わらないこと");
    assert_eq!(right[96], 0.25, "右チャンネルの外側は変わらないこと");
    // 同じ係数を掛けるため、元の比が保たれる
    assert_eq!(left[48] / right[48], 2.0, "チャンネル間の比が保たれること");
}

#[test]
fn a_short_buffer_is_faded_as_a_whole() {
    let mut buffer = samples(10);
    fade_splice_edge(&mut [buffer.as_mut_slice()], 48_000, AudioSpliceEdge::Head);
    assert_eq!(buffer[0], 0.0, "端が 0 になること");
    assert_eq!(buffer[10 - 1], 0.5 * 9.0 / 10.0, "全体をフェードすること");
}

#[test]
fn nothing_happens_for_an_empty_buffer() {
    let mut empty: Vec<f32> = Vec::new();
    fade_splice_edge(&mut [empty.as_mut_slice()], 48_000, AudioSpliceEdge::Head);
    assert!(empty.is_empty(), "空のバッファは変わらないこと");
    // チャンネルが 1 つも無いときも落ちない
    fade_splice_edge(&mut [], 48_000, AudioSpliceEdge::Tail);
}

#[test]
fn fading_i16_head_makes_the_edge_silent() {
    let mut buffer = vec![10_000i16; 960];
    fade_splice_edge_i16(&mut [buffer.as_mut_slice()], 48_000, AudioSpliceEdge::Head);
    assert_eq!(buffer[0], 0, "端が 0 になること");
    assert_eq!(
        buffer[96], 10_000,
        "落とす長さだけ離れた位置は元の値のままであること"
    );
    assert_eq!(buffer[959], 10_000, "落とす長さの外は変わらないこと");
    assert!(
        buffer[1] < buffer[2] && buffer[2] < buffer[94],
        "端へ向けて小さくなること: 1={}, 2={}, 94={}",
        buffer[1],
        buffer[2],
        buffer[94]
    );
}

#[test]
fn fading_i16_tail_makes_the_edge_silent() {
    let mut buffer = vec![-10_000i16; 960];
    let original = -10_000i16;
    fade_splice_edge_i16(&mut [buffer.as_mut_slice()], 48_000, AudioSpliceEdge::Tail);
    assert_eq!(buffer[959], 0, "末尾が 0 になること");
    assert_eq!(
        buffer[863], original,
        "落とす長さだけ離れた位置は元の値のままであること"
    );
    assert_eq!(buffer[0], original, "落とす長さの外は変わらないこと");
    // 負の値でも絶対値が減る
    assert!(
        buffer[958].abs() < buffer[957].abs() && buffer[957].abs() < buffer[865].abs(),
        "絶対値が末尾へ向けて小さくなること"
    );
}

#[test]
fn fading_i16_covers_every_channel() {
    let mut left = vec![10_000i16; 960];
    let mut right = vec![5_000i16; 960];
    fade_splice_edge_i16(
        &mut [left.as_mut_slice(), right.as_mut_slice()],
        48_000,
        AudioSpliceEdge::Head,
    );
    assert_eq!(left[0], 0, "左チャンネルの端が 0 になること");
    assert_eq!(right[0], 0, "右チャンネルの端が 0 になること");
    assert_eq!(left[96], 10_000, "左チャンネルの外側は変わらないこと");
    assert_eq!(right[96], 5_000, "右チャンネルの外側は変わらないこと");
}

#[test]
fn fading_i16_leaves_short_and_empty_buffers_alone() {
    // バッファが落とす長さより短いときは全体をフェードする
    let mut short = vec![1_000i16; 10];
    fade_splice_edge_i16(&mut [short.as_mut_slice()], 48_000, AudioSpliceEdge::Head);
    assert_eq!(short[0], 0, "端が 0 になること");
    assert_eq!(short[9], 900, "全体をフェードすること");

    // 空のバッファでは何もしない
    let mut empty: Vec<i16> = Vec::new();
    fade_splice_edge_i16(&mut [empty.as_mut_slice()], 48_000, AudioSpliceEdge::Tail);
    assert!(empty.is_empty(), "空のバッファは変わらないこと");
    fade_splice_edge_i16(&mut [], 48_000, AudioSpliceEdge::Head);
}

#[test]
fn fading_i16_never_grows_the_sample() {
    // 最大値でも掛け算が溢れないこと (16 bit の範囲を超えない)
    let mut buffer = vec![i16::MAX; 960];
    fade_splice_edge_i16(&mut [buffer.as_mut_slice()], 48_000, AudioSpliceEdge::Head);
    assert!(
        buffer.iter().all(|&sample| sample >= 0),
        "符号が反転しないこと"
    );
    assert_eq!(buffer[0], 0, "端が 0 になること");
    assert_eq!(buffer[96], i16::MAX, "外側は元の値のままであること");
    for index in 1..96 {
        assert!(
            buffer[index] <= buffer[index + 1],
            "端へ向けて単調に大きくなること: index={index}"
        );
    }
}

// ---------------------------------------------------------------------------------------
// 出力側の接続部品 (moqt-js の audioSpliceFade.test.ts が固定する規則)
// ---------------------------------------------------------------------------------------

/// Opus の 1 フレーム (8 ms、16 kHz)
const SAMPLE_RATE: u32 = 16_000;
const FRAME_SAMPLES: usize = 128;

/// 落とす長さ (2 ms = 32 サンプル)
const FADE_SAMPLES: usize = 32;

/// 一定値のフレームを作る (端の変化を読みやすくする)
fn constant_frame(value: f32) -> Vec<f32> {
    vec![value; FRAME_SAMPLES]
}

#[test]
fn smooth_splice_head_does_not_create_a_step_over_the_previous_sound() {
    // 補間の開始に元の音より大きい段差を作らない。既知の直前の末尾から接続し、
    // 落とす長さの外は補間波形へ戻る
    let previous: Vec<f32> = (0..FRAME_SAMPLES)
        .map(|index| -0.2 - index as f32 * 0.001)
        .collect();
    let next = constant_frame(-0.1);
    let output = smooth_splice_head(&[previous.as_slice()], &[next.as_slice()], SAMPLE_RATE);
    assert_eq!(
        output[0][0],
        previous[FRAME_SAMPLES - 1],
        "既知の直前の末尾から接続すること"
    );
    assert_eq!(
        output[0][FADE_SAMPLES], next[FADE_SAMPLES],
        "接続後は補間波形へ戻ること"
    );
    // 段差が自然な変化幅以内なら、通常の波形へフェードを掛けない
    let constant = constant_frame(0.5);
    assert_eq!(
        smooth_splice_head(&[constant.as_slice()], &[constant.as_slice()], SAMPLE_RATE)[0],
        constant,
        "同じ音へ毎回フェードを掛けないこと"
    );
}

#[test]
fn interpolate_splice_gap_connects_the_two_ends_linearly() {
    // 周期推定できない子音でも、既知の 2 端点の間にある短い隙間は無音へ落とさずに繋ぐ
    let previous = constant_frame(0.2);
    let next = constant_frame(-0.2);
    let output = interpolate_splice_gap(&[previous.as_slice()], &[next.as_slice()], 16);
    assert_eq!(output[0].len(), 16);
    // 直前の最終サンプルから次の先頭まで、1 サンプルあたり 0.4 / 17 ずつ動く
    let mut expected = 0.2f64;
    for sample in output[0].iter().copied().chain(std::iter::once(-0.2f32)) {
        assert!(
            (f64::from(sample) - expected).abs() <= 0.4 / 17.0 + 1e-7,
            "{sample} が {expected} から離れすぎている"
        );
        expected = f64::from(sample);
    }
}

#[test]
fn cross_fade_head_keeps_the_level_of_a_continuous_sound() {
    // 補間と実音が同じ波形なら、継ぎ目のために一度 0 へ落とす必要はない
    let frame = constant_frame(0.5);
    let output = cross_fade_head(&[frame.as_slice()], &[frame.as_slice()], SAMPLE_RATE);
    assert_eq!(output[0], frame, "連続する音の音量を変えないこと");
    assert_eq!(frame[0], 0.5, "入力を変更しないこと");
}

#[test]
fn cross_fade_head_connects_different_phases_without_a_step() {
    // 位相が異なる音を段差を作らずに繋ぐ
    let continuation = constant_frame(0.5);
    let channel = constant_frame(-0.5);
    let output = cross_fade_head(
        &[continuation.as_slice()],
        &[channel.as_slice()],
        SAMPLE_RATE,
    );
    assert_eq!(output[0][0], 0.5, "補間の続きから始めること");
    assert_eq!(output[0][FADE_SAMPLES], -0.5, "実音へ戻ること");
    let mut maximum = 0.0f64;
    for index in 1..=FADE_SAMPLES {
        maximum = maximum.max((output[0][index] - output[0][index - 1]).abs() as f64);
    }
    assert!(
        maximum <= 1.0 / FADE_SAMPLES as f64 + 1e-6,
        "急な位相飛びを作らないこと: {maximum}"
    );
}

#[test]
fn cross_fade_head_treats_missing_or_short_continuation_samples_as_zero() {
    // チャンネルごとに短い continuation を渡しても落ちない。moqt-js は範囲外を 0 として
    // 混ぜる (`previous[index] ?? 0`)
    let continuation_full = constant_frame(0.5);
    let short: Vec<f32> = vec![0.25; 8];
    let empty: Vec<f32> = Vec::new();
    let channel_left = constant_frame(-0.5);
    let channel_right = constant_frame(-0.25);
    let output = cross_fade_head(
        &[
            continuation_full.as_slice(),
            short.as_slice(),
            empty.as_slice(),
        ],
        &[
            channel_left.as_slice(),
            channel_right.as_slice(),
            channel_right.as_slice(),
        ],
        SAMPLE_RATE,
    );
    // 0 サンプル目の重みは 0 なので、続きの先頭がそのまま出る
    assert_eq!(output[0][0], 0.5);
    assert_eq!(output[1][0], 0.25);
    // 空の continuation はすべて 0 として混ざる (実音へ向かう重みだけが残る)
    assert_eq!(output[2][0], 0.0);
    let weight = 1.0 / FADE_SAMPLES as f64;
    assert!((f64::from(output[2][1]) - (-0.25 * weight)).abs() < 1e-6);
    // 続きのチャンネルが無いときは実音をそのまま返す
    let output = cross_fade_head(
        &[continuation_full.as_slice()],
        &[channel_left.as_slice(), channel_right.as_slice()],
        SAMPLE_RATE,
    );
    assert_eq!(output[1], channel_right);
}

#[test]
fn merge_faded_planes_returns_empty_for_a_zero_length_plane() {
    // 長さ 0 の平面は空のバッファになる (moqt-js は長さ 0 の Float32Array を返す)
    let empty: Vec<f32> = Vec::new();
    for edges in [
        AudioSpliceEdges {
            head: true,
            tail: false,
        },
        AudioSpliceEdges {
            head: true,
            tail: true,
        },
    ] {
        assert!(
            merge_faded_planes(&[empty.as_slice()], SAMPLE_RATE, edges).is_empty(),
            "平面 1 本でも長さ 0 なら空を返すこと"
        );
        assert!(
            merge_faded_planes(&[empty.as_slice(), empty.as_slice()], SAMPLE_RATE, edges)
                .is_empty(),
            "平面 2 本でも長さ 0 なら空を返すこと"
        );
    }
}

#[test]
fn merge_faded_planes_fades_the_edges_and_merges_the_planes() {
    // 端を落とし、平面の並びで 1 つのバッファへ戻す
    let frame = constant_frame(0.5);
    let merged = merge_faded_planes(
        &[frame.as_slice(), frame.as_slice()],
        SAMPLE_RATE,
        AudioSpliceEdges {
            head: true,
            tail: true,
        },
    );
    assert_eq!(merged.len(), FRAME_SAMPLES * 2, "平面の数だけ長くなること");
    assert_eq!(merged[0], 0.0, "第 1 チャンネルの先頭が 0 になること");
    assert_eq!(
        merged[FRAME_SAMPLES - 1],
        0.0,
        "第 1 チャンネルの末尾が 0 になること"
    );
    assert_eq!(
        merged[FRAME_SAMPLES], 0.0,
        "第 2 チャンネルの先頭が 0 になること"
    );
    assert!(
        (f64::from(merged[FADE_SAMPLES]) - 0.5).abs() < 1e-6,
        "落とす長さの外は元の値であること"
    );
    // 端を指定しなければ値を変えない
    let merged = merge_faded_planes(
        &[frame.as_slice()],
        SAMPLE_RATE,
        AudioSpliceEdges::default(),
    );
    assert_eq!(merged[0], 0.5, "先頭がそのままであること");
    assert_eq!(merged[FRAME_SAMPLES - 1], 0.5, "末尾がそのままであること");
    // チャンネルが無ければ空
    assert!(merge_faded_planes(&[], SAMPLE_RATE, AudioSpliceEdges::default()).is_empty());
}
