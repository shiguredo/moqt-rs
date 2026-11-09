//! 鳴らす時刻の決定のプロパティテスト
//!
//! 任意の入力列で、計画 (schedule) の値が契約の範囲に収まり、実際に出力へ積んだ開始位置と
//! 長さで確定 (commit) した後の予約位置と統計が実測と一致することを検証する。時刻基準と
//! 到着基準の両方、捨てる決定、詰める決定、伸ばす決定、予約できない前置きを含む隙間、
//! 前の音の終わりへ揃える決定は、入力の偶然に頼らず `tests/test_playout/scheduler.rs` の
//! 固定した境界で確かめる。

use pbt::common::test_runner;
use shiguredo_moqt::playout::scheduler::{
    AUDIO_PLAYOUT_DELAY_US, AUDIO_PLAYOUT_MAX_CONCEAL_US, AUDIO_PLAYOUT_MIN_CONCEAL_US,
    AUDIO_PLAYOUT_MIN_LEAD_US, AUDIO_PLAYOUT_STRETCH_MIN_US, AudioPlayoutBasis, AudioPlayoutCommit,
    AudioPlayoutDecision, AudioPlayoutDropReason, AudioPlayoutInput, AudioPlayoutScheduler,
};
use shiguredo_moqt::playout::timeline::{TIMELINE_ARRIVAL_DELAY_US, TIMELINE_AUDIO_DELAY_FLOOR_US};

/// 計画は鳴らす時刻と詰める・伸ばす長さと隙間の契約を満たす。
/// 確定は実際の開始位置と長さだけを予約位置と統計へ反映する。
#[test]
fn play_decisions_are_bounded_and_commits_match_the_actual_values() -> noprop::TestResult {
    // 到達を数える。どれかが 0 のままなら、生成器がその分岐へ到達しておらず検証が
    // 空振りしている
    let timestamp_basis_seen = std::cell::Cell::new(0usize);
    let arrival_basis_seen = std::cell::Cell::new(0usize);
    let dropped_seen = std::cell::Cell::new(0usize);
    let compressed_seen = std::cell::Cell::new(0usize);
    let expanded_seen = std::cell::Cell::new(0usize);
    let partial_concealment_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut scheduler = AudioPlayoutScheduler::new();
        let mut now_us = noprop::sample_usize_in(ctx, 0..1_000) as i64 * 1_000;
        let mut expected_compressed_us = 0i64;
        let mut expected_expanded_us = 0i64;
        let mut expected_concealed_us = 0i64;
        let mut expected_concealments = 0u64;
        // 直前に確定した音の終わりを呼び出し側でも持つ
        let mut previous_end_us: Option<i64> = None;
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
            // 到着基準の遅れは [50 ms, 100 ms] の境界を必ず含める (呼び出し側が
            // `audio_arrival_delay_us` で求めた値)
            let arrival_delay_us = noprop::sample_with_boundaries(
                ctx,
                &[TIMELINE_AUDIO_DELAY_FLOOR_US, TIMELINE_ARRIVAL_DELAY_US],
                noprop::Ratio::one_nth(4),
                |ctx| noprop::sample_usize_in(ctx, 0..200) as i64 * 1_000,
            );
            // 目標ごとの不感帯。timestamp 経路だけが使う
            let stretch_min_us = if noprop::sample_bool(ctx) {
                Some(noprop::sample_usize_in(ctx, 0..=30_000) as i64)
            } else {
                None
            };
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
                stretch_min_us,
            };
            let drops_before = scheduler.drops();
            let plan = scheduler.schedule(input);
            match plan.decision() {
                AudioPlayoutDecision::Play {
                    start_at_us,
                    basis,
                    compress_us,
                    expand_us,
                    total_gap_us,
                    conceal_us,
                    unrecoverable_gap_us,
                } => {
                    // どちらの基準で並べたかを数える (検証と同じ場所で数える)
                    match basis {
                        AudioPlayoutBasis::Timestamp => {
                            timestamp_basis_seen.set(timestamp_basis_seen.get() + 1);
                        }
                        AudioPlayoutBasis::Arrival => {
                            arrival_basis_seen.set(arrival_basis_seen.get() + 1);
                        }
                    }
                    // 目標を守ると指定していても、目標が無ければ到着基準になる。また、
                    // 目標から離れすぎて音が途切れていれば、到着基準へ並べ直す
                    if !input.enforce_target || input.target_start_us.is_none() {
                        assert_eq!(
                            basis,
                            AudioPlayoutBasis::Arrival,
                            "目標を使えないときは到着基準で並べる: start_at_us={start_at_us}"
                        );
                    }
                    // snap は開始を前の音の終わりへ最大 AUDIO_PLAYOUT_MIN_CONCEAL_US だけ
                    // 手前へ戻すことがある
                    assert!(
                        start_at_us
                            >= now_us + AUDIO_PLAYOUT_MIN_LEAD_US - AUDIO_PLAYOUT_MIN_CONCEAL_US,
                        "鳴らす時刻が今 + 下限より大きく前になった: start_at_us={start_at_us} now_us={now_us}"
                    );
                    // 時刻基準で目標が残っているときは、snap の分だけ手前までに留まる
                    if basis == AudioPlayoutBasis::Timestamp
                        && let Some(target_start_us) = input.target_start_us
                    {
                        assert!(
                            start_at_us >= target_start_us - AUDIO_PLAYOUT_MIN_CONCEAL_US,
                            "snap で目標より大きく手前に鳴った: start_at_us={start_at_us} target_start_us={target_start_us}"
                        );
                    }
                    assert!(
                        compress_us >= 0,
                        "詰める長さが負になった: compress_us={compress_us}"
                    );
                    assert!(
                        compress_us <= duration_us / 2,
                        "詰める長さが音の半分を超えた: compress_us={compress_us} duration_us={duration_us}"
                    );
                    // 詰めるのは、目標からの遅れが不感帯に届いたときだけである
                    let target_us = if basis == AudioPlayoutBasis::Timestamp {
                        input
                            .target_start_us
                            .expect("時刻基準なら目標がある")
                    } else {
                        arrival_us.saturating_add(arrival_delay_us)
                    };
                    let error_us = start_at_us.saturating_sub(target_us);
                    let deadband_us =
                        stretch_min_us.unwrap_or(AUDIO_PLAYOUT_STRETCH_MIN_US);
                    if compress_us > 0 {
                        assert!(
                            error_us >= deadband_us,
                            "不感帯の中で詰めた: compress_us={compress_us} error_us={error_us} deadband_us={deadband_us} start_at_us={start_at_us} target_us={target_us} stretch_min_us={stretch_min_us:?} basis={basis:?} now_us={now_us} arrival_us={arrival_us} arrival_delay_us={arrival_delay_us}"
                        );
                        // 到着基準の圧縮は、隙間を挟まない音だけを対象にする
                        if basis == AudioPlayoutBasis::Arrival {
                            assert_eq!(
                                total_gap_us, 0,
                                "到着基準で隙間を挟む音を詰めた: start_at_us={start_at_us}"
                            );
                        }
                        compressed_seen.set(compressed_seen.get() + 1);
                    }
                    // 隙間の全長は、直前の音の終わりから今回の開始までである
                    let expected_gap_us = previous_end_us.map_or(0, |previous_end_us| {
                        start_at_us.saturating_sub(previous_end_us).max(0)
                    });
                    assert_eq!(
                        total_gap_us, expected_gap_us,
                        "隙間の全長が開始と直前の音の終わりの差でない: start_at_us={start_at_us} previous_end_us={previous_end_us:?}"
                    );
                    assert!(
                        (0..=total_gap_us).contains(&unrecoverable_gap_us),
                        "予約できない長さが全長を超えた: total={total_gap_us} unrecoverable={unrecoverable_gap_us}"
                    );
                    assert!(
                        conceal_us >= 0
                            && conceal_us <= total_gap_us.saturating_sub(unrecoverable_gap_us),
                        "補間する長さが予約できる区間を超えた: total={total_gap_us} unrecoverable={unrecoverable_gap_us} conceal={conceal_us}"
                    );
                    assert!(
                        conceal_us <= AUDIO_PLAYOUT_MAX_CONCEAL_US,
                        "補間する長さが上限を超えた: conceal_us={conceal_us}"
                    );
                    if conceal_us > 0 {
                        assert!(
                            conceal_us > AUDIO_PLAYOUT_MIN_CONCEAL_US,
                            "下限以下の区間を補間しようとした: conceal_us={conceal_us}"
                        );
                    }
                    if unrecoverable_gap_us > 0 {
                        partial_concealment_seen.set(partial_concealment_seen.get() + 1);
                    }
                    // 伸長は到着基準だけで要求し、音の長さ以内である
                    assert!(
                        expand_us >= 0 && expand_us <= duration_us,
                        "伸長の長さが音の長さを超えた: expand_us={expand_us} duration_us={duration_us}"
                    );
                    if expand_us > 0 {
                        assert_eq!(
                            basis,
                            AudioPlayoutBasis::Arrival,
                            "timestamp 経路で伸長を要求した"
                        );
                        assert!(
                            start_at_us < target_us,
                            "深さが戻っているのに伸長を要求した: start_at_us={start_at_us} target_us={target_us}"
                        );
                        expanded_seen.set(expanded_seen.get() + 1);
                    }
                    assert_eq!(
                        scheduler.drops(),
                        drops_before,
                        "鳴らす決定では捨てた数が増えない: start_at_us={start_at_us}"
                    );

                    // 実際に積んだ値として、詰める要求の半分を詰め、伸ばす要求の半分を
                    // 伸ばした長さで確定する (呼び出し側の適用結果を模した値ではなく、
                    // 確定がその値だけを反映することの検証である)
                    let applied_compress_us = compress_us / 2;
                    let applied_expand_us = expand_us / 2;
                    let played_us = duration_us
                        .saturating_sub(applied_compress_us)
                        .saturating_add(applied_expand_us);
                    let play = scheduler
                        .commit(
                            plan,
                            AudioPlayoutCommit {
                                start_at_us,
                                played_us,
                            },
                        )
                        .expect("計画した開始位置と正の長さで確定できること");
                    expected_compressed_us += duration_us.saturating_sub(played_us).max(0);
                    expected_expanded_us += played_us.saturating_sub(duration_us).max(0);
                    assert_eq!(scheduler.compressed_us(), expected_compressed_us);
                    assert_eq!(scheduler.expanded_us(), expected_expanded_us);
                    assert_eq!(play.start_at_us, start_at_us);
                    assert_eq!(play.played_us, played_us);
                    assert_eq!(play.arrival_us, input.arrival_us);
                    assert_eq!(play.target_start_us, input.target_start_us);
                    assert_eq!(play.basis, basis);
                    previous_end_us = Some(start_at_us.saturating_add(played_us));
                    assert_eq!(scheduler.last_end_us(), previous_end_us);

                    // 隙間の半分を材料化したものとして、実際に補間した長さを記録する
                    let concealed_us = (total_gap_us / 2).min(conceal_us);
                    if concealed_us > 0 {
                        expected_concealed_us += concealed_us;
                        expected_concealments += 1;
                        scheduler.record_rendered_concealment(concealed_us, 1);
                    }
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
                    // 捨てた音は予約位置も実績も変えない。前の音の終わりも動かない
                    assert_eq!(scheduler.last_end_us(), previous_end_us);
                    // 捨てた計画は確定できない
                    assert!(
                        scheduler
                            .commit(
                                plan,
                                AudioPlayoutCommit {
                                    start_at_us: now_us,
                                    played_us: duration_us,
                                },
                            )
                            .is_err(),
                        "捨てた計画を確定できてしまった"
                    );
                    dropped_seen.set(dropped_seen.get() + 1);
                }
            }
            assert_eq!(scheduler.concealed_us(), expected_concealed_us);
            assert_eq!(scheduler.concealments(), expected_concealments);
        }
        Ok(())
    })?;
    assert!(
        timestamp_basis_seen.get() > 0,
        "時刻基準で並べた決定に到達していない\n{runner}"
    );
    assert!(
        arrival_basis_seen.get() > 0,
        "到着基準で並べた決定に到達していない\n{runner}"
    );
    assert!(
        dropped_seen.get() > 0,
        "並べすぎで捨てる決定に到達していない\n{runner}"
    );
    assert!(
        compressed_seen.get() > 0,
        "詰める決定に到達していない\n{runner}"
    );
    assert!(
        expanded_seen.get() > 0,
        "到着基準の深さを戻す伸長に到達していない\n{runner}"
    );
    assert!(
        partial_concealment_seen.get() > 0,
        "予約できない隙間を含む決定に到達していない\n{runner}"
    );
    Ok(())
}
