//! 棄却で飛んだ境目のフェードのプロパティテスト
//!
//! 任意の音声に対して、落とす長さの外のサンプルを変えないこと、端へ向けて単調に小さくなる
//! こと、元の絶対値を超えないことを検証する。端を 0 へ落とすだけの処理であり、音を足したり
//! 増幅したりしない。

use pbt::common::test_runner;
use shiguredo_moqt::playout::splice_fade::{
    AudioSpliceEdge, fade_splice_edge, splice_fade_samples,
};

/// ばらつきのある音声を作る (-1.0 以上 1.0 以下)
fn samples(ctx: &mut noprop::TestCaseContext, length: usize) -> Vec<f32> {
    (0..length)
        .map(|_| (noprop::sample_usize_in(ctx, 0..=2_000) as f32 - 1_000.0) / 1_000.0)
        .collect()
}

/// 端を落としても、落とす長さの外のサンプルは変わらないこと
#[test]
fn fading_never_touches_the_samples_outside_the_edge() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let length = noprop::sample_usize_in(ctx, 1..=512);
        let sample_rate = noprop::sample_with_boundaries(
            ctx,
            &[8_000, 44_100, 48_000],
            noprop::Ratio::one_nth(4),
            |ctx| noprop::sample_usize_in(ctx, 1..=96_000) as u32,
        );
        let edge = if noprop::sample_bool(ctx) {
            AudioSpliceEdge::Head
        } else {
            AudioSpliceEdge::Tail
        };
        let original = samples(ctx, length);
        let mut faded = original.clone();
        fade_splice_edge(&mut [faded.as_mut_slice()], sample_rate, edge);

        let fade_samples = splice_fade_samples(length, sample_rate);
        assert!(fade_samples >= 1 && fade_samples <= length);
        // 落とす長さの外は元のまま
        let (faded_outside, original_outside) = match edge {
            AudioSpliceEdge::Head => (&faded[fade_samples..], &original[fade_samples..]),
            AudioSpliceEdge::Tail => (
                &faded[..length - fade_samples],
                &original[..length - fade_samples],
            ),
        };
        assert_eq!(
            faded_outside, original_outside,
            "落とす長さの外は変わらないこと: length={length}, sample_rate={sample_rate}, fade_samples={fade_samples}"
        );
        // 端は 0
        let edge_position = match edge {
            AudioSpliceEdge::Head => 0,
            AudioSpliceEdge::Tail => length - 1,
        };
        assert_eq!(faded[edge_position], 0.0, "端が 0 になること");
        // 元の絶対値を超えない (音を足したり増幅したりしない)
        for (index, (&before, &after)) in original.iter().zip(faded.iter()).enumerate() {
            assert!(
                after.abs() <= before.abs() + f32::EPSILON,
                "元の絶対値を超えないこと: index={index}, before={before}, after={after}"
            );
        }
        Ok(())
    })?;
    Ok(())
}

/// 端へ向けて単調に小さくなること (係数が 0 から 1 へ戻る)
#[test]
fn fading_decreases_toward_the_edge() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let length = noprop::sample_usize_in(ctx, 2..=512);
        let sample_rate = noprop::sample_with_boundaries(
            ctx,
            &[8_000, 44_100, 48_000],
            noprop::Ratio::one_nth(4),
            |ctx| noprop::sample_usize_in(ctx, 1..=96_000) as u32,
        );
        let edge = if noprop::sample_bool(ctx) {
            AudioSpliceEdge::Head
        } else {
            AudioSpliceEdge::Tail
        };
        // 同じ値で埋める。係数だけが変化するため、単調性を比べられる
        let value = 0.5f32;
        let mut faded = vec![value; length];
        fade_splice_edge(&mut [faded.as_mut_slice()], sample_rate, edge);

        let fade_samples = splice_fade_samples(length, sample_rate);
        for index in 1..fade_samples {
            let (previous, current) = match edge {
                AudioSpliceEdge::Head => (faded[index - 1], faded[index]),
                AudioSpliceEdge::Tail => (faded[length - index], faded[length - 1 - index]),
            };
            assert!(
                previous <= current,
                "端へ向けて単調に小さくなること: index={index}, previous={previous}, current={current}, fade_samples={fade_samples}"
            );
        }
        // 落とす長さの外があるときだけ、元の値のままであることを確かめる
        // (バッファが落とす長さ以下なら外側は無い)
        if fade_samples < length {
            let outside = match edge {
                AudioSpliceEdge::Head => faded[fade_samples],
                AudioSpliceEdge::Tail => faded[length - 1 - fade_samples],
            };
            assert_eq!(outside, value, "落とす長さの外は元の値のままであること");
        }
        Ok(())
    })?;
    Ok(())
}
