//! 到着の遅れの分布から目標遅延を決めるテスト
//!
//! 公開 API (`AudioDelayManager`) の契約を確認する。

use shiguredo_moqt::playout::delay::{AUDIO_DELAY_START_MS, AudioDelayManager};

/// 一定の間隔と遅れで観測を続ける (ミリ秒単位で指定する)
fn observe_constant(manager: &mut AudioDelayManager, start_ms: i64, count: i64, delay_ms: i64) {
    for index in 0..count {
        let capture_us = (start_ms + index * 20) * 1_000;
        manager.observe(capture_us + delay_ms * 1_000, capture_us);
    }
}

#[test]
fn target_delay_starts_at_eighty_and_drops_to_the_minimum_without_jitter() {
    let mut manager = AudioDelayManager::new();
    // ヒストグラムへ入れる前は 80 ms
    assert_eq!(manager.target_delay_ms(), AUDIO_DELAY_START_MS);

    // 揺らぎの無い観測を続けると、500 ms を超えたところで区間の最大 (0 ms) が入り、
    // 分位点の最小値になる
    observe_constant(&mut manager, 1_000, 40, 100);
    assert_eq!(manager.target_delay_ms(), 20);
}

#[test]
fn only_the_interval_maximum_is_learned() {
    let mut manager = AudioDelayManager::new();
    // 基準になる遅れ 0 の観測
    manager.observe(1_000_000, 1_000_000);
    // 区間の中間で 100 ms 遅れて届く
    manager.observe(1_120_000, 1_020_000);
    // 以降は遅れ 0 で区間の終わりまで届く (区間の最大は 100 ms のまま)
    for index in 0..28i64 {
        let capture_us = (1_040 + index * 20) * 1_000;
        manager.observe(capture_us, capture_us);
    }
    // 区間を終える観測 (500 ms を超える) は 0 ms の遅れ
    manager.observe(1_600_000, 1_600_000);
    // 区間の最大 100 ms (バケット 5) が使われ、(5 + 1) * 20 = 120 ms になる。
    // 観測のたびに学習する実装なら区間内の 0 ms の観測が支配して 20 ms になる
    assert_eq!(manager.target_delay_ms(), 120);
}

#[test]
fn target_delay_falls_when_the_jitter_stops() {
    let mut manager = AudioDelayManager::new();
    // 遅れ 0 と 100 ms を交互に届ける (窓の最小は 0 のまま、区間の最大は 100 ms)
    for index in 0..80i64 {
        let capture_us = (1_000 + index * 20) * 1_000;
        let delay_ms = if index % 2 == 0 { 0 } else { 100 };
        manager.observe(capture_us + delay_ms * 1_000, capture_us);
    }
    assert_eq!(manager.target_delay_ms(), 120);

    // 揺らぎが消えると forget factor により目標遅延が下がる
    let last_capture_ms = 1_000 + 80 * 20;
    for index in 0..300i64 {
        let capture_us = (last_capture_ms + index * 600) * 1_000;
        manager.observe(capture_us, capture_us);
    }
    assert_eq!(manager.target_delay_ms(), 20);
}

#[test]
fn reset_returns_to_the_start_delay() {
    let mut manager = AudioDelayManager::new();
    observe_constant(&mut manager, 1_000, 40, 0);
    assert_eq!(manager.target_delay_ms(), 20);
    manager.reset();
    assert_eq!(manager.target_delay_ms(), AUDIO_DELAY_START_MS);
}
