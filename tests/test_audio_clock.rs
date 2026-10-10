//! 音声の TIMESTAMP を配信側の壁時計へ合わせる規則のテスト
//!
//! 公開 API (`AudioTimestampClock`) の契約を確認する。マイクやキャプチャ機器から届く音声の
//! メディア時刻は壁時計と同じ時計ではない。刻み (サンプルの間隔) はそのままに、原点だけを
//! 「読み出した壁時計 - メディア時刻」の窓の最小値へ合わせる。一定のずれ・ドリフト・段差の
//! それぞれで、補正の動きを固定する。

use shiguredo_moqt::audio_clock::{
    AUDIO_TIMESTAMP_OFFSET_STEP_MIN_SAMPLES, AUDIO_TIMESTAMP_OFFSET_STEP_US,
    AUDIO_TIMESTAMP_OFFSET_STEP_WINDOW_US, AUDIO_TIMESTAMP_OFFSET_WINDOW_US, AudioTimestampClock,
};

/// 2026-09-25 付近の壁時計 (マイクロ秒)。`performance.timeOrigin + performance.now()` に相当する
const EPOCH_US: i64 = 1_790_263_445_000_000;

/// 音声のフレーム間隔 (マイクロ秒)。Opus の 20 ms フレーム
const FRAME_US: i64 = 20_000;

/// 音声の時計が壁時計から遅れている分 (マイクロ秒)。300 ms とする
const CLOCK_LAG_US: i64 = 300_000;

/// 撮ってから読むまでの、最も小さい遅れ (マイクロ秒)
const MIN_READ_DELAY_US: i64 = 5_000;

/// 1 フレームを読んだものとして記録する
///
/// フレームは 20 ms ごとに実時間どおりに届く。撮った時刻の壁時計は
/// 「フレームの番号 × 20 ms + 音声の時計の遅れ」であり、読んだ時刻はさらに読み出しの
/// 遅れだけ後になる。`audio_us` に渡す値を変えることで、音声の時計のずれ・ドリフト・
/// 段差を表せる。
fn record_frame(clock: &mut AudioTimestampClock, index: i64, audio_us: i64, read_delay_us: i64) {
    let capture_wall_clock_us = EPOCH_US + index * FRAME_US + CLOCK_LAG_US;
    clock.record(capture_wall_clock_us + read_delay_us, audio_us);
}

/// 一定のずれ (CLOCK_LAG_US) で index 番目のフレームを読む
fn record_steady_frame(clock: &mut AudioTimestampClock, index: i64, read_delay_us: i64) {
    record_frame(clock, index, index * FRAME_US, read_delay_us);
}

/// いま使っている補正 (マイクロ秒)。壁時計の原点を引いてテストの中で読みやすくする
fn applied_offset_us(clock: &AudioTimestampClock) -> i64 {
    clock
        .applied_us()
        .expect("観測を記録したので補正が決まっていること")
        - EPOCH_US
}

/// 観測が 1 つも無いうちは補正できない
///
/// 呼び出し側が従来の換算へ落とせるように `None` を返す。
#[test]
fn apply_returns_none_without_observation() {
    let clock = AudioTimestampClock::new();
    assert_eq!(clock.apply(0), None, "観測が無ければ換算できないこと");
    assert_eq!(clock.applied_us(), None, "観測が無ければ補正も無いこと");
    assert_eq!(clock.snapshot(), None, "観測が無ければ統計も無いこと");
}

/// 音声の時計が壁時計から一定にずれているとき、補正は最小の読み出しの遅れへ落ち着く
///
/// 送る TIMESTAMP は「撮った時刻 + 最小の遅れ」になり、間隔は音声の TIMESTAMP の間隔
/// そのままになる。
#[test]
fn apply_keeps_the_offset_and_the_interval_for_a_steady_skew() {
    let mut clock = AudioTimestampClock::new();
    for index in 0..300 {
        // 読み出しの遅れを 5 ms / 6 ms / 7 ms で揺らす
        record_steady_frame(&mut clock, index, MIN_READ_DELAY_US + (index % 3) * 1_000);
    }
    assert_eq!(
        applied_offset_us(&clock),
        CLOCK_LAG_US + MIN_READ_DELAY_US,
        "補正が最小の読み出しの遅れへ落ち着くこと"
    );

    // 20 ms の間隔がそのまま保たれる
    let first = clock.apply(0).expect("観測があるので換算できること");
    let second = clock.apply(FRAME_US).expect("観測があるので換算できること");
    assert_eq!(second - first, FRAME_US, "同じ補正の間は間隔が保たれること");
    // TIMESTAMP は「撮った時刻 + 最小の遅れ」である
    assert_eq!(
        first,
        EPOCH_US + CLOCK_LAG_US + MIN_READ_DELAY_US,
        "TIMESTAMP が撮った時刻 + 最小の遅れになること"
    );
}

/// 音声の時計がゆっくり遅れていくとき、窓の最小値は窓の分だけ古い床を指す
///
/// 補正は止まらずに動き続け、受信側から見た「TIMESTAMP が壁時計から遅れる量」は窓の幅で
/// 抑えられる。
#[test]
fn apply_follows_slow_drift_with_the_window_minimum() {
    let mut clock = AudioTimestampClock::new();
    // 音声の時計が 1 フレーム (20 ms) ごとに 1 ms 遅れる (50 ms / 秒 のドリフト)
    let drift_per_frame_us = 1_000;
    let frames = 400;
    for index in 0..frames {
        record_frame(
            &mut clock,
            index,
            index * FRAME_US - index * drift_per_frame_us,
            MIN_READ_DELAY_US,
        );
    }

    // 2 秒 (100 フレーム) の窓の分だけ古い床を使う
    let window_frames = AUDIO_TIMESTAMP_OFFSET_WINDOW_US / FRAME_US;
    let expected_us =
        CLOCK_LAG_US + (frames - 1 - window_frames) * drift_per_frame_us + MIN_READ_DELAY_US;
    let applied_us = applied_offset_us(&clock);
    assert!(
        (expected_us - 2 * drift_per_frame_us..=expected_us + 2 * drift_per_frame_us)
            .contains(&applied_us),
        "窓の最小値がドリフトへ追従すること expected={expected_us} applied={applied_us}"
    );
    // 最初の床に張り付かず、ドリフトへ動いている
    assert!(
        applied_us > CLOCK_LAG_US + MIN_READ_DELAY_US + 200_000,
        "補正が最初の床に張り付いていないこと applied={applied_us}"
    );
}

/// 音声の時計が 500 ms 遅れる段差では、2 秒の窓が埋まるのを待たずに直近の窓から取り直す
///
/// 待つと、その間だけ TIMESTAMP が実際より古くなり、受信側の再生の目標が過去へずれて音が
/// 捨てられる。
#[test]
fn apply_retakes_the_offset_on_a_backward_step() {
    let mut clock = AudioTimestampClock::new();
    for index in 0..100 {
        record_steady_frame(&mut clock, index, MIN_READ_DELAY_US);
    }
    assert_eq!(
        applied_offset_us(&clock),
        CLOCK_LAG_US + MIN_READ_DELAY_US,
        "段差の前は一定のずれへ合っていること"
    );

    // 音声の時計が 500 ms 後ろへ飛んだ (読み出しの壁時計は実時間どおりに進む)
    let step_us = 500_000;
    let step_window_frames = AUDIO_TIMESTAMP_OFFSET_STEP_WINDOW_US / FRAME_US;
    for index in 100..100 + step_window_frames {
        record_frame(
            &mut clock,
            index,
            index * FRAME_US - step_us,
            MIN_READ_DELAY_US,
        );
    }
    // 直近の 0.5 秒の窓にまだ古い観測が残っている間は取り直さない
    assert_eq!(
        applied_offset_us(&clock),
        CLOCK_LAG_US + MIN_READ_DELAY_US,
        "古い観測が直近の窓に残っている間は取り直さないこと"
    );

    // 直近の 0.5 秒が段差後の観測だけになると取り直す
    let index = 100 + step_window_frames;
    record_frame(
        &mut clock,
        index,
        index * FRAME_US - step_us,
        MIN_READ_DELAY_US,
    );
    assert_eq!(
        applied_offset_us(&clock),
        CLOCK_LAG_US + step_us + MIN_READ_DELAY_US,
        "段差後の床へ取り直すこと"
    );
}

/// 段差で取り直したあとは、古い床へ戻らない
///
/// 取り直しのときに古い観測を捨てるためである。残すと次の記録で 2 秒の窓の最小値が古い床に
/// 戻り、その間だけ TIMESTAMP が実際より古くなる。
#[test]
fn apply_keeps_the_retaken_offset_after_a_backward_step() {
    let mut clock = AudioTimestampClock::new();
    for index in 0..100 {
        record_steady_frame(&mut clock, index, MIN_READ_DELAY_US);
    }
    let step_us = 500_000;
    let step_window_frames = AUDIO_TIMESTAMP_OFFSET_STEP_WINDOW_US / FRAME_US;
    // 直近の 0.5 秒が段差後の観測だけになるまで読んで取り直す
    for index in 100..=100 + step_window_frames {
        record_frame(
            &mut clock,
            index,
            index * FRAME_US - step_us,
            MIN_READ_DELAY_US,
        );
    }
    assert_eq!(
        applied_offset_us(&clock),
        CLOCK_LAG_US + step_us + MIN_READ_DELAY_US,
        "段差後の床へ取り直すこと"
    );

    // 取り直した後の観測でも、古い床へ戻らず段差後の床のままである
    // (古い観測を捨てていないと、2 秒の窓の最小値へ戻ってしまう)
    for index in 101 + step_window_frames..=110 + step_window_frames {
        record_frame(
            &mut clock,
            index,
            index * FRAME_US - step_us,
            MIN_READ_DELAY_US,
        );
        assert_eq!(
            applied_offset_us(&clock),
            CLOCK_LAG_US + step_us + MIN_READ_DELAY_US,
            "取り直した後に古い床へ戻らないこと: index={index}"
        );
    }
}

/// 音声の時計が前に飛ぶ段差 (床が下がる) は、待たずに即座へ合わせる
///
/// TIMESTAMP が実際より新しくなると、受信側は音声を「まだ鳴らす時刻ではない」と扱い、
/// 再生が遅れる。
#[test]
fn apply_follows_a_forward_step_immediately() {
    let mut clock = AudioTimestampClock::new();
    for index in 0..100 {
        record_steady_frame(&mut clock, index, MIN_READ_DELAY_US);
    }
    assert_eq!(
        applied_offset_us(&clock),
        CLOCK_LAG_US + MIN_READ_DELAY_US,
        "段差の前は一定のずれへ合っていること"
    );

    // 音声の時計が 500 ms 前に飛んだ (ずれが 500 ms 減った)
    record_frame(&mut clock, 100, 100 * FRAME_US + 500_000, MIN_READ_DELAY_US);
    assert_eq!(
        applied_offset_us(&clock),
        CLOCK_LAG_US - 500_000 + MIN_READ_DELAY_US,
        "前に飛ぶ段差へ即座に合わせること"
    );
}

/// 読み出しの遅れが閾値未満ぶれても、段差とはみなさない
///
/// 段差として取り直すと TIMESTAMP が実際より新しくなり、受信側の基準が動く。
#[test]
fn apply_keeps_the_offset_when_the_read_delay_varies() {
    let mut clock = AudioTimestampClock::new();
    for index in 0..100 {
        record_steady_frame(&mut clock, index, MIN_READ_DELAY_US);
    }
    // 閾値の 50 ms 手前まで遅れて読めたフレームが 1 秒続く
    let blip_us = AUDIO_TIMESTAMP_OFFSET_STEP_US - 50_000;
    for index in 100..150 {
        record_steady_frame(&mut clock, index, MIN_READ_DELAY_US + blip_us);
    }
    assert_eq!(
        applied_offset_us(&clock),
        CLOCK_LAG_US + MIN_READ_DELAY_US,
        "閾値未満のぶれでは取り直さないこと"
    );
    // ぶれが戻っても補正は動かない
    for index in 150..200 {
        record_steady_frame(&mut clock, index, MIN_READ_DELAY_US);
    }
    assert_eq!(
        applied_offset_us(&clock),
        CLOCK_LAG_US + MIN_READ_DELAY_US,
        "ぶれが戻っても補正が動かないこと"
    );
}

/// 段差とみなすには、直近の窓に必要な数の観測が要る
///
/// 数個だけ遅れて読めた観測では取り直さない。
#[test]
fn apply_needs_enough_recent_samples_to_treat_a_step() {
    let mut clock = AudioTimestampClock::new();
    // 100 フレームを一定の遅れで読み、そのあと 0.5 秒ぶんの観測を飛ばす
    for index in 0..100 {
        record_steady_frame(&mut clock, index, MIN_READ_DELAY_US);
    }
    let step_us = 500_000;
    let first_stepped_index = 100 + AUDIO_TIMESTAMP_OFFSET_STEP_WINDOW_US / FRAME_US;
    // 必要な数より 1 個少ない観測では、段差とみなさない
    for index in first_stepped_index
        ..first_stepped_index + AUDIO_TIMESTAMP_OFFSET_STEP_MIN_SAMPLES as i64 - 1
    {
        record_frame(
            &mut clock,
            index,
            index * FRAME_US - step_us,
            MIN_READ_DELAY_US,
        );
    }
    assert_eq!(
        applied_offset_us(&clock),
        CLOCK_LAG_US + MIN_READ_DELAY_US,
        "直近の窓の観測が必要な数に足りない間は取り直さないこと"
    );

    // 必要な数に達すると取り直す
    let index = first_stepped_index + AUDIO_TIMESTAMP_OFFSET_STEP_MIN_SAMPLES as i64 - 1;
    record_frame(
        &mut clock,
        index,
        index * FRAME_US - step_us,
        MIN_READ_DELAY_US,
    );
    assert_eq!(
        applied_offset_us(&clock),
        CLOCK_LAG_US + step_us + MIN_READ_DELAY_US,
        "必要な数に達したら取り直すこと"
    );
}

/// 統計は「一定か、ドリフトか、段差か」を実機で読み分けるための値である
///
/// 現在値・最小・最大は生の観測 (補正を当てる前) を出し、傾きは 10 秒と 60 秒の窓で出す。
#[test]
fn snapshot_reports_current_min_max_and_slopes() {
    let mut clock = AudioTimestampClock::new();
    let drift_per_frame_us = 1_000;
    let frames = 1_000;
    for index in 0..frames {
        record_frame(
            &mut clock,
            index,
            index * FRAME_US - index * drift_per_frame_us,
            MIN_READ_DELAY_US,
        );
    }

    let stats = clock.snapshot().expect("観測したので統計があること");
    let current_us = CLOCK_LAG_US + (frames - 1) * drift_per_frame_us + MIN_READ_DELAY_US;
    assert_eq!(
        stats.current_us - EPOCH_US,
        current_us,
        "現在値が直近の観測であること"
    );
    assert_eq!(
        stats.min_us - EPOCH_US,
        CLOCK_LAG_US + MIN_READ_DELAY_US,
        "最小値が最初の観測であること"
    );
    assert_eq!(
        stats.max_us - EPOCH_US,
        current_us,
        "最大値が直近の観測であること"
    );
    // 50 ms / 秒 のドリフトを、10 秒と 60 秒のどちらの窓でも読み取れる。60 秒の窓は
    // 観測が窓を埋めていないため前半と後半の境目がフレームの間に落ち、値が少しずれる
    let slope_10s_us_per_second = stats
        .slope_10s_us_per_second
        .expect("10 秒の窓で傾きが求まること");
    let slope_60s_us_per_second = stats
        .slope_60s_us_per_second
        .expect("60 秒の窓で傾きが求まること");
    assert!(
        (49_000..=51_000).contains(&slope_10s_us_per_second),
        "10 秒の窓で 50 ms / 秒 のドリフトを読み取ること slope={slope_10s_us_per_second}"
    );
    assert!(
        (49_000..=51_000).contains(&slope_60s_us_per_second),
        "60 秒の窓で 50 ms / 秒 のドリフトを読み取ること slope={slope_60s_us_per_second}"
    );
    assert_eq!(
        stats.applied_us.map(|applied_us| applied_us - EPOCH_US),
        Some(applied_offset_us(&clock)),
        "統計の補正が使っている補正と一致すること"
    );
    assert_eq!(
        stats.samples, frames as u64,
        "観測した数が記録した数であること"
    );
}

/// 一定のずれでは傾きが 0 になる (ドリフトと読み分けられる)
#[test]
fn snapshot_reports_zero_slope_for_a_steady_skew() {
    let mut clock = AudioTimestampClock::new();
    for index in 0..600 {
        record_steady_frame(&mut clock, index, MIN_READ_DELAY_US + (index % 3) * 1_000);
    }
    let stats = clock.snapshot().expect("観測したので統計があること");
    assert_eq!(
        stats.slope_10s_us_per_second,
        Some(0),
        "一定のずれでは 10 秒の窓の傾きが 0 になること"
    );
    assert_eq!(
        stats.slope_60s_us_per_second,
        Some(0),
        "一定のずれでは 60 秒の窓の傾きが 0 になること"
    );
}

/// 観測が窓 (60 秒) より短いうちは、傾きを出すだけの幅が無い
#[test]
fn snapshot_reports_values_before_the_window_is_full() {
    let mut clock = AudioTimestampClock::new();
    record_steady_frame(&mut clock, 0, MIN_READ_DELAY_US);
    let stats = clock.snapshot().expect("観測したので統計があること");
    assert_eq!(
        stats.current_us - EPOCH_US,
        CLOCK_LAG_US + MIN_READ_DELAY_US,
        "観測が 1 つでも現在値と最小・最大を出すこと"
    );
    assert_eq!(
        stats.min_us, stats.max_us,
        "観測が 1 つなら最小と最大が同じであること"
    );
    assert_eq!(
        stats.slope_10s_us_per_second, None,
        "幅が無ければ 10 秒の窓の傾きは求まらないこと"
    );
    assert_eq!(
        stats.slope_60s_us_per_second, None,
        "幅が無ければ 60 秒の窓の傾きは求まらないこと"
    );
}

/// 統計の最小・最大・サンプル数は、補正の取り直しでは消えない
///
/// 生の観測の証拠を残し、実機で段差やドリフトの量を確かめられるようにする。
#[test]
fn snapshot_keeps_min_max_and_samples_across_a_step_retake() {
    let mut clock = AudioTimestampClock::new();
    for index in 0..100 {
        record_steady_frame(&mut clock, index, MIN_READ_DELAY_US);
    }
    let step_us = 500_000;
    // 直近の 0.5 秒が段差後の観測だけになるまで読む (取り直しが起きる)
    let last_index = 100 + AUDIO_TIMESTAMP_OFFSET_STEP_WINDOW_US / FRAME_US;
    for index in 100..=last_index {
        record_frame(
            &mut clock,
            index,
            index * FRAME_US - step_us,
            MIN_READ_DELAY_US,
        );
    }

    let stats = clock.snapshot().expect("観測したので統計があること");
    assert_eq!(
        stats.min_us - EPOCH_US,
        CLOCK_LAG_US + MIN_READ_DELAY_US,
        "取り直しでも観測した最小値が消えないこと"
    );
    assert_eq!(
        stats.max_us - EPOCH_US,
        CLOCK_LAG_US + step_us + MIN_READ_DELAY_US,
        "取り直しでも観測した最大値が消えないこと"
    );
    assert_eq!(
        stats.samples,
        (last_index + 1) as u64,
        "取り直しでも観測した数が消えないこと"
    );
    assert_eq!(
        stats.applied_us.map(|applied_us| applied_us - EPOCH_US),
        Some(CLOCK_LAG_US + step_us + MIN_READ_DELAY_US),
        "補正は段差後の床へ取り直していること"
    );
}

/// 読み出しの遅れが揺らいでも、傾きは窓の床 (最小の観測) から求める
///
/// 窓を前半と後半に分け、両端とも最小の観測を使うため、途中の 1 回だけ大きく遅れて読めた
/// 観測に傾きが引っ張られない。
#[test]
fn snapshot_slope_uses_the_minimum_of_the_window() {
    let mut clock = AudioTimestampClock::new();
    // 一定のずれで読み続け、真ん中で 1 回だけ 150 ms 遅れて読む
    for index in 0..600 {
        let read_delay_us = if index == 300 {
            MIN_READ_DELAY_US + 150_000
        } else {
            MIN_READ_DELAY_US
        };
        record_steady_frame(&mut clock, index, read_delay_us);
    }

    let stats = clock.snapshot().expect("観測したので統計があること");
    assert_eq!(
        stats.slope_10s_us_per_second,
        Some(0),
        "10 秒の窓の傾きが 1 回の遅れに引っ張られないこと"
    );
    assert_eq!(
        stats.slope_60s_us_per_second,
        Some(0),
        "60 秒の窓の傾きが 1 回の遅れに引っ張られないこと"
    );
}

/// 配信をやり直したら、前の配信の観測と補正を持ち越さない
#[test]
fn reset_clears_observations_and_offset() {
    let mut clock = AudioTimestampClock::new();
    record_steady_frame(&mut clock, 0, MIN_READ_DELAY_US);
    assert!(clock.snapshot().is_some(), "記録したので統計があること");

    clock.reset();
    assert_eq!(clock.snapshot(), None, "消したら統計が無いこと");
    assert_eq!(clock.apply(0), None, "消したら換算できないこと");
    assert_eq!(clock.applied_us(), None, "消したら補正が無いこと");
}
