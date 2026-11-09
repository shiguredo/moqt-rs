//! 音声の再生の観測値のテスト
//!
//! 公開 API (`AudioPlayoutTimingStats` / `summarize_timings`) の契約を確認する。

use shiguredo_moqt::playout::scheduler::{
    AUDIO_PLAYOUT_DELAY_US, AudioPlayoutBasis, AudioPlayoutCommit, AudioPlayoutDecision,
    AudioPlayoutInput, AudioPlayoutPlay, AudioPlayoutScheduler,
};
use shiguredo_moqt::playout::timing::{
    AUDIO_PLAYOUT_TIMING_WINDOW_US, AudioMissReason, AudioMissTotal, AudioPlayoutGap,
    AudioPlayoutMiss, AudioPlayoutTimingSnapshot, AudioPlayoutTimingStats, MAX_RECENT_AUDIO_MISSES,
    TimingSummary, summarize_timings,
};

/// 鳴らすと決めた音の値を作る
fn play(
    arrival_us: i64,
    target_us: Option<i64>,
    start_us: i64,
    played_us: i64,
    basis: AudioPlayoutBasis,
) -> AudioPlayoutPlay {
    AudioPlayoutPlay {
        arrival_us,
        target_start_us: target_us,
        start_at_us: start_us,
        played_us,
        basis,
    }
}

/// 鳴らさなかった音の記録を作る
fn miss(
    at_us: i64,
    reason: AudioMissReason,
    duration_us: i64,
    target_us: Option<i64>,
    arrival_us: Option<i64>,
) -> AudioPlayoutMiss {
    AudioPlayoutMiss {
        at_us,
        reason,
        duration_us,
        target_us,
        arrival_us,
    }
}

#[test]
fn record_play_returns_recent_values_and_distributions() {
    let mut stats = AudioPlayoutTimingStats::new();
    // 余裕 80 ms (予定 180 - 到着 100)、鳴るまで 90 ms、予定から 10 ms 遅れ
    stats.record_play(play(
        100_000,
        Some(180_000),
        190_000,
        20_000,
        AudioPlayoutBasis::Timestamp,
    ));
    // 余裕 20 ms、鳴るまで 30 ms、予定から 10 ms 遅れ
    stats.record_play(play(
        200_000,
        Some(220_000),
        230_000,
        20_000,
        AudioPlayoutBasis::Timestamp,
    ));
    // 予定を過ぎて届いた音 (余裕 -20 ms)。鳴るまで 10 ms、予定から 30 ms 遅れ
    stats.record_play(play(
        300_000,
        Some(280_000),
        310_000,
        20_000,
        AudioPlayoutBasis::Timestamp,
    ));

    let snapshot = stats.snapshot(400_000);
    assert_eq!(
        snapshot.last_target_us,
        Some(280_000),
        "直近の再生予定時刻を返すこと"
    );
    assert_eq!(
        snapshot.last_arrival_us,
        Some(300_000),
        "直近の到着時刻を返すこと"
    );
    assert_eq!(
        snapshot.last_start_us,
        Some(310_000),
        "直近に鳴り始める時刻を返すこと"
    );
    assert_eq!(
        snapshot.last_slack_us,
        Some(-20_000),
        "直近の余裕 (負は間に合っていない) を返すこと"
    );
    assert_eq!(
        snapshot.last_start_delay_us,
        Some(10_000),
        "直近の鳴るまでの時間を返すこと"
    );
    assert_eq!(
        snapshot.last_lateness_us,
        Some(30_000),
        "直近の予定からの遅れを返すこと"
    );
    // 昇順に並べて nearest-rank 法で求める (3 件なら p50 は 2 番目、p95 と max は 3 番目)
    assert_eq!(
        snapshot.slack_us,
        Some(TimingSummary {
            p50: 20_000,
            p95: 80_000,
            max: 80_000,
        }),
        "予定に対する余裕の分布を返すこと"
    );
    assert_eq!(
        snapshot.start_delay_us,
        Some(TimingSummary {
            p50: 30_000,
            p95: 90_000,
            max: 90_000,
        }),
        "到着から鳴り始めるまでの時間の分布を返すこと"
    );
    assert_eq!(
        snapshot.lateness_us,
        Some(TimingSummary {
            p50: 10_000,
            p95: 30_000,
            max: 30_000,
        }),
        "予定からの遅れの分布を返すこと"
    );
    assert_eq!(
        snapshot.played_frames, 3,
        "鳴らすと決めた音の数を数えること"
    );
    assert_eq!(
        snapshot.played_us, 60_000,
        "鳴らすと決めた音の長さを足すこと"
    );
    assert_eq!(
        snapshot.arrival_planned_frames, 0,
        "時間軸の予定で鳴らした音を数えないこと"
    );
    assert_eq!(snapshot.unplanned_frames, 0);
    assert_eq!(snapshot.missed_frames, 0);
    assert_eq!(snapshot.missed_us, 0);
}

#[test]
fn record_play_counts_arrival_basis_frames() {
    // 到着基準の計画で鳴らした音 (壁時計の TIMESTAMP を持たない、jitter buffer が無効、
    // トラックの基準が共有されていない) は、予定を持たないため余裕と遅れを分布へ入れず、
    // arrival_planned_frames に数える
    let mut stats = AudioPlayoutTimingStats::new();
    stats.record_play(play(
        100_000,
        None,
        110_000,
        20_000,
        AudioPlayoutBasis::Arrival,
    ));

    let snapshot = stats.snapshot(200_000);
    assert_eq!(snapshot.last_target_us, None);
    assert_eq!(
        snapshot.last_slack_us, None,
        "予定が無ければ余裕は None であること"
    );
    assert_eq!(
        snapshot.last_lateness_us, None,
        "予定が無ければ遅れは None であること"
    );
    assert_eq!(
        snapshot.start_delay_us,
        Some(TimingSummary {
            p50: 10_000,
            p95: 10_000,
            max: 10_000,
        }),
        "予定が無くても鳴るまでの時間は記録すること"
    );
    assert_eq!(
        snapshot.slack_us, None,
        "到着基準の計画の音を分布へ入れないこと"
    );
    assert_eq!(snapshot.lateness_us, None);
    assert_eq!(snapshot.arrival_planned_frames, 1);
    assert_eq!(
        snapshot.unplanned_frames, 0,
        "計画に載っているため数えないこと"
    );
    assert_eq!(snapshot.played_frames, 1);
    assert_eq!(snapshot.played_us, 20_000);
}

#[test]
fn record_play_counts_unplanned_frames() {
    // 到着基準の計画も持たないまま鳴らした音は unplanned_frames に数える。時間軸の予定も
    // 到着基準の計画も渡さなかった呼び出し側の取りこぼしであり、通常は 0 になる
    let mut stats = AudioPlayoutTimingStats::new();
    stats.record_play(play(
        100_000,
        None,
        110_000,
        20_000,
        AudioPlayoutBasis::Timestamp,
    ));

    let snapshot = stats.snapshot(200_000);
    assert_eq!(snapshot.arrival_planned_frames, 0);
    assert_eq!(snapshot.unplanned_frames, 1);
    assert_eq!(snapshot.played_frames, 1);
    assert_eq!(snapshot.last_target_us, None);
}

#[test]
fn record_miss_counts_by_reason_and_keeps_recent_events() {
    let mut stats = AudioPlayoutTimingStats::new();
    stats.record_miss(miss(
        100_000,
        AudioMissReason::Backlog,
        20_000,
        Some(500_000),
        Some(100_000),
    ));
    // 予定も到着も分からない (追いつきの途中の音)
    stats.record_miss(miss(200_000, AudioMissReason::CatchUp, 20_000, None, None));

    let snapshot = stats.snapshot(300_000);
    assert_eq!(
        snapshot.missed_frames, 2,
        "鳴らさなかった音の数を数えること"
    );
    assert_eq!(
        snapshot.missed_us, 40_000,
        "鳴らさなかった音の長さを足すこと"
    );
    assert_eq!(
        snapshot.missed_by_reason.backlog,
        AudioMissTotal {
            count: 1,
            duration_us: 20_000,
        }
    );
    assert_eq!(
        snapshot.missed_by_reason.catch_up,
        AudioMissTotal {
            count: 1,
            duration_us: 20_000,
        }
    );
    assert_eq!(
        snapshot.missed_by_reason.get(AudioMissReason::Error),
        AudioMissTotal {
            count: 0,
            duration_us: 0,
        },
        "記録していない理由は 0 のままであること"
    );
    assert_eq!(
        snapshot.missed_by_reason.get(AudioMissReason::Stopped),
        AudioMissTotal::default()
    );
    assert_eq!(snapshot.recent_misses.len(), 2);
    assert_eq!(
        snapshot.recent_misses[0].at_us, 100_000,
        "鳴らさなかった時刻を残すこと"
    );
    assert_eq!(snapshot.recent_misses[0].reason, AudioMissReason::Backlog);
    assert_eq!(snapshot.recent_misses[0].duration_us, 20_000);
    assert_eq!(
        snapshot.recent_misses[0].slack_us,
        Some(400_000),
        "捨てた時点の余裕を残すこと"
    );
    assert_eq!(
        snapshot.recent_misses[1].slack_us, None,
        "予定が無ければ余裕は None であること"
    );
}

#[test]
fn record_miss_keeps_slack_only_when_target_and_arrival_are_both_known() {
    let mut stats = AudioPlayoutTimingStats::new();
    // 予定も到着も分かる (余裕は負にもなる)
    stats.record_miss(miss(
        100_000,
        AudioMissReason::Backlog,
        10_000,
        Some(90_000),
        Some(100_000),
    ));
    // 到着だけ分かる (予定が無い)
    stats.record_miss(miss(
        200_000,
        AudioMissReason::Error,
        10_000,
        None,
        Some(200_000),
    ));
    // 予定だけ分かる (到着が分からない)
    stats.record_miss(miss(
        300_000,
        AudioMissReason::CatchUp,
        10_000,
        Some(300_000),
        None,
    ));
    // どちらも分からない
    stats.record_miss(miss(400_000, AudioMissReason::Stopped, 10_000, None, None));

    let snapshot = stats.snapshot(500_000);
    assert_eq!(
        snapshot.recent_misses[0].slack_us,
        Some(-10_000),
        "予定を過ぎて届いた音の余裕は負になること"
    );
    assert_eq!(
        snapshot.recent_misses[1].slack_us, None,
        "予定が無ければ余裕を残さないこと"
    );
    assert_eq!(
        snapshot.recent_misses[2].slack_us, None,
        "到着が分からなければ余裕を残さないこと"
    );
    assert_eq!(snapshot.recent_misses[3].slack_us, None);
    assert_eq!(snapshot.missed_frames, 4, "余裕が分からなくても数えること");
    assert_eq!(snapshot.missed_us, 40_000);
}

#[test]
fn record_miss_clamps_a_negative_length() {
    // 長さが負の記録は 0 として数える (累積が減らないこと)
    let mut stats = AudioPlayoutTimingStats::new();
    stats.record_miss(miss(100_000, AudioMissReason::Backlog, -10_000, None, None));

    let snapshot = stats.snapshot(200_000);
    assert_eq!(
        snapshot.missed_frames, 1,
        "長さが負でも鳴らさなかった音として数えること"
    );
    assert_eq!(snapshot.missed_us, 0, "負の長さを足さないこと");
    assert_eq!(
        snapshot.recent_misses[0].duration_us, 0,
        "直近の一覧の長さも 0 にすること"
    );
}

#[test]
fn record_play_clamps_a_negative_length() {
    // 長さが負の音は 0 として数える (累積が減らないこと)
    let mut stats = AudioPlayoutTimingStats::new();
    stats.record_play(play(0, None, 10_000, -5_000, AudioPlayoutBasis::Arrival));

    let snapshot = stats.snapshot(20_000);
    assert_eq!(
        snapshot.played_frames, 1,
        "鳴らすと決めた音として数えること"
    );
    assert_eq!(snapshot.played_us, 0, "負の長さを引かないこと");
}

#[test]
fn record_miss_drops_the_oldest_recent_events_over_the_limit() {
    let mut stats = AudioPlayoutTimingStats::new();
    for index in 0..(MAX_RECENT_AUDIO_MISSES as i64 + 5) {
        stats.record_miss(miss(index, AudioMissReason::Backlog, 20_000, None, None));
    }

    let snapshot = stats.snapshot(100);
    assert_eq!(snapshot.recent_misses.len(), MAX_RECENT_AUDIO_MISSES);
    // 最初の 5 件は捨てられ、6 件目の時刻から残る
    assert_eq!(snapshot.recent_misses[0].at_us, 5, "古い方から捨てること");
    assert_eq!(
        snapshot.missed_frames,
        MAX_RECENT_AUDIO_MISSES as u64 + 5,
        "累積の数は捨てないこと"
    );
    assert_eq!(
        snapshot.missed_us,
        (MAX_RECENT_AUDIO_MISSES as i64 + 5) * 20_000
    );
}

#[test]
fn record_stopped_counts_only_the_sounds_that_did_not_finish() {
    let mut stats = AudioPlayoutTimingStats::new();
    // 100 から 120 まで鳴る音 (止めたときには鳴り終わっている) と、200 から 220 まで鳴る音
    stats.record_play(play(0, None, 100_000, 20_000, AudioPlayoutBasis::Arrival));
    stats.record_play(play(0, None, 200_000, 20_000, AudioPlayoutBasis::Arrival));
    // 130 で止める。既に鳴り終わった 1 つ目は数えず、2 つ目は 20 ms すべてが鳴らなかった
    stats.record_stopped(130_000);

    let snapshot = stats.snapshot(200_000);
    assert_eq!(snapshot.missed_frames, 1, "鳴り終わった音を数えないこと");
    assert_eq!(snapshot.missed_us, 20_000);
    assert_eq!(
        snapshot.missed_by_reason.stopped,
        AudioMissTotal {
            count: 1,
            duration_us: 20_000,
        }
    );
    // 一覧を消した後の停止では数えない (二重に数えないこと)
    stats.record_stopped(300_000);
    assert_eq!(
        stats.snapshot(300_000).missed_frames,
        1,
        "同じ音を二重に数えないこと"
    );
}

#[test]
fn record_stopped_counts_only_the_remaining_length_of_a_started_sound() {
    // 鳴り始めている音は、止めた時点からの残りだけを数える。既に聞こえた分は鳴らなかった
    // とはみなさない
    let mut stats = AudioPlayoutTimingStats::new();
    stats.record_play(play(0, None, 250_000, 20_000, AudioPlayoutBasis::Arrival));
    stats.record_stopped(260_000);

    assert_eq!(
        stats.snapshot(300_000).missed_us,
        10_000,
        "残りの 10 ms だけを数えること"
    );
}

#[test]
fn record_stopped_ignores_a_sound_with_no_length() {
    // 長さ 0 の音は、まだ鳴り始めていなくても数える分が無い
    let mut stats = AudioPlayoutTimingStats::new();
    stats.record_play(play(0, None, 200_000, 0, AudioPlayoutBasis::Arrival));
    stats.record_stopped(150_000);

    assert_eq!(
        stats.snapshot(300_000).missed_frames,
        0,
        "長さ 0 の音を鳴らなかったことにしないこと"
    );
}

#[test]
fn snapshot_drops_records_out_of_the_window_but_keeps_totals() {
    // 分布は直近の窓の値から求める。窓より古い記録は分布から落ちるが、累積の数と直近の
    // 値は残る
    let mut stats = AudioPlayoutTimingStats::with_window_us(1_000_000);
    stats.record_play(play(
        0,
        Some(10_000),
        20_000,
        20_000,
        AudioPlayoutBasis::Timestamp,
    ));
    stats.record_play(play(
        500_000,
        Some(510_000),
        520_000,
        20_000,
        AudioPlayoutBasis::Timestamp,
    ));

    let snapshot = stats.snapshot(2_000_000);
    assert_eq!(
        snapshot.slack_us, None,
        "窓の外の値だけになれば None になること"
    );
    assert_eq!(snapshot.start_delay_us, None);
    assert_eq!(snapshot.played_frames, 2);
    assert_eq!(
        snapshot.last_target_us,
        Some(510_000),
        "直近の値は窓に関係なく残ること"
    );
}

#[test]
fn snapshot_keeps_a_record_on_the_window_boundary() {
    // 窓の境目 (今 - 窓) の記録は、窓の中として残る
    let mut stats = AudioPlayoutTimingStats::with_window_us(1_000_000);
    stats.record_play(play(
        1_000_000,
        Some(1_010_000),
        1_020_000,
        20_000,
        AudioPlayoutBasis::Timestamp,
    ));

    let snapshot = stats.snapshot(2_000_000);
    assert_eq!(
        snapshot.start_delay_us,
        Some(TimingSummary {
            p50: 20_000,
            p95: 20_000,
            max: 20_000,
        }),
        "窓の境目の記録を落とさないこと"
    );
}

#[test]
fn snapshot_keeps_an_empty_window_when_nothing_is_recorded() {
    // 何も記録していなければ、初期状態のままである (既定の窓の計器は Default でも作れる)
    let mut stats = AudioPlayoutTimingStats::default();
    assert_eq!(
        stats.snapshot(0),
        AudioPlayoutTimingSnapshot::default(),
        "何も記録していなければ初期状態を返すこと"
    );
}

#[test]
fn reset_returns_to_the_initial_state() {
    let mut stats = AudioPlayoutTimingStats::new();
    stats.record_play(play(
        0,
        Some(10_000),
        20_000,
        20_000,
        AudioPlayoutBasis::Timestamp,
    ));
    stats.record_miss(miss(
        30_000,
        AudioMissReason::Error,
        20_000,
        Some(10_000),
        Some(0),
    ));
    stats.record_stopped(40_000);
    stats.record_gap(AudioPlayoutGap {
        uncovered_us: 20_000,
        unrecoverable_us: 0,
    });
    stats.record_underrun(10_000);
    stats.reset();

    assert_eq!(
        stats.snapshot(100_000),
        AudioPlayoutTimingSnapshot::default(),
        "すべての記録を捨てて初期状態へ戻すこと"
    );
}

#[test]
fn record_stretch_unavailable_counts_cumulatively_and_resets() {
    let mut stats = AudioPlayoutTimingStats::new();
    // 数える前は 0 である (0 なら時間圧縮と補間が使えている)
    assert_eq!(
        stats.snapshot(0).stretch_unavailable_frames,
        0,
        "数える前は 0 であること"
    );
    // 呼んだ回数だけ数える
    stats.record_stretch_unavailable();
    stats.record_stretch_unavailable();
    assert_eq!(
        stats.snapshot(1_000).stretch_unavailable_frames,
        2,
        "呼んだ回数だけ数えること"
    );
    // 累積であり、分布の窓の外へ出ても残る
    assert_eq!(
        stats
            .snapshot(AUDIO_PLAYOUT_TIMING_WINDOW_US * 2)
            .stretch_unavailable_frames,
        2,
        "窓の外へ出ても累積を残すこと"
    );
    // 購読のやり直しでは 0 に戻る
    stats.reset();
    assert_eq!(
        stats
            .snapshot(AUDIO_PLAYOUT_TIMING_WINDOW_US * 2)
            .stretch_unavailable_frames,
        0,
        "reset で 0 に戻ること"
    );
}

#[test]
fn record_gap_counts_the_uncovered_total_and_maximum() {
    let mut stats = AudioPlayoutTimingStats::new();
    // 数える前はすべて 0 である
    let initial = stats.snapshot(0);
    assert_eq!(initial.gaps, 0);
    assert_eq!(initial.gap_total_us, 0);
    assert_eq!(initial.gap_max_us, 0);
    assert_eq!(initial.unrecoverable_gaps, 0);
    assert_eq!(initial.unrecoverable_gap_total_us, 0);

    // 補間で埋まらなかった長さを数える。うち 2 件は予約できる最も早い時刻より前だった
    stats.record_gap(AudioPlayoutGap {
        uncovered_us: 20_000,
        unrecoverable_us: 10_000,
    });
    stats.record_gap(AudioPlayoutGap {
        uncovered_us: 40_000,
        unrecoverable_us: 40_000,
    });
    stats.record_gap(AudioPlayoutGap {
        uncovered_us: 10_000,
        unrecoverable_us: 0,
    });
    let snapshot = stats.snapshot(1_000_000);
    assert_eq!(snapshot.gaps, 3, "補間で埋まらなかった回数を数えること");
    assert_eq!(
        snapshot.gap_total_us, 70_000,
        "補間で埋まらなかった長さの合計を数えること"
    );
    assert_eq!(
        snapshot.gap_max_us, 40_000,
        "補間で埋まらなかった長さの最大を返すこと"
    );
    assert_eq!(
        snapshot.unrecoverable_gaps, 2,
        "予約できなかった回数を数えること"
    );
    assert_eq!(
        snapshot.unrecoverable_gap_total_us, 50_000,
        "予約できなかった長さの合計を数えること"
    );

    // 0 以下の長さは数えない
    stats.record_gap(AudioPlayoutGap {
        uncovered_us: 0,
        unrecoverable_us: 10_000,
    });
    stats.record_gap(AudioPlayoutGap {
        uncovered_us: -1,
        unrecoverable_us: 0,
    });
    assert_eq!(stats.snapshot(2_000_000).gaps, 3);

    // 累積であり、分布の窓の外へ出ても残る
    let snapshot = stats.snapshot(AUDIO_PLAYOUT_TIMING_WINDOW_US * 2);
    assert_eq!(snapshot.gaps, 3);
    assert_eq!(snapshot.gap_total_us, 70_000);

    // 購読のやり直しでは 0 に戻る
    stats.reset();
    let snapshot = stats.snapshot(AUDIO_PLAYOUT_TIMING_WINDOW_US * 2);
    assert_eq!(snapshot.gaps, 0);
    assert_eq!(snapshot.gap_total_us, 0);
    assert_eq!(snapshot.gap_max_us, 0);
    assert_eq!(snapshot.unrecoverable_gaps, 0);
    assert_eq!(snapshot.unrecoverable_gap_total_us, 0);
}

#[test]
fn record_rendered_gap_and_boundary_follow_the_output() {
    let mut stats = AudioPlayoutTimingStats::new();
    // 出力側が描画した未補間区間は、補間の要求とは別に数える
    stats.record_rendered_gap(30_000, 2, 20_000);
    let snapshot = stats.snapshot(1_000_000);
    assert_eq!(snapshot.gaps, 2);
    assert_eq!(snapshot.gap_total_us, 30_000);
    assert_eq!(snapshot.gap_max_us, 20_000);
    assert_eq!(snapshot.unrecoverable_gaps, 2);
    assert_eq!(snapshot.unrecoverable_gap_total_us, 30_000);

    // 負の長さは数えない
    stats.record_rendered_gap(-1, 1, 0);
    stats.record_rendered_gap(1_000, 1, -1);
    assert_eq!(stats.snapshot(2_000_000).gaps, 2);

    // バッファ境界の段差は、最大を常に更新し、回数を足す
    stats.record_rendered_boundary(0.02, 1);
    stats.record_rendered_boundary(0.08, 2);
    let snapshot = stats.snapshot(2_000_000);
    assert_eq!(snapshot.boundary_step_count, 3);
    assert!((snapshot.boundary_step_max - 0.08).abs() < 1e-6);

    // 負の値は記録しない
    stats.record_rendered_boundary(-0.1, 1);
    assert_eq!(stats.snapshot(2_000_000).boundary_step_count, 3);

    // 購読のやり直しでは 0 に戻る
    stats.reset();
    let snapshot = stats.snapshot(2_000_000);
    assert_eq!(snapshot.gaps, 0);
    assert_eq!(snapshot.unrecoverable_gaps, 0);
    assert_eq!(snapshot.boundary_step_count, 0);
    assert_eq!(snapshot.boundary_step_max, 0.0);
}

#[test]
fn pending_tracks_the_reserved_sounds() {
    let mut stats = AudioPlayoutTimingStats::new();
    // 20 ms の音を 3 つ並べる (0 ms、20 ms、40 ms から鳴り始める)。到着はすべて 0 ms
    // として、記録のたびに前の予約が落とされないようにする
    for start_us in [0, 20_000, 40_000] {
        stats.record_play(play(
            0,
            Some(start_us),
            start_us,
            20_000,
            AudioPlayoutBasis::Timestamp,
        ));
    }
    assert_eq!(
        stats.pending_end_us(0),
        Some(60_000),
        "最後に鳴り終わる予約の時刻を返すこと"
    );
    assert_eq!(
        stats.pending_us(0),
        60_000,
        "鳴り終わっていない長さの合計を返すこと"
    );
    // 30 ms の時点では、1 つ目は鳴り終わっており、残りは 10 ms と 20 ms
    assert_eq!(stats.pending_us(30_000), 30_000);
    assert_eq!(stats.pending_end_us(70_000), None);
}

#[test]
fn record_underrun_counts_the_count_total_and_maximum() {
    let mut stats = AudioPlayoutTimingStats::new();
    let initial = stats.snapshot(0);
    assert_eq!(initial.underruns, 0);
    assert_eq!(initial.underrun_total_us, 0);
    assert_eq!(initial.underrun_max_us, 0);

    stats.record_underrun(5_000);
    stats.record_underrun(25_000);
    let snapshot = stats.snapshot(1_000_000);
    assert_eq!(snapshot.underruns, 2, "出力が枯れた回数を数えること");
    assert_eq!(
        snapshot.underrun_total_us, 30_000,
        "枯れた長さの合計を数えること"
    );
    assert_eq!(
        snapshot.underrun_max_us, 25_000,
        "枯れた長さの最大を返すこと"
    );

    // 0 以下の長さは枯れていないため数えない
    stats.record_underrun(0);
    stats.record_underrun(-1);
    assert_eq!(stats.snapshot(2_000_000).underruns, 2);

    // 購読のやり直しでは 0 に戻る
    stats.reset();
    let snapshot = stats.snapshot(2_000_000);
    assert_eq!(snapshot.underruns, 0);
    assert_eq!(snapshot.underrun_total_us, 0);
    assert_eq!(snapshot.underrun_max_us, 0);
}

#[test]
fn summarize_timings_uses_nearest_rank() {
    // 百分位は nearest-rank 法で求める (値を昇順に並べて ceil(p * n) 番目)。1 から 100 の
    // 100 個なら p50 は 50、p95 は 95、max は 100 になる
    let values: Vec<i64> = (1..=100).rev().collect();
    assert_eq!(
        summarize_timings(&values),
        Some(TimingSummary {
            p50: 50,
            p95: 95,
            max: 100,
        }),
        "nearest-rank 法で p50 / p95 / max を求めること"
    );
    // 1 個なら 3 つとも同じ値、空なら None
    assert_eq!(
        summarize_timings(&[7]),
        Some(TimingSummary {
            p50: 7,
            p95: 7,
            max: 7,
        })
    );
    assert_eq!(summarize_timings(&[]), None);

    // 渡した値を並べ替えない (呼び出し側が窓の値を持ち続けるため)
    let values = [3i64, 1, 2];
    let _ = summarize_timings(&values);
    assert_eq!(values, [3, 1, 2], "渡した値を並べ替えないこと");
}

#[test]
fn scheduler_play_is_recorded_without_conversion() {
    // スケジューラが残した値を、そのまま計器へ渡せること
    let mut scheduler = AudioPlayoutScheduler::new();
    let mut stats = AudioPlayoutTimingStats::new();
    // 目標 180 ms に間に合う音。到着は 100 ms、長さは 20 ms
    let decision = scheduler.schedule(AudioPlayoutInput {
        now_us: 0,
        arrival_us: 100_000,
        timestamp_us: 5_000,
        duration_us: 20_000,
        target_start_us: Some(180_000),
        enforce_target: true,
        delay_us: AUDIO_PLAYOUT_DELAY_US,
        arrival_delay_us: AUDIO_PLAYOUT_DELAY_US,
        presentation_delay_us: AUDIO_PLAYOUT_DELAY_US,
        stretch_min_us: None,
    });
    assert_eq!(
        decision.decision(),
        AudioPlayoutDecision::Play {
            start_at_us: 180_000,
            basis: AudioPlayoutBasis::Timestamp,
            compress_us: 0,
            expand_us: 0,
            total_gap_us: 0,
            unrecoverable_gap_us: 0,
            conceal_us: 0,
        },
        "目標に間に合う音は目標の時刻で鳴らすこと"
    );
    let play = scheduler
        .commit(
            decision,
            AudioPlayoutCommit {
                start_at_us: 180_000,
                played_us: 20_000,
            },
        )
        .expect("実際に積んだ値で確定できること");
    assert_eq!(play.arrival_us, 100_000);
    assert_eq!(play.target_start_us, Some(180_000));
    assert_eq!(play.start_at_us, 180_000);
    assert_eq!(
        play.played_us, 20_000,
        "詰めないときは音の長さがそのまま鳴ること"
    );
    assert_eq!(play.basis, AudioPlayoutBasis::Timestamp);

    stats.record_play(play);
    let snapshot = stats.snapshot(200_000);
    assert_eq!(
        snapshot.last_slack_us,
        Some(80_000),
        "予定に対する余裕 (予定 - 到着) を記録すること"
    );
    assert_eq!(
        snapshot.last_start_delay_us,
        Some(80_000),
        "到着から鳴り始めるまでの時間を記録すること"
    );
    assert_eq!(
        snapshot.last_lateness_us,
        Some(0),
        "予定どおりに鳴ったことを記録すること"
    );
    assert_eq!(snapshot.played_frames, 1);
    assert_eq!(snapshot.played_us, 20_000);
}

#[test]
fn confirmed_play_length_reaches_the_stats() {
    // 時間伸長と実際の出力終端を確定した後に record_play へ渡すと、伸びた分と実終端が
    // 計器 (played_us / pending / 停止時に切った長さ) に入る
    let mut scheduler = AudioPlayoutScheduler::new();
    let mut stats = AudioPlayoutTimingStats::new();
    // 到着基準の 1 つ目は 50 ms から 70 ms まで鳴る
    let first = scheduler.schedule(AudioPlayoutInput {
        now_us: 0,
        arrival_us: 0,
        timestamp_us: 0,
        duration_us: 20_000,
        target_start_us: None,
        enforce_target: false,
        delay_us: AUDIO_PLAYOUT_DELAY_US,
        arrival_delay_us: AUDIO_PLAYOUT_DELAY_US,
        presentation_delay_us: AUDIO_PLAYOUT_DELAY_US,
        stretch_min_us: None,
    });
    scheduler
        .commit(
            first,
            AudioPlayoutCommit {
                start_at_us: 50_000,
                played_us: 20_000,
            },
        )
        .expect("1 つ目の音を確定できること");
    assert!(matches!(
        first.decision(),
        AudioPlayoutDecision::Play { .. }
    ));
    // 200 ms 過ぎて届いた音は 210 ms から鳴り、深さを戻すために 20 ms の伸長を要求する
    let decision = scheduler.schedule(AudioPlayoutInput {
        now_us: 200_000,
        arrival_us: 200_000,
        timestamp_us: 40_000,
        duration_us: 20_000,
        target_start_us: None,
        enforce_target: false,
        delay_us: AUDIO_PLAYOUT_DELAY_US,
        arrival_delay_us: AUDIO_PLAYOUT_DELAY_US,
        presentation_delay_us: AUDIO_PLAYOUT_DELAY_US,
        stretch_min_us: None,
    });
    let AudioPlayoutDecision::Play { expand_us, .. } = decision.decision() else {
        panic!("鳴らす決定になること");
    };
    assert_eq!(expand_us, 20_000, "再同期後の伸長を要求すること");

    // 実際に 20 ms 伸ばし、バッファへ書いた実際の終端を 250 ms で確定する
    let play = scheduler
        .commit(
            decision,
            AudioPlayoutCommit {
                start_at_us: 210_000,
                played_us: 40_000,
            },
        )
        .expect("伸長した後の長さで確定できること");
    assert_eq!(
        play.played_us, 40_000,
        "20 ms の音に 20 ms の伸長を確定すると 40 ms になること"
    );

    // 確定した値のまま計器へ渡す
    stats.record_play(play);
    let snapshot = stats.snapshot(210_000);
    assert_eq!(snapshot.played_us, 40_000, "実際に鳴った長さを数えること");
    assert_eq!(
        stats.pending_end_us(210_000),
        Some(250_000),
        "予約の終端が確定した実終端になること"
    );
    assert_eq!(
        stats.pending_us(210_000),
        40_000,
        "鳴り終わっていない長さが伸びた分を含むこと"
    );

    // 220 ms で停止すると、残りの 30 ms が鳴らなかった長さとして数えられる
    stats.record_stopped(220_000);
    let snapshot = stats.snapshot(220_000);
    assert_eq!(
        snapshot.missed_us, 30_000,
        "停止時に切った長さは確定した実終端から求めること"
    );
    assert_eq!(
        snapshot
            .missed_by_reason
            .get(AudioMissReason::Stopped)
            .duration_us,
        30_000,
        "停止の理由別の累積にも実終端を使うこと"
    );
}
