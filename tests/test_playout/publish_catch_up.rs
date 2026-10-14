//! 配信側の音声の追いつきのテスト
//!
//! 公開 API (`AudioPublishCatchUp`) の契約を確認する。moqt-js の
//! `src/audioPublishCatchUp.ts` が固定する規則 (上限・始める条件・やめる条件・
//! 読み出しの遅れ・投入の対応づけ・クールダウン) を移植する。

use shiguredo_moqt::playout::publish_catch_up::{
    AUDIO_PUBLISH_CATCH_UP_COOLDOWN_MS, AUDIO_PUBLISH_CATCH_UP_MIN_MS, AudioPublishCatchUp,
    AudioPublishCatchUpChunk, AudioPublishCatchUpFrame, AudioPublishCatchUpPolicy,
};

/// TIMESTAMP に足している補正 (マイクロ秒)
const APPLIED_OFFSET_US: i64 = 1_000_000;

/// 読み出しの遅れ `read_lag_ms` で 20 ms のフレームを読む
fn frame(now_ms: i64, timestamp_us: i64, read_lag_ms: i64) -> AudioPublishCatchUpFrame {
    AudioPublishCatchUpFrame {
        now_ms,
        timestamp_us,
        read_wall_clock_us: timestamp_us + APPLIED_OFFSET_US + read_lag_ms * 1_000,
        applied_offset_us: Some(APPLIED_OFFSET_US),
        duration_us: Some(20_000),
    }
}

/// 符号化の出力を返す (長さの分だけキューを消費する)
fn encode(catch_up: &mut AudioPublishCatchUp, timestamp_us: i64, duration_us: i64) {
    catch_up.record_encoded_chunk(AudioPublishCatchUpChunk {
        timestamp_us,
        duration_us: Some(duration_us),
    });
}

#[test]
fn a_frame_with_a_healthy_lag_is_encoded() {
    let mut catch_up = AudioPublishCatchUp::new();
    // 読み出しごとに符号化の出力が返る (キューに溜まらない)
    for index in 0..10i64 {
        let timestamp_us = index * 20_000;
        assert!(
            catch_up.evaluate(frame(index * 20, timestamp_us, 10)),
            "健全な遅れでは符号化すること: index={index}"
        );
        encode(&mut catch_up, timestamp_us, 20_000);
    }
    let stats = catch_up.snapshot();
    assert_eq!(stats.dropped_frames, 0, "捨てないこと");
    assert_eq!(stats.pending_ms, 0, "キューがはけていること");
    assert_eq!(stats.pending_frames, 0, "キューのフレーム数が 0 であること");
    assert!(!stats.catching_up, "追いつきは始まらないこと");
    assert_eq!(stats.read_lag_ms, 10, "読み出しの遅れを出すこと");
    assert_eq!(stats.lag_ms, Some(10), "遅れを出すこと");
}

#[test]
fn a_growing_queue_starts_dropping_and_resumes_at_one_packet() {
    let mut catch_up = AudioPublishCatchUp::new();
    // 出力が返らないまま 20 ms ごとに読み出す
    let mut encoded = 0;
    let mut dropped = 0;
    for index in 0..20i64 {
        if catch_up.evaluate(frame(index * 20, index * 20_000, 10)) {
            encoded += 1;
        } else {
            dropped += 1;
        }
    }
    assert!(encoded > 0, "最初は符号化すること");
    assert!(dropped > 0, "キューが伸びたら捨て始めること");
    let stats = catch_up.snapshot();
    assert!(stats.catching_up, "追いつきの最中になること");
    assert_eq!(stats.catch_up_starts, 1, "始めた回数を数えること");
    assert_eq!(stats.dropped_ms, dropped * 20, "捨てた長さを数えること");

    // キューが 1 パケット (20 ms) 以下まで減ったら再開する
    let pending_ms = stats.pending_ms;
    encode(&mut catch_up, 0, pending_ms * 1_000);
    assert_eq!(catch_up.snapshot().pending_ms, 0, "キューがはけること");
    assert!(
        catch_up.evaluate(frame(420, 420_000, 10)),
        "キューが 1 パケット以下まで減ったら再開すること"
    );
    assert!(!catch_up.snapshot().catching_up, "追いつきを終えること");
}

#[test]
fn a_frame_that_is_late_at_read_time_is_dropped_immediately() {
    let mut catch_up = AudioPublishCatchUp::new();
    // まず健全なフレームで床 (10 ms) を決める
    assert!(catch_up.evaluate(frame(0, 0, 10)));
    encode(&mut catch_up, 0, 20_000);
    assert_eq!(
        catch_up.snapshot().limit_ms,
        AUDIO_PUBLISH_CATCH_UP_MIN_MS,
        "上限は絶対値の下限になること"
    );

    // 読み出しの遅れが上限 (60 ms) を超えるフレームは、その場で捨てる
    assert!(
        !catch_up.evaluate(frame(20, 20_000, 100)),
        "読み出しが遅れたフレームはその場で捨てること"
    );
    let stats = catch_up.snapshot();
    assert_eq!(stats.dropped_frames, 1, "捨てた数を数えること");
    assert_eq!(stats.dropped_ms, 20, "捨てた長さを数えること");
    assert!(
        !stats.catching_up,
        "キューに溜まっていないため状態を持ち越さないこと"
    );
    assert_eq!(stats.pending_frames, 0, "キューに残さないこと");
    assert_eq!(stats.catch_up_starts, 0, "始めた回数を増やさないこと");
}

#[test]
fn repeated_timestamps_do_not_overwrite_the_records() {
    let mut catch_up = AudioPublishCatchUp::new();
    // 同じ TIMESTAMP のフレームが続く。記録の鍵に TIMESTAMP を使うと上書きされ、
    // 長さだけが足されて記録が残らなくなる
    for index in 0..3i64 {
        assert!(
            catch_up.evaluate(frame(index * 20, 0, 10)),
            "同じ TIMESTAMP でも符号化すること"
        );
    }
    let stats = catch_up.snapshot();
    assert_eq!(stats.pending_ms, 60, "長さを足し合わせること");
    assert_eq!(stats.pending_frames, 3, "記録の数を保つこと");

    // 出力が 3 つぶんを覆うとキューが空になる
    encode(&mut catch_up, 0, 60_000);
    let stats = catch_up.snapshot();
    assert_eq!(stats.pending_frames, 0, "記録が残らないこと");
    assert_eq!(stats.pending_ms, 0, "長さが残らないこと");
}

#[test]
fn the_catch_up_ends_when_the_records_are_gone() {
    let mut catch_up = AudioPublishCatchUp::new();
    for index in 0..20i64 {
        catch_up.evaluate(frame(index * 20, index * 20_000, 10));
    }
    assert!(catch_up.snapshot().catching_up, "捨てていること");

    // 記録より長い出力 (捨てた分を覆った) で記録が空になる。追いつきの間は投入しないため
    // 符号化の出力も返らず、長さの集計がずれたまま残ると解除できなくなる
    encode(&mut catch_up, 0, 1_000_000);
    assert_eq!(catch_up.snapshot().pending_frames, 0, "記録が空になること");
    assert!(
        catch_up.evaluate(frame(420, 420_000, 10)),
        "記録が空なら再開すること"
    );
    assert!(!catch_up.snapshot().catching_up, "追いつきを終えること");
}

#[test]
fn the_cooldown_blocks_starting_by_lateness_alone() {
    let mut catch_up = AudioPublishCatchUp::new();
    // キューを伸ばして捨て始めさせる
    for index in 0..20i64 {
        catch_up.evaluate(frame(index * 20, index * 20_000, 10));
    }
    assert!(catch_up.snapshot().catching_up, "捨てていること");

    // キューをはけさせて再開する
    let pending_ms = catch_up.snapshot().pending_ms;
    encode(&mut catch_up, 0, pending_ms * 1_000);
    let resumed_at_ms = 420;
    assert!(
        catch_up.evaluate(frame(resumed_at_ms, resumed_at_ms * 1_000, 10)),
        "キューがはけたら再開すること"
    );

    // キューが上限 (60 ms) を超えない範囲で、遅れが上限を超える状態を続ける。始める条件は
    // 満たすが、やめた直後のクールダウン (1 秒) の間は始めない
    let mut started_ms: Option<i64> = None;
    for step in 0..60i64 {
        let now_ms = resumed_at_ms + step * 20;
        let timestamp_us = now_ms * 1_000;
        if catch_up.evaluate(frame(now_ms, timestamp_us, 50)) {
            // キューを 40 ms に保つ (上限は超えないが、遅れは超える)
            if catch_up.snapshot().pending_ms > 40 {
                encode(&mut catch_up, timestamp_us, 20_000);
            }
        } else if started_ms.is_none() {
            started_ms = Some(now_ms);
        }
    }
    let started_ms = started_ms.expect("クールダウンが過ぎたら始めること");
    assert!(
        started_ms.saturating_sub(resumed_at_ms) >= AUDIO_PUBLISH_CATCH_UP_COOLDOWN_MS,
        "クールダウンが過ぎるまで始めないこと: 再開 {resumed_at_ms}, 開始 {started_ms}"
    );
}

#[test]
fn the_keep_policy_never_drops_a_frame() {
    let mut catch_up = AudioPublishCatchUp::with_policy(AudioPublishCatchUpPolicy::Keep);
    // 読み出しが大きく遅れても、キューが伸びても捨てない
    for index in 0..20i64 {
        assert!(
            catch_up.evaluate(frame(index * 20, index * 20_000, 500)),
            "`keep` では捨てないこと: index={index}"
        );
    }
    let stats = catch_up.snapshot();
    assert_eq!(stats.dropped_frames, 0, "捨てた数は 0 のままであること");
    assert!(!stats.catching_up, "追いつきは始まらないこと");
    assert!(stats.lag_ms.is_some(), "遅れの観測は続けること");
}

#[test]
fn reset_clears_the_records_and_the_accumulation() {
    let mut catch_up = AudioPublishCatchUp::new();
    for index in 0..20i64 {
        catch_up.evaluate(frame(index * 20, index * 20_000, 10));
    }
    assert!(catch_up.snapshot().catching_up, "捨てていること");

    catch_up.reset();
    let stats = catch_up.snapshot();
    assert_eq!(stats.pending_ms, 0, "キューが消えること");
    assert_eq!(stats.pending_frames, 0, "記録が消えること");
    assert_eq!(stats.dropped_frames, 0, "捨てた数が消えること");
    assert_eq!(stats.catch_up_starts, 0, "始めた回数が消えること");
    assert!(!stats.catching_up, "追いつきが終わること");
    assert_eq!(stats.lag_ms, None, "遅れの観測が消えること");
    assert_eq!(stats.floor_ms, None, "床が消えること");
}
