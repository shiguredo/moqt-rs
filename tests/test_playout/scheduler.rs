//! 鳴らす時刻を決めるスケジューラのテスト
//!
//! 公開 API (`AudioPlayoutScheduler`) の契約を確認する。

use shiguredo_moqt::playout::scheduler::{
    AUDIO_PLAYOUT_BACKLOG_US, AUDIO_PLAYOUT_DELAY_US, AUDIO_PLAYOUT_MAX_CONCEAL_US,
    AUDIO_PLAYOUT_MAX_LATENESS_US, AUDIO_PLAYOUT_MIN_CONCEAL_US, AudioPlayoutDecision,
    AudioPlayoutInput, AudioPlayoutScheduler,
};

/// 目標あり (守る) の入力を作る
fn input(
    now_us: i64,
    timestamp_us: i64,
    duration_us: i64,
    target_start_us: Option<i64>,
) -> AudioPlayoutInput {
    AudioPlayoutInput {
        now_us,
        timestamp_us,
        duration_us,
        target_start_us,
        enforce_target: target_start_us.is_some(),
        delay_us: AUDIO_PLAYOUT_DELAY_US,
        presentation_delay_us: AUDIO_PLAYOUT_DELAY_US,
    }
}

#[test]
fn sound_in_time_plays_at_the_target() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let decision = scheduler.schedule(input(0, 5_000, 20_000, Some(100_000)));
    assert_eq!(
        decision,
        AudioPlayoutDecision::Play {
            start_at_us: 100_000,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
    assert_eq!(scheduler.lateness_us(), 0);
}

#[test]
fn late_sound_is_shifted_and_compressed() {
    let mut scheduler = AudioPlayoutScheduler::new();
    // 目標 100 ms を 60 ms 過ぎて届いた。今 + 10 ms へずらし、10 ms (音の半分) 詰める
    let decision = scheduler.schedule(input(150_000, 5_000, 20_000, Some(100_000)));
    assert_eq!(
        decision,
        AudioPlayoutDecision::Play {
            start_at_us: 160_000,
            compress_us: 10_000,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
    assert_eq!(scheduler.lateness_us(), 60_000);
}

#[test]
fn overlapping_sound_is_appended_to_the_previous_one() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 20_000, Some(100_000)));
    assert_eq!(
        first,
        AudioPlayoutDecision::Play {
            start_at_us: 100_000,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
    // 前の音の終わり (120 ms) が目標 (110 ms) より後ろにあるため、そこへ繋げる
    let second = scheduler.schedule(input(0, 20_000, 20_000, Some(110_000)));
    assert_eq!(
        second,
        AudioPlayoutDecision::Play {
            start_at_us: 120_000,
            compress_us: 10_000,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
}

#[test]
fn compression_returns_to_the_target() {
    let mut scheduler = AudioPlayoutScheduler::new();
    // 目標 0 に間に合わず 10 ms ずらして鳴らし、10 ms 詰める
    let first = scheduler.schedule(input(0, 0, 20_000, Some(0)));
    assert_eq!(
        first,
        AudioPlayoutDecision::Play {
            start_at_us: 10_000,
            compress_us: 10_000,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
    scheduler.confirm_stretch(10_000);
    // 詰めた分だけ前の音の終わりが早くなり、次の音は目標どおりに鳴る
    let second = scheduler.schedule(input(0, 20_000, 20_000, Some(20_000)));
    assert_eq!(
        second,
        AudioPlayoutDecision::Play {
            start_at_us: 20_000,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
}

#[test]
fn too_many_queued_sounds_are_dropped() {
    let mut scheduler = AudioPlayoutScheduler::new();
    // 上限 (80 ms + 220 ms = 300 ms) を 1 µs 超えた目標は捨てる
    let decision = scheduler.schedule(input(
        0,
        0,
        20_000,
        Some(AUDIO_PLAYOUT_DELAY_US + AUDIO_PLAYOUT_BACKLOG_US + 1),
    ));
    assert_eq!(decision, AudioPlayoutDecision::Drop);
    assert_eq!(scheduler.drops(), 1);
}

#[test]
fn sound_too_far_from_the_target_is_dropped() {
    let mut scheduler = AudioPlayoutScheduler::new();
    // 目標 (0) から 500 ms を超えて離れた音は捨てて目標へ戻す
    let decision = scheduler.schedule(input(
        AUDIO_PLAYOUT_MAX_LATENESS_US + 20_000,
        0,
        20_000,
        Some(0),
    ));
    assert_eq!(decision, AudioPlayoutDecision::Drop);
    assert_eq!(scheduler.drops(), 1);
    assert_eq!(scheduler.lateness_us(), 0);
}

#[test]
fn unapplied_compression_remains_as_lateness() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 20_000, Some(0)));
    assert_eq!(
        first,
        AudioPlayoutDecision::Play {
            start_at_us: 10_000,
            compress_us: 10_000,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
    // 10 ms の要求に対して 6 ms しか詰められなかった
    scheduler.confirm_stretch(6_000);
    assert_eq!(scheduler.compressed_us(), 6_000);
    // 詰められなかった 4 ms は音が後ろへ伸び、次の音は 24 ms から鳴り、その分を詰める
    let second = scheduler.schedule(input(0, 20_000, 20_000, Some(20_000)));
    assert_eq!(
        second,
        AudioPlayoutDecision::Play {
            start_at_us: 24_000,
            compress_us: 4_000,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
}

#[test]
fn arrival_based_places_sounds_by_timestamp() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 20_000, None));
    assert_eq!(
        first,
        AudioPlayoutDecision::Play {
            start_at_us: AUDIO_PLAYOUT_DELAY_US,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
    // 続く音は TIMESTAMP の間隔どおりに並ぶ (前の音の終わりにちょうど繋がる)
    let second = scheduler.schedule(input(20_000, 20_000, 20_000, None));
    assert_eq!(
        second,
        AudioPlayoutDecision::Play {
            start_at_us: AUDIO_PLAYOUT_DELAY_US + 20_000,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
}

#[test]
fn arrival_based_rebases_after_a_late_sound() {
    let mut scheduler = AudioPlayoutScheduler::new();
    scheduler.schedule(input(0, 0, 20_000, None));
    // 過ぎてから届いた音で基準を取り直す
    let decision = scheduler.schedule(input(200_000, 40_000, 20_000, None));
    assert_eq!(
        decision,
        AudioPlayoutDecision::Play {
            start_at_us: 280_000,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
    assert_eq!(scheduler.rebases(), 1);
}

#[test]
fn arrival_based_rebases_on_a_big_timestamp_jump() {
    let mut scheduler = AudioPlayoutScheduler::new();
    scheduler.schedule(input(0, 0, 20_000, None));
    // TIMESTAMP が大きく飛んだ。前の音のすぐ後ろ (今 + 遅れ) から並べ直す
    let decision = scheduler.schedule(input(100_000, 1_000_000, 20_000, None));
    assert_eq!(
        decision,
        AudioPlayoutDecision::Play {
            start_at_us: 180_000,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
    assert_eq!(scheduler.rebases(), 1);
}

#[test]
fn arrival_based_drops_when_the_rebase_cannot_fit() {
    let mut scheduler = AudioPlayoutScheduler::new();
    // 長い音の後ろが上限を超えて埋まっている
    scheduler.schedule(input(0, 0, 1_000_000, None));
    let decision = scheduler.schedule(input(100_000, 2_000_000, 20_000, None));
    assert_eq!(decision, AudioPlayoutDecision::Drop);
    assert_eq!(scheduler.drops(), 1);
}

#[test]
fn limit_boundary_is_played() {
    let mut scheduler = AudioPlayoutScheduler::new();
    // 上限ちょうど (80 ms + 220 ms) は捨てない
    let decision = scheduler.schedule(input(
        0,
        0,
        20_000,
        Some(AUDIO_PLAYOUT_DELAY_US + AUDIO_PLAYOUT_BACKLOG_US),
    ));
    assert!(matches!(decision, AudioPlayoutDecision::Play { .. }));
    assert_eq!(scheduler.drops(), 0);
}

#[test]
fn lateness_boundary_is_played() {
    let mut scheduler = AudioPlayoutScheduler::new();
    // 遅れが 500 ms ちょうどになる音は捨てない
    let now_us = AUDIO_PLAYOUT_MAX_LATENESS_US - 10_000;
    let decision = scheduler.schedule(input(now_us, 0, 20_000, Some(0)));
    assert_eq!(
        decision,
        AudioPlayoutDecision::Play {
            start_at_us: AUDIO_PLAYOUT_MAX_LATENESS_US,
            compress_us: 10_000,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
    assert_eq!(scheduler.lateness_us(), AUDIO_PLAYOUT_MAX_LATENESS_US);
}

#[test]
fn unconfirmed_stretch_is_treated_as_unapplied() {
    let mut scheduler = AudioPlayoutScheduler::new();
    scheduler.schedule(input(0, 0, 20_000, Some(0)));
    // confirm せずに次の音を予約すると、要求は適用されなかったものとして扱われる
    let second = scheduler.schedule(input(0, 20_000, 20_000, Some(20_000)));
    assert_eq!(
        second,
        AudioPlayoutDecision::Play {
            start_at_us: 30_000,
            compress_us: 10_000,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
}

#[test]
fn mode_switch_flushes_the_pending_stretch() {
    let mut scheduler = AudioPlayoutScheduler::new();
    // 到着基準で基準を作る
    scheduler.schedule(input(0, 0, 20_000, None));
    // 目標ありで 10 ms の詰めを要求する (confirm しない)
    scheduler.schedule(input(20_000, 20_000, 20_000, Some(30_000)));
    // 到着基準へ戻るときも、未確認の要求を適用されなかったものとして扱う
    let arrival = scheduler.schedule(input(20_000, 25_000, 20_000, None));
    assert_eq!(
        arrival,
        AudioPlayoutDecision::Play {
            start_at_us: 120_000,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
}

#[test]
fn negative_duration_does_not_panic() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let decision = scheduler.schedule(input(0, 0, -2, Some(0)));
    assert_eq!(
        decision,
        AudioPlayoutDecision::Play {
            start_at_us: 10_000,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
    // 次の予約でも panic しない
    scheduler.schedule(input(0, 20_000, 20_000, Some(20_000)));
}

#[test]
fn presentation_delay_raises_the_backlog_limit() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let mut request = input(0, 0, 20_000, Some(300_001));
    request.presentation_delay_us = 500_000;
    // 上限は max(80 ms, 500 ms) + 220 ms = 720 ms なので、300.001 ms 先の目標は捨てない
    assert_eq!(
        scheduler.schedule(request),
        AudioPlayoutDecision::Play {
            start_at_us: 300_001,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
}

#[test]
fn reset_keeps_the_counters_and_reset_stats_clears_them() {
    let mut scheduler = AudioPlayoutScheduler::new();
    scheduler.schedule(input(0, 0, 20_000, None));
    scheduler.schedule(input(200_000, 40_000, 20_000, None));
    assert_eq!(scheduler.rebases(), 1);

    scheduler.reset();
    // 基準の取り直しでは累積統計を消さない
    assert_eq!(scheduler.rebases(), 1);

    scheduler.reset_stats();
    assert_eq!(scheduler.rebases(), 0);
    assert_eq!(scheduler.drops(), 0);
    assert_eq!(scheduler.compressed_us(), 0);
    assert_eq!(scheduler.lateness_us(), 0);
}

#[test]
fn gap_between_sounds_is_reported() {
    let mut scheduler = AudioPlayoutScheduler::new();
    // 最初の音は目標 100 ms に間に合い、120 ms まで鳴る
    let first = scheduler.schedule(input(0, 0, 20_000, Some(100_000)));
    assert_eq!(
        first,
        AudioPlayoutDecision::Play {
            start_at_us: 100_000,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
    // 次の音の目標は 160 ms。前の音の終わりから 40 ms 空く
    let second = scheduler.schedule(input(0, 20_000, 20_000, Some(160_000)));
    assert_eq!(
        second,
        AudioPlayoutDecision::Play {
            start_at_us: 160_000,
            compress_us: 0,
            gap_start_us: 120_000,
            gap_us: 40_000,
        }
    );
    assert_eq!(scheduler.concealments(), 0);
}

#[test]
fn gap_is_capped_at_the_max_concealment() {
    let mut scheduler = AudioPlayoutScheduler::new();
    scheduler.schedule(input(0, 0, 20_000, Some(100_000)));
    // 前の音の終わり (120 ms) から 200 ms 空く。上限 (100 ms) で切り、残りは無音のまま残す
    let second = scheduler.schedule(input(50_000, 20_000, 20_000, Some(320_000)));
    assert_eq!(
        second,
        AudioPlayoutDecision::Play {
            start_at_us: 320_000,
            compress_us: 0,
            gap_start_us: 120_000,
            gap_us: AUDIO_PLAYOUT_MAX_CONCEAL_US,
        }
    );
}

#[test]
fn gap_at_the_minimum_is_not_concealed() {
    let mut scheduler = AudioPlayoutScheduler::new();
    scheduler.schedule(input(0, 0, 20_000, Some(100_000)));
    // 隙間がちょうど下限 (5 ms) なら補間しない (継ぎ目が耳につくため)
    let second = scheduler.schedule(input(
        0,
        20_000,
        20_000,
        Some(120_000 + AUDIO_PLAYOUT_MIN_CONCEAL_US),
    ));
    assert_eq!(
        second,
        AudioPlayoutDecision::Play {
            start_at_us: 120_000 + AUDIO_PLAYOUT_MIN_CONCEAL_US,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
    assert_eq!(scheduler.concealments(), 0);
}

#[test]
fn gap_just_above_the_minimum_is_concealed() {
    let mut scheduler = AudioPlayoutScheduler::new();
    scheduler.schedule(input(0, 0, 20_000, Some(100_000)));
    // 下限のすぐ上 (6 ms) の隙間は補間する
    let second = scheduler.schedule(input(0, 20_000, 20_000, Some(126_000)));
    assert_eq!(
        second,
        AudioPlayoutDecision::Play {
            start_at_us: 126_000,
            compress_us: 0,
            gap_start_us: 120_000,
            gap_us: 6_000,
        }
    );
}

#[test]
fn gap_before_the_lead_is_not_concealed() {
    let mut scheduler = AudioPlayoutScheduler::new();
    // 最初の音は今 (0) に間に合わず 10 ms へずらして鳴り、30 ms まで鳴る
    scheduler.schedule(input(0, 0, 20_000, Some(0)));
    // 前の音の終わり (30 ms) が今 + 余裕 (25 + 10 = 35 ms) より前の隙間は予約できない
    let second = scheduler.schedule(input(25_000, 20_000, 20_000, Some(40_000)));
    assert_eq!(
        second,
        AudioPlayoutDecision::Play {
            start_at_us: 40_000,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
}

#[test]
fn arrival_based_reports_the_gap() {
    let mut scheduler = AudioPlayoutScheduler::new();
    // 到着基準: 最初の音は 80 ms から 100 ms まで鳴る
    scheduler.schedule(input(0, 0, 20_000, None));
    // 1 つ抜けた音 (TIMESTAMP が 40 ms) が届く。前の音の終わりから 20 ms 空く
    let second = scheduler.schedule(input(20_000, 40_000, 20_000, None));
    assert_eq!(
        second,
        AudioPlayoutDecision::Play {
            start_at_us: 120_000,
            compress_us: 0,
            gap_start_us: 100_000,
            gap_us: 20_000,
        }
    );
}

#[test]
fn confirm_concealment_counts_only_the_applied_length() {
    let mut scheduler = AudioPlayoutScheduler::new();
    scheduler.schedule(input(0, 0, 20_000, Some(100_000)));
    // 40 ms の隙間を要求し、その半分だけ埋められた
    scheduler.schedule(input(0, 20_000, 20_000, Some(160_000)));
    scheduler.confirm_concealment(20_000);
    assert_eq!(scheduler.concealments(), 1);
    assert_eq!(scheduler.concealed_us(), 20_000);

    // 要求 (20 ms) より多く返しても要求までに切る
    let third = scheduler.schedule(input(0, 40_000, 20_000, Some(200_000)));
    assert_eq!(
        third,
        AudioPlayoutDecision::Play {
            start_at_us: 200_000,
            compress_us: 0,
            gap_start_us: 180_000,
            gap_us: 20_000,
        }
    );
    scheduler.confirm_concealment(40_000);
    assert_eq!(scheduler.concealments(), 2);
    assert_eq!(scheduler.concealed_us(), 40_000);
}

#[test]
fn unconfirmed_concealment_is_treated_as_unapplied() {
    let mut scheduler = AudioPlayoutScheduler::new();
    scheduler.schedule(input(0, 0, 20_000, Some(100_000)));
    // 40 ms の隙間を要求したが confirm せずに、隙間の無い音を予約する
    scheduler.schedule(input(0, 20_000, 20_000, Some(160_000)));
    let third = scheduler.schedule(input(0, 40_000, 20_000, Some(180_000)));
    assert_eq!(
        third,
        AudioPlayoutDecision::Play {
            start_at_us: 180_000,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
    // 前の要求は適用されなかったものとして扱う
    scheduler.confirm_concealment(40_000);
    assert_eq!(scheduler.concealments(), 0);
    assert_eq!(scheduler.concealed_us(), 0);
}

#[test]
fn confirm_concealment_without_a_request_is_ignored() {
    let mut scheduler = AudioPlayoutScheduler::new();
    scheduler.confirm_concealment(50_000);
    assert_eq!(scheduler.concealments(), 0);
    assert_eq!(scheduler.concealed_us(), 0);

    // 隙間の無い音のあとに確認しても数えない
    scheduler.schedule(input(0, 0, 20_000, Some(100_000)));
    scheduler.confirm_concealment(50_000);
    assert_eq!(scheduler.concealments(), 0);
    assert_eq!(scheduler.concealed_us(), 0);
}

#[test]
fn reset_keeps_the_concealment_stats() {
    let mut scheduler = AudioPlayoutScheduler::new();
    scheduler.schedule(input(0, 0, 20_000, Some(100_000)));
    // 40 ms の隙間の半分だけ埋められた
    scheduler.schedule(input(0, 20_000, 20_000, Some(160_000)));
    scheduler.confirm_concealment(20_000);
    assert_eq!(scheduler.concealments(), 1);
    assert_eq!(scheduler.concealed_us(), 20_000);

    scheduler.reset();
    // 基準の取り直しでは累積統計を消さない
    assert_eq!(scheduler.concealments(), 1);
    assert_eq!(scheduler.concealed_us(), 20_000);

    scheduler.reset_stats();
    assert_eq!(scheduler.concealments(), 0);
    assert_eq!(scheduler.concealed_us(), 0);
}
