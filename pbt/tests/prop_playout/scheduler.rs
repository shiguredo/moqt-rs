//! 鳴らす時刻の決定のプロパティテスト
//!
//! 任意の入力列で、鳴らすと決めた音の時刻と詰める長さが契約の範囲に収まり、統計が
//! 実測と一致することを検証する。

use pbt::common::test_runner;
use shiguredo_moqt::playout::scheduler::{
    AUDIO_PLAYOUT_DELAY_US, AUDIO_PLAYOUT_MIN_LEAD_US, AudioPlayoutDecision, AudioPlayoutInput,
    AudioPlayoutScheduler,
};

/// 鳴らすと決めた音は今から下限以上先に鳴り、詰める長さは音の長さの半分以内。
/// 破棄の統計は捨てた回数と一致し、詰めた合計は実際に詰めた量と一致する
#[test]
fn play_decisions_are_bounded_and_stats_match() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut scheduler = AudioPlayoutScheduler::new();
        let mut now_us = noprop::sample_usize_in(ctx, 0..1_000) as i64 * 1_000;
        let mut expected_compressed_us = 0i64;
        for _ in 0..noprop::sample_usize_in(ctx, 1..16) {
            now_us += noprop::sample_usize_in(ctx, 0..100) as i64 * 1_000;
            let timestamp_us = noprop::sample_usize_in(ctx, 0..1_000_000) as i64;
            let duration_us = noprop::sample_usize_in(ctx, 1..50) as i64 * 1_000;
            let target_start_us = if noprop::sample_bool(ctx) {
                Some(now_us + noprop::sample_usize_in(ctx, 0..500) as i64 * 1_000)
            } else {
                None
            };
            let input = AudioPlayoutInput {
                now_us,
                timestamp_us,
                duration_us,
                target_start_us,
                enforce_target: noprop::sample_bool(ctx),
                delay_us: AUDIO_PLAYOUT_DELAY_US,
                presentation_delay_us: AUDIO_PLAYOUT_DELAY_US,
            };
            let drops_before = scheduler.drops();
            match scheduler.schedule(input) {
                AudioPlayoutDecision::Play {
                    start_at_us,
                    compress_us,
                } => {
                    assert!(
                        start_at_us >= now_us + AUDIO_PLAYOUT_MIN_LEAD_US,
                        "start_at_us={start_at_us} now_us={now_us}"
                    );
                    assert!(compress_us >= 0);
                    assert!(compress_us <= duration_us / 2);
                    assert_eq!(scheduler.drops(), drops_before);
                    // 要求した詰める量の半分を実際に詰めたものとして返す
                    let applied_us = compress_us / 2;
                    expected_compressed_us += applied_us;
                    scheduler.confirm_stretch(applied_us);
                }
                AudioPlayoutDecision::Drop => {
                    assert_eq!(scheduler.drops(), drops_before + 1);
                }
            }
            assert_eq!(scheduler.compressed_us(), expected_compressed_us);
        }
        Ok(())
    })?;
    Ok(())
}
