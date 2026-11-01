//! 鳴らす時刻の決定のプロパティテスト
//!
//! 任意の入力列で、鳴らすと決めた音の時刻と詰める長さと隙間が契約の範囲に収まり、
//! 統計が実測と一致することを検証する。目標の時刻で並べた決定と到着基準で並べた決定の
//! 両方に到達したこと、捨てる理由が常に並べすぎであること、再同期後の時間伸長と
//! 部分的に予約できない隙間 (snap と 3 分割) に到達したことも確かめる。

use pbt::common::test_runner;
use shiguredo_moqt::playout::scheduler::{
    AUDIO_PLAYOUT_DELAY_US, AUDIO_PLAYOUT_MAX_CONCEAL_US, AUDIO_PLAYOUT_MIN_CONCEAL_US,
    AUDIO_PLAYOUT_MIN_LEAD_US, AUDIO_PLAYOUT_STRETCH_MIN_US, AudioPlayoutBasis,
    AudioPlayoutDecision, AudioPlayoutDropReason, AudioPlayoutInput, AudioPlayoutScheduler,
};
use shiguredo_moqt::playout::timeline::{TIMELINE_ARRIVAL_DELAY_US, TIMELINE_AUDIO_DELAY_FLOOR_US};

/// 鳴らすと決めた音は今から下限以上先 (snap の許容だけ手前まで) に鳴る。
/// 詰める長さは音の長さの半分以内で、遅れが不感帯に届いたときだけであり、キューの余裕
/// (直前の音の終わり − (今 + 最小の余裕)) 以内である。隙間の 3 分割は全長と予約できない
/// 長さと補間の長さの関係を満たし、補間するのは下限を超えるときに限る。
/// 再同期後の伸長は到着基準だけで要求し、音の長さ以内である。
#[test]
fn play_decisions_are_bounded_and_stats_match() -> noprop::TestResult {
    // 到達を数える。どれかが 0 のままなら、生成器がその分岐へ到達しておらず検証が
    // 空振りしている
    let timestamp_basis_seen = std::cell::Cell::new(0usize);
    let arrival_basis_seen = std::cell::Cell::new(0usize);
    let dropped_seen = std::cell::Cell::new(0usize);
    let compressed_seen = std::cell::Cell::new(0usize);
    let expanded_seen = std::cell::Cell::new(0usize);
    let partial_concealment_seen = std::cell::Cell::new(0usize);
    let snapped_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut scheduler = AudioPlayoutScheduler::new();
        let mut now_us = noprop::sample_usize_in(ctx, 0..1_000) as i64 * 1_000;
        let mut expected_compressed_us = 0i64;
        let mut expected_concealed_us = 0i64;
        let mut expected_concealments = 0u64;
        // 直前の音の終わり (schedule が記録する値) を呼び出し側でも持つ
        let mut previous_end_us: Option<i64> = None;
        let mut expected_expanded_us = 0i64;
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
            // 目標の時刻を使うか。使わないときは必ず到着基準で並べる
            let uses_target = input.enforce_target && input.target_start_us.is_some();
            let drops_before = scheduler.drops();
            match scheduler.schedule(input) {
                AudioPlayoutDecision::Play {
                    start_at_us,
                    basis,
                    compress_us,
                    expand_us,
                    gap_start_us,
                    conceal_us,
                    total_gap_us,
                    unrecoverable_gap_us,
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
                    if uses_target {
                        if basis == AudioPlayoutBasis::Timestamp {
                            // 目標の時刻で並べた決定の遅れは、目標から鳴らす時刻までの差
                            // である (snap で目標より手前になる場合は 0 に丸める)
                            let target_start_us = input
                                .target_start_us
                                .expect("目標を使うと決めた入力には目標がある");
                            assert_eq!(
                                scheduler.lateness_us(),
                                start_at_us.saturating_sub(target_start_us).max(0),
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
                    // snap は開始を前の音の終わりへ最大 AUDIO_PLAYOUT_MIN_CONCEAL_US だけ
                    // 手前へ戻すことがある
                    assert!(
                        start_at_us >= now_us + AUDIO_PLAYOUT_MIN_LEAD_US - AUDIO_PLAYOUT_MIN_CONCEAL_US,
                        "鳴らす時刻が今 + 下限より大きく前になった: start_at_us={start_at_us} now_us={now_us}"
                    );
                    // 目標の時刻で並べたときだけ、snap が最大で下限 (0.5 ms) だけ手前へ
                    // 戻す。到着基準は目標を使えない (TIMESTAMP が壁時計からずれている)
                    // ため、目標より手前に鳴ることがある
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
                    // 詰めるのは、遅れが不感帯に届いたときだけである。timestamp 経路は
                    // 目標ごとの不感帯、到着基準は scheduler の既定値を使う
                    let deadband_us = if basis == AudioPlayoutBasis::Timestamp {
                        stretch_min_us.unwrap_or(AUDIO_PLAYOUT_STRETCH_MIN_US)
                    } else {
                        AUDIO_PLAYOUT_STRETCH_MIN_US
                    };
                    assert!(
                        compress_us == 0 || scheduler.lateness_us() >= deadband_us,
                        "不感帯の中で詰めた: compress_us={compress_us} lateness_us={} deadband_us={deadband_us}",
                        scheduler.lateness_us()
                    );
                    // 詰める量はキューの余裕 (直前の音の終わり − (今 + 最小の余裕)) 以内で
                    // ある。timestamp 経路は隙間を挟んでいても詰めるため、余裕だけで検査する
                    if compress_us > 0 {
                        let slack_us = previous_end_us.map_or(0, |previous_end_us| {
                            previous_end_us
                                .saturating_sub(now_us.saturating_add(AUDIO_PLAYOUT_MIN_LEAD_US))
                        });
                        assert!(
                            compress_us <= slack_us,
                            "キューの余裕を超えて詰めた: compress_us={compress_us} slack_us={slack_us} now_us={now_us}"
                        );
                        compressed_seen.set(compressed_seen.get() + 1);
                    }
                    if basis == AudioPlayoutBasis::Arrival && compress_us > 0 {
                        // 到着基準の圧縮は、隙間を挟まない音だけを対象にする
                        assert!(
                            previous_end_us.is_some_and(|previous_end_us| start_at_us <= previous_end_us),
                            "到着基準で隙間を挟む音を詰めた: start_at_us={start_at_us} previous_end_us={previous_end_us:?}"
                        );
                    }
                    // 到着基準では、前の音と重ならない
                    if basis == AudioPlayoutBasis::Arrival
                        && let Some(previous_end_us) = previous_end_us
                    {
                        assert!(
                            start_at_us >= previous_end_us,
                            "到着基準で前の音と重なった: start_at_us={start_at_us} previous_end_us={previous_end_us}"
                        );
                    }
                    // 隙間の全長は、直前の音の終わりから今回の開始までである
                    let expected_gap_us = previous_end_us.map_or(0, |previous_end_us| {
                        start_at_us.saturating_sub(previous_end_us).max(0)
                    });
                    assert_eq!(
                        total_gap_us, expected_gap_us,
                        "隙間の全長が開始と直前の音の終わりの差でない: start_at_us={start_at_us} previous_end_us={previous_end_us:?}"
                    );
                    assert!(total_gap_us >= 0, "隙間の全長が負になった");
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
                    assert_eq!(
                        gap_start_us.is_some(),
                        conceal_us > 0,
                        "補間の先頭は補間するときだけ返す: gap_start_us={gap_start_us:?} conceal_us={conceal_us}"
                    );
                    if let Some(gap_start_us) = gap_start_us {
                        assert!(
                            gap_start_us <= start_at_us,
                            "補間の先頭が今回の開始より後になった: gap_start_us={gap_start_us} start_at_us={start_at_us}"
                        );
                        assert_eq!(
                            conceal_us,
                            start_at_us
                                .saturating_sub(gap_start_us)
                                .min(AUDIO_PLAYOUT_MAX_CONCEAL_US),
                            "補間する長さの決め方が違う: gap_start_us={gap_start_us} start_at_us={start_at_us} conceal_us={conceal_us}"
                        );
                        if total_gap_us > AUDIO_PLAYOUT_MIN_CONCEAL_US {
                            assert!(
                                conceal_us > AUDIO_PLAYOUT_MIN_CONCEAL_US,
                                "下限以下の区間を補間しようとした: conceal_us={conceal_us}"
                            );
                        }
                    }
                    if unrecoverable_gap_us > 0 {
                        partial_concealment_seen.set(partial_concealment_seen.get() + 1);
                    }
                    if previous_end_us.is_some_and(|previous_end_us| {
                        start_at_us == previous_end_us && start_at_us < now_us + AUDIO_PLAYOUT_MIN_LEAD_US + AUDIO_PLAYOUT_MIN_CONCEAL_US
                    }) {
                        snapped_seen.set(snapped_seen.get() + 1);
                    }
                    // 再同期後の深さの回復 (時間伸長) は到着基準だけで要求する
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
                            start_at_us < arrival_us.saturating_add(arrival_delay_us),
                            "深さが戻っているのに伸長を要求した: start_at_us={start_at_us}"
                        );
                        expanded_seen.set(expanded_seen.get() + 1);
                    }
                    assert_eq!(
                        scheduler.drops(),
                        drops_before,
                        "鳴らす決定では捨てた数が増えない: start_at_us={start_at_us}"
                    );
                    // 要求した詰める量の半分を実際に適用したものとして返す
                    let applied_us = compress_us / 2;
                    expected_compressed_us += applied_us;
                    scheduler.confirm_stretch(applied_us);
                    // 要求した伸長の半分を実際に適用したものとして返す
                    let applied_expand_us = expand_us / 2;
                    expected_expanded_us += applied_expand_us;
                    scheduler.confirm_expansion(applied_expand_us);
                    // 実際に鳴る長さは、実際に詰めた分と実際に伸ばした分を反映する
                    // (moqt-js の playSound が実フレーム数を recordPlay へ渡すのと同じ)
                    let expected_played_us = duration_us
                        .saturating_sub(applied_us)
                        .saturating_add(applied_expand_us);
                    assert_eq!(
                        scheduler
                            .last_play()
                            .expect("鳴らすと決めた音の値がある")
                            .played_us,
                        expected_played_us,
                        "実際に鳴る長さが詰めた分と伸ばした分を反映しない: duration_us={duration_us} applied_us={applied_us} applied_expand_us={applied_expand_us}"
                    );
                    // 直前の音の終わりを写し取る。schedule は要求した詰める量を反映した
                    // 終わりを記録し、confirm_stretch が実際に詰めた長さとの差を反映する。
                    // confirm_expansion は伸ばした分だけ終わりを後ろへ動かす
                    previous_end_us = Some(
                        start_at_us
                            .saturating_add(duration_us)
                            .saturating_sub(compress_us)
                            .saturating_add(compress_us.saturating_sub(applied_us))
                            .saturating_add(applied_expand_us),
                    );
                    // 隙間の半分を補間したものとして返す (要求した長さまでに切られる)
                    let concealed_us = (total_gap_us / 2).min(conceal_us);
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
            assert_eq!(scheduler.expanded_us(), expected_expanded_us);
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
    assert!(
        compressed_seen.get() > 0,
        "詰める決定に到達していない\n{runner}"
    );
    assert!(
        expanded_seen.get() > 0,
        "再同期後の伸長に到達していない\n{runner}"
    );
    assert!(
        partial_concealment_seen.get() > 0,
        "予約できない隙間を含む決定に到達していない\n{runner}"
    );
    assert!(
        snapped_seen.get() > 0,
        "前の音の終わりへ揃える決定に到達していない\n{runner}"
    );
    Ok(())
}
