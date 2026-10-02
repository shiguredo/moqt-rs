//! 受信した音声と映像を鳴らす・表示する時刻を決める共通の時間軸
//!
//! 送信側の TIMESTAMP (LOC の TIMESTAMP) と受信側の時計を結び、トラックごとの遅延を
//! 足した「鳴らす時刻・表示する時刻」を返す。音声と映像で同じ時間軸を使うため、A/V の
//! 同期はトラックごとの遅延の差だけで決まる。時計もデバイスも触らず、時刻はすべて
//! 呼び出し側が引数で渡す。

use crate::playout::delay::AudioDelayManager;
use crate::playout::scheduler::AUDIO_PLAYOUT_MIN_LEAD_US;
use crate::playout::sync::{
    StreamSynchronization, SyncDelays, SyncMeasurement, compute_relative_delay,
};

/// TIMESTAMP がこれ以上飛んだら基準を張り直す (マイクロ秒)
///
/// 配信元の切り替えや、配信が長く止まったあとの再開で基準が古くなるのを避ける。
pub const TIMELINE_DISCONTINUITY_US: i64 = 2_000_000;

/// 同期の制御を行う間隔 (マイクロ秒)
pub const TIMELINE_SYNC_INTERVAL_US: i64 = 1_000_000;

/// 同期の制御が動かないときに、学習した目標遅延へ向けて下げる速さ (マイクロ秒/秒)
///
/// 学習した目標遅延が下がっても、すぐに下げると音が途切れる。少しずつ下げる。
pub const TIMELINE_DELAY_DECAY_US_PER_SECOND: i64 = 20_000;

/// 再生・表示の対象にするトラック
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Track {
    /// 音声
    Audio,
    /// 映像
    Video,
}

impl Track {
    /// 配列の添字
    pub fn index(self) -> usize {
        match self {
            Self::Audio => 0,
            Self::Video => 1,
        }
    }

    /// ログや表示に使う名前
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Video => "video",
        }
    }
}

/// トラック 1 つぶんの状態
#[derive(Debug)]
struct TrackState {
    /// 到着の揺らぎから目標遅延を求める
    delay: AudioDelayManager,
    /// 直近に受信した (到着時刻, TIMESTAMP) (マイクロ秒)
    latest: Option<(i64, i64)>,
    /// 鳴らす・表示するために足す遅延 (マイクロ秒)
    playout_delay_us: i64,
    /// 基準を張り直した回数
    rebases: u64,
}

impl TrackState {
    fn new() -> Self {
        Self {
            delay: AudioDelayManager::new(),
            latest: None,
            playout_delay_us: 0,
            rebases: 0,
        }
    }

    /// 学習した目標遅延を下限にした遅延 (マイクロ秒)
    fn minimum_delay_us(&self) -> i64 {
        self.delay
            .target_delay_ms()
            .saturating_mul(1_000)
            .max(AUDIO_PLAYOUT_MIN_LEAD_US)
    }

    /// 同期の制御に渡す実測
    fn measurement(&self) -> Option<SyncMeasurement> {
        let (arrival_us, capture_us) = self.latest?;
        Some(SyncMeasurement {
            latest_receive_time_us: arrival_us,
            latest_capture_time_us: capture_us,
        })
    }

    /// トラックの状態にする (基準を張り直す)
    fn reset(&mut self) {
        self.delay.reset();
        self.latest = None;
        self.playout_delay_us = 0;
    }
}

/// 音声と映像に共通の再生時刻を決める
///
/// 受信のたびに [`PlayoutTimeline::observe`] を呼び、鳴らす・表示する時刻を
/// [`PlayoutTimeline::present_us`] で求める。1 秒ごとに [`PlayoutTimeline::sync`] を
/// 呼ぶと、音声と映像の遅延の差が不感帯を超えたときに両者の遅延を調整する。
#[derive(Debug)]
pub struct PlayoutTimeline {
    /// 音声と映像の状態
    tracks: [TrackState; 2],
    /// 音声と映像の遅延の差を制御する
    sync: StreamSynchronization,
    /// 直近に同期の制御を行った時刻
    last_sync_us: Option<i64>,
}

impl PlayoutTimeline {
    /// 基準の無い状態で作る
    pub fn new() -> Self {
        Self {
            tracks: [TrackState::new(), TrackState::new()],
            sync: StreamSynchronization::new(),
            last_sync_us: None,
        }
    }

    /// 受信したフレームを 1 つ記録する
    ///
    /// `arrival_us` は受信 (復号の出力) の時刻、`capture_us` はそのデータの TIMESTAMP
    /// (どちらもマイクロ秒)。TIMESTAMP が大きく飛んだときと戻ったときは、配信元の
    /// 切り替えとみなして基準を張り直す。
    pub fn observe(&mut self, track: Track, arrival_us: i64, capture_us: i64) {
        let state = &mut self.tracks[track.index()];
        if let Some((_, latest_capture_us)) = state.latest {
            let jumped = capture_us
                .saturating_sub(latest_capture_us)
                .saturating_abs()
                > TIMELINE_DISCONTINUITY_US
                || capture_us < latest_capture_us;
            if jumped {
                state.reset();
                state.rebases = state.rebases.saturating_add(1);
            }
        }
        let is_first = state.latest.is_none();
        state.delay.observe(arrival_us, capture_us);
        state.latest = Some((arrival_us, capture_us));
        if is_first || state.playout_delay_us < state.minimum_delay_us() {
            // 最初のフレームと、学習した目標遅延が伸びたときはすぐに反映する
            state.playout_delay_us = state.minimum_delay_us();
        }
    }

    /// この TIMESTAMP を鳴らす・表示する時刻 (マイクロ秒)
    ///
    /// 基準 (直近の窓で最も早く届いた観測) に TIMESTAMP の差と遅延を足した値である。
    /// 基準がまだ無ければ None。戻り値が今より前になることもある (間に合わなかった
    /// フレーム) が、その扱いは呼び出し側が決める。
    pub fn present_us(&self, track: Track, capture_us: i64) -> Option<i64> {
        let state = &self.tracks[track.index()];
        let (basis_arrival_us, basis_capture_us) = state.delay.earliest_arrival()?;
        Some(
            basis_arrival_us
                .saturating_add(capture_us.saturating_sub(basis_capture_us))
                .saturating_add(state.playout_delay_us),
        )
    }

    /// 基準から見たこのフレームの到着の遅れ (マイクロ秒)
    ///
    /// 観測の窓は [`crate::playout::delay::AUDIO_DELAY_HISTORY_WINDOW_MS`] ミリ秒である。
    pub fn relative_delay_us(&self, track: Track, arrival_us: i64, capture_us: i64) -> i64 {
        let state = &self.tracks[track.index()];
        let Some((basis_arrival_us, basis_capture_us)) = state.delay.earliest_arrival() else {
            return 0;
        };
        arrival_us
            .saturating_sub(basis_arrival_us)
            .saturating_sub(capture_us.saturating_sub(basis_capture_us))
            .max(0)
    }

    /// 今使っている遅延 (マイクロ秒)
    pub fn playout_delay_us(&self, track: Track) -> i64 {
        self.tracks[track.index()].playout_delay_us
    }

    /// 学習した目標遅延 (マイクロ秒)
    ///
    /// 観測がまだ無いときは [`crate::playout::delay::AUDIO_DELAY_START_MS`] の値になる。
    pub fn learned_delay_us(&self, track: Track) -> i64 {
        self.tracks[track.index()]
            .delay
            .target_delay_ms()
            .saturating_mul(1_000)
    }

    /// 遅延を直接決める (アプリが方針を持つ場合)
    pub fn set_playout_delay_us(&mut self, track: Track, delay_us: i64) {
        let state = &mut self.tracks[track.index()];
        state.playout_delay_us = delay_us.max(AUDIO_PLAYOUT_MIN_LEAD_US);
    }

    /// 音声と映像の遅延の差を制御する
    ///
    /// [`TIMELINE_SYNC_INTERVAL_US`] ごとに 1 回だけ動く。両方のトラックの観測が
    /// 揃っていて、かつずれが不感帯を超えたときに `Some` を返す。返した遅延は
    /// [`PlayoutTimeline::playout_delay_us`] に反映済みである。
    pub fn sync(&mut self, now_us: i64) -> Option<SyncDelays> {
        if let Some(last_sync_us) = self.last_sync_us
            && now_us.saturating_sub(last_sync_us) < TIMELINE_SYNC_INTERVAL_US
        {
            return None;
        }
        let audio = self.tracks[Track::Audio.index()].measurement()?;
        let video = self.tracks[Track::Video.index()].measurement()?;
        let relative_delay_ms = compute_relative_delay(audio, video)?;
        // 基準の遅延は、学習した目標遅延の大きい方に合わせる (両方が少なくともその分遅れる)
        let base_delay_ms = self.tracks[Track::Audio.index()]
            .minimum_delay_us()
            .max(self.tracks[Track::Video.index()].minimum_delay_us())
            / 1_000;
        self.sync.set_target_buffering_delay(base_delay_ms);
        let current_audio_delay_ms = self.tracks[Track::Audio.index()].playout_delay_us / 1_000;
        let current_video_delay_ms = self.tracks[Track::Video.index()].playout_delay_us / 1_000;
        self.last_sync_us = Some(now_us);
        let Some(delays) = self.sync.compute_delays(
            relative_delay_ms,
            current_audio_delay_ms,
            current_video_delay_ms,
        ) else {
            // ずれが不感帯の中では制御しない。学習した目標遅延が下がっている場合は、
            // 音が途切れないように少しずつ下げる
            self.decay_delays();
            return None;
        };
        self.tracks[Track::Audio.index()].playout_delay_us =
            delays.audio_delay_ms.saturating_mul(1_000);
        self.tracks[Track::Video.index()].playout_delay_us =
            delays.video_delay_ms.saturating_mul(1_000);
        Some(delays)
    }

    /// 学習した目標遅延へ向けて遅延を少しずつ下げる
    fn decay_delays(&mut self) {
        for index in 0..self.tracks.len() {
            let minimum_us = self.tracks[index].minimum_delay_us();
            let current_us = self.tracks[index].playout_delay_us;
            if current_us > minimum_us {
                self.tracks[index].playout_delay_us =
                    (current_us - TIMELINE_DELAY_DECAY_US_PER_SECOND).max(minimum_us);
            }
        }
    }

    /// 基準と学習を消す (購読のやり直し)。張り直しの回数は残す
    pub fn reset(&mut self) {
        for state in &mut self.tracks {
            state.reset();
        }
        self.last_sync_us = None;
    }

    /// 基準を張り直した回数 (音声と映像の合計)
    pub fn rebases(&self) -> u64 {
        self.tracks
            .iter()
            .map(|state| state.rebases)
            .fold(0, |total, count| total.saturating_add(count))
    }
}

impl Default for PlayoutTimeline {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playout::delay::AUDIO_DELAY_START_MS;

    /// 音声の観測を 1 つ足す
    fn observe_audio(timeline: &mut PlayoutTimeline, arrival_us: i64, capture_us: i64) {
        timeline.observe(Track::Audio, arrival_us, capture_us);
    }

    /// 学習が始まる前に使う遅延が AUDIO_DELAY_START_MS であること
    #[test]
    fn starts_with_the_default_delay() {
        let timeline = PlayoutTimeline::new();
        assert_eq!(
            timeline.playout_delay_us(Track::Audio),
            0,
            "観測が無ければ遅延は無い"
        );
        assert_eq!(
            timeline.learned_delay_us(Track::Audio),
            AUDIO_DELAY_START_MS * 1_000,
            "学習前は既定の目標遅延"
        );
    }

    /// TIMESTAMP の差どおりに鳴らす時刻が進むこと
    #[test]
    fn present_time_follows_the_timestamp() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        let delay_us = timeline.playout_delay_us(Track::Audio);
        assert_eq!(delay_us, 80_000, "最初の観測では既定の目標遅延を使う");
        assert_eq!(
            timeline.present_us(Track::Audio, 1_000_000),
            Some(10_080_000),
            "基準の時刻に遅延を足した時刻に鳴らす"
        );
        assert_eq!(
            timeline.present_us(Track::Audio, 1_020_000),
            Some(10_100_000),
            "TIMESTAMP が 20 ms 進めば鳴らす時刻も 20 ms 進む"
        );
    }

    /// 遅れて届いた観測が基準を動かさないこと
    #[test]
    fn late_arrival_does_not_move_the_basis() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        let present_us = timeline.present_us(Track::Audio, 1_000_000);
        // 同じ TIMESTAMP のフレームが 50 ms 遅れて届いても、基準は早く届いた方のまま
        observe_audio(&mut timeline, 10_050_000, 1_000_000);
        assert_eq!(
            timeline.present_us(Track::Audio, 1_000_000),
            present_us,
            "遅れて届いた観測では基準が動かない"
        );
        assert_eq!(
            timeline.relative_delay_us(Track::Audio, 10_050_000, 1_000_000),
            50_000,
            "遅れは相対遅延として見える"
        );
    }

    /// 早く届いた観測で基準が動くこと
    #[test]
    fn earlier_arrival_moves_the_basis() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_050_000, 1_000_000);
        observe_audio(&mut timeline, 10_050_000, 1_020_000);
        // 経路が空いて 20 ms 早く届くようになった
        observe_audio(&mut timeline, 10_040_000, 1_040_000);
        assert_eq!(
            timeline.present_us(Track::Audio, 1_040_000),
            Some(10_040_000 + timeline.playout_delay_us(Track::Audio)),
            "早く届いた観測が新しい基準になる"
        );
    }

    /// TIMESTAMP が大きく飛んだら基準を張り直すこと
    #[test]
    fn timestamp_jump_rebases() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        let rebases = timeline.rebases();
        // 3 秒進んだ (配信元の切り替えなど)
        observe_audio(&mut timeline, 13_000_000, 4_000_000);
        assert_eq!(timeline.rebases(), rebases + 1, "大きく飛んだら張り直す");
        assert_eq!(
            timeline.present_us(Track::Audio, 4_000_000),
            Some(13_000_000 + timeline.playout_delay_us(Track::Audio)),
            "新しい基準で鳴らす時刻を求める"
        );
        // 戻ったときも張り直す
        observe_audio(&mut timeline, 20_000_000, 2_000_000);
        assert_eq!(timeline.rebases(), rebases + 2, "戻ったときも張り直す");
    }

    /// 同期は両方のトラックの観測が揃ってから動くこと
    #[test]
    fn sync_needs_both_tracks() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        assert!(
            timeline.sync(20_000_000).is_none(),
            "映像の観測が無ければ制御しない"
        );
        timeline.observe(Track::Video, 10_000_000, 1_000_000);
        assert!(
            timeline.sync(20_000_000).is_none(),
            "ずれが不感帯の中では制御しない"
        );
    }

    /// 映像が相対的に遅れているとき音声を遅らせること
    ///
    /// ずれの推定は 4 回の移動平均なので、1 回の制御では動かないことがある。
    /// 制御は 1 秒ごとに 1 回だけ動く。
    #[test]
    fn sync_delays_the_audio_when_the_video_is_late() {
        let mut timeline = PlayoutTimeline::new();
        // 音声は 10 ms で届き、映像は同じ TIMESTAMP で 300 ms 遅れて届く
        observe_audio(&mut timeline, 10_010_000, 1_000_000);
        timeline.observe(Track::Video, 10_310_000, 1_000_000);
        let audio_before = timeline.playout_delay_us(Track::Audio);
        let now_us = 20_000_000;
        let delays = timeline.sync(now_us).expect("ずれが大きいので制御する");
        assert!(
            delays.audio_delay_ms * 1_000 > audio_before,
            "映像に合わせて音声を遅らせる: {delays:?}"
        );
        assert_eq!(
            timeline.playout_delay_us(Track::Audio),
            delays.audio_delay_ms * 1_000,
            "決めた遅延が反映される"
        );
        assert!(
            timeline.sync(now_us + 500_000).is_none(),
            "間隔を空けずに呼んでも制御しない"
        );
        assert!(
            timeline.sync(now_us + TIMELINE_SYNC_INTERVAL_US).is_some(),
            "間隔を空ければ制御する"
        );
    }

    /// 制御が動かないときは学習した目標遅延へ向けて下がること
    #[test]
    fn delays_decay_toward_the_learned_target() {
        let mut timeline = PlayoutTimeline::new();
        // 揺らぎが無い観測を続けて学習を 20 ms まで下げる
        for index in 0..40i64 {
            let capture_us = 1_000_000 + index * 20_000;
            observe_audio(&mut timeline, capture_us, capture_us);
            timeline.observe(Track::Video, capture_us, capture_us);
        }
        // 最初の遅延は既定の 80 ms である
        assert_eq!(timeline.playout_delay_us(Track::Audio), 80_000);
        assert_eq!(timeline.learned_delay_us(Track::Audio), 20_000);
        // 制御が動かない状態で 1 秒ごとに呼ぶと、20 ms ずつ下がる
        timeline.sync(10_000_000);
        assert_eq!(
            timeline.playout_delay_us(Track::Audio),
            60_000,
            "20 ms 下がる"
        );
        timeline.sync(11_000_000);
        timeline.sync(12_000_000);
        assert_eq!(
            timeline.playout_delay_us(Track::Audio),
            20_000,
            "学習した目標遅延で止まる"
        );
        timeline.sync(13_000_000);
        assert_eq!(
            timeline.playout_delay_us(Track::Audio),
            20_000,
            "目標より下げない"
        );
        assert_eq!(
            timeline.playout_delay_us(Track::Video),
            20_000,
            "映像も同じように下がる"
        );
    }

    /// 張り直しと学習を消しても回数は残ること
    #[test]
    fn reset_clears_the_state() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        timeline.observe(Track::Video, 10_000_000, 1_000_000);
        let rebases = timeline.rebases();
        timeline.reset();
        assert!(
            timeline.present_us(Track::Audio, 1_000_000).is_none(),
            "基準が消える"
        );
        assert_eq!(timeline.playout_delay_us(Track::Audio), 0, "遅延も消える");
        assert_eq!(timeline.rebases(), rebases, "張り直しの回数は残る");
    }
}
