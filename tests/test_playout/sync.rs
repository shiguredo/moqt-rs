//! A/V 同期の遅延制御のテスト
//!
//! 公開 API (`compute_relative_delay` / `StreamSynchronization`) の契約を確認する。

use shiguredo_moqt::playout::sync::{
    SYNC_MAX_CHANGE_MS, SYNC_MAX_DELTA_DELAY_MS, StreamSynchronization, SyncMeasurement,
    compute_relative_delay,
};

#[test]
fn relative_delay_is_the_receive_and_capture_difference() {
    let audio = SyncMeasurement {
        latest_receive_time_us: 1_000_000,
        latest_capture_time_us: 900_000,
    };
    let video = SyncMeasurement {
        latest_receive_time_us: 1_400_000,
        latest_capture_time_us: 900_000,
    };
    // 映像の方が 400 ms 遅く届いている
    assert_eq!(compute_relative_delay(audio, video), Some(400));
}

#[test]
fn relative_delay_at_the_limit_is_controlled_and_over_the_limit_is_not() {
    let audio = SyncMeasurement {
        latest_receive_time_us: 0,
        latest_capture_time_us: 0,
    };
    let at_limit = SyncMeasurement {
        latest_receive_time_us: SYNC_MAX_DELTA_DELAY_MS * 1_000,
        latest_capture_time_us: 0,
    };
    assert_eq!(
        compute_relative_delay(audio, at_limit),
        Some(SYNC_MAX_DELTA_DELAY_MS)
    );

    let over_limit = SyncMeasurement {
        latest_receive_time_us: (SYNC_MAX_DELTA_DELAY_MS + 1) * 1_000,
        latest_capture_time_us: 0,
    };
    assert_eq!(compute_relative_delay(audio, over_limit), None);
}

#[test]
fn deadband_does_not_change_delays() {
    let mut synchronization = StreamSynchronization::new();
    // ずれ 20 ms は不感帯 (30 ms) の中
    assert_eq!(synchronization.compute_delays(20, 100, 100), None);
}

#[test]
fn one_side_is_moved_per_call() {
    let mut synchronization = StreamSynchronization::new();
    // 映像が 200 ms 遅れている → 音声を遅らせ、映像は動かさない
    let delays = synchronization
        .compute_delays(200, 100, 100)
        .expect("不感帯を超えるため制御する");
    // ずれの推定は (0 * 3 + 200) / 4 = 50、動かす量はその半分の 25 ms
    assert_eq!(delays.audio_delay_ms, 25);
    assert_eq!(delays.video_delay_ms, 0);

    // 続けて呼ぶとずれの推定が戻っているため、動く量は再び 25 ms になる
    let delays = synchronization
        .compute_delays(200, 100, 100)
        .expect("不感帯を超えるため制御する");
    assert_eq!(delays.audio_delay_ms, 50);
    assert_eq!(delays.video_delay_ms, 0);
}

#[test]
fn one_call_moves_at_most_eighty_ms() {
    let mut synchronization = StreamSynchronization::new();
    // ずれの推定は 4000 / 4 = 1000 ms、動かす量は上限 80 ms で切られる
    let delays = synchronization
        .compute_delays(4_000, 100, 100)
        .expect("不感帯を超えるため制御する");
    assert_eq!(delays.audio_delay_ms, SYNC_MAX_CHANGE_MS);
    assert_eq!(delays.video_delay_ms, 0);
}

#[test]
fn delays_do_not_exceed_the_base_by_more_than_ten_seconds() {
    let mut synchronization = StreamSynchronization::new();
    let mut last = None;
    for _ in 0..200 {
        last = synchronization.compute_delays(4_000, 100, 100);
        let delays = last.expect("不感帯を超え続けるため制御する");
        assert!(delays.audio_delay_ms <= SYNC_MAX_DELTA_DELAY_MS);
    }
    assert_eq!(
        last.expect("不感帯を超え続けるため制御する").audio_delay_ms,
        SYNC_MAX_DELTA_DELAY_MS
    );
}

#[test]
fn trimming_the_delayed_side_is_reflected() {
    let mut synchronization = StreamSynchronization::new();
    // 映像が進んでいる → 映像を 80 ms 遅らせる
    let delays = synchronization
        .compute_delays(-1_000, 0, 0)
        .expect("不感帯を超えるため制御する");
    assert_eq!(delays.audio_delay_ms, 0);
    assert_eq!(delays.video_delay_ms, 80);

    // 映像が遅れている → 遅らせた 80 ms を削って基準 (0 ms) へ戻す
    let delays = synchronization
        .compute_delays(1_000, 0, 80)
        .expect("不感帯を超えるため制御する");
    assert_eq!(delays.audio_delay_ms, 0);
    assert_eq!(delays.video_delay_ms, 0);
}

#[test]
fn trimming_the_audio_delay_is_reflected() {
    let mut synchronization = StreamSynchronization::new();
    // 映像が遅れている → 音声を 80 ms 遅らせる
    let delays = synchronization
        .compute_delays(4_000, 0, 0)
        .expect("不感帯を超えるため制御する");
    assert_eq!(delays.audio_delay_ms, 80);
    assert_eq!(delays.video_delay_ms, 0);

    // 映像が進んでいる → 音声の 80 ms を削って基準 (0 ms) へ戻す
    let delays = synchronization
        .compute_delays(-4_000, 80, 0)
        .expect("不感帯を超えるため制御する");
    assert_eq!(delays.audio_delay_ms, 0);
    assert_eq!(delays.video_delay_ms, 0);
}

#[test]
fn reversal_after_the_limit_acts_immediately() {
    let mut synchronization = StreamSynchronization::new();
    // 上限 (10 秒) まで音声を遅らせる
    for _ in 0..200 {
        synchronization.compute_delays(4_000, 100, 100);
    }
    // 逆方向に動かすと、内部の状態も上限で切られているためすぐ下がる
    let delays = synchronization
        .compute_delays(-4_000, 10_000, 0)
        .expect("不感帯を超えるため制御する");
    assert!(delays.audio_delay_ms < 10_000);
}

#[test]
fn video_delay_does_not_go_below_the_base() {
    let mut synchronization = StreamSynchronization::new();
    synchronization.set_target_buffering_delay(200);
    // 映像が 200 ms 遅れている → 音声を遅らせる。映像は基準の 200 ms のまま
    let delays = synchronization
        .compute_delays(0, 200, 400)
        .expect("不感帯を超えるため制御する");
    assert_eq!(delays.video_delay_ms, 200);
    assert_eq!(delays.audio_delay_ms, 225);
}
