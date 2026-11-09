//! 鳴らす時刻を決めるスケジューラのテスト
//!
//! 公開 API (`AudioPlayoutScheduler`) の契約を確認する。計画 (schedule) は状態を進めず、
//! 実際に出力へ積んだ開始位置と長さで確定 (commit) して初めて予約位置と実績が進むことを
//! 中心に確かめる。

use shiguredo_moqt::playout::scheduler::{
    AUDIO_PLAYOUT_BACKLOG_US, AUDIO_PLAYOUT_DELAY_US, AUDIO_PLAYOUT_MAX_CONCEAL_US,
    AUDIO_PLAYOUT_MAX_DELAY_US, AudioPlayoutBasis, AudioPlayoutCommit, AudioPlayoutCommitError,
    AudioPlayoutConfig, AudioPlayoutDecision, AudioPlayoutDropReason, AudioPlayoutInput,
    AudioPlayoutPlan, AudioPlayoutPlay, AudioPlayoutScheduler,
};
use shiguredo_moqt::playout::timeline::{
    TIMELINE_ARRIVAL_DELAY_US, audio_arrival_delay_us_with_min,
};

/// 目標あり (守る) の入力を作る
///
/// 到着の基準は今の時刻と同じにし、到着基準の遅れは [`AUDIO_PLAYOUT_DELAY_US`] にする。
/// 到着の基準と到着基準の遅れを変えるときは、作った入力のフィールドを書き換える。
fn input(
    now_us: i64,
    timestamp_us: i64,
    duration_us: i64,
    target_start_us: Option<i64>,
) -> AudioPlayoutInput {
    AudioPlayoutInput {
        now_us,
        arrival_us: now_us,
        timestamp_us,
        duration_us,
        target_start_us,
        enforce_target: target_start_us.is_some(),
        delay_us: AUDIO_PLAYOUT_DELAY_US,
        arrival_delay_us: AUDIO_PLAYOUT_DELAY_US,
        presentation_delay_us: AUDIO_PLAYOUT_DELAY_US,
        stretch_min_us: None,
    }
}

/// 計画から比べたい値だけを取り出す
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Play {
    start_at_us: i64,
    basis: AudioPlayoutBasis,
    compress_us: i64,
    expand_us: i64,
    total_gap_us: i64,
    conceal_us: i64,
    unrecoverable_gap_us: i64,
}

/// 再生の計画を取り出す (捨てる計画ならテストを失敗させる)
fn play(plan: AudioPlayoutPlan) -> Play {
    match plan.decision() {
        AudioPlayoutDecision::Play {
            start_at_us,
            basis,
            compress_us,
            expand_us,
            total_gap_us,
            conceal_us,
            unrecoverable_gap_us,
        } => Play {
            start_at_us,
            basis,
            compress_us,
            expand_us,
            total_gap_us,
            conceal_us,
            unrecoverable_gap_us,
        },
        AudioPlayoutDecision::Drop { reason } => {
            panic!("再生の計画を期待したが、捨てる計画になった (理由 {reason:?})")
        }
    }
}

/// 計画した開始位置と、指定した長さで確定する
fn commit_as_planned(
    scheduler: &mut AudioPlayoutScheduler,
    plan: AudioPlayoutPlan,
    played_us: i64,
) -> AudioPlayoutPlay {
    let start_at_us = play(plan).start_at_us;
    scheduler
        .commit(
            plan,
            AudioPlayoutCommit {
                start_at_us,
                played_us,
            },
        )
        .expect("計画した位置と長さで確定できること")
}

/// 目標 100 ms に 20 ms の音を 1 つ確定した状態にする
///
/// 今 0、TIMESTAMP 0、目標 100 ms の音は 100 ms から 120 ms まで鳴る。
fn schedule_queued_sound(scheduler: &mut AudioPlayoutScheduler) {
    let decision = scheduler.schedule(input(0, 0, 20_000, Some(100_000)));
    assert_eq!(
        play(decision),
        Play {
            start_at_us: 100_000,
            basis: AudioPlayoutBasis::Timestamp,
            compress_us: 0,
            expand_us: 0,
            total_gap_us: 0,
            conceal_us: 0,
            unrecoverable_gap_us: 0,
        },
        "目標の時刻に並べること"
    );
    commit_as_planned(scheduler, decision, 20_000);
    assert_eq!(scheduler.last_end_us(), Some(120_000));
}

#[test]
fn sound_in_time_plays_at_the_target() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let decision = scheduler.schedule(input(0, 5_000, 20_000, Some(100_000)));
    assert_eq!(
        play(decision),
        Play {
            start_at_us: 100_000,
            basis: AudioPlayoutBasis::Timestamp,
            compress_us: 0,
            expand_us: 0,
            total_gap_us: 0,
            conceal_us: 0,
            unrecoverable_gap_us: 0,
        },
        "目標どおりに鳴らすときは目標の時刻を基準にすること"
    );
    assert_eq!(
        scheduler.last_end_us(),
        None,
        "計画しただけでは予約位置を進めないこと"
    );
    commit_as_planned(&mut scheduler, decision, 20_000);
    assert_eq!(scheduler.last_end_us(), Some(120_000));
    assert_eq!(scheduler.lateness_us(), 0);
}

#[test]
fn late_sound_without_a_previous_sound_is_not_compressed() {
    let mut scheduler = AudioPlayoutScheduler::new();
    // 目標 100 ms を 60 ms 過ぎて届いた。今 + 10 ms へずらすが、並べてある音が無いため詰めない
    let decision = scheduler.schedule(input(150_000, 5_000, 20_000, Some(100_000)));
    assert_eq!(
        play(decision),
        Play {
            start_at_us: 160_000,
            basis: AudioPlayoutBasis::Timestamp,
            compress_us: 0,
            expand_us: 0,
            total_gap_us: 0,
            conceal_us: 0,
            unrecoverable_gap_us: 0,
        },
        "直前の音が無いときは詰めないこと"
    );
    commit_as_planned(&mut scheduler, decision, 20_000);
    assert_eq!(
        scheduler.lateness_us(),
        60_000,
        "遅れはそのまま記録すること"
    );
    assert_eq!(scheduler.compressed_us(), 0, "詰めた合計が増えないこと");
}

#[test]
fn lateness_inside_the_stretch_deadband_is_not_compressed() {
    let mut scheduler = AudioPlayoutScheduler::new();
    schedule_queued_sound(&mut scheduler);
    // 前の音の終わり (120 ms) の 5 ms 後を目標にする。不感帯 (20 ms) の内側
    let decision = scheduler.schedule(input(20_000, 20_000, 20_000, Some(125_000)));
    assert_eq!(
        play(decision),
        Play {
            start_at_us: 125_000,
            basis: AudioPlayoutBasis::Timestamp,
            compress_us: 0,
            expand_us: 0,
            total_gap_us: 5_000,
            conceal_us: 5_000,
            unrecoverable_gap_us: 0,
        },
        "不感帯の内側では詰めないこと"
    );
}

#[test]
fn compression_at_the_stretch_deadband_is_capped_by_half_the_duration() {
    let mut scheduler = AudioPlayoutScheduler::new();
    schedule_queued_sound(&mut scheduler);
    // 前の音の終わり (120 ms) に置くが、目標は 100 ms なので遅れが不感帯と同じ 20 ms
    let decision = scheduler.schedule(input(20_000, 20_000, 20_000, Some(100_000)));
    assert_eq!(
        play(decision),
        Play {
            start_at_us: 120_000,
            basis: AudioPlayoutBasis::Timestamp,
            compress_us: 10_000,
            expand_us: 0,
            total_gap_us: 0,
            conceal_us: 0,
            unrecoverable_gap_us: 0,
        },
        "不感帯と同じ遅れは詰め始め、音の長さの半分までにすること"
    );
    commit_as_planned(&mut scheduler, decision, 10_000);
    assert_eq!(scheduler.compressed_us(), 10_000);
}

#[test]
fn overlapping_sound_is_appended_to_the_previous_one_and_compressed() {
    let mut scheduler = AudioPlayoutScheduler::new();
    schedule_queued_sound(&mut scheduler);
    // 前の音の終わり (120 ms) より 20 ms 前を目標にする。重ねず、前の音の終わりへ置く
    let decision = scheduler.schedule(input(20_000, 20_000, 20_000, Some(100_000)));
    assert_eq!(
        play(decision),
        Play {
            start_at_us: 120_000,
            basis: AudioPlayoutBasis::Timestamp,
            compress_us: 10_000,
            expand_us: 0,
            total_gap_us: 0,
            conceal_us: 0,
            unrecoverable_gap_us: 0,
        },
        "重なる音は前の音の終わりへ置き、遅れの分を詰めること"
    );
}

#[test]
fn compression_is_capped_by_the_queue_slack() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 100_000, Some(100_000)));
    commit_as_planned(&mut scheduler, first, 100_000);
    assert_eq!(scheduler.last_end_us(), Some(200_000));
    // 前の音の終わり (200 ms) の 5 ms 前から積める。遅れは 30 ms だが、詰められるのは 5 ms
    let decision = scheduler.schedule(input(185_000, 100_000, 20_000, Some(170_000)));
    assert_eq!(
        play(decision),
        Play {
            start_at_us: 200_000,
            basis: AudioPlayoutBasis::Timestamp,
            compress_us: 5_000,
            expand_us: 0,
            total_gap_us: 0,
            conceal_us: 0,
            unrecoverable_gap_us: 0,
        },
        "キューの余裕を超えて詰めないこと"
    );
}

#[test]
fn zero_queue_slack_is_not_compressed() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 100_000, Some(100_000)));
    commit_as_planned(&mut scheduler, first, 100_000);
    // 今 + 最小の余裕 (10 ms) が、ちょうど前の音の終わり (200 ms) に等しい
    let decision = scheduler.schedule(input(190_000, 100_000, 20_000, Some(170_000)));
    assert_eq!(
        play(decision),
        Play {
            start_at_us: 200_000,
            basis: AudioPlayoutBasis::Timestamp,
            compress_us: 0,
            expand_us: 0,
            total_gap_us: 0,
            conceal_us: 0,
            unrecoverable_gap_us: 0,
        },
        "キューの余裕が 0 のときは詰めないこと"
    );
}

#[test]
fn negative_queue_slack_is_not_compressed() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 100_000, Some(100_000)));
    commit_as_planned(&mut scheduler, first, 100_000);
    // 今 + 最小の余裕 (205 ms) が、前の音の終わり (200 ms) より後
    let decision = scheduler.schedule(input(195_000, 100_000, 20_000, Some(170_000)));
    assert_eq!(
        play(decision),
        Play {
            start_at_us: 205_000,
            basis: AudioPlayoutBasis::Timestamp,
            compress_us: 0,
            expand_us: 0,
            total_gap_us: 5_000,
            conceal_us: 0,
            unrecoverable_gap_us: 5_000,
        },
        "キューの余裕が負のときは詰めないこと"
    );
}

#[test]
fn commit_advances_the_position_by_the_actual_start_and_length() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first_plan = scheduler.schedule(input(1_000_000, 0, 20_000, None));
    let first = play(first_plan);
    assert_eq!(first.start_at_us, 1_050_000, "到着 + 下限から鳴らすこと");
    // 実際には 12 ms 遅れて、881 / 44100 秒だけ積めた
    let actual_start_us = first.start_at_us + 12_000;
    let actual_played_us = 19_977;
    let committed = scheduler
        .commit(
            first_plan,
            AudioPlayoutCommit {
                start_at_us: actual_start_us,
                played_us: actual_played_us,
            },
        )
        .expect("実際の値で確定できること");
    assert_eq!(committed.start_at_us, actual_start_us);
    assert_eq!(committed.played_us, actual_played_us);
    let last_end_us = actual_start_us + actual_played_us;
    assert_eq!(scheduler.last_end_us(), Some(last_end_us));
    let next = scheduler.schedule(input(1_000_000, 40_000, 20_000, None));
    assert_eq!(
        play(next).start_at_us,
        last_end_us + 20_000,
        "確定した終端と TIMESTAMP の差から次の位置を決めること"
    );
    assert_eq!(play(next).total_gap_us, 20_000);
}

#[test]
fn failed_plan_does_not_change_the_position() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let failed = scheduler.schedule(input(1_000_000, 0, 20_000, None));
    let accepted = scheduler.schedule(input(1_000_000, 20_000, 20_000, None));
    assert_eq!(
        play(accepted).start_at_us,
        play(failed).start_at_us,
        "確定しなかった計画は位置を進めないこと"
    );
    assert_eq!(scheduler.compressed_us(), 0);
    assert_eq!(scheduler.rebases(), 0);
    // 失効した計画は確定できない
    assert_eq!(
        scheduler.commit(
            failed,
            AudioPlayoutCommit {
                start_at_us: play(failed).start_at_us,
                played_us: 20_000,
            },
        ),
        Err(AudioPlayoutCommitError::NoPendingPlan)
    );
    commit_as_planned(&mut scheduler, accepted, 20_000);
    assert_eq!(
        play(scheduler.schedule(input(1_000_000, 40_000, 20_000, None))).start_at_us,
        1_070_000,
        "確定した計画だけが次の位置へ反映されること"
    );
}

#[test]
fn commit_rejects_an_expired_double_or_invalid_plan() {
    let mut scheduler = AudioPlayoutScheduler::new();
    // 計画を作ってからやり直すと、その計画では確定できない
    let expired = scheduler.schedule(input(0, 0, 20_000, None));
    scheduler.reset();
    assert_eq!(
        scheduler.commit(
            expired,
            AudioPlayoutCommit {
                start_at_us: play(expired).start_at_us,
                played_us: 20_000,
            },
        ),
        Err(AudioPlayoutCommitError::NoPendingPlan),
        "失効した計画では確定できないこと"
    );
    let plan = scheduler.schedule(input(0, 0, 20_000, None));
    let start_at_us = play(plan).start_at_us;
    assert_eq!(
        scheduler.commit(
            plan,
            AudioPlayoutCommit {
                start_at_us: start_at_us - 1,
                played_us: 20_000,
            },
        ),
        Err(AudioPlayoutCommitError::StartBeforePlan),
        "計画より前の開始では確定できないこと"
    );
    assert_eq!(
        scheduler.commit(
            plan,
            AudioPlayoutCommit {
                start_at_us,
                played_us: 0,
            },
        ),
        Err(AudioPlayoutCommitError::EmptyPlayed),
        "長さ 0 では確定できないこと"
    );
    commit_as_planned(&mut scheduler, plan, 20_000);
    assert_eq!(
        scheduler.commit(
            plan,
            AudioPlayoutCommit {
                start_at_us,
                played_us: 20_000,
            },
        ),
        Err(AudioPlayoutCommitError::NoPendingPlan),
        "二重に確定できないこと"
    );
}

#[test]
fn reset_expires_the_plan_but_keeps_the_cumulative_stats() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let old = scheduler.schedule(input(0, 0, 20_000, None));
    scheduler.record_rendered_concealment(10_000, 1);
    scheduler.reset();
    assert_eq!(
        scheduler.commit(
            old,
            AudioPlayoutCommit {
                start_at_us: play(old).start_at_us,
                played_us: 20_000,
            },
        ),
        Err(AudioPlayoutCommitError::NoPendingPlan),
        "やり直すと確定待ちの計画を失効させること"
    );
    assert_eq!(scheduler.concealed_us(), 10_000);
    assert_eq!(scheduler.concealments(), 1);
    assert_eq!(
        play(scheduler.schedule(input(1_000_000, 1_000_000, 20_000, None))).start_at_us,
        1_050_000,
        "やり直した後は到着 + 下限から並べ直すこと"
    );
}

#[test]
fn reset_stats_clears_accumulated_values() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 20_000, Some(100_000)));
    commit_as_planned(&mut scheduler, first, 20_000);
    scheduler.record_rendered_concealment(5_000, 1);
    scheduler.reset_stats();
    assert_eq!(scheduler.drops(), 0);
    assert_eq!(scheduler.rebases(), 0);
    assert_eq!(scheduler.compressed_us(), 0);
    assert_eq!(scheduler.expanded_us(), 0);
    assert_eq!(scheduler.concealed_us(), 0);
    assert_eq!(scheduler.concealments(), 0);
}

#[test]
fn a_batch_of_old_sounds_keeps_the_unprocessed_end_within_the_limit() {
    // 到着基準と時刻基準の両方で、過去 10 秒分を 1 音ずつ流し込む。圧縮の成否を仮定せず、
    // 未加工の終端が上限 (300 ms) を超えないこと
    for timestamp_basis in [false, true] {
        let mut scheduler = AudioPlayoutScheduler::new();
        let now_us = 20_000_000;
        let mut end_us = now_us;
        for index in 0..1_000 {
            let target_start_us = if timestamp_basis {
                Some(10_000_000 + index * 10_000)
            } else {
                None
            };
            let decision =
                scheduler.schedule(input(now_us, index * 10_000, 10_000, target_start_us));
            if let AudioPlayoutDecision::Drop { reason } = decision.decision() {
                assert_eq!(reason, AudioPlayoutDropReason::Backlog);
                continue;
            }
            let planned = play(decision);
            end_us = planned.start_at_us + 10_000;
            assert!(
                end_us <= now_us + AUDIO_PLAYOUT_MAX_DELAY_US,
                "未加工の終端が上限を超えないこと: end_us={end_us}"
            );
            commit_as_planned(&mut scheduler, decision, 10_000);
        }
        assert!(scheduler.drops() > 0, "過去の音は途中で捨てること");
        assert!(
            matches!(
                scheduler
                    .schedule(input(end_us, 10_000_000, 10_000, None))
                    .decision(),
                AudioPlayoutDecision::Play { .. }
            ),
            "捨てた後も次の音は並べられること"
        );
    }
}

#[test]
fn a_dropped_sound_does_not_move_the_arrival_anchor() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 20_000, None));
    commit_as_planned(&mut scheduler, first, 20_000);
    // 到着は 240 ms、TIMESTAMP は 10 秒。到着基準の目標は 290 ms になり、長さ 20 ms を
    // 足すと上限 (300 ms) を超えるため捨てる
    let mut request = input(0, 10_000_000, 20_000, None);
    request.arrival_us = 240_000;
    assert_eq!(
        scheduler.schedule(request).decision(),
        AudioPlayoutDecision::Drop {
            reason: AudioPlayoutDropReason::Backlog,
        },
        "上限を超える音は捨てること"
    );
    assert_eq!(scheduler.rebases(), 0, "捨てた音は基準を動かさないこと");
    assert_eq!(
        play(scheduler.schedule(input(0, 20_000, 20_000, None))).start_at_us,
        70_000,
        "基準は捨てる前のままであること"
    );
}

#[test]
fn applied_compression_is_carried_into_the_next_position_in_both_bases() {
    for timestamp_basis in [false, true] {
        let mut scheduler = AudioPlayoutScheduler::new();
        // 1 つ目はどちらの経路でも時刻基準で 1.2 秒に置く
        let first = scheduler.schedule(input(1_000_000, 0, 20_000, Some(1_200_000)));
        commit_as_planned(&mut scheduler, first, 20_000);
        let second = scheduler.schedule(input(
            1_020_000,
            20_000,
            20_000,
            if timestamp_basis {
                Some(1_070_000)
            } else {
                None
            },
        ));
        assert!(
            play(second).compress_us > 0,
            "目標に遅れた音は詰めること: {second:?}"
        );
        commit_as_planned(&mut scheduler, second, 15_000);
        assert_eq!(
            scheduler.compressed_us(),
            5_000,
            "実際に詰めた長さだけを数えること"
        );
        let third = scheduler.schedule(input(
            1_040_000,
            40_000,
            20_000,
            if timestamp_basis {
                Some(1_090_000)
            } else {
                None
            },
        ));
        assert_eq!(
            play(third).start_at_us,
            1_235_000,
            "確定した長さで次の位置が決まること"
        );
    }
}

#[test]
fn an_arrival_gap_recovery_expands_and_counts_one_rebase() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 20_000, None));
    commit_as_planned(&mut scheduler, first, 20_000);
    // 200 ms の停止の後。到着基準の目標 (210 ms) より前になるため、伸長して深さを戻す
    let recovery = scheduler.schedule(input(200_000, 20_000, 20_000, None));
    let planned = play(recovery);
    assert_eq!(planned.start_at_us, 210_000);
    assert!(
        planned.expand_us > 0,
        "浅くなった到着基準の深さを戻すこと: {planned:?}"
    );
    commit_as_planned(&mut scheduler, recovery, 25_000);
    assert_eq!(scheduler.expanded_us(), 5_000);
    assert_eq!(scheduler.rebases(), 1, "遅れて届いた音として数えること");
    assert_eq!(
        play(scheduler.schedule(input(220_000, 40_000, 20_000, None))).start_at_us,
        235_000,
        "伸ばした分だけ次の位置が後ろになること"
    );
}

#[test]
fn arrival_based_places_the_first_sound_from_the_arrival_position() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let decision = scheduler.schedule(input(0, 0, 20_000, None));
    assert_eq!(
        play(decision),
        Play {
            start_at_us: 50_000,
            basis: AudioPlayoutBasis::Arrival,
            compress_us: 0,
            expand_us: 0,
            total_gap_us: 0,
            conceal_us: 0,
            unrecoverable_gap_us: 0,
        },
        "最初の音は到着 + 再生の遅れから並べること"
    );
}

#[test]
fn arrival_based_orders_sounds_by_timestamp() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 20_000, None));
    commit_as_planned(&mut scheduler, first, 20_000);
    let second = scheduler.schedule(input(20_000, 20_000, 20_000, None));
    assert_eq!(
        play(second).start_at_us,
        70_000,
        "TIMESTAMP の差どおりに並べること"
    );
    assert_eq!(play(second).total_gap_us, 0);
}

#[test]
fn arrival_based_uses_the_configured_arrival_delay() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let mut request = input(0, 0, 20_000, None);
    request.arrival_delay_us = 80_000;
    let decision = scheduler.schedule(request);
    assert_eq!(
        play(decision).start_at_us,
        80_000,
        "到着基準の遅れをそのまま使うこと"
    );
}

#[test]
fn arrival_based_rebases_after_a_late_sound() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 20_000, None));
    commit_as_planned(&mut scheduler, first, 20_000);
    // 予定 (70 ms) より 130 ms 遅れて、205 ms に届いた
    let mut request = input(200_000, 20_000, 20_000, None);
    request.arrival_us = 200_000;
    let decision = scheduler.schedule(request);
    assert_eq!(
        play(decision).start_at_us,
        210_000,
        "今から鳴らせる最も早い時刻へずらすこと"
    );
    commit_as_planned(&mut scheduler, decision, 20_000);
    assert_eq!(
        scheduler.rebases(),
        1,
        "遅れを基準の取り直しとして数えること"
    );
}

#[test]
fn a_small_resync_jump_is_not_counted_as_a_rebase() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 20_000, None));
    commit_as_planned(&mut scheduler, first, 20_000);
    // 予定は 70 ms、今 + 最小の余裕は 65 ms。ずらす幅が 10 ms 未満なので数えない
    let mut request = input(55_000, 20_000, 20_000, None);
    request.arrival_us = 55_000;
    let decision = scheduler.schedule(request);
    assert_eq!(play(decision).start_at_us, 70_000);
    commit_as_planned(&mut scheduler, decision, 20_000);
    assert_eq!(scheduler.rebases(), 0);
}

#[test]
fn arrival_based_rebases_on_a_big_timestamp_jump() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 20_000, None));
    commit_as_planned(&mut scheduler, first, 20_000);
    // TIMESTAMP が 1 秒飛んだ。到着 + 再生の遅れ (70 ms) から並べ直す
    let decision = scheduler.schedule(input(20_000, 1_000_000, 20_000, None));
    assert_eq!(
        play(decision),
        Play {
            start_at_us: 70_000,
            basis: AudioPlayoutBasis::Arrival,
            compress_us: 0,
            expand_us: 0,
            total_gap_us: 0,
            conceal_us: 0,
            unrecoverable_gap_us: 0,
        },
        "到着 + 再生の遅れから並べ直すこと"
    );
    commit_as_planned(&mut scheduler, decision, 20_000);
    assert_eq!(scheduler.rebases(), 1);
}

#[test]
fn arrival_based_drops_when_the_rebase_cannot_fit() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 20_000, None));
    commit_as_planned(&mut scheduler, first, 20_000);
    // 到着が 280 ms 遅れ、到着 + 再生の遅れ (330 ms) が上限 (300 ms) を超える
    let mut request = input(0, 1_000_000, 20_000, None);
    request.arrival_us = 280_000;
    assert_eq!(
        scheduler.schedule(request).decision(),
        AudioPlayoutDecision::Drop {
            reason: AudioPlayoutDropReason::Backlog,
        },
        "並べ直しても上限を超える音は捨てること"
    );
}

#[test]
fn a_late_sound_keeps_playing_while_the_previous_one_is_sounding() {
    let mut scheduler = AudioPlayoutScheduler::new();
    schedule_queued_sound(&mut scheduler);
    let mut request = input(0, 20_000, 20_000, Some(0));
    request.arrival_us = 0;
    let decision = scheduler.schedule(request);
    assert_eq!(
        play(decision).basis,
        AudioPlayoutBasis::Timestamp,
        "前の音がまだ鳴っている間は到着基準へ並べ直さないこと"
    );
    assert_eq!(play(decision).start_at_us, 120_000);
    commit_as_planned(&mut scheduler, decision, 20_000);
    assert_eq!(scheduler.rebases(), 0);
}

#[test]
fn a_late_sound_is_rebased_by_arrival_when_the_sound_stopped() {
    let mut scheduler = AudioPlayoutScheduler::new();
    schedule_queued_sound(&mut scheduler);
    // 前の音は 120 ms に鳴り終わっている。目標 (100 ms) を 600 ms 過ぎて届いた
    let mut request = input(700_000, 20_000, 20_000, Some(100_000));
    request.arrival_us = 700_000;
    let decision = scheduler.schedule(request);
    assert_eq!(
        play(decision).basis,
        AudioPlayoutBasis::Arrival,
        "音が途切れたら到着基準へ並べ直すこと"
    );
    assert_eq!(
        play(decision).start_at_us,
        750_000,
        "到着 + 再生の遅れから鳴らすこと"
    );
    commit_as_planned(&mut scheduler, decision, 20_000);
    assert_eq!(scheduler.rebases(), 1);
}

#[test]
fn the_limit_boundary_is_played_and_one_microsecond_over_is_dropped() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 20_000, None));
    commit_as_planned(&mut scheduler, first, 20_000);
    let limit_us = AUDIO_PLAYOUT_DELAY_US + AUDIO_PLAYOUT_BACKLOG_US;
    // 今 + 上限ちょうどに終わる音は鳴らす
    // 前の音の終わり (70 ms) から、今 + 上限ちょうどまで鳴る長さ
    let mut request = input(0, 20_000, limit_us - 70_000, None);
    request.arrival_us = 0;
    assert!(
        matches!(
            scheduler.schedule(request).decision(),
            AudioPlayoutDecision::Play { .. }
        ),
        "終端が上限ちょうどの音は鳴らすこと"
    );
    // 1 マイクロ秒でも超えたら捨てる
    let mut request = input(0, 40_000, limit_us - 70_000 + 1, None);
    request.arrival_us = 0;
    assert_eq!(
        scheduler.schedule(request).decision(),
        AudioPlayoutDecision::Drop {
            reason: AudioPlayoutDropReason::Backlog,
        },
        "終端が上限を超える音は捨てること"
    );
}

#[test]
fn presentation_delay_raises_the_backlog_limit() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 20_000, Some(100_000)));
    commit_as_planned(&mut scheduler, first, 20_000);
    // 表示の遅れを 400 ms にすると、時刻基準の上限は 400 + 250 ms になる
    let mut request = input(0, 20_000, 20_000, Some(600_000));
    request.presentation_delay_us = 400_000;
    let decision = scheduler.schedule(request);
    assert!(
        matches!(decision.decision(), AudioPlayoutDecision::Play { .. }),
        "表示の遅れが大きいときはその分だけ待てること"
    );
}

#[test]
fn gap_is_reported_in_full_and_only_the_concealment_is_capped() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 20_000, Some(100_000)));
    commit_as_planned(&mut scheduler, first, 20_000);
    // 前の音の終わり (120 ms) から 150 ms 空けて、270 ms を目標にする
    let decision = scheduler.schedule(input(20_000, 20_000, 20_000, Some(270_000)));
    let planned = play(decision);
    assert_eq!(
        planned.total_gap_us, 150_000,
        "隙間の全長は切り捨てないこと"
    );
    assert_eq!(
        planned.conceal_us, AUDIO_PLAYOUT_MAX_CONCEAL_US,
        "補間する長さだけを上限で切ること"
    );
    assert_eq!(planned.unrecoverable_gap_us, 0);
}

#[test]
fn a_gap_at_the_minimum_is_not_concealed() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 20_000, Some(100_000)));
    commit_as_planned(&mut scheduler, first, 20_000);
    // 前の音の終わり (120 ms) のちょうど 0.5 ms 後は、前の音の終わりへ揃える
    let decision = scheduler.schedule(input(20_000, 20_000, 20_000, Some(120_500)));
    let planned = play(decision);
    assert_eq!(planned.start_at_us, 120_000);
    assert_eq!(planned.total_gap_us, 0, "1〜2 サンプルの無音を作らないこと");
    assert_eq!(planned.conceal_us, 0);
}

#[test]
fn a_gap_just_above_the_minimum_is_concealed() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 20_000, Some(100_000)));
    commit_as_planned(&mut scheduler, first, 20_000);
    let decision = scheduler.schedule(input(20_000, 20_000, 20_000, Some(121_000)));
    let planned = play(decision);
    assert_eq!(planned.total_gap_us, 1_000);
    assert_eq!(
        planned.conceal_us, 1_000,
        "下限より長い隙間は補間してよいこと"
    );
}

#[test]
fn a_gap_before_the_lead_is_counted_as_unrecoverable() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 20_000, Some(100_000)));
    commit_as_planned(&mut scheduler, first, 20_000);
    // 今 (180 ms) + 最小の余裕 (10 ms) まで予約できない。前の音の終わりは 120 ms
    let decision = scheduler.schedule(input(180_000, 20_000, 20_000, Some(300_000)));
    let planned = play(decision);
    assert_eq!(planned.start_at_us, 300_000);
    assert_eq!(planned.total_gap_us, 180_000);
    assert_eq!(
        planned.unrecoverable_gap_us, 70_000,
        "予約できる最も早い時刻より前を数えること"
    );
}

#[test]
fn an_underrun_advances_the_last_end_and_does_not_double_count_the_gap() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 20_000, Some(100_000)));
    commit_as_planned(&mut scheduler, first, 20_000);
    // 出力が 200 ms まで枯れた
    scheduler.note_underrun(200_000);
    assert_eq!(scheduler.last_end_us(), Some(200_000));
    // 次の音は今 + 最小の余裕から鳴り、枯れた時間を隙間として材料化しない
    let decision = scheduler.schedule(input(200_000, 20_000, 20_000, None));
    let planned = play(decision);
    assert_eq!(planned.start_at_us, 210_000);
    assert_eq!(planned.total_gap_us, 10_000, "今から並べた分だけにすること");
    assert_eq!(
        planned.conceal_us, 0,
        "今 + 最小の余裕より前は補間できないこと"
    );
    assert_eq!(planned.unrecoverable_gap_us, 10_000);
}

#[test]
fn underrun_while_still_sounding_does_nothing() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 20_000, Some(100_000)));
    commit_as_planned(&mut scheduler, first, 20_000);
    scheduler.note_underrun(110_000);
    assert_eq!(
        scheduler.last_end_us(),
        Some(120_000),
        "鳴っている間は終わりを動かさないこと"
    );
}

#[test]
fn negative_duration_does_not_panic() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let decision = scheduler.schedule(input(0, 0, -1, None));
    // 長さは 0 として扱い、到着 + 下限に置く
    assert_eq!(play(decision).start_at_us, 50_000);
    assert_eq!(
        scheduler.commit(
            decision,
            AudioPlayoutCommit {
                start_at_us: 50_000,
                played_us: 0,
            },
        ),
        Err(AudioPlayoutCommitError::EmptyPlayed),
        "長さ 0 の音は確定できないこと"
    );
}

#[test]
fn custom_config_clamps_negative_values() {
    let scheduler = AudioPlayoutScheduler::with_config(AudioPlayoutConfig {
        backlog_us: -1,
        min_lead_us: -1,
        stretch_min_us: -1,
    });
    assert_eq!(scheduler.config().backlog_us, 0);
    assert_eq!(scheduler.config().min_lead_us, 0);
    assert_eq!(scheduler.config().stretch_min_us, 0);
}

#[test]
fn arrival_delay_helper_clamps_to_the_configured_floor_and_the_upper_limit() {
    assert_eq!(
        audio_arrival_delay_us_with_min(10_000, 30_000),
        30_000,
        "学習した値が下限より小さいときは下限を使うこと"
    );
    assert_eq!(
        audio_arrival_delay_us_with_min(80_000, 30_000),
        80_000,
        "学習した値が下限以上ならそのまま使うこと"
    );
    assert_eq!(
        audio_arrival_delay_us_with_min(500_000, 30_000),
        TIMELINE_ARRIVAL_DELAY_US,
        "上限を超えないこと"
    );
    assert_eq!(
        audio_arrival_delay_us_with_min(10_000, 500_000),
        TIMELINE_ARRIVAL_DELAY_US,
        "下限が上限より大きくても上限で切ること"
    );
}

#[test]
fn the_stretch_deadband_follows_the_input() {
    let mut scheduler = AudioPlayoutScheduler::new();
    schedule_queued_sound(&mut scheduler);
    // 前の音の終わり (120 ms) に置くが、目標は 100 ms なので遅れは 20 ms。不感帯 10 ms なら
    // 詰め始め、音の長さの半分 (10 ms) で切る
    let mut request = input(20_000, 20_000, 20_000, Some(100_000));
    request.stretch_min_us = Some(10_000);
    let decision = scheduler.schedule(request);
    assert_eq!(play(decision).compress_us, 10_000);
}

#[test]
fn a_lateness_beyond_the_maximum_is_still_played_when_the_sound_is_continuous() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 250_000, None));
    commit_as_planned(&mut scheduler, first, 250_000);
    // 前の音は 50 ms から 300 ms まで鳴る。今は 200 ms で、目標は -400 ms なので
    // 500 ms 以上遅れているが、音が続いている間は到着基準へ並べ直さない
    let mut request = input(200_000, 250_000, 20_000, Some(-400_000));
    request.arrival_us = 200_000;
    let decision = scheduler.schedule(request);
    assert_eq!(play(decision).basis, AudioPlayoutBasis::Timestamp);
    assert!(matches!(
        decision.decision(),
        AudioPlayoutDecision::Play { .. }
    ));
}
