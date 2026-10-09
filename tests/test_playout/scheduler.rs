//! 鳴らす時刻を決めるスケジューラのテスト
//!
//! 公開 API (`AudioPlayoutScheduler`) の契約を確認する。

use shiguredo_moqt::playout::scheduler::{
    AUDIO_PLAYOUT_BACKLOG_US, AUDIO_PLAYOUT_DELAY_US, AUDIO_PLAYOUT_MAX_CONCEAL_US,
    AUDIO_PLAYOUT_MAX_LATENESS_US, AUDIO_PLAYOUT_MIN_CONCEAL_US, AudioPlayoutBasis,
    AudioPlayoutDecision, AudioPlayoutDropReason, AudioPlayoutInput, AudioPlayoutScheduler,
};
use shiguredo_moqt::playout::timeline::{TIMELINE_ARRIVAL_DELAY_US, TIMELINE_AUDIO_DELAY_FLOOR_US};

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
            basis: AudioPlayoutBasis::Timestamp,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        },
        "目標どおりに鳴らすときは目標の時刻を基準にすること"
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
            basis: AudioPlayoutBasis::Timestamp,
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
            basis: AudioPlayoutBasis::Timestamp,
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
            basis: AudioPlayoutBasis::Timestamp,
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
            basis: AudioPlayoutBasis::Timestamp,
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
            basis: AudioPlayoutBasis::Timestamp,
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
    assert_eq!(
        decision,
        AudioPlayoutDecision::Drop {
            reason: AudioPlayoutDropReason::Backlog,
        },
        "並べすぎの音は理由を付けて捨てること"
    );
    assert_eq!(scheduler.drops(), 1);
}

#[test]
fn a_late_sound_keeps_playing_while_the_previous_one_is_sounding() {
    let mut scheduler = AudioPlayoutScheduler::new();
    // 1 つ目は目標 100 ms に間に合い、1_100 ms まで鳴る長い音である
    let first = scheduler.schedule(input(0, 0, 1_000_000, Some(100_000)));
    assert_eq!(
        first,
        AudioPlayoutDecision::Play {
            start_at_us: 100_000,
            basis: AudioPlayoutBasis::Timestamp,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
    // 2 つ目は目標 200 ms に対して 900 ms 遅れるが、直前の音 (1_100 ms まで) がまだ
    // 鳴っている。音が連続しているため到着基準へ並べ直さず、その終わりへ繋げて鳴らす
    let second = scheduler.schedule(input(0, 20_000, 20_000, Some(200_000)));
    assert_eq!(
        second,
        AudioPlayoutDecision::Play {
            start_at_us: 1_100_000,
            basis: AudioPlayoutBasis::Timestamp,
            compress_us: 10_000,
            gap_start_us: 0,
            gap_us: 0,
        },
        "直前の音の終わりへ繋げて順序と連続性を保つこと"
    );
    assert_eq!(scheduler.drops(), 0, "鳴り遅れでは捨てないこと");
    assert_eq!(
        scheduler.rebases(),
        0,
        "音がまだ鳴っている間は到着基準へ並べ直さないこと"
    );
    assert_eq!(
        scheduler.lateness_us(),
        900_000,
        "遅れはそのまま記録すること"
    );
}

#[test]
fn a_late_sound_is_rebased_by_arrival_when_the_sound_stopped() {
    let mut scheduler = AudioPlayoutScheduler::new();
    // 目標 (0) から 500 ms を超えて離れて届いた。まだ一度も鳴らしていない (音が途切れて
    // いる) ため、捨てずに到着基準の小さな目標 (到着 520 ms + 80 ms) へ並べ直して鳴らす
    let decision = scheduler.schedule(input(
        AUDIO_PLAYOUT_MAX_LATENESS_US + 20_000,
        0,
        20_000,
        Some(0),
    ));
    assert_eq!(
        decision,
        AudioPlayoutDecision::Play {
            start_at_us: 600_000,
            basis: AudioPlayoutBasis::Arrival,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        },
        "音が途切れているときは到着基準へ並べ直すこと"
    );
    assert_eq!(scheduler.drops(), 0, "鳴り遅れでは捨てないこと");
    assert_eq!(scheduler.rebases(), 1, "到着基準へ並べ直したこと");
    assert_eq!(
        scheduler.lateness_us(),
        0,
        "並べ直したら遅れは 0 に戻ること"
    );
}

#[test]
fn unapplied_compression_remains_as_lateness() {
    let mut scheduler = AudioPlayoutScheduler::new();
    let first = scheduler.schedule(input(0, 0, 20_000, Some(0)));
    assert_eq!(
        first,
        AudioPlayoutDecision::Play {
            start_at_us: 10_000,
            basis: AudioPlayoutBasis::Timestamp,
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
            basis: AudioPlayoutBasis::Timestamp,
            compress_us: 4_000,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
}

#[test]
fn over_applied_compression_moves_the_previous_end_earlier() {
    let mut scheduler = AudioPlayoutScheduler::new();
    // 目標 6 ms に対して 10 ms から鳴らすため、4 ms 詰める要求が出る
    let first = scheduler.schedule(input(0, 0, 20_000, Some(6_000)));
    assert_eq!(
        first,
        AudioPlayoutDecision::Play {
            start_at_us: 10_000,
            basis: AudioPlayoutBasis::Timestamp,
            compress_us: 4_000,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
    // 波形の周期が要求より長く、10 ms 詰められた (要求より 6 ms 長く削れた)
    scheduler.confirm_stretch(10_000);
    assert_eq!(
        scheduler.compressed_us(),
        10_000,
        "実際に詰めた長さを記録すること"
    );
    // 前の音の終わりが予定 (26 ms) より 6 ms 手前の 20 ms になるため、次の音 (目標 30 ms)
    // との間に 10 ms の隙間ができる。詰めすぎた分を戻さないと 4 ms と見積もって補間しない
    let second = scheduler.schedule(input(0, 20_000, 20_000, Some(30_000)));
    assert_eq!(
        second,
        AudioPlayoutDecision::Play {
            start_at_us: 30_000,
            basis: AudioPlayoutBasis::Timestamp,
            compress_us: 0,
            gap_start_us: 20_000,
            gap_us: 10_000,
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
            basis: AudioPlayoutBasis::Arrival,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        },
        "目標が無いときは到着 + 到着基準の遅れから鳴らすこと"
    );
    // 続く音は TIMESTAMP の間隔どおりに並ぶ (前の音の終わりにちょうど繋がる)
    let second = scheduler.schedule(input(20_000, 20_000, 20_000, None));
    assert_eq!(
        second,
        AudioPlayoutDecision::Play {
            start_at_us: AUDIO_PLAYOUT_DELAY_US + 20_000,
            basis: AudioPlayoutBasis::Arrival,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
}

#[test]
fn arrival_based_places_the_first_sound_from_the_arrival_position() {
    let mut scheduler = AudioPlayoutScheduler::new();
    // 今の時刻は 150 ms だが、音が届いた時点で実際に鳴っている位置は 100 ms である。
    // 到着基準の遅れ (80 ms) は到着の位置から数えるため 180 ms から鳴る (今の時刻から
    // 数えると 230 ms になる)
    let mut request = input(150_000, 0, 20_000, None);
    request.arrival_us = 100_000;
    let decision = scheduler.schedule(request);
    assert_eq!(
        decision,
        AudioPlayoutDecision::Play {
            start_at_us: 180_000,
            basis: AudioPlayoutBasis::Arrival,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        },
        "到着の位置から到着基準の遅れだけ後ろに置くこと"
    );
    assert_eq!(
        scheduler.rebases(),
        0,
        "予約できる最も早い時刻より後ろなので基準を取り直さないこと"
    );
}

#[test]
fn arrival_based_uses_the_arrival_delay() {
    // 学習した再生の遅れ (delay_us) が大きくても、到着基準の遅れで並べる
    let mut scheduler = AudioPlayoutScheduler::new();
    let mut request = input(1_000_000, 0, 20_000, None);
    request.delay_us = 400_000;
    request.arrival_us = 1_000_000;
    request.arrival_delay_us = TIMELINE_ARRIVAL_DELAY_US;
    assert_eq!(
        scheduler.schedule(request),
        AudioPlayoutDecision::Play {
            start_at_us: 1_100_000,
            basis: AudioPlayoutBasis::Arrival,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        },
        "到着基準の遅れの上限 (100 ms) を使うこと"
    );

    // 下限 (80 ms) のときは到着 + 80 ms から鳴る
    let mut scheduler = AudioPlayoutScheduler::new();
    let mut request = input(1_000_000, 0, 20_000, None);
    request.arrival_us = 1_000_000;
    request.arrival_delay_us = TIMELINE_AUDIO_DELAY_FLOOR_US;
    assert_eq!(
        scheduler.schedule(request),
        AudioPlayoutDecision::Play {
            start_at_us: 1_080_000,
            basis: AudioPlayoutBasis::Arrival,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        },
        "到着基準の遅れの下限 (80 ms) を使うこと"
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
            basis: AudioPlayoutBasis::Arrival,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
    assert_eq!(scheduler.rebases(), 1);
}

#[test]
fn resync_jump_below_the_minimum_is_not_counted_as_a_rebase() {
    let mut scheduler = AudioPlayoutScheduler::new();
    // 1 つ目は到着基準で 80 ms から鳴り、100 ms まで鳴る
    let first = scheduler.schedule(input(0, 0, 20_000, None));
    assert_eq!(
        first,
        AudioPlayoutDecision::Play {
            start_at_us: AUDIO_PLAYOUT_DELAY_US,
            basis: AudioPlayoutBasis::Arrival,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
    // 出力のバッファが到着 + 再生の遅れ (110 ms) より先に進んでおり、予約できる最も早い
    // 時刻 (今 + 10 ms = 110 ms) へずらすだけになる。ずらす幅は 105 ms から 5 ms であり、
    // 10 ms 未満なので基準を取り直した回数に数えない
    let mut request = input(100_000, 25_000, 20_000, None);
    request.arrival_us = 30_000;
    let second = scheduler.schedule(request);
    assert_eq!(
        second,
        AudioPlayoutDecision::Play {
            start_at_us: 110_000,
            basis: AudioPlayoutBasis::Arrival,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
    assert_eq!(
        scheduler.rebases(),
        0,
        "10 ms 未満のずらしは並べ直しとして数えないこと"
    );

    // 到着 + 再生の遅れ (180 ms) へ置き直す幅が 75 ms あるときは、並べ直しとして数える
    let mut scheduler = AudioPlayoutScheduler::new();
    scheduler.schedule(input(0, 0, 20_000, None));
    let mut request = input(100_000, 25_000, 20_000, None);
    request.arrival_us = 80_000;
    request.arrival_delay_us = TIMELINE_ARRIVAL_DELAY_US;
    let second = scheduler.schedule(request);
    assert_eq!(
        second,
        AudioPlayoutDecision::Play {
            start_at_us: 180_000,
            basis: AudioPlayoutBasis::Arrival,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        }
    );
    assert_eq!(scheduler.rebases(), 1, "10 ms 以上のずらしは数えること");
}

#[test]
fn arrival_based_rebases_on_a_big_timestamp_jump() {
    let mut scheduler = AudioPlayoutScheduler::new();
    scheduler.schedule(input(0, 0, 20_000, None));
    // TIMESTAMP が大きく飛んだ。前の音のすぐ後ろ (か到着 + 再生の遅れ) から並べ直す
    let decision = scheduler.schedule(input(100_000, 1_000_000, 20_000, None));
    assert_eq!(
        decision,
        AudioPlayoutDecision::Play {
            start_at_us: 180_000,
            basis: AudioPlayoutBasis::Arrival,
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
    assert_eq!(
        decision,
        AudioPlayoutDecision::Drop {
            reason: AudioPlayoutDropReason::Backlog,
        },
        "並べすぎの音は理由を付けて捨てること"
    );
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
            basis: AudioPlayoutBasis::Timestamp,
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
            basis: AudioPlayoutBasis::Timestamp,
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
            basis: AudioPlayoutBasis::Arrival,
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
            basis: AudioPlayoutBasis::Timestamp,
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
            basis: AudioPlayoutBasis::Timestamp,
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
            basis: AudioPlayoutBasis::Timestamp,
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
            basis: AudioPlayoutBasis::Timestamp,
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
            basis: AudioPlayoutBasis::Timestamp,
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
            basis: AudioPlayoutBasis::Timestamp,
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
            basis: AudioPlayoutBasis::Timestamp,
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
            basis: AudioPlayoutBasis::Timestamp,
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
            basis: AudioPlayoutBasis::Arrival,
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
            basis: AudioPlayoutBasis::Timestamp,
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
            basis: AudioPlayoutBasis::Timestamp,
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

#[test]
fn last_play_returns_the_decided_sound() {
    // 鳴らすと決めた音の値は、決定と入力をそのまま写したものになる
    let mut scheduler = AudioPlayoutScheduler::new();
    assert_eq!(
        scheduler.last_play(),
        None,
        "まだ鳴らすと決めていなければ None であること"
    );

    scheduler.schedule(input(150_000, 5_000, 20_000, Some(100_000)));
    let play = scheduler.last_play().expect("鳴らすと決めた音の値がある");
    assert_eq!(
        play.start_at_us, 160_000,
        "決定した鳴り始める時刻を返すこと"
    );
    assert_eq!(play.arrival_us, 150_000, "到着の基準を返すこと");
    assert_eq!(
        play.target_start_us,
        Some(100_000),
        "目標の開始時刻を返すこと"
    );
    assert_eq!(
        play.basis,
        AudioPlayoutBasis::Timestamp,
        "鳴らす時刻を決めるのに使った計画を返すこと"
    );
    assert_eq!(
        play.played_us, 10_000,
        "要求した詰める長さ (10 ms) を引いた長さを返すこと"
    );
    let play_again = scheduler.last_play().expect("鳴らすと決めた音の値がある");
    assert_eq!(play_again, play, "同じ値を何度でも読めること");
}

#[test]
fn last_play_follows_the_applied_stretch() {
    // 実際に詰めた長さが返ってきたら、鳴る長さをその分だけ直すこと
    let mut scheduler = AudioPlayoutScheduler::new();
    scheduler.schedule(input(150_000, 5_000, 20_000, Some(100_000)));
    // 要求した 10 ms のうち 5 ms しか詰められなかった。残りの 5 ms は音が後ろへ伸びる
    scheduler.confirm_stretch(5_000);
    assert_eq!(
        scheduler
            .last_play()
            .expect("鳴らすと決めた音の値がある")
            .played_us,
        15_000,
        "詰められなかった分だけ鳴る長さが伸びること"
    );

    // 要求より長く詰められたときは、詰めすぎた分だけ手前で終わる
    let mut scheduler = AudioPlayoutScheduler::new();
    scheduler.schedule(input(150_000, 5_000, 20_000, Some(100_000)));
    scheduler.confirm_stretch(12_000);
    assert_eq!(
        scheduler
            .last_play()
            .expect("鳴らすと決めた音の値がある")
            .played_us,
        8_000,
        "詰めすぎた分だけ鳴る長さが短くなること"
    );

    // 確認が返ってこないまま次の音を並べたときは、適用されなかったものとして扱う
    let mut scheduler = AudioPlayoutScheduler::new();
    scheduler.schedule(input(150_000, 5_000, 20_000, Some(100_000)));
    scheduler.schedule(input(150_000, 25_000, 20_000, Some(200_000)));
    let previous = scheduler.last_play().expect("鳴らすと決めた音の値がある");
    assert_eq!(
        previous.played_us, 20_000,
        "詰められなかった音は長さがそのままになること"
    );
    assert_eq!(previous.start_at_us, 200_000, "直近の決定で置き換わること");
}

#[test]
fn last_play_is_kept_by_a_drop_and_cleared_by_reset() {
    let mut scheduler = AudioPlayoutScheduler::new();
    scheduler.schedule(input(0, 0, 20_000, Some(100_000)));
    let play = scheduler.last_play().expect("鳴らすと決めた音の値がある");

    // 並べすぎで捨てる決定では、直前に鳴らすと決めた音の値を残す
    let dropped = scheduler.schedule(AudioPlayoutInput {
        now_us: 0,
        arrival_us: 0,
        timestamp_us: 20_000,
        duration_us: 20_000,
        target_start_us: Some(1_000_000),
        enforce_target: true,
        delay_us: AUDIO_PLAYOUT_DELAY_US,
        arrival_delay_us: AUDIO_PLAYOUT_DELAY_US,
        presentation_delay_us: AUDIO_PLAYOUT_DELAY_US,
    });
    assert_eq!(
        dropped,
        AudioPlayoutDecision::Drop {
            reason: AudioPlayoutDropReason::Backlog,
        },
        "並べすぎの音は捨てること"
    );
    assert_eq!(
        scheduler.last_play(),
        Some(play),
        "捨てる決定では直前に鳴らすと決めた音の値を残すこと"
    );

    // 購読のやり直しでは消える
    scheduler.reset();
    assert_eq!(scheduler.last_play(), None, "購読のやり直しで消えること");
}

#[test]
fn last_play_follows_the_arrival_basis() {
    // 到着基準へ並べ直した音も、目標が渡されていればそのまま残すこと
    let mut scheduler = AudioPlayoutScheduler::new();
    let decision = scheduler.schedule(AudioPlayoutInput {
        now_us: 500_000,
        arrival_us: 500_000,
        timestamp_us: 5_000,
        duration_us: 20_000,
        target_start_us: Some(0),
        enforce_target: true,
        delay_us: AUDIO_PLAYOUT_DELAY_US,
        arrival_delay_us: AUDIO_PLAYOUT_DELAY_US,
        presentation_delay_us: AUDIO_PLAYOUT_DELAY_US,
    });
    assert_eq!(
        decision,
        AudioPlayoutDecision::Play {
            start_at_us: 580_000,
            basis: AudioPlayoutBasis::Arrival,
            compress_us: 0,
            gap_start_us: 0,
            gap_us: 0,
        },
        "目標から離れすぎた音は到着基準へ並べ直すこと"
    );
    let play = scheduler.last_play().expect("鳴らすと決めた音の値がある");
    assert_eq!(
        play.basis,
        AudioPlayoutBasis::Arrival,
        "到着基準へ並べ直したことを返すこと"
    );
    assert_eq!(
        play.target_start_us,
        Some(0),
        "使えなかった目標もそのまま返すこと"
    );
    assert_eq!(
        play.played_us, 20_000,
        "到着基準では詰めないため音の長さがそのままになること"
    );
}
