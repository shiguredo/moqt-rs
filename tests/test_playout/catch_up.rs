//! 受信側の音声の追いつきのテスト
//!
//! 公開 API (`AudioReceiveCatchUp`) の契約を確認する。moqt-js の
//! `src/playout/audioReceiveCatchUp.ts` が固定する規則 (床・上限・始める条件・クールダウン・
//! 観測が途切れたときの床・飛んだ音の数え方・入力の遅れを引いた「復号が足している遅れ」で
//! の判定) を移植する。

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
///
/// 入力の遅れは測れないものとする (`None`)。測れる場合の観測は
/// [`observation_with_input_lag`] で作る。
fn observation_at(now_ms: i64, lag_ms: i64, timestamp_us: i64) -> AudioReceiveCatchUpObservation {
    AudioReceiveCatchUpObservation {
        now_ms,
        lag_ms,
        input_lag_ms: None,
        duration_ms: 20,
        timestamp_us,
    }
}

/// 入力の遅れを指定して 20 ms の音を 1 つ観測する。TIMESTAMP は観測の時刻と同じ値にする
fn observation_with_input_lag(
    now_ms: i64,
    lag_ms: i64,
    input_lag_ms: Option<i64>,
) -> AudioReceiveCatchUpObservation {
    AudioReceiveCatchUpObservation {
        now_ms,
        lag_ms,
        input_lag_ms,
        duration_ms: 20,
        timestamp_us: now_ms * 1_000,
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
    assert_eq!(stats.lag_ms, Some(20), "観測した出力の遅れを残すこと");
    assert_eq!(
        stats.input_lag_ms, None,
        "入力の遅れを渡していないので測れないままであること"
    );
    assert_eq!(
        stats.decode_lag_ms,
        Some(20),
        "入力の遅れが測れないときは出力の遅れそのものであること"
    );
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
    assert_eq!(stats.input_lag_ms, None, "入力の遅れの観測が消えること");
    assert_eq!(
        stats.decode_lag_ms, None,
        "「復号が足している遅れ」の観測が消えること"
    );
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

/// 入力の遅れを引いた判定で使う、健全なときの入力の遅れ (ミリ秒)
const HEALTHY_INPUT_LAG_MS: i64 = 25;

/// 健全なときの、到着から復号の出力までの分 (ミリ秒)。出力の遅れは 30 ms になる
const HEALTHY_DECODE_LAG_MS: i64 = 5;

/// 受信側がまとめて遅れている間の入力の遅れ (ミリ秒)。実測の段差 (250 ms 前後) に合わせる
const BLOCKED_INPUT_LAG_MS: i64 = 250;

/// 復号器に溜まる分 (ミリ秒)。出力の遅れ 250 ms − 入力の遅れ 25 ms。実時間でしか出てこない
const STUCK_DECODE_LAG_MS: i64 = 225;

/// 受信側がまとめて遅れ始める時刻 (ミリ秒)。8 秒
const BLOCK_START_MS: i64 = 8_000;

/// 受信側の遅れが直る時刻 (ミリ秒)。20 秒。復号器に溜まった分は残る
const RECOVERY_MS: i64 = 20_000;

/// 受信側が溜めたまま戻らない状態の観測の長さ (ミリ秒)。60 秒
const STUCK_SCENARIO_MS: i64 = 60_000;

/// 1 音の長さ (ミリ秒)
const FRAME_INTERVAL_MS: i64 = 20;

/// 受信側が溜めたまま戻らない状態を駆動した結果
struct StuckDecoderRun {
    /// 作り直しを求めた時刻 (ミリ秒)
    start_times_ms: Vec<i64>,
    /// 受信側の遅れが直った後 (20 秒以降) に作り直しを求めた時刻 (ミリ秒)
    after_recovery_start_times_ms: Vec<i64>,
    /// 受信側の遅れが直った後に観測した出力の遅れの最小値 (ミリ秒)
    min_lag_after_recovery_ms: i64,
    /// 最後に観測した出力の遅れ (ミリ秒)
    final_lag_ms: i64,
    /// 最後の床 (ミリ秒)
    final_floor_ms: Option<i64>,
    /// 最後の「復号が足している遅れ」 (ミリ秒)
    final_decode_lag_ms: Option<i64>,
}

/// 受信側が溜めたまま戻らない状態を、観測を組み立てて作る
///
/// - 0〜8 秒: 健全。入力の遅れ 25 ms、復号の出力の遅れ 30 ms
/// - 8〜20 秒: 受信側がまとめて遅れる。Object の到着も復号の出力も 250 ms 前後遅れるが、
///   復号器の中には溜まっていない (入力の遅れも同じだけ上がる)
/// - 20 秒以降: 受信側は直り、入力の遅れは 25 ms へ戻る。ところが、遅れていた間に届いた
///   Object がまとめて復号へ入り、復号器の中に 225 ms が溜まったままになる (実時間でしか
///   出てこない)。作り直すと捨てられ、出力の遅れは 30 ms へ戻る
///
/// `use_input_lag` が `false` のときは入力の遅れを渡さず、導入前の挙動 (出力の遅れだけを
/// 見る) を駆動する。
fn observe_stuck_decoder(
    catch_up: &mut AudioReceiveCatchUp,
    use_input_lag: bool,
) -> StuckDecoderRun {
    let mut start_times_ms = Vec::new();
    let mut stuck_decode_lag_ms = 0;
    let mut was_blocked = false;
    let mut min_lag_after_recovery_ms = i64::MAX;
    let mut final_lag_ms = 0;
    for index in 0..STUCK_SCENARIO_MS / FRAME_INTERVAL_MS {
        let now_ms = index * FRAME_INTERVAL_MS;
        let blocked = (BLOCK_START_MS..RECOVERY_MS).contains(&now_ms);
        let input_lag_ms = if blocked {
            BLOCKED_INPUT_LAG_MS
        } else {
            HEALTHY_INPUT_LAG_MS
        };
        if was_blocked && !blocked {
            // 遅れていた分がまとめて復号へ入り、復号器の中に溜まる
            stuck_decode_lag_ms = STUCK_DECODE_LAG_MS;
        }
        was_blocked = blocked;
        let lag_ms = input_lag_ms + HEALTHY_DECODE_LAG_MS + stuck_decode_lag_ms;
        final_lag_ms = lag_ms;
        if now_ms >= RECOVERY_MS {
            min_lag_after_recovery_ms = min_lag_after_recovery_ms.min(lag_ms);
        }
        let decision = catch_up.observe(observation_with_input_lag(
            now_ms,
            lag_ms,
            if use_input_lag {
                Some(input_lag_ms)
            } else {
                None
            },
        ));
        if decision.rebuild_decoder {
            start_times_ms.push(now_ms);
            // 復号器を作り直した。溜まっていた分は捨てられる
            stuck_decode_lag_ms = 0;
        }
    }
    let stats = catch_up.snapshot();
    StuckDecoderRun {
        after_recovery_start_times_ms: start_times_ms
            .iter()
            .copied()
            .filter(|at_ms| *at_ms >= RECOVERY_MS)
            .collect(),
        start_times_ms,
        min_lag_after_recovery_ms: if min_lag_after_recovery_ms == i64::MAX {
            0
        } else {
            min_lag_after_recovery_ms
        },
        final_lag_ms,
        final_floor_ms: stats.floor_ms,
        final_decode_lag_ms: stats.decode_lag_ms,
    }
}

/// 受信側が溜めたまま戻らない状態では、入力の遅れが健全なままなので、直った時点で作り直しを
/// 求める。床は「復号が足している遅れ」の最小値であるため、出力の遅れが 250 ms のまま 5 秒
/// 続いても上がらない。
///
/// 受信側が遅れている間 (8〜20 秒) は、入力の遅れも同じだけ上がるため作り直しを求めない
/// (経路が遅いだけの状態で、作り直しても戻らない)。
#[test]
fn a_stuck_decoder_is_rebuilt_when_the_receiver_recovers() {
    let mut catch_up = AudioReceiveCatchUp::new();
    let run = observe_stuck_decoder(&mut catch_up, true);

    assert!(
        run.start_times_ms.iter().all(|at_ms| *at_ms >= RECOVERY_MS),
        "経路が遅いだけの状態では作り直さないこと: {:?}",
        run.start_times_ms
    );
    assert_eq!(
        run.after_recovery_start_times_ms,
        vec![RECOVERY_MS],
        "受信側が直った時点で作り直すこと"
    );
    assert_eq!(
        run.min_lag_after_recovery_ms,
        HEALTHY_INPUT_LAG_MS + HEALTHY_DECODE_LAG_MS,
        "作り直しが効いて、出力の遅れが健全な値へ戻ること"
    );
    assert_eq!(
        run.final_lag_ms,
        HEALTHY_INPUT_LAG_MS + HEALTHY_DECODE_LAG_MS,
        "最後まで健全な値のままであること"
    );
    assert_eq!(
        run.final_floor_ms,
        Some(HEALTHY_DECODE_LAG_MS),
        "床は「復号が足している遅れ」の最小値であり、出力の遅れへ追いつかないこと"
    );
    assert_eq!(
        run.final_decode_lag_ms,
        Some(HEALTHY_DECODE_LAG_MS),
        "最後の「復号が足している遅れ」が健全な値であること"
    );
}

/// 入力の遅れを渡さない (導入前の挙動) と同じ入力では、出力の遅れが 250 ms のまま 5 秒続いた
/// 時点で床が 250 ms へ追いつき、それ以降は作り直しを求めなくなる。20 秒に受信側が直っても、
/// 復号器に溜まった音は実時間でしか出てこないため、40 秒間 250 ms のまま固定される。
#[test]
fn without_the_input_lag_the_floor_catches_up_to_the_output_lag() {
    let mut catch_up = AudioReceiveCatchUp::new();
    let run = observe_stuck_decoder(&mut catch_up, false);

    assert!(
        run.start_times_ms.len() > 1,
        "段差の直後は作り直しを求めること: {:?}",
        run.start_times_ms
    );
    assert!(
        run.after_recovery_start_times_ms.is_empty(),
        "床が上がった後は作り直しを求めないこと: {:?}",
        run.after_recovery_start_times_ms
    );
    assert_eq!(
        run.final_floor_ms,
        Some(BLOCKED_INPUT_LAG_MS + HEALTHY_DECODE_LAG_MS),
        "床が出力の遅れへ追いつくこと"
    );
    assert_eq!(
        run.final_lag_ms,
        BLOCKED_INPUT_LAG_MS + HEALTHY_DECODE_LAG_MS,
        "出力の遅れが 250 ms のまま固定されること"
    );
}

/// 経路が恒久的に悪化した状態 (入力の遅れも出力の遅れも同じだけ上がる) では、作り直しを
/// 求めない。復号器の中に溜まっていないため、作り直しても遅れは戻らず、音を飛ばすだけに
/// なる。入力の遅れを渡さない場合は、同じ入力でも段差として作り直しを求める (導入前の挙動)。
#[test]
fn a_permanently_worse_path_does_not_start_the_catch_up() {
    let frames = STUCK_SCENARIO_MS / FRAME_INTERVAL_MS;
    let mut catch_up = AudioReceiveCatchUp::new();
    let mut starts = Vec::new();
    for index in 0..frames {
        let now_ms = index * FRAME_INTERVAL_MS;
        // 5 音の健全な状態の後に、経路が恒久的に 250 ms 遅くなる。Object の到着も復号の
        // 出力も同じだけ遅れる (復号器の中には溜まっていない)
        let (lag_ms, input_lag_ms) = if index < 5 {
            (
                HEALTHY_INPUT_LAG_MS + HEALTHY_DECODE_LAG_MS,
                HEALTHY_INPUT_LAG_MS,
            )
        } else {
            (
                BLOCKED_INPUT_LAG_MS + HEALTHY_DECODE_LAG_MS,
                BLOCKED_INPUT_LAG_MS,
            )
        };
        if catch_up
            .observe(observation_with_input_lag(
                now_ms,
                lag_ms,
                Some(input_lag_ms),
            ))
            .rebuild_decoder
        {
            starts.push(now_ms);
        }
    }
    assert!(
        starts.is_empty(),
        "入力の遅れも同じだけ上がるため求めないこと: {starts:?}"
    );
    let stats = catch_up.snapshot();
    assert_eq!(
        stats.lag_ms,
        Some(BLOCKED_INPUT_LAG_MS + HEALTHY_DECODE_LAG_MS),
        "出力の遅れは経路の悪化を映すこと"
    );
    assert_eq!(
        stats.decode_lag_ms,
        Some(HEALTHY_DECODE_LAG_MS),
        "復号が足している遅れは健全なままであること"
    );
    assert_eq!(
        stats.floor_ms,
        Some(HEALTHY_DECODE_LAG_MS),
        "床は「復号が足している遅れ」の最小値であること"
    );

    // 入力の遅れを渡さない場合は、同じ入力でも段差として作り直しを求める (導入前の挙動)。
    // 床が出力の遅れへ追いつくまで、クールダウンごとに繰り返す
    let mut legacy = AudioReceiveCatchUp::new();
    let mut legacy_starts = Vec::new();
    for index in 0..frames {
        let now_ms = index * FRAME_INTERVAL_MS;
        let lag_ms = if index < 5 {
            HEALTHY_INPUT_LAG_MS + HEALTHY_DECODE_LAG_MS
        } else {
            BLOCKED_INPUT_LAG_MS + HEALTHY_DECODE_LAG_MS
        };
        if legacy.observe(observation(now_ms, lag_ms)).rebuild_decoder {
            legacy_starts.push(now_ms);
        }
    }
    assert!(
        legacy_starts.len() > 1,
        "導入前は段差で作り直しを繰り返すこと: {legacy_starts:?}"
    );
}

/// 入力の遅れが測れないとき (TIMESTAMP が無い、Timescale がある) は、出力の遅れをそのまま
/// 使う。入力の遅れを導入する前と同じ挙動であり、測れないことを理由に作り直しを止めない。
#[test]
fn an_unmeasurable_input_lag_uses_the_output_lag() {
    let mut catch_up = AudioReceiveCatchUp::new();
    let mut start_times_ms = Vec::new();
    for index in 0..25 {
        let now_ms = index * 20;
        // 健全な 20 ms が 5 音続いた後、出力の遅れが 200 ms へ段差で上がる
        let lag_ms = if index < 5 { 20 } else { 200 };
        if catch_up
            .observe(observation_with_input_lag(now_ms, lag_ms, None))
            .rebuild_decoder
        {
            start_times_ms.push(now_ms);
        }
    }
    assert_eq!(
        start_times_ms,
        vec![100],
        "測れないときも、出力の遅れで判定すること"
    );
    let stats = catch_up.snapshot();
    assert_eq!(stats.input_lag_ms, None, "測れなかったことが残ること");
    assert_eq!(stats.lag_ms, Some(200), "出力の遅れをそのまま残すこと");
    assert_eq!(
        stats.decode_lag_ms,
        Some(200),
        "入力の遅れが無いときは出力の遅れそのものであること"
    );
}

/// 統計の意味: 出力の遅れ (`lag_ms`) と最大の遅れ (`max_lag_ms`) は生の値を残し、床
/// (`floor_ms`) と上限は「復号が足している遅れ」(`decode_lag_ms`) で決める。入力の遅れが
/// 出力の遅れを上回るとき (2 つの遅れを別の時点で測った丸め) は 0 に寄せる。
#[test]
fn the_stats_keep_the_output_lag_and_judge_by_the_decode_lag() {
    let mut catch_up = AudioReceiveCatchUp::new();
    // 入力の遅れ 180 ms、出力の遅れ 200 ms が 5 秒続く (復号が足している遅れは 20 ms)
    for step in 0..25 {
        catch_up.observe(observation_with_input_lag(step * 250, 200, Some(180)));
    }
    let stats = catch_up.snapshot();
    assert_eq!(stats.lag_ms, Some(200), "生の出力の遅れを残すこと");
    assert_eq!(stats.input_lag_ms, Some(180), "入力の遅れを残すこと");
    assert_eq!(stats.decode_lag_ms, Some(20), "引いた値を残すこと");
    assert_eq!(
        stats.floor_ms,
        Some(20),
        "床は「復号が足している遅れ」の最小値であること"
    );
    assert_eq!(
        stats.limit_ms, AUDIO_RECEIVE_CATCH_UP_MIN_MS,
        "上限は絶対値の下限になること"
    );
    assert_eq!(
        stats.max_lag_ms,
        Some(200),
        "最大の遅れは生の出力の遅れであること"
    );
    assert!(!stats.catching_up, "引いた値が健全なら始めないこと");

    // 入力の遅れが出力の遅れを上回るときは、負にせず 0 に寄せる
    let mut rounding = AudioReceiveCatchUp::new();
    rounding.observe(observation_with_input_lag(0, 10, Some(12)));
    let stats = rounding.snapshot();
    assert_eq!(
        stats.decode_lag_ms,
        Some(0),
        "負の「復号が足している遅れ」は 0 にすること"
    );
    assert_eq!(
        stats.input_lag_ms,
        Some(12),
        "入力の遅れは渡した値をそのまま残すこと"
    );

    // 入力の遅れが測れないときは、負の出力の遅れ (配信側の時計が先へ進んでいる) もそのまま残す
    let mut future = AudioReceiveCatchUp::new();
    future.observe(observation_with_input_lag(0, -19, None));
    assert_eq!(
        future.snapshot().decode_lag_ms,
        Some(-19),
        "測れないときは出力の遅れをそのまま使うこと"
    );
}
