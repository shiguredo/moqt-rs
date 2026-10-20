//! 再生の隙間の材料化のプロパティテスト
//!
//! 任意の隙間の長さ・補間の要求・直前の音の形式に対して、隙間が要求したサンプル数ちょうどで
//! 作られること、補間した長さが要求と隙間の長さの中に収まること、隙間の最後と今回の音の
//! 先頭が 0 になること、補間しないときは無音で埋まること、同じ入力なら同じ結果になることを
//! 検証する。補間が成功する場合としない場合の両方に到達したことも確かめる。

use pbt::common::test_runner;
use shiguredo_moqt::playout::gap::materialize;

/// 周期のある音を作る
///
/// 400 Hz (48 kHz で 120 サンプル周期) の矩形波である。三角関数を使わず、周期だけを作る。
fn periodic_audio(period: usize, frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|index| {
            let phase = (index % period) as f32 / period as f32;
            if phase < 0.5 { 0.5 } else { -0.5 }
        })
        .collect()
}

/// 隙間の長さ (サンプル数) を、実装と同じ四捨五入で求める
fn gap_samples(gap_us: i64, sample_rate: u32) -> usize {
    let scaled = gap_us.saturating_mul(i64::from(sample_rate));
    let rounded = usize::try_from(scaled.saturating_add(500_000) / 1_000_000)
        .expect("隙間の長さがサンプル数に収まる");
    rounded.max(1)
}

/// 隙間は要求した長さで作られ、補間とフェードの契約を満たす
#[test]
fn materialized_gaps_follow_the_contract() -> noprop::TestResult {
    // 補間が成功する場合としない場合の両方を通ったことを数える
    let concealed_seen = std::cell::Cell::new(0usize);
    let silent_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        // 対応するサンプルレートから選ぶ
        let sample_rate = [8_000u32, 16_000, 32_000, 48_000]
            [noprop::sample_usize_in(ctx, 0..4)];
        let gap_us = noprop::sample_with_boundaries(
            ctx,
            &[0, 1, 4_999, 5_000, 5_001, 100_000, 100_001],
            noprop::Ratio::one_nth(3),
            |ctx| noprop::sample_usize_in(ctx, 0..=200_000) as i64,
        );
        let conceal_us = noprop::sample_with_boundaries(
            ctx,
            &[0, 1, 5_000, 100_000, 100_001],
            noprop::Ratio::one_nth(3),
            |ctx| noprop::sample_usize_in(ctx, 0..=200_000) as i64,
        );
        let channels = noprop::sample_usize_in(ctx, 1..=2);
        // 今回の音のチャンネルは長さを変える (フェードはチャンネルごとの長さで切る)
        let current_lengths: Vec<usize> = (0..channels)
            .map(|_| noprop::sample_usize_in(ctx, 0..=2_000))
            .collect();
        let mut current: Vec<Vec<f32>> = current_lengths
            .iter()
            .map(|length| vec![0.5f32; *length])
            .collect();
        // 直前の音は、ある / ない、形式が合う / 合わない (チャンネル数・長さ) を混ぜる
        let previous = if noprop::sample_bool(ctx) {
            let previous_channels = if noprop::sample_bool(ctx) {
                channels
            } else {
                channels + 1
            };
            let frames = noprop::sample_with_boundaries(
                ctx,
                &[0, 1, 119, 120, 599, 600, 4_800],
                noprop::Ratio::one_nth(3),
                |ctx| noprop::sample_usize_in(ctx, 0..=4_800),
            );
            let uniform = noprop::sample_bool(ctx);
            Some(
                (0..previous_channels)
                    .map(|channel| {
                        let frames = if uniform {
                            frames
                        } else {
                            frames + channel
                        };
                        periodic_audio(120, frames)
                    })
                    .collect::<Vec<Vec<f32>>>(),
            )
        } else {
            None
        };

        let mut channel_slices: Vec<&mut [f32]> = current
            .iter_mut()
            .map(|channel| channel.as_mut_slice())
            .collect();
        let gap = materialize(
            previous.as_deref(),
            &mut channel_slices,
            sample_rate,
            gap_us,
            conceal_us,
        );

        // 作れない入力では空
        if gap_us <= 0 {
            assert!(
                gap.channels.is_empty(),
                "隙間の長さが 0 以下なら空を返すこと: gap_us={gap_us}"
            );
            assert_eq!(gap.concealed_us, 0);
            return Ok(());
        }
        // 作れる入力では、今回の音と同じ本数・要求した長さの隙間になる
        let expected_length = gap_samples(gap_us, sample_rate);
        assert_eq!(
            gap.channels.len(),
            current.len(),
            "チャンネル数が今回の音と違う: gap_us={gap_us} sample_rate={sample_rate}"
        );
        for channel in &gap.channels {
            assert_eq!(
                channel.len(),
                expected_length,
                "隙間の長さが要求と違う: gap_us={gap_us} sample_rate={sample_rate} expected={expected_length}"
            );
            assert_eq!(
                channel[expected_length - 1],
                0.0,
                "隙間の最後のサンプルは 0 であること: gap_us={gap_us}"
            );
        }
        // 補間した長さは、要求と隙間の全長の中に収まる
        let conceal_limit_us = conceal_us.min(gap_us);
        assert!(
            gap.concealed_us >= 0 && gap.concealed_us <= conceal_limit_us,
            "補間した長さが要求の外: concealed_us={} conceal_us={conceal_us} gap_us={gap_us}",
            gap.concealed_us
        );
        // 補間しないときは無音で埋まる (先頭のフェードも、直前の音の最終サンプルが 0 なら無音)
        if gap.concealed_us == 0 {
            let all_zero = gap
                .channels
                .iter()
                .all(|channel| channel.iter().all(|sample| *sample == 0.0));
            // 形式が合う直前の音があるときは、先頭のフェードがその最終サンプルから始まる
            let head_nonzero = previous.as_ref().is_some_and(|previous| {
                previous.len() == channels
                    && !previous[0].is_empty()
                    && previous
                        .iter()
                        .all(|channel| channel.len() == previous[0].len())
                    && previous
                        .iter()
                        .any(|channel| channel.last().copied().unwrap_or(0.0) != 0.0)
            });
            if !head_nonzero {
                assert!(all_zero, "補間しない隙間は無音で埋まること: gap_us={gap_us}");
            }
            silent_seen.set(silent_seen.get() + 1);
        } else {
            concealed_seen.set(concealed_seen.get() + 1);
        }
        // 今回の音の先頭は 0 から立ち上がる
        for channel in &current {
            if !channel.is_empty() {
                assert_eq!(
                    channel[0], 0.0,
                    "今回の音の先頭は 0 であること: gap_us={gap_us}"
                );
            }
        }
        // すべてのサンプルが有限である (入力も有限である)
        for channel in &gap.channels {
            assert!(
                channel.iter().all(|sample| sample.is_finite()),
                "隙間の音が有限でない: gap_us={gap_us} conceal_us={conceal_us} sample_rate={sample_rate}"
            );
        }

        // 同じ入力なら同じ結果になる
        let mut again: Vec<Vec<f32>> = current_lengths
            .iter()
            .map(|length| vec![0.5f32; *length])
            .collect();
        let mut again_slices: Vec<&mut [f32]> = again
            .iter_mut()
            .map(|channel| channel.as_mut_slice())
            .collect();
        let gap_again = materialize(
            previous.as_deref(),
            &mut again_slices,
            sample_rate,
            gap_us,
            conceal_us,
        );
        assert_eq!(gap, gap_again, "同じ入力なら同じ結果になること");
        Ok(())
    })?;
    assert!(
        concealed_seen.get() > 0,
        "補間が成功する場合を通っていない\n{runner}"
    );
    assert!(
        silent_seen.get() > 0,
        "補間が成功しない場合を通っていない\n{runner}"
    );
    Ok(())
}
