//! 音声の TIMESTAMP を配信側の壁時計へ合わせる処理
//!
//! LOC の TIMESTAMP は Timescale を載せないとき、取得した時刻の壁時計 (Unix epoch
//! マイクロ秒) である (draft-ietf-moq-loc-04 §2.3.1.1)。ところがマイクやキャプチャ機器から
//! 届く音声フレームのメディア時刻は `performance.now()` と同じ時計ではない。そのまま
//! 壁時計として送ると、受信側は「音声は数百 ms 遅れて届いている」と解釈し、リップシンクの
//! ために映像をその分だけ遅らせる。
//!
//! そこで音声フレームのメディア時刻の刻み (サンプルの間隔) はそのまま使い、原点 (オフセット)
//! だけを壁時計へ合わせる。オフセットは「読み出した壁時計 - 音声の TIMESTAMP」であり、
//! これは「音声の時計と壁時計のずれ」と「撮ってから読むまでの遅れ (0 以上)」の和である。
//! 遅れの最小値を窓で取ることで、ずれに最小の遅れを足した値を推定する。
//!
//! 窓の最小値は 2 つの動きに追従する。
//!
//! - ゆっくりしたドリフト: 窓が滑るにつれて最小値が動く
//! - 段差: 直近の窓の最小値が適用中の値より [`AUDIO_TIMESTAMP_OFFSET_STEP_US`] 以上
//!   大きくなったら、古い観測を捨ててその値へ取り直す。段差は音声の時計そのものが飛んだ
//!   のであり、遅れが増えたのではない。取り直しを入れるのは、窓が埋まるまで TIMESTAMP が
//!   実際より古いままになり、受信側の再生の目標が過去へずれて音が捨てられるためである。
//!
//! この 2 つの規則でも、補正の値は「観測した最大値と最小値の差」の範囲でしか動かない。
//! 段差やドリフトの量そのものは [`AudioTimestampClock::snapshot`] が返す生の観測の統計
//! (現在値・最小・最大・傾き) で確かめる。
//!
//! 時計もデバイスも触らず、フレームのメディア時刻とそのとき読んだ壁時計を引数で受ける。

use alloc::collections::VecDeque;

/// 補正に使う観測の窓 (マイクロ秒)
///
/// 短くするほどドリフトと段差への追従が速くなり、経路ではなく読み出しの揺らぎで最小値が
/// 動きやすくなる。音声は 20 ms ごとに読むため、2 秒の窓には約 100 個の観測が入る。
/// 読み出しの揺らぎは数 ms であり、最小値はほぼ一定になる。
pub const AUDIO_TIMESTAMP_OFFSET_WINDOW_US: i64 = 2_000_000;

/// 段差とみなす、適用中の補正からの上振れ (マイクロ秒)
///
/// ドリフトでも「直近の窓の最小値」は「補正の窓の最小値」より大きくなる。その差は
/// ドリフトの速さ × (補正の窓 - 直近の窓) であり、実測した速さ (50 ms/秒 程度) でも
/// 100 ms に届かない。読み出しの遅れが 200 ms 以上ぶれて、その状態が 0.5 秒続くことは
/// 考えにくいため、段差と遅れの増加をこの値で分ける。
pub const AUDIO_TIMESTAMP_OFFSET_STEP_US: i64 = 200_000;

/// 段差を見る直近の窓 (マイクロ秒)
pub const AUDIO_TIMESTAMP_OFFSET_STEP_WINDOW_US: i64 = 500_000;

/// 段差とみなすのに必要な、直近の窓の観測の数
///
/// 音声は 20 ms ごとに届くため、0.5 秒の窓には約 25 個入る。5 個未満では
/// 「たまたま遅れて読めた数個」だけで取り直してしまう。
pub const AUDIO_TIMESTAMP_OFFSET_STEP_MIN_SAMPLES: usize = 5;

/// 傾きを求める短い窓 (マイクロ秒)
pub const AUDIO_TIMESTAMP_SLOPE_WINDOW_US: i64 = 10_000_000;

/// 傾きを求める長い窓 (マイクロ秒)
pub const AUDIO_TIMESTAMP_SLOPE_LONG_WINDOW_US: i64 = 60_000_000;

/// 観測した 1 つの音声フレームの対応
#[derive(Debug, Clone, Copy)]
struct AudioTimestampObservation {
    /// 読み出したときの壁時計 (Unix epoch マイクロ秒)
    read_wall_clock_us: i64,
    /// 読み出した壁時計 - 音声の TIMESTAMP (マイクロ秒)
    offset_us: i64,
}

/// 窓の中の観測のまとめ
#[derive(Debug, Clone, Copy)]
struct OffsetWindow {
    /// 最小のオフセット (マイクロ秒)
    min_us: i64,
    /// 窓の中の観測の数
    count: usize,
}

/// 音声の TIMESTAMP を壁時計へ合わせる (配信の音声トラックごとに 1 つ持つ)
///
/// 音声フレームを読むたびに [`AudioTimestampClock::record`] を呼び、符号化した chunk の
/// メディア時刻を [`AudioTimestampClock::apply`] で壁時計のマイクロ秒へ換算する。
pub struct AudioTimestampClock {
    /// 直近の観測。時刻の昇順に入る
    observations: VecDeque<AudioTimestampObservation>,
    /// TIMESTAMP に足している補正 (マイクロ秒)。まだ観測が無ければ `None`
    applied_offset_us: Option<i64>,
    /// 観測した最小値 (マイクロ秒)。補正の取り直しでは消さない
    min_offset_us: Option<i64>,
    /// 観測した最大値 (マイクロ秒)。補正の取り直しでは消さない
    max_offset_us: Option<i64>,
    /// 観測した数 (セッション中)
    sample_count: u64,
}

impl AudioTimestampClock {
    /// まだ 1 つも観測していない状態で作る
    pub fn new() -> Self {
        Self {
            observations: VecDeque::new(),
            applied_offset_us: None,
            min_offset_us: None,
            max_offset_us: None,
            sample_count: 0,
        }
    }

    /// 読み出した音声フレームの対応を記録する (フレームを読むたびに呼ぶ)
    ///
    /// `read_wall_clock_us` は読み出したときの壁時計 (Unix epoch マイクロ秒)、
    /// `audio_timestamp_us` はそのフレームのメディア時刻 (マイクロ秒) である。観測は
    /// 読み出した壁時計の昇順で渡す。窓の外へ出た観測は先頭から捨てる。
    ///
    /// 観測は「読み出した壁時計 - メディア時刻」であり、「音声の時計と壁時計のずれ」と
    /// 「撮ってから読むまでの遅れ (0 以上)」の和である。遅れの最小値を窓で取ることで、
    /// ずれに最小の遅れを足した値を推定する。
    pub fn record(&mut self, read_wall_clock_us: i64, audio_timestamp_us: i64) {
        let offset_us = read_wall_clock_us.saturating_sub(audio_timestamp_us);

        self.observations.push_back(AudioTimestampObservation {
            read_wall_clock_us,
            offset_us,
        });
        self.sample_count = self.sample_count.saturating_add(1);
        self.min_offset_us = Some(
            self.min_offset_us
                .map_or(offset_us, |min_offset_us| min_offset_us.min(offset_us)),
        );
        self.max_offset_us = Some(
            self.max_offset_us
                .map_or(offset_us, |max_offset_us| max_offset_us.max(offset_us)),
        );

        // 窓 (補正の 2 秒・段差の 0.5 秒・傾きの 60 秒) のどれにも入らない古い観測を捨てる。
        // 残すと観測の数だけが増えて走査が遅くなる
        let observation_window_us =
            AUDIO_TIMESTAMP_SLOPE_LONG_WINDOW_US.max(AUDIO_TIMESTAMP_OFFSET_STEP_WINDOW_US);
        self.prune(read_wall_clock_us.saturating_sub(observation_window_us));

        self.update_applied_offset(read_wall_clock_us);
    }

    /// 符号化された chunk のメディア時刻を壁時計 (Unix epoch マイクロ秒) へ換算する
    ///
    /// 補正は記録のたびにしか動かないため、同じ補正を当てた chunk どうしの間隔は
    /// `audio_timestamp_us` の間隔そのままになる。補正を当てる前 (観測が 1 つも無い) は
    /// `None` を返し、呼び出し側が従来の換算へ落とせるようにする。LOC の Timestamp は
    /// vi64 で負を表せないため、Unix epoch より前にはしない。
    pub fn apply(&self, audio_timestamp_us: i64) -> Option<i64> {
        let applied_offset_us = self.applied_offset_us?;
        Some(audio_timestamp_us.saturating_add(applied_offset_us).max(0))
    }

    /// いま使っている補正 (マイクロ秒)。まだ決まっていなければ `None`
    pub fn applied_us(&self) -> Option<i64> {
        self.applied_offset_us
    }

    /// 観測の統計 (現在値・最小・最大・10 秒と 60 秒の傾き)
    ///
    /// 値はすべて「読み出した壁時計 - 音声の TIMESTAMP」の生の観測である (補正を当てる
    /// 前の値)。まだ観測が無ければ `None`。傾きは観測が窓を埋めていなくても、観測がある
    /// 範囲で求める (傾きを出せるだけの幅が無いときは `None`)。
    pub fn snapshot(&self) -> Option<AudioTimestampOffsetStats> {
        let latest = self.observations.back()?;
        Some(AudioTimestampOffsetStats {
            current_us: latest.offset_us,
            min_us: self.min_offset_us.unwrap_or(latest.offset_us),
            max_us: self.max_offset_us.unwrap_or(latest.offset_us),
            slope_10s_us_per_second: self.slope_us_per_second(AUDIO_TIMESTAMP_SLOPE_WINDOW_US),
            slope_60s_us_per_second: self.slope_us_per_second(AUDIO_TIMESTAMP_SLOPE_LONG_WINDOW_US),
            applied_us: self.applied_offset_us,
            samples: self.sample_count,
        })
    }

    /// 観測と補正を消す (配信のやり直し)
    pub fn reset(&mut self) {
        self.observations.clear();
        self.applied_offset_us = None;
        self.min_offset_us = None;
        self.max_offset_us = None;
        self.sample_count = 0;
    }

    /// 補正を窓の最小値へ合わせる。段差なら窓を取り直す
    ///
    /// 窓の最小値へ合わせるのは、ゆっくりしたドリフトでも補正が止まらないようにするためで
    /// ある (窓が滑るにつれて最小値が動く)。段差 (時計そのものが飛んだ) のときだけ、2 秒の
    /// 窓が埋まるのを待たずに直近の窓から取り直す。待つと、その間だけ TIMESTAMP が実際より
    /// 古くなり、受信側の再生の目標が過去へずれて音が捨てられる。
    fn update_applied_offset(&mut self, read_wall_clock_us: i64) {
        let Some(window) = self.offset_window(read_wall_clock_us, AUDIO_TIMESTAMP_OFFSET_WINDOW_US)
        else {
            return;
        };
        if let Some(applied_us) = self.applied_offset_us {
            // 段差かどうか。直近の窓がすべて適用中の値より上にあるときだけ取り直す
            let recent =
                self.offset_window(read_wall_clock_us, AUDIO_TIMESTAMP_OFFSET_STEP_WINDOW_US);
            if let Some(recent) = recent
                && recent.count >= AUDIO_TIMESTAMP_OFFSET_STEP_MIN_SAMPLES
                && recent.min_us.saturating_sub(applied_us) >= AUDIO_TIMESTAMP_OFFSET_STEP_US
            {
                self.applied_offset_us = Some(recent.min_us);
                // 古い観測を捨てる。残すと次の記録でまた古い床へ戻ってしまう
                self.prune(
                    read_wall_clock_us.saturating_sub(AUDIO_TIMESTAMP_OFFSET_STEP_WINDOW_US),
                );
                return;
            }
        }

        // 床へ合わせる。下がる方向 (より早く読めた) も上がる方向 (ドリフト) も同じ規則で
        // ある。補正が実際より大きくなる (TIMESTAMP が実際より新しくなる) と、受信側の
        // 再生の目標が過去になって音が捨てられるため、常に観測した床を超えない値にする
        self.applied_offset_us = Some(window.min_us);
    }

    /// 直近の窓の最小値と観測の数
    fn offset_window(&self, read_wall_clock_us: i64, window_us: i64) -> Option<OffsetWindow> {
        self.offset_window_between(
            read_wall_clock_us.saturating_sub(window_us),
            read_wall_clock_us,
        )
    }

    /// 指定した範囲 (マイクロ秒、両端を含む) の最小値と観測の数
    ///
    /// 新しい方から走査し、窓より古い観測に達したら打ち切る。観測は時刻の昇順に入るため、
    /// それより前の観測もすべて窓の外である。
    fn offset_window_between(&self, since_us: i64, until_us: i64) -> Option<OffsetWindow> {
        let mut min_us: Option<i64> = None;
        let mut count = 0usize;
        for observation in self.observations.iter().rev() {
            if observation.read_wall_clock_us < since_us {
                break;
            }
            if observation.read_wall_clock_us > until_us {
                continue;
            }
            min_us = Some(min_us.map_or(observation.offset_us, |min_us| {
                min_us.min(observation.offset_us)
            }));
            count += 1;
        }
        min_us.map(|min_us| OffsetWindow { min_us, count })
    }

    /// 窓の中の最小の観測。無ければ `None`
    fn min_observation_between(
        &self,
        since_us: i64,
        until_us: i64,
    ) -> Option<&AudioTimestampObservation> {
        let mut best: Option<&AudioTimestampObservation> = None;
        for observation in self.observations.iter().rev() {
            if observation.read_wall_clock_us < since_us {
                break;
            }
            if observation.read_wall_clock_us > until_us {
                continue;
            }
            if best.is_none_or(|best| observation.offset_us < best.offset_us) {
                best = Some(observation);
            }
        }
        best
    }

    /// 窓の傾き (マイクロ秒 / 秒)
    ///
    /// 窓を前半と後半に分け、それぞれの最小の観測の差から求める。両端とも最小値 (床) を
    /// 使うため、読み出しの遅れの揺らぎを受けにくい。観測がまだ窓を埋めていないときは、
    /// 観測がある範囲で求める (傾きを出せるだけの幅が無いときは `None`)。
    fn slope_us_per_second(&self, window_us: i64) -> Option<i64> {
        let latest = self.observations.back()?;
        let oldest = self.observations.front()?;
        let since_us = latest
            .read_wall_clock_us
            .saturating_sub(window_us)
            .max(oldest.read_wall_clock_us);
        let middle_us = since_us + (latest.read_wall_clock_us - since_us) / 2;
        let older = self.min_observation_between(since_us, middle_us)?;
        let newer = self.min_observation_between(middle_us, latest.read_wall_clock_us)?;
        let span_us = newer
            .read_wall_clock_us
            .saturating_sub(older.read_wall_clock_us);
        if span_us <= 0 {
            return None;
        }
        // 傾きは「離れた 2 つの床の差 ÷ 経過時間」である。i64 の積で桁あふれしないよう
        // i128 で計算し、i64 に収まらない値は飽和させる
        let difference_us = i128::from(newer.offset_us) - i128::from(older.offset_us);
        let slope_us_per_second = difference_us * 1_000_000 / i128::from(span_us);
        match i64::try_from(slope_us_per_second) {
            Ok(slope_us_per_second) => Some(slope_us_per_second),
            Err(_) => Some(if slope_us_per_second.is_negative() {
                i64::MIN
            } else {
                i64::MAX
            }),
        }
    }

    /// 指定した時刻より古い観測を先頭から捨てる
    ///
    /// 古い観測は `VecDeque::pop_front` で捨てる (moqt-js の head インデックスは配列を
    /// 詰め直さないための工夫であり、`VecDeque` が同じ役割を果たす)。
    fn prune(&mut self, oldest_us: i64) {
        while let Some(observation) = self.observations.front() {
            if observation.read_wall_clock_us >= oldest_us {
                break;
            }
            self.observations.pop_front();
        }
    }
}

impl Default for AudioTimestampClock {
    fn default() -> Self {
        Self::new()
    }
}

/// 観測の統計 (配信側の Publisher 統計とログに出す)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioTimestampOffsetStats {
    /// 直近に観測した「読み出しの壁時計 - 音声の TIMESTAMP」(マイクロ秒)
    pub current_us: i64,
    /// 観測した最小値 (マイクロ秒)。補正の取り直しでは消えない
    pub min_us: i64,
    /// 観測した最大値 (マイクロ秒)。補正の取り直しでは消えない
    pub max_us: i64,
    /// 直近 10 秒の傾き (マイクロ秒 / 秒)。一定なら 0、ずれ続けるなら 0 から離れる。
    /// 傾きを出せるだけの幅が無ければ `None`
    pub slope_10s_us_per_second: Option<i64>,
    /// 直近 60 秒の傾き (マイクロ秒 / 秒)。傾きを出せるだけの幅が無ければ `None`
    pub slope_60s_us_per_second: Option<i64>,
    /// TIMESTAMP に足している補正 (マイクロ秒)。まだ決まっていなければ `None`
    pub applied_us: Option<i64>,
    /// 観測した数
    pub samples: u64,
}
