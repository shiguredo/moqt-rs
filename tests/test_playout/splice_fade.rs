//! 棄却で飛んだ境目のフェードのテスト
//!
//! 公開 API (`fade_splice_edge` / `splice_fade_samples`) の契約を確認する。moqt-js の
//! `src/audioSpliceFade.ts` が固定する規則 (落とす長さ・端の落とし方・外側を変えないこと)
//! を移植する。

use shiguredo_moqt::playout::splice_fade::{
    AudioSpliceEdge, fade_splice_edge, splice_fade_samples,
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
