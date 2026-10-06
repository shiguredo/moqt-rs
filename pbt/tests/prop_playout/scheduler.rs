//! 鳴らす時刻の決定のプロパティテスト
//!
//! 任意の入力列で、鳴らすと決めた音の時刻と詰める長さと補間する隙間が契約の範囲に収まり、
//! 統計が実測と一致することを検証する。

use pbt::common::test_runner;
use shiguredo_moqt::playout::scheduler::{
    AUDIO_PLAYOUT_DELAY_US, AUDIO_PLAYOUT_MAX_CONCEAL_US, AUDIO_PLAYOUT_MIN_CONCEAL_US,
    AUDIO_PLAYOUT_MIN_LEAD_US, AudioPlayoutDecision, AudioPlayoutInput, AudioPlayoutScheduler,
};

/// 鳴らすと決めた音は今から下限以上先に鳴り、詰める長さは音の長さの半分以内。
/// 補間する隙間は下限より長く上限以内で、補間しないときは開始も 0。
/// 破棄の統計は捨てた回数と一致し、詰めた合計と補間した合計は実際に適用した量と一致する
#[test]
fn play_decisions_are_bounded_and_stats_match() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut scheduler = AudioPlayoutScheduler::new();
        let mut now_us = noprop::sample_usize_in(ctx, 0..1_000) as i64 * 1_000;
        let mut expected_compressed_us = 0i64;
        let mut expected_concealed_us = 0i64;
        let mut expected_concealments = 0u64;
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
                    gap_start_us,
                    gap_us,
                } => {
                    assert!(
                        start_at_us >= now_us + AUDIO_PLAYOUT_MIN_LEAD_US,
                        "start_at_us={start_at_us} now_us={now_us}"
                    );
                    assert!(compress_us >= 0);
                    assert!(compress_us <= duration_us / 2);
                    // 補間する隙間は、下限を超えて上限以内である
                    assert!(
                        gap_us == 0 || gap_us > AUDIO_PLAYOUT_MIN_CONCEAL_US,
                        "gap_us={gap_us}"
                    );
                    assert!(gap_us <= AUDIO_PLAYOUT_MAX_CONCEAL_US);
                    // 補間しないときは開始も返さない
                    assert!(gap_us != 0 || gap_start_us == 0);
                    // 隙間は今回の開始より前で終わる
                    assert!(gap_start_us.saturating_add(gap_us) <= start_at_us);
                    assert_eq!(scheduler.drops(), drops_before);
                    // 要求した詰める量と隙間の半分を実際に適用したものとして返す
                    let applied_us = compress_us / 2;
                    expected_compressed_us += applied_us;
                    scheduler.confirm_stretch(applied_us);
                    let concealed_us = gap_us / 2;
                    if concealed_us > 0 {
                        expected_concealments += 1;
                        expected_concealed_us += concealed_us;
                    }
                    scheduler.confirm_concealment(concealed_us);
                }
                AudioPlayoutDecision::Drop => {
                    assert_eq!(scheduler.drops(), drops_before + 1);
                }
            }
            assert_eq!(scheduler.compressed_us(), expected_compressed_us);
            assert_eq!(scheduler.concealed_us(), expected_concealed_us);
            assert_eq!(scheduler.concealments(), expected_concealments);
        }
        Ok(())
    })?;
    Ok(())
}
