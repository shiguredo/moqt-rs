//! 受信側の音声の追いつきのテスト
//!
//! 公開 API (`AudioReceiveCatchUp`) の契約を確認する。moqt-js の
//! `src/audioReceiveCatchUp.ts` が固定する規則 (床・上限・始める条件・クールダウン・
//! 観測が途切れたときの床・飛んだ音の数え方) を移植する。

use shiguredo_moqt::playout::catch_up::{
    AUDIO_RECEIVE_CATCH_UP_CONFIRM_MS, AUDIO_RECEIVE_CATCH_UP_COOLDOWN_MS,
    AUDIO_RECEIVE_CATCH_UP_FLOOR_WINDOW_MS, AUDIO_RECEIVE_CATCH_UP_GROWTH_MS,
    AUDIO_RECEIVE_CATCH_UP_JUMP_MS, AUDIO_RECEIVE_CATCH_UP_MIN_MS, AudioReceiveCatchUp,
    AudioReceiveCatchUpObservation,
};

/// 20 ms の音を 1 つ観測する。TIMESTAMP は観測の時刻と同じ値にする
fn observation(now_ms: i64, lag_ms: i64) -> AudioReceiveCatchUpObservation {
    observation_at(now_ms, lag_ms, now_ms * 1_000)
}

/// TIMESTAMP を指定して 20 ms の音を 1 つ観測する
fn observation_at(now_ms: i64, lag_ms: i64, timestamp_us: i64) -> AudioReceiveCatchUpObservation {
    AudioReceiveCatchUpObservation {
        now_ms,
        lag_ms,
        duration_ms: 20,
        timestamp_us,
    }
}

/// 遅れ 20 ms の健全な観測を 5 秒ぶん流す (床が 20 ms に落ち着く)
fn observe_healthy_run(catch_up: &mut AudioReceiveCatchUp) {
    for step in 0..25 {
        catch_up.observe(observation(step * 250, 20));
    }
}

#[test]
fn a_healthy_lag_does_not_start_the_catch_up() {
    let mut catch_up = AudioReceiveCatchUp::new();
    for step in 0..25 {
        let decision = catch_up.observe(observation(step * 250, 20));
        assert!(
            !decision.rebuild_decoder,
            "健全な状態では作り直しを求めないこと: step={step}"
        );
    }
    let stats = catch_up.snapshot();
    assert_eq!(stats.floor_ms, Some(20), "床は観測した最小値になること");
    assert_eq!(
        stats.limit_ms, AUDIO_RECEIVE_CATCH_UP_MIN_MS,
        "上限は絶対値の下限になること"
    );
    assert!(!stats.catching_up, "追いつきは始まらないこと");
    assert_eq!(stats.catch_up_starts, 0, "始めた回数は 0 のままであること");
}

#[test]
fn a_jump_over_the_floor_starts_immediately() {
    let mut catch_up = AudioReceiveCatchUp::new();
    observe_healthy_run(&mut catch_up);

    // 床 (20 ms) に JUMP (100 ms) を足した 120 ms を超える遅れ
    let decision = catch_up.observe(observation(6_250, 150));
    assert!(
        decision.rebuild_decoder,
        "床から {AUDIO_RECEIVE_CATCH_UP_JUMP_MS} ms を超えたら持続を待たずに求めること"
    );
    let stats = catch_up.snapshot();
    assert!(stats.catching_up, "追いつきの最中になること");
    assert_eq!(stats.catch_up_starts, 1, "始めた回数を数えること");
}

#[test]
fn a_sustained_lag_over_the_limit_starts_after_the_confirm() {
    let mut catch_up = AudioReceiveCatchUp::new();
    observe_healthy_run(&mut catch_up);

    // 上限 (60 ms) を超えるが、床 + JUMP (120 ms) は超えない遅れ
    let start_ms = 6_250;
    assert!(
        !catch_up.observe(observation(start_ms, 100)).rebuild_decoder,
        "1 回だけでは始めないこと"
    );
    assert!(
        !catch_up
            .observe(observation(start_ms + 50, 100))
            .rebuild_decoder,
        "持続が足りないうちは始めないこと"
    );
    assert!(
        catch_up
            .observe(observation(
                start_ms + AUDIO_RECEIVE_CATCH_UP_CONFIRM_MS,
                100
            ))
            .rebuild_decoder,
        "{AUDIO_RECEIVE_CATCH_UP_CONFIRM_MS} ms 続いたら始めること"
    );
}

#[test]
fn the_catch_up_ends_when_the_lag_returns_under_the_limit() {
    let mut catch_up = AudioReceiveCatchUp::new();
    observe_healthy_run(&mut catch_up);

    // 床 + JUMP を超える遅れで始める
    let mut now_ms = 6_250;
    assert!(
        catch_up.observe(observation(now_ms, 200)).rebuild_decoder,
        "床から 100 ms を超えたら始めること"
    );
    assert!(catch_up.snapshot().catching_up, "追いつきの最中になること");

    // 遅れが上限へ戻る
    now_ms += 100;
    let decision = catch_up.observe(observation(now_ms, 20));
    assert!(!decision.rebuild_decoder, "遅れが戻ったら求めないこと");
    assert!(
        !catch_up.snapshot().catching_up,
        "遅れが戻ったら追いつきを終えること"
    );
}

#[test]
fn the_cooldown_blocks_starting_by_lateness_alone() {
    let mut catch_up = AudioReceiveCatchUp::new();
    observe_healthy_run(&mut catch_up);

    let start_ms = 6_250;
    assert!(
        catch_up.observe(observation(start_ms, 150)).rebuild_decoder,
        "床から 100 ms を超えたら始めること"
    );
    // 直後に同じ遅れでも、クールダウンの間は始めない
    assert!(
        !catch_up
            .observe(observation(start_ms + 100, 150))
            .rebuild_decoder,
        "クールダウンの間は遅れだけを根拠に始めないこと"
    );
    assert!(
        catch_up
            .observe(observation(
                start_ms + AUDIO_RECEIVE_CATCH_UP_COOLDOWN_MS,
                150
            ))
            .rebuild_decoder,
        "クールダウンが過ぎたら始めること"
    );
}

#[test]
fn the_floor_is_held_across_a_gap_in_the_observations() {
    let mut catch_up = AudioReceiveCatchUp::new();
    observe_healthy_run(&mut catch_up);
    assert_eq!(
        catch_up.snapshot().floor_ms,
        Some(20),
        "床が決まっていること"
    );

    // 窓より長く途切れた後、途切れる前の床より大きい遅れが現れる
    let now_ms = 6_000 + AUDIO_RECEIVE_CATCH_UP_FLOOR_WINDOW_MS + 1_000;
    catch_up.observe(observation(now_ms, 150));
    assert_eq!(
        catch_up.snapshot().floor_ms,
        Some(20),
        "途切れる前の床を保つこと (現れた遅れを床にしない)"
    );
}

#[test]
fn a_larger_floor_raises_the_limit() {
    let mut catch_up = AudioReceiveCatchUp::new();
    // 遅れが 200 ms で安定している環境
    for step in 0..25 {
        catch_up.observe(observation(step * 250, 200));
    }
    let stats = catch_up.snapshot();
    assert_eq!(stats.floor_ms, Some(200), "床が環境の定常値になること");
    assert_eq!(
        stats.limit_ms,
        200 + AUDIO_RECEIVE_CATCH_UP_GROWTH_MS,
        "上限は床からの増加で決まること"
    );
    assert!(!stats.catching_up, "定常値では始めないこと");
    assert_eq!(stats.max_lag_ms, Some(200), "観測した最大の遅れを出すこと");
}

#[test]
fn skipped_audio_is_counted_only_during_the_catch_up() {
    let mut catch_up = AudioReceiveCatchUp::new();
    observe_healthy_run(&mut catch_up);

    // 追いつきの前の跳び (750 ms) は数えない。遅れは健全なままなので追いつきは始まらない
    catch_up.observe(observation_at(6_250, 20, 6_250_000));
    catch_up.observe(observation_at(6_350, 20, 7_000_000));
    assert_eq!(
        catch_up.snapshot().skipped_frames,
        0,
        "追いつきの前の跳びは数えないこと"
    );
    assert!(
        !catch_up.snapshot().catching_up,
        "まだ追いつきは始まらないこと"
    );

    // 床 + JUMP を超える遅れで追いつきを始める
    assert!(
        catch_up
            .observe(observation_at(6_450, 200, 7_020_000))
            .rebuild_decoder,
        "床から 100 ms を超えたら始めること"
    );

    // 追いつきの間の跳び (730 ms) は数える。1 音 (20 ms) を引いた 710 ms が飛んだ分である
    catch_up.observe(observation_at(6_550, 200, 7_750_000));
    let stats = catch_up.snapshot();
    assert_eq!(
        stats.last_skipped_ms,
        Some(710),
        "跳びから 1 音を引いた長さを数えること"
    );
    assert_eq!(
        stats.skipped_frames, 36,
        "跳びが 1 音の何個分かを四捨五入して 1 を引くこと"
    );
    assert_eq!(stats.skipped_ms, 710, "飛んだ長さの合計を数えること");
}

#[test]
fn reset_clears_the_observation_and_the_accumulation() {
    let mut catch_up = AudioReceiveCatchUp::new();
    observe_healthy_run(&mut catch_up);
    catch_up.observe(observation(6_250, 200));
    assert!(catch_up.snapshot().catching_up, "追いつきの最中になること");

    catch_up.reset();
    let stats = catch_up.snapshot();
    assert_eq!(stats.lag_ms, None, "遅れの観測が消えること");
    assert_eq!(stats.floor_ms, None, "床が消えること");
    assert_eq!(stats.max_lag_ms, None, "最大の遅れが消えること");
    assert!(!stats.catching_up, "追いつきが終わること");
    assert_eq!(stats.catch_up_starts, 0, "始めた回数が消えること");
    assert_eq!(stats.skipped_frames, 0, "飛んだ音の数が消えること");
    assert_eq!(
        stats.limit_ms, AUDIO_RECEIVE_CATCH_UP_MIN_MS,
        "上限が下限へ戻ること"
    );
}
