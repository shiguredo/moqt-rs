//! 音声の TIMESTAMP を配信側の壁時計へ合わせる規則のプロパティテスト
//!
//! 任意の観測列に対して、補正が規則どおりの窓の最小値へ動くこと、段差の取り直しが
//! 「直近の窓の観測数」と「適用中の補正からの閾値」の両方を満たすときだけ起きること、
//! 補正が観測した最小値と最大値の範囲から出ないことを検証する。あわせて、`apply` が補正を
//! 足すだけで同じ補正の間は間隔が保たれ、負にならないこと、`reset` で観測が無い状態へ
//! 戻ることも検証する。分岐ごとに到達を数え、検証が空振りしていないことも確かめる。

use std::cell::Cell;

use pbt::common::test_runner;
use shiguredo_moqt::audio_clock::{
    AUDIO_TIMESTAMP_OFFSET_STEP_MIN_SAMPLES, AUDIO_TIMESTAMP_OFFSET_STEP_US,
    AUDIO_TIMESTAMP_OFFSET_STEP_WINDOW_US, AUDIO_TIMESTAMP_OFFSET_WINDOW_US,
    AUDIO_TIMESTAMP_SLOPE_LONG_WINDOW_US, AudioTimestampClock,
};

/// 観測の間隔の重み (音声のフレーム間隔 / 少し空く / 直近の窓が空くほど空く)
///
/// 音声は 20 ms ごとに届くが、取りこぼしや処理の遅れで間隔が空く。直近の 0.5 秒より長く
/// 空くと、段差を見る窓に観測が入らない状態を作れる。
const INTERVAL_WEIGHTS: [u32; 3] = [8, 3, 3];

/// ずれの動かし方の重み (一定 / ゆっくりしたドリフト / 閾値未満のぶれ / 段差)
const OFFSET_WEIGHTS: [u32; 4] = [4, 4, 3, 4];

/// テスト側のモデルが選んだ、1 回の記録での補正の更新
#[derive(Debug, Clone, Copy)]
struct ModelOutcome {
    /// 規則どおりに更新した後の補正 (マイクロ秒)
    applied_us: i64,
    /// 段差とみなして直近の窓の最小値へ取り直した
    step_retake: bool,
    /// 段差とみなすには直近の窓の観測が足りなかった
    step_needs_samples: bool,
    /// 段差とみなす閾値に届かなかった
    below_threshold: bool,
}

/// テスト側のモデル
///
/// 実装 (走査と `VecDeque`) ではなく、`AudioTimestampClock` の doc コメントに書いた規則を
/// そのまま写す。観測は読み出した壁時計の昇順に入る。
struct ObservationModel {
    /// 窓の計算に使う観測 (読み出した壁時計, オフセット)。捨てる規則も実装と同じにする
    observations: Vec<(i64, i64)>,
    /// いまの補正 (マイクロ秒)
    applied_us: Option<i64>,
    /// 観測した最小値 (マイクロ秒)。補正の取り直しでは消さない
    min_offset_us: Option<i64>,
    /// 観測した最大値 (マイクロ秒)。補正の取り直しでは消さない
    max_offset_us: Option<i64>,
}

impl ObservationModel {
    /// まだ 1 つも観測していない状態で作る
    fn new() -> Self {
        Self {
            observations: Vec::new(),
            applied_us: None,
            min_offset_us: None,
            max_offset_us: None,
        }
    }

    /// 観測を記録し、規則どおりに補正を更新する
    fn record(&mut self, read_wall_clock_us: i64, audio_timestamp_us: i64) -> ModelOutcome {
        let offset_us = read_wall_clock_us.saturating_sub(audio_timestamp_us);
        self.observations.push((read_wall_clock_us, offset_us));
        self.min_offset_us = Some(
            self.min_offset_us
                .map_or(offset_us, |min_offset_us| min_offset_us.min(offset_us)),
        );
        self.max_offset_us = Some(
            self.max_offset_us
                .map_or(offset_us, |max_offset_us| max_offset_us.max(offset_us)),
        );

        // 統計と補正に使う窓の外の観測を捨てる
        let observation_window_us =
            AUDIO_TIMESTAMP_SLOPE_LONG_WINDOW_US.max(AUDIO_TIMESTAMP_OFFSET_STEP_WINDOW_US);
        self.prune(read_wall_clock_us.saturating_sub(observation_window_us));

        let (window_min_us, _) = self
            .window(read_wall_clock_us, AUDIO_TIMESTAMP_OFFSET_WINDOW_US)
            .expect("記録した観測が必ず補正の窓に入る");
        let recent = self.window(read_wall_clock_us, AUDIO_TIMESTAMP_OFFSET_STEP_WINDOW_US);
        let mut outcome = ModelOutcome {
            applied_us: window_min_us,
            step_retake: false,
            step_needs_samples: false,
            below_threshold: false,
        };
        if let (Some(applied_us), Some((recent_min_us, recent_count))) = (self.applied_us, recent) {
            if recent_min_us.saturating_sub(applied_us) >= AUDIO_TIMESTAMP_OFFSET_STEP_US {
                if recent_count >= AUDIO_TIMESTAMP_OFFSET_STEP_MIN_SAMPLES {
                    outcome.applied_us = recent_min_us;
                    outcome.step_retake = true;
                    // 古い観測を捨てる。残すと次の記録でまた古い床へ戻ってしまう
                    self.prune(
                        read_wall_clock_us.saturating_sub(AUDIO_TIMESTAMP_OFFSET_STEP_WINDOW_US),
                    );
                } else {
                    outcome.step_needs_samples = true;
                }
            } else if recent_count >= AUDIO_TIMESTAMP_OFFSET_STEP_MIN_SAMPLES {
                outcome.below_threshold = true;
            }
        }
        self.applied_us = Some(outcome.applied_us);
        outcome
    }

    /// 指定した窓 (両端を含む) の最小値と観測の数
    fn window(&self, read_wall_clock_us: i64, window_us: i64) -> Option<(i64, usize)> {
        let since_us = read_wall_clock_us.saturating_sub(window_us);
        let mut min_us: Option<i64> = None;
        let mut count = 0usize;
        for &(read_us, offset_us) in &self.observations {
            if read_us < since_us || read_us > read_wall_clock_us {
                continue;
            }
            min_us = Some(min_us.map_or(offset_us, |min_us| min_us.min(offset_us)));
            count += 1;
        }
        min_us.map(|min_us| (min_us, count))
    }

    /// 観測したオフセットが時刻の昇順で単調に増えるかどうか
    ///
    /// 単調に増えるなら、窓を前半と後半に分けた 2 つの最小の観測の差は 0 以上になる
    /// (同じ観測を選んだときは差が 0 になる)。
    fn offsets_are_non_decreasing(&self) -> bool {
        self.observations
            .windows(2)
            .all(|pair| pair[0].1 <= pair[1].1)
    }

    /// 観測したオフセットがすべて同じかどうか
    ///
    /// すべて同じなら、窓を前半と後半に分けた 2 つの最小の観測の差は 0 になる。
    fn offsets_are_all_equal(&self) -> bool {
        self.observations
            .windows(2)
            .all(|pair| pair[0].1 == pair[1].1)
    }

    /// 観測した最小値と最大値 (取り直しでは消えない値)
    fn bounds_us(&self) -> (i64, i64) {
        (
            self.min_offset_us.expect("観測を記録したので最小値がある"),
            self.max_offset_us.expect("観測を記録したので最大値がある"),
        )
    }

    /// 補正の窓 (2 秒) の最小値と、直近の窓 (0.5 秒) の最小値
    fn window_bounds_us(&self, read_wall_clock_us: i64) -> (i64, i64) {
        (
            self.window(read_wall_clock_us, AUDIO_TIMESTAMP_OFFSET_WINDOW_US)
                .expect("記録した観測が必ず補正の窓に入る")
                .0,
            self.window(read_wall_clock_us, AUDIO_TIMESTAMP_OFFSET_STEP_WINDOW_US)
                .expect("記録した観測が必ず直近の窓に入る")
                .0,
        )
    }

    /// 直近に観測したオフセット (マイクロ秒)
    fn current_us(&self) -> i64 {
        self.observations
            .last()
            .expect("観測を記録したので直近の観測がある")
            .1
    }

    /// 指定した時刻より古い観測を捨てる
    fn prune(&mut self, oldest_us: i64) {
        self.observations
            .retain(|&(read_us, _)| read_us >= oldest_us);
    }
}

/// 任意の観測列で、補正が規則どおりの窓の最小値へ動き、観測した範囲から出ない
#[test]
fn applied_offset_follows_the_window_and_step_rules() -> noprop::TestResult {
    // 到達した分岐を数える。0 のままなら検証が空振りしている
    let saw_empty = Cell::new(false);
    let saw_window_min = Cell::new(false);
    let saw_drift_follow = Cell::new(false);
    let saw_decrease = Cell::new(false);
    let saw_step_retake = Cell::new(false);
    let saw_step_needs_samples = Cell::new(false);
    let saw_below_threshold = Cell::new(false);
    let saw_slope = Cell::new(false);
    let saw_no_slope = Cell::new(false);
    let mut runner = test_runner()?;

    runner.run(256, |ctx| {
        let mut clock = AudioTimestampClock::new();
        let mut model = ObservationModel::new();
        let records = noprop::sample_with_boundaries(
            ctx,
            &[0usize, 1, 2, 128],
            noprop::Ratio::one_nth(4),
            |ctx| noprop::sample_usize_in(ctx, 0..=128),
        );
        let mut read_us = noprop::sample_usize_in(ctx, 0..=2_000_000) as i64;
        let mut offset_us = noprop::sample_usize_in(ctx, 0..=5_000_000) as i64;
        // いまのずれの動かし方 (1 記録あたりのドリフト / ぶれの有無) と、それが続く記録数
        let mut drift_per_record_us = 0i64;
        let mut jitter = false;
        let mut hold = 0usize;
        let mut previous_applied_us = None;

        if records == 0 {
            // 観測が無ければ補正も統計も無い
            assert_eq!(clock.applied_us(), None, "観測が無ければ補正が無いこと");
            assert_eq!(clock.snapshot(), None, "観測が無ければ統計が無いこと");
            saw_empty.set(true);
        }

        for index in 0..records {
            let interval_us = match noprop::sample_weighted_index(ctx, &INTERVAL_WEIGHTS) {
                0 => noprop::sample_usize_in(ctx, 16_000..=26_000) as i64,
                1 => noprop::sample_usize_in(ctx, 26_000..=300_000) as i64,
                _ => noprop::sample_usize_in(ctx, 450_000..=1_800_000) as i64,
            };
            read_us += interval_us;

            if hold == 0 {
                // 時計の動きは一度に変わらず、しばらく同じ傾向が続く
                match noprop::sample_weighted_index(ctx, &OFFSET_WEIGHTS) {
                    0 => {
                        drift_per_record_us = 0;
                        jitter = false;
                        hold = noprop::sample_usize_in(ctx, 5..=30);
                    }
                    1 => {
                        let magnitude_us = noprop::sample_usize_in(ctx, 1_000..=5_000) as i64;
                        drift_per_record_us = if noprop::sample_bool(ctx) {
                            magnitude_us
                        } else {
                            -magnitude_us
                        };
                        jitter = false;
                        hold = noprop::sample_usize_in(ctx, 20..=60);
                    }
                    2 => {
                        drift_per_record_us = 0;
                        jitter = true;
                        hold = noprop::sample_usize_in(ctx, 5..=30);
                    }
                    _ => {
                        let magnitude_us = noprop::sample_usize_in(
                            ctx,
                            AUDIO_TIMESTAMP_OFFSET_STEP_US as usize..=1_000_000,
                        ) as i64;
                        offset_us += if noprop::sample_bool(ctx) {
                            magnitude_us
                        } else {
                            -magnitude_us
                        };
                        drift_per_record_us = 0;
                        jitter = false;
                        hold = noprop::sample_usize_in(ctx, 5..=40);
                    }
                }
            }
            hold -= 1;
            offset_us += drift_per_record_us;
            if jitter {
                offset_us += noprop::sample_usize_in(ctx, 0..=150_000) as i64 - 75_000;
            }

            let audio_timestamp_us = read_us - offset_us;
            clock.record(read_us, audio_timestamp_us);
            let outcome = model.record(read_us, audio_timestamp_us);

            let applied_us = clock
                .applied_us()
                .expect("観測を記録したので補正が決まっていること");
            assert_eq!(
                applied_us, outcome.applied_us,
                "補正が規則どおりでない: applied={applied_us} expected={}",
                outcome.applied_us
            );
            // 補正は観測した範囲の外へ出ない
            let (all_min_us, all_max_us) = model.bounds_us();
            assert!(
                (all_min_us..=all_max_us).contains(&applied_us),
                "補正が観測した範囲から出た: applied={applied_us} min={all_min_us} max={all_max_us}"
            );
            // 補正は常に窓の最小値であり、直近の窓の最小値を超えない
            let (window_min_us, recent_min_us) = model.window_bounds_us(read_us);
            assert!(
                (window_min_us..=recent_min_us).contains(&applied_us),
                "補正が窓の最小値の範囲から出た: applied={applied_us} window={window_min_us} recent={recent_min_us}"
            );

            if outcome.step_retake {
                saw_step_retake.set(true);
            } else {
                saw_window_min.set(true);
                if previous_applied_us.is_some_and(|previous_us| applied_us > previous_us) {
                    saw_drift_follow.set(true);
                }
            }
            if outcome.step_needs_samples {
                saw_step_needs_samples.set(true);
            }
            if outcome.below_threshold {
                saw_below_threshold.set(true);
            }
            if previous_applied_us.is_some_and(|previous_us| applied_us < previous_us) {
                saw_decrease.set(true);
            }
            previous_applied_us = Some(applied_us);

            // 統計は生の観測 (補正を当てる前) を出し、取り直しでも消えない
            let stats = clock.snapshot().expect("観測を記録したので統計があること");
            assert_eq!(
                stats.current_us,
                model.current_us(),
                "現在値が直近の観測であること"
            );
            assert_eq!(
                stats.min_us, all_min_us,
                "観測した最小値が取り直しで消えないこと"
            );
            assert_eq!(
                stats.max_us, all_max_us,
                "観測した最大値が取り直しで消えないこと"
            );
            assert_eq!(stats.samples, (index + 1) as u64, "観測した数");
            assert_eq!(
                stats.applied_us,
                Some(applied_us),
                "統計の補正が使っている補正と一致すること"
            );
            // 傾きは「前半と後半の最小の観測の差 ÷ 経過時間」である。向きは生の観測から
            // 決まるため、単調に増える観測列では負にならず、すべて同じなら 0 になる
            if let Some(slope_10s_us_per_second) = stats.slope_10s_us_per_second {
                assert!(
                    !model.offsets_are_non_decreasing() || slope_10s_us_per_second >= 0,
                    "増え続ける観測列の 10 秒の窓の傾きが負になった: slope={slope_10s_us_per_second}"
                );
                assert!(
                    !model.offsets_are_all_equal() || slope_10s_us_per_second == 0,
                    "同じ値の観測列の 10 秒の窓の傾きが 0 でない: slope={slope_10s_us_per_second}"
                );
                saw_slope.set(true);
            } else {
                saw_no_slope.set(true);
            }
            if let Some(slope_60s_us_per_second) = stats.slope_60s_us_per_second {
                assert!(
                    !model.offsets_are_non_decreasing() || slope_60s_us_per_second >= 0,
                    "増え続ける観測列の 60 秒の窓の傾きが負になった: slope={slope_60s_us_per_second}"
                );
                assert!(
                    !model.offsets_are_all_equal() || slope_60s_us_per_second == 0,
                    "同じ値の観測列の 60 秒の窓の傾きが 0 でない: slope={slope_60s_us_per_second}"
                );
                saw_slope.set(true);
            } else {
                saw_no_slope.set(true);
            }
        }
        Ok(())
    })?;

    assert!(
        saw_empty.get(),
        "観測が無い場合を一度も検証していない\n{runner}"
    );
    assert!(
        saw_window_min.get(),
        "補正を窓の最小値へ合わせる場合を一度も検証していない\n{runner}"
    );
    assert!(
        saw_drift_follow.get(),
        "ドリフトで補正が上がる場合を一度も検証していない\n{runner}"
    );
    assert!(
        saw_decrease.get(),
        "段差で補正が下がる場合を一度も検証していない\n{runner}"
    );
    assert!(
        saw_step_retake.get(),
        "段差で取り直す場合を一度も検証していない\n{runner}"
    );
    assert!(
        saw_step_needs_samples.get(),
        "観測が足りず取り直さない場合を一度も検証していない\n{runner}"
    );
    assert!(
        saw_below_threshold.get(),
        "閾値未満で取り直さない場合を一度も検証していない\n{runner}"
    );
    assert!(
        saw_slope.get(),
        "傾きが求まる場合を一度も検証していない\n{runner}"
    );
    assert!(
        saw_no_slope.get(),
        "傾きが求まらない場合を一度も検証していない\n{runner}"
    );
    Ok(())
}

/// 任意の観測列で、`apply` が補正を足すだけであり、負にならず、間隔が保たれる
#[test]
fn apply_offsets_the_timestamp_without_going_negative() -> noprop::TestResult {
    let saw_empty = Cell::new(false);
    let saw_observed = Cell::new(false);
    let saw_interval_preserved = Cell::new(false);
    let saw_clamped = Cell::new(false);
    let mut runner = test_runner()?;

    runner.run(256, |ctx| {
        let mut clock = AudioTimestampClock::new();
        let records = noprop::sample_usize_in(ctx, 0..=8);
        let mut read_us = noprop::sample_usize_in(ctx, 0..=5_000_000) as i64;
        let mut offset_us = noprop::sample_usize_in(ctx, 0..=5_000_000) as i64;
        for _ in 0..records {
            read_us += noprop::sample_usize_in(ctx, 1..=1_000_000) as i64;
            // 読み出しの遅れと段差を任意に与える
            offset_us += noprop::sample_usize_in(ctx, 0..=1_500_000) as i64 - 750_000;
            clock.record(read_us, read_us - offset_us);
        }

        if records == 0 {
            // 観測が無ければ換算できない
            assert_eq!(clock.apply(0), None, "観測が無ければ換算できないこと");
            saw_empty.set(true);
            return Ok(());
        }
        saw_observed.set(true);
        let applied_us = clock
            .applied_us()
            .expect("観測を記録したので補正が決まっていること");

        // 補正は記録のたびにしか動かない。同じ補正の間はどの 2 点でも間隔が保たれる
        let first_timestamp_us = noprop::sample_usize_in(ctx, 0..=16_000_000) as i64 - 8_000_000;
        let second_timestamp_us = noprop::sample_usize_in(ctx, 0..=16_000_000) as i64 - 8_000_000;
        let first_us = clock
            .apply(first_timestamp_us)
            .expect("観測があるので換算できること");
        let second_us = clock
            .apply(second_timestamp_us)
            .expect("観測があるので換算できること");
        assert!(
            first_us >= 0,
            "Unix epoch より前を返さないこと first={first_us}"
        );
        assert!(
            second_us >= 0,
            "Unix epoch より前を返さないこと second={second_us}"
        );
        assert_eq!(
            first_us,
            first_timestamp_us.saturating_add(applied_us).max(0),
            "補正を足すだけであること first={first_us}"
        );
        assert_eq!(
            second_us,
            second_timestamp_us.saturating_add(applied_us).max(0),
            "補正を足すだけであること second={second_us}"
        );
        if first_us > 0 && second_us > 0 {
            assert_eq!(
                second_us - first_us,
                second_timestamp_us - first_timestamp_us,
                "同じ補正の間は間隔が保たれること first={first_us} second={second_us}"
            );
            saw_interval_preserved.set(true);
        } else {
            saw_clamped.set(true);
        }
        Ok(())
    })?;

    assert!(
        saw_empty.get(),
        "観測が無い場合を一度も検証していない\n{runner}"
    );
    assert!(
        saw_observed.get(),
        "観測がある場合を一度も検証していない\n{runner}"
    );
    assert!(
        saw_interval_preserved.get(),
        "間隔が保たれる場合を一度も検証していない\n{runner}"
    );
    assert!(
        saw_clamped.get(),
        "Unix epoch より前を 0 に丸める場合を一度も検証していない\n{runner}"
    );
    Ok(())
}

/// 任意の観測列で、`reset` が観測と補正を消して初期状態へ戻す
#[test]
fn reset_returns_to_the_empty_state() -> noprop::TestResult {
    let saw_observed = Cell::new(false);
    let saw_empty = Cell::new(false);
    let mut runner = test_runner()?;

    runner.run(128, |ctx| {
        let mut clock = AudioTimestampClock::new();
        let records = noprop::sample_usize_in(ctx, 0..=8);
        let mut read_us = noprop::sample_usize_in(ctx, 0..=5_000_000) as i64;
        let mut offset_us = noprop::sample_usize_in(ctx, 0..=5_000_000) as i64;
        for _ in 0..records {
            read_us += noprop::sample_usize_in(ctx, 1..=1_000_000) as i64;
            offset_us += noprop::sample_usize_in(ctx, 0..=1_500_000) as i64 - 750_000;
            clock.record(read_us, read_us - offset_us);
        }
        if records == 0 {
            saw_empty.set(true);
        } else {
            saw_observed.set(true);
            assert!(
                clock.applied_us().is_some(),
                "観測を記録したので補正があること"
            );
        }

        clock.reset();
        assert_eq!(clock.applied_us(), None, "リセットで補正が消えること");
        assert_eq!(clock.apply(0), None, "リセットで換算できなくなること");
        assert_eq!(clock.snapshot(), None, "リセットで観測が消えること");

        // リセット後の観測だけで補正が決まる
        let offset_us = 42_000;
        clock.record(read_us + 1_000_000, read_us + 1_000_000 - offset_us);
        assert_eq!(
            clock.apply(0),
            Some(offset_us),
            "リセット後は新しい観測だけで補正が決まること"
        );
        let stats = clock.snapshot().expect("観測を記録したので統計があること");
        assert_eq!(
            stats.min_us, offset_us,
            "リセットで観測した最小値が消えること"
        );
        assert_eq!(
            stats.max_us, offset_us,
            "リセットで観測した最大値が消えること"
        );
        assert_eq!(stats.samples, 1, "リセットで観測した数が 0 に戻ること");
        Ok(())
    })?;

    assert!(
        saw_observed.get(),
        "観測を記録する場合を一度も検証していない\n{runner}"
    );
    assert!(
        saw_empty.get(),
        "観測が無い場合を一度も検証していない\n{runner}"
    );
    Ok(())
}
