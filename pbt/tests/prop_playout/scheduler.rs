//! 鳴らす時刻の決定のプロパティテスト
//!
//! 任意の入力列で、鳴らすと決めた音の時刻と詰める長さと補間する隙間が契約の範囲に収まり、
//! 統計が実測と一致することを検証する。目標の時刻で並べた決定と到着基準で並べた決定の
//! 両方に到達したことと、捨てる理由が常に並べすぎであることも確かめる。

use pbt::common::test_runner;
use shiguredo_moqt::playout::scheduler::{
    AUDIO_PLAYOUT_DELAY_US, AUDIO_PLAYOUT_MAX_CONCEAL_US, AUDIO_PLAYOUT_MIN_CONCEAL_US,
    AUDIO_PLAYOUT_MIN_LEAD_US, AudioPlayoutBasis, AudioPlayoutDecision, AudioPlayoutDropReason,
    AudioPlayoutInput, AudioPlayoutScheduler,
};
use shiguredo_moqt::playout::timeline::{TIMELINE_ARRIVAL_DELAY_US, TIMELINE_AUDIO_DELAY_FLOOR_US};

/// 鳴らすと決めた音は今から下限以上先に鳴り、詰める長さは音の長さの半分以内。
/// 補間する隙間は下限より長く上限以内で、補間しないときは開始も 0。
/// 破棄の統計は捨てた回数と一致し、詰めた合計と補間した合計は実際に適用した量と一致する。
/// 決定の計画 (目標の時刻か到着基準か) の両方と、捨てる決定に到達していること
#[test]
fn play_decisions_are_bounded_and_stats_match() -> noprop::TestResult {
    // 目標の時刻で並べた決定と到着基準で並べた決定の両方を通ったことを数える。どちらかが
    // 0 のままなら、生成器がその分岐へ到達しておらず、検証が空振りしている
    let timestamp_basis_seen = std::cell::Cell::new(0usize);
    let arrival_basis_seen = std::cell::Cell::new(0usize);
    let dropped_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut scheduler = AudioPlayoutScheduler::new();
        let mut now_us = noprop::sample_usize_in(ctx, 0..1_000) as i64 * 1_000;
        let mut expected_compressed_us = 0i64;
        let mut expected_concealed_us = 0i64;
        let mut expected_concealments = 0u64;
        for _ in 0..noprop::sample_usize_in(ctx, 1..16) {
            now_us += noprop::sample_usize_in(ctx, 0..100) as i64 * 1_000;
            let timestamp_us = noprop::sample_usize_in(ctx, 0..1_000_000) as i64;
            let duration_us = noprop::sample_usize_in(ctx, 1..50) as i64 * 1_000;
            // 目標は今の時刻の前後の幅から選ぶ。過去にある目標は鳴り遅れの分岐 (到着基準へ
            // 並べ直す分岐と、音が鳴っている間は遅れたまま鳴らす分岐) を、先にある目標は
            // 並べすぎの分岐を踏む
            let target_offset_us = noprop::sample_with_boundaries(
                ctx,
                &[-500_000, -100_000, 0, 100_000, 500_000],
                noprop::Ratio::one_nth(4),
                |ctx| noprop::sample_usize_in(ctx, 0..1_200) as i64 * 1_000 - 600_000,
            );
            let target_start_us = if noprop::sample_bool(ctx) {
                Some(now_us + target_offset_us)
            } else {
                None
            };
            // 到着の基準は今の時刻の前後の幅から選ぶ。到着 + 到着基準の遅れが予約できる
            // 最も早い時刻より前になる場合は、ずらすだけの分岐 (10 ms 未満なら数えない)
            // を踏む
            let arrival_offset_us = noprop::sample_with_boundaries(
                ctx,
                &[-200_000, -100_000, -80_000, 0, 80_000],
                noprop::Ratio::one_nth(4),
                |ctx| noprop::sample_usize_in(ctx, 0..600) as i64 * 1_000 - 300_000,
            );
            let arrival_us = now_us + arrival_offset_us;
            // 到着基準の遅れは [80 ms, 100 ms] の境界を必ず含める (呼び出し側が
            // `audio_arrival_delay_us` で求めた値)
            let arrival_delay_us = noprop::sample_with_boundaries(
                ctx,
                &[TIMELINE_AUDIO_DELAY_FLOOR_US, TIMELINE_ARRIVAL_DELAY_US],
                noprop::Ratio::one_nth(4),
                |ctx| noprop::sample_usize_in(ctx, 0..200) as i64 * 1_000,
            );
            let input = AudioPlayoutInput {
                now_us,
                arrival_us,
                timestamp_us,
                duration_us,
                target_start_us,
                enforce_target: noprop::sample_bool(ctx),
                delay_us: AUDIO_PLAYOUT_DELAY_US,
                arrival_delay_us,
                presentation_delay_us: AUDIO_PLAYOUT_DELAY_US,
            };
            // 目標の時刻を使うか。使わないときは必ず到着基準で並べる
            let uses_target = input.enforce_target && input.target_start_us.is_some();
            let drops_before = scheduler.drops();
            match scheduler.schedule(input) {
                AudioPlayoutDecision::Play {
                    start_at_us,
                    basis,
                    compress_us,
                    gap_start_us,
                    gap_us,
                } => {
                    // どちらの計画で並べたかを数える (検証と同じ場所で数える)
                    match basis {
                        AudioPlayoutBasis::Timestamp => {
                            timestamp_basis_seen.set(timestamp_basis_seen.get() + 1);
                        }
                        AudioPlayoutBasis::Arrival => {
                            arrival_basis_seen.set(arrival_basis_seen.get() + 1);
                        }
                    }
                    // 到着基準で並べたかどうかと、決定と記録した遅れの整合を確かめる
                    if uses_target {
                        if basis == AudioPlayoutBasis::Timestamp {
                            // 目標の時刻で並べた決定の遅れは、目標から鳴らす時刻までの差である
                            // (到着基準へ並べ直したときは 0 に戻る)
                            let target_start_us = input
                                .target_start_us
                                .expect("目標を使うと決めた入力には目標がある");
                            assert_eq!(
                                scheduler.lateness_us(),
                                start_at_us.saturating_sub(target_start_us),
                                "目標の時刻で並べた決定の遅れが一致しない: start_at_us={start_at_us} target_start_us={target_start_us}"
                            );
                        }
                    } else {
                        assert_eq!(
                            basis,
                            AudioPlayoutBasis::Arrival,
                            "目標を使えないときは到着基準で並べる: start_at_us={start_at_us}"
                        );
                    }
                    assert!(
                        start_at_us >= now_us + AUDIO_PLAYOUT_MIN_LEAD_US,
                        "鳴らす時刻が今 + 下限より前になった: start_at_us={start_at_us} now_us={now_us}"
                    );
                    assert!(
                        compress_us >= 0,
                        "詰める長さが負になった: compress_us={compress_us}"
                    );
                    assert!(
                        compress_us <= duration_us / 2,
                        "詰める長さが音の半分を超えた: compress_us={compress_us} duration_us={duration_us}"
                    );
                    // 補間する隙間は、下限を超えて上限以内である
                    assert!(
                        gap_us == 0 || gap_us > AUDIO_PLAYOUT_MIN_CONCEAL_US,
                        "補間する隙間が下限以下になった: gap_us={gap_us}"
                    );
                    assert!(
                        gap_us <= AUDIO_PLAYOUT_MAX_CONCEAL_US,
                        "補間する隙間が上限を超えた: gap_us={gap_us}"
                    );
                    // 補間しないときは開始も返さない
                    assert!(
                        gap_us != 0 || gap_start_us == 0,
                        "補間しないのに隙間の開始が返った: gap_us={gap_us} gap_start_us={gap_start_us}"
                    );
                    // 隙間は今回の開始より前で終わる
                    assert!(
                        gap_start_us.saturating_add(gap_us) <= start_at_us,
                        "隙間が今回の開始を越えた: gap_start_us={gap_start_us} gap_us={gap_us} start_at_us={start_at_us}"
                    );
                    assert_eq!(
                        scheduler.drops(),
                        drops_before,
                        "鳴らす決定では捨てた数が増えない: start_at_us={start_at_us}"
                    );
                    // 要求した詰める量と隙間の半分を実際に適用したものとして返す
                    let applied_us = compress_us / 2;
                    expected_compressed_us += applied_us;
                    scheduler.confirm_stretch(applied_us);
                    let concealed_us = gap_us / 2;
                    if concealed_us > 0 {
                        expected_concealments += 1;
                        expected_concealed_us += concealed_us;
                    }
                    scheduler.confirm_concealment(concealed_us);
                }
                AudioPlayoutDecision::Drop { reason } => {
                    assert_eq!(
                        reason,
                        AudioPlayoutDropReason::Backlog,
                        "捨てる理由は並べすぎだけである"
                    );
                    assert_eq!(
                        scheduler.drops(),
                        drops_before + 1,
                        "捨てた決定では捨てた数が 1 増える"
                    );
                    dropped_seen.set(dropped_seen.get() + 1);
                }
            }
            assert_eq!(scheduler.compressed_us(), expected_compressed_us);
            assert_eq!(scheduler.concealed_us(), expected_concealed_us);
            assert_eq!(scheduler.concealments(), expected_concealments);
        }
        Ok(())
    })?;
    assert!(
        timestamp_basis_seen.get() > 0,
        "目標の時刻で並べた決定に到達していない\n{runner}"
    );
    assert!(
        arrival_basis_seen.get() > 0,
        "到着基準で並べた決定に到達していない\n{runner}"
    );
    assert!(
        dropped_seen.get() > 0,
        "並べすぎで捨てる決定に到達していない\n{runner}"
    );
    Ok(())
}
