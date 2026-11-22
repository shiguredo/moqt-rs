//! 表示の遅れの上限を求める設定計算
//!
//! `timeline.rs` と `timeline/sync.rs` から使う private ヘルパを置く。
//! クレート外へは公開しない。

use super::*;

/// 表示の遅れの上限 (マイクロ秒)
///
/// `maxPresentationDelay` と映像キューの上限の小さい方にする。
pub(super) fn presentation_cap_us(config: &TimelineConfig, frame_interval_us: Option<i64>) -> i64 {
    let max_us = config.max_presentation_delay_ms.saturating_mul(1_000);
    max_us.min(playout_queue_cap_us(
        config.video_queue_limit,
        frame_interval_us,
    ))
}
