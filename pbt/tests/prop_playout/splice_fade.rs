//! 棄却で飛んだ境目のフェードのプロパティテスト
//!
//! 任意の音声に対して、落とす長さの外のサンプルを変えないこと、端へ向けて単調に小さくなる
//! こと、元の絶対値を超えないことを検証する。端を 0 へ落とすだけの処理であり、音を足したり
//! 増幅したりしない。

use pbt::common::test_runner;
use shiguredo_moqt::playout::splice_fade::{
    AudioSpliceEdge, AudioSpliceEdges, cross_fade_head, fade_splice_edge, fade_splice_edge_i16,
    interpolate_splice_gap, merge_faded_planes, smooth_splice_head, splice_fade_samples,
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

/// 16 bit PCM のフェードでも、落とす長さの外のサンプルは変わらないこと
#[test]
fn fading_i16_never_touches_the_samples_outside_the_edge() -> noprop::TestResult {
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
        // 16 bit の全範囲を含める (負の値でも同じ規則で落ちること)
        let original: Vec<i16> = (0..length)
            .map(|_| noprop::sample_usize_in(ctx, 0..=65_535) as i16)
            .collect();
        let mut faded = original.clone();
        fade_splice_edge_i16(&mut [faded.as_mut_slice()], sample_rate, edge);

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
            "落とす長さの外は変わらないこと: length={length}, sample_rate={sample_rate}"
        );
        // 端は 0
        let edge_position = match edge {
            AudioSpliceEdge::Head => 0,
            AudioSpliceEdge::Tail => length - 1,
        };
        assert_eq!(faded[edge_position], 0, "端が 0 になること");
        // 元の絶対値を超えない (掛け算で増幅しない)
        for (index, (&before, &after)) in original.iter().zip(faded.iter()).enumerate() {
            assert!(
                i32::from(after).abs() <= i32::from(before).abs(),
                "元の絶対値を超えないこと: index={index}, before={before}, after={after}"
            );
        }
        Ok(())
    })?;
    Ok(())
}

/// 16 bit PCM のフェードでも、端へ向けて単調に小さくなること
#[test]
fn fading_i16_decreases_toward_the_edge() -> noprop::TestResult {
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
        let value = 10_000i16;
        let mut faded = vec![value; length];
        fade_splice_edge_i16(&mut [faded.as_mut_slice()], sample_rate, edge);

        let fade_samples = splice_fade_samples(length, sample_rate);
        for index in 1..fade_samples {
            let (previous, current) = match edge {
                AudioSpliceEdge::Head => (faded[index - 1], faded[index]),
                AudioSpliceEdge::Tail => (faded[length - index], faded[length - 1 - index]),
            };
            assert!(
                previous <= current,
                "端へ向けて単調に小さくなること: index={index}, previous={previous}, current={current}"
            );
        }
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

/// 任意の音声列を作る (-1.0 以上 1.0 以下)
fn sample_signal(ctx: &mut noprop::TestCaseContext, length: usize) -> Vec<f32> {
    (0..length)
        .map(|_| (noprop::sample_usize_in(ctx, 0..=2_000) as f32 - 1_000.0) / 1_000.0)
        .collect()
}

/// 先頭のクロスフェードは両端を保ち、混ぜる長さの外は実音のままであること
///
/// チャンネル数や長さが続き音で違っても落ちないこと (moqt-js は足りないサンプルを 0 と
/// して混ぜ、チャンネルが無ければ実音をそのまま返す) も確かめる。
#[test]
fn cross_fade_head_keeps_the_endpoints() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let length = noprop::sample_usize_in(ctx, 1..=512);
        let sample_rate = noprop::sample_with_boundaries(
            ctx,
            &[8_000, 44_100, 48_000],
            noprop::Ratio::one_nth(4),
            |ctx| noprop::sample_usize_in(ctx, 1..=96_000) as u32,
        );
        let channel_count = noprop::sample_usize_in(ctx, 1..=2);
        // 続き音はチャンネル数と長さを実音と変える
        let continuation_channel_count = noprop::sample_usize_in(ctx, 0..=channel_count);
        let continuations: Vec<Vec<f32>> = (0..continuation_channel_count)
            .map(|_| {
                let continuation_length = noprop::sample_usize_in(ctx, 0..=length);
                sample_signal(ctx, continuation_length)
            })
            .collect();
        let channels: Vec<Vec<f32>> = (0..channel_count)
            .map(|_| sample_signal(ctx, length))
            .collect();
        let saved = channels.clone();
        let continuation_slices: Vec<&[f32]> =
            continuations.iter().map(|channel| channel.as_slice()).collect();
        let channel_slices: Vec<&[f32]> =
            channels.iter().map(|channel| channel.as_slice()).collect();
        let output = cross_fade_head(&continuation_slices, &channel_slices, sample_rate);
        assert_eq!(output.len(), channel_count);
        let fade = splice_fade_samples(usize::MAX, sample_rate)
            .min(length)
            .min(continuations.first().map_or(0, Vec::len));
        for (index, channel) in output.iter().enumerate() {
            assert_eq!(channel.len(), length);
            let Some(continuation) = continuations.get(index) else {
                // 続きのチャンネルが無いときは実音をそのまま返す
                assert_eq!(channel, &channels[index]);
                continue;
            };
            if fade > 0 {
                // 先頭は続き音 (足りない分は 0) から始まる
                assert_eq!(
                    channel[0],
                    continuation.first().copied().unwrap_or(0.0),
                    "先頭は補間の続き (無ければ 0) であること: index={index}"
                );
                // 混ぜる長さの外は実音のまま
                assert_eq!(
                    &channel[fade..],
                    &channels[index][fade..],
                    "混ぜる長さの外は実音のままであること: index={index}"
                );
                // 混ぜた範囲は実音と続き音 (足りない分は 0) の間の値になる
                for position in 0..fade {
                    let previous =
                        f64::from(continuation.get(position).copied().unwrap_or(0.0));
                    let current = f64::from(channels[index][position]);
                    let value = f64::from(channel[position]);
                    assert!(
                        value >= previous.min(current) - 1e-6
                            && value <= previous.max(current) + 1e-6,
                        "position={position} の値が両端の外: value={value} previous={previous} current={current}"
                    );
                }
            } else {
                assert_eq!(channel, &channels[index], "混ぜないときは実音のままであること");
            }
        }
        assert_eq!(channels, saved, "入力を変更しないこと");
        Ok(())
    })?;
    Ok(())
}

/// 継ぎ目の段差が自然な変化幅以下なら何も変えず、掛けるときも混ぜる長さの外は実音のまま
#[test]
fn smooth_splice_head_only_changes_the_head() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let length = noprop::sample_usize_in(ctx, 1..=512);
        let sample_rate = noprop::sample_with_boundaries(
            ctx,
            &[8_000, 44_100, 48_000],
            noprop::Ratio::one_nth(4),
            |ctx| noprop::sample_usize_in(ctx, 1..=96_000) as u32,
        );
        let previous = sample_signal(ctx, length);
        let channel = sample_signal(ctx, length);
        let previous_slices: [&[f32]; 1] = [previous.as_slice()];
        let channel_slices: [&[f32]; 1] = [channel.as_slice()];
        let output = smooth_splice_head(&previous_slices, &channel_slices, sample_rate);
        let output = &output[0];
        assert_eq!(output.len(), length);
        // 直前の音の自然な変化幅
        let natural_step = previous.windows(2).fold(0.0f64, |maximum, pair| {
            maximum.max((f64::from(pair[1]) - f64::from(pair[0])).abs())
        });
        let tail = previous[length - 1];
        if (f64::from(channel[0]) - f64::from(tail)).abs() <= natural_step {
            assert_eq!(output, &channel, "段差が自然なら何もしないこと");
        } else {
            let fade = splice_fade_samples(usize::MAX, sample_rate)
                .min(length)
                .min(length);
            if fade < length {
                assert_eq!(
                    &output[fade..],
                    &channel[fade..],
                    "混ぜる長さの外は実音のままであること"
                );
            }
            assert_eq!(output[0], tail, "直前の音の末尾から接続すること");
        }
        Ok(())
    })?;
    Ok(())
}

/// 短い隙間の補間は両端の間の直線になり、端を越えないこと
#[test]
fn interpolate_splice_gap_stays_between_the_ends() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        let frames = noprop::sample_usize_in(ctx, 0..=64);
        let previous = sample_signal(ctx, 1);
        let next = sample_signal(ctx, 1);
        let previous_slices: [&[f32]; 1] = [previous.as_slice()];
        let next_slices: [&[f32]; 1] = [next.as_slice()];
        let output = interpolate_splice_gap(&previous_slices, &next_slices, frames);
        assert_eq!(output[0].len(), frames);
        let from = f64::from(previous[0]);
        let to = f64::from(next[0]);
        for (index, sample) in output[0].iter().enumerate() {
            let value = f64::from(*sample);
            assert!(
                value >= from.min(to) - 1e-6 && value <= from.max(to) + 1e-6,
                "index={index} の値が両端の外: value={value} from={from} to={to}"
            );
            // 直前の最終サンプルから次の先頭へ向かう向きであること
            let weight = (index as f64 + 1.0) / (frames as f64 + 1.0);
            assert!((value - (from + (to - from) * weight)).abs() < 1e-6);
        }
        Ok(())
    })?;
    Ok(())
}

/// 平面の結合は端を落とし、長さと外側の値を保つこと
#[test]
fn merge_faded_planes_keeps_the_length_and_fades_the_edges() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        let length = noprop::sample_usize_in(ctx, 0..=512);
        let channel_count = noprop::sample_usize_in(ctx, 1..=2);
        let sample_rate = noprop::sample_with_boundaries(
            ctx,
            &[8_000, 44_100, 48_000],
            noprop::Ratio::one_nth(4),
            |ctx| noprop::sample_usize_in(ctx, 1..=96_000) as u32,
        );
        let planes: Vec<Vec<f32>> = (0..channel_count)
            .map(|_| sample_signal(ctx, length))
            .collect();
        let slices: Vec<&[f32]> = planes.iter().map(|plane| plane.as_slice()).collect();
        let edges = AudioSpliceEdges {
            head: noprop::sample_bool(ctx),
            tail: noprop::sample_bool(ctx),
        };
        let merged = merge_faded_planes(&slices, sample_rate, edges);
        if length == 0 {
            // 長さ 0 の平面は空のバッファになる (moqt-js も長さ 0 を返す)
            assert!(merged.is_empty(), "長さ 0 の平面では空を返すこと");
            return Ok(());
        }
        assert_eq!(merged.len(), length * channel_count);
        let fade = splice_fade_samples(length, sample_rate);
        for (index, plane) in planes.iter().enumerate() {
            let offset = index * length;
            for position in 0..length {
                let value = merged[offset + position];
                assert!(value.is_finite());
                assert!(value.abs() <= plane[position].abs() + 1e-6, "音を増幅しないこと");
            }
            if edges.head {
                assert_eq!(merged[offset], 0.0, "先頭が 0 になること");
            }
            if edges.tail {
                assert_eq!(merged[offset + length - 1], 0.0, "末尾が 0 になること");
            }
            // フェードを掛けない位置は元の値のまま (フェードの長さが音の半分を超えると
            // 先頭と末尾のフェードが重なるため、位置ごとに判定する)
            for position in 0..length {
                let in_head = edges.head && position < fade;
                let in_tail = edges.tail && position + fade >= length;
                if !in_head && !in_tail {
                    assert_eq!(
                        merged[offset + position],
                        plane[position],
                        "フェードを掛けない位置は元の値のままであること: position={position} fade={fade}"
                    );
                }
            }
        }
        Ok(())
    })?;
    Ok(())
}
