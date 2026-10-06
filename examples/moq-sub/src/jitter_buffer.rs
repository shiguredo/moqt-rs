//! 到着の揺らぎを吸収する音声の jitter buffer
//!
//! デコード済みの音声を、LOC の TIMESTAMP と受信側の壁時計から決まる鳴らす時刻まで
//! 保持する。時計も再生機器も触らず、現在時刻は呼び出し側が引数で渡す。

use std::collections::VecDeque;

use shiguredo_moqt::playout::scheduler::AUDIO_PLAYOUT_MAX_LATENESS_US;
use shiguredo_moqt::playout::timeline::{PlayoutTimeline, Track};

use crate::decoder::DecodedAudioFrame;

/// 保持する音声の上限 (件数)
///
/// 目標遅延の学習 (`playout::delay`) が扱う上限は 20 ms × 100 バケット = 2 秒で、
/// Opus の 20 ms フレームでは 100 枚にあたる。これを超えて保持しても鳴らす機会が無い
/// ため、余裕を見た件数で打ち切る。
const MAX_PENDING_FRAMES: usize = 128;

/// 鳴らす時刻まで保持する音声
struct PendingAudio {
    /// 保持している音声
    frame: DecodedAudioFrame,
    /// 鳴らす時刻 (受信側の壁時計、マイクロ秒)
    play_at_us: i64,
}

/// 音声を鳴らす時刻まで保持する jitter buffer
///
/// [`AudioJitterBuffer::push`] に到着時刻を渡すと、LOC の TIMESTAMP と受信側の壁時計の
/// 関係を学習して鳴らす時刻を決める。[`AudioJitterBuffer::pop_releasable`] はその時刻の
/// 手前まで来た音を返す。
///
/// 鳴らす順は届いた順のままにする (復号の出力は TIMESTAMP の順である)。表示の遅れが
/// 下がると、後から届いた音の方が鳴らす時刻が早くなり得るため、鳴らす時刻で並べ替えると
/// 音の順が入れ替わる。
///
/// 保持する件数には上限があり、超えた分は古い方から捨てる。再生が止まったあとに古い音を
/// まとめて鳴らさないよう、[`AudioJitterBuffer::drop_late`] で閾値を超えて遅れた音も捨てる。
pub struct AudioJitterBuffer {
    /// 保持中の音声 (届いた順)
    pending: VecDeque<PendingAudio>,
    /// 遅れすぎて捨てた数
    dropped_late: u64,
    /// 保持数の上限を超えて捨てた数
    dropped_overflow: u64,
}

impl AudioJitterBuffer {
    /// 何も保持していない状態で作る
    pub fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            dropped_late: 0,
            dropped_overflow: 0,
        }
    }

    /// デコード済みの音声を 1 つ加える
    ///
    /// `now_us` は到着時刻 (受信側の壁時計、マイクロ秒)。`timeline` は映像と共有する時間軸
    /// であり、ここで観測を足す。鳴らす時刻が観測から決まらないとき (基準がまだ無い、基準を
    /// 取り直した直後) は `now_us` とし、すぐ鳴らせる音として扱う。
    pub fn push(&mut self, frame: DecodedAudioFrame, now_us: i64, timeline: &mut PlayoutTimeline) {
        let timestamp_us = frame.pts_us;
        timeline.observe(Track::Audio, now_us, timestamp_us);
        let play_at_us = timeline
            .present_us(Track::Audio, timestamp_us)
            .unwrap_or(now_us);
        // 届いた順のまま保持する。復号の出力は TIMESTAMP の順であり、鳴らす時刻で
        // 並べ替えると、表示の遅れが下がったときに音の順が入れ替わる
        self.pending.push_back(PendingAudio { frame, play_at_us });
        // 上限を超えた分は古い方から捨てる。遅れを伸ばし続けないためである
        if self.pending.len() > MAX_PENDING_FRAMES {
            self.pending.pop_front();
            self.dropped_overflow += 1;
        }
    }

    /// 鳴らす時刻の `lead_us` 手前まで来た最も古い音声を取り出す
    ///
    /// `lead_us` は再生機器のバッファが空にならないよう先行して積む分である。
    /// まだ早い音しか無いときは `None` を返す。
    pub fn pop_releasable(&mut self, now_us: i64, lead_us: i64) -> Option<DecodedAudioFrame> {
        let play_at_us = self.pending.front()?.play_at_us;
        if play_at_us.saturating_sub(lead_us) > now_us {
            return None;
        }
        self.pending.pop_front().map(|pending| pending.frame)
    }

    /// 閾値を超えて遅れた音声を捨てて、捨てた数を返す
    ///
    /// 閾値は `playout::scheduler` が使う値と同じものを使い、example 側で独自の値を
    /// 決めない。
    pub fn drop_late(&mut self, now_us: i64) -> u64 {
        let mut dropped = 0;
        while let Some(pending) = self.pending.front() {
            if pending
                .play_at_us
                .saturating_add(AUDIO_PLAYOUT_MAX_LATENESS_US)
                >= now_us
            {
                break;
            }
            self.pending.pop_front();
            dropped += 1;
        }
        self.dropped_late += dropped;
        dropped
    }

    /// 鳴らす時刻に関わらず最も早い音声を取り出す
    ///
    /// 配信が終わったときに保持している音をすべて吐き出すために使う。
    pub fn pop_oldest(&mut self) -> Option<DecodedAudioFrame> {
        self.pending.pop_front().map(|pending| pending.frame)
    }

    /// 保持している音声の数
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// 保持している音声が無いかどうか
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// 遅れすぎて捨てた数
    pub fn dropped_late(&self) -> u64 {
        self.dropped_late
    }

    /// 保持数の上限を超えて捨てた数
    pub fn dropped_overflow(&self) -> u64 {
        self.dropped_overflow
    }
}

impl Default for AudioJitterBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用の音声フレームを作る。`channels` は同一の TIMESTAMP を持つフレームを
    /// 区別するために使う
    fn audio_frame(pts_us: i64, channels: u8) -> DecodedAudioFrame {
        DecodedAudioFrame {
            pcm: vec![0; 2],
            sample_rate: 48_000,
            channels,
            pts_us,
        }
    }

    /// テスト用の組 (時間軸 + jitter buffer)
    ///
    /// `AudioJitterBuffer::push` は映像と共有する時間軸を受け取る。テストでも実際の型を
    /// 1 つにまとめて持ち、振る舞いを差し替えずに呼び出しを短くする。
    struct Fixture {
        timeline: PlayoutTimeline,
        buffer: AudioJitterBuffer,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                timeline: PlayoutTimeline::new(),
                buffer: AudioJitterBuffer::new(),
            }
        }

        /// 到着時刻を固定して 1 つ加える
        fn push_at(&mut self, pts_us: i64, channels: u8, now_us: i64) {
            self.push(audio_frame(pts_us, channels), now_us);
        }

        fn push(&mut self, frame: DecodedAudioFrame, now_us: i64) {
            self.buffer.push(frame, now_us, &mut self.timeline);
        }

        /// いま時間軸が決めている目標遅延 (マイクロ秒)
        fn target_delay_us(&self) -> Option<i64> {
            self.timeline.presentation_delay_us(Track::Audio)
        }

        fn len(&self) -> usize {
            self.buffer.len()
        }

        fn is_empty(&self) -> bool {
            self.buffer.is_empty()
        }

        fn pop_releasable(&mut self, now_us: i64, lead_us: i64) -> Option<DecodedAudioFrame> {
            self.buffer.pop_releasable(now_us, lead_us)
        }

        fn pop_oldest(&mut self) -> Option<DecodedAudioFrame> {
            self.buffer.pop_oldest()
        }

        fn drop_late(&mut self, now_us: i64) -> u64 {
            self.buffer.drop_late(now_us)
        }

        fn dropped_late(&self) -> u64 {
            self.buffer.dropped_late()
        }

        fn dropped_overflow(&self) -> u64 {
            self.buffer.dropped_overflow()
        }
    }

    #[test]
    fn push_keeps_arrival_order() {
        let mut buffer = Fixture::new();
        // 復号の出力は TIMESTAMP の順であり、並べ替えはしない
        buffer.push_at(8_000_000, 1, 10_000_000);
        buffer.push_at(8_020_000, 2, 10_020_000);
        assert_eq!(buffer.len(), 2, "両方とも保持すること");
        assert_eq!(
            buffer
                .pop_oldest()
                .expect("保持しているので取り出せること")
                .channels,
            1,
            "先に届いた音を先に取り出すこと"
        );
        assert_eq!(
            buffer
                .pop_oldest()
                .expect("保持しているので取り出せること")
                .channels,
            2,
            "残りの音も取り出せること"
        );
        assert!(buffer.is_empty(), "すべて取り出したら空になること");
    }

    #[test]
    fn pop_releasable_waits_until_lead_time() {
        let mut buffer = Fixture::new();
        let now_us = 1_000_000;
        buffer.push_at(0, 1, now_us);
        let delay_us = buffer
            .target_delay_us()
            .expect("観測したので目標遅延が決まること");
        assert!(delay_us > 0, "目標遅延は正の値になること");
        assert!(
            buffer.pop_releasable(now_us, delay_us - 1).is_none(),
            "目標遅延の手前では取り出さないこと"
        );
        assert!(
            buffer.pop_releasable(now_us, delay_us).is_some(),
            "目標遅延ぶん先行すれば取り出せること"
        );
        assert!(buffer.is_empty(), "取り出したら空になること");
    }

    #[test]
    fn pop_releasable_returns_none_when_empty() {
        let mut buffer = Fixture::new();
        assert!(
            buffer.pop_releasable(1_000_000, 0).is_none(),
            "空なら取り出せないこと"
        );
        assert!(buffer.pop_oldest().is_none(), "空なら取り出せないこと");
    }

    #[test]
    fn same_play_time_keeps_arrival_order() {
        let mut buffer = Fixture::new();
        // 同じ TIMESTAMP の音は鳴らす時刻も同じになる。到着順を保つこと
        buffer.push_at(0, 1, 10_000_000);
        buffer.push_at(0, 2, 10_000_001);
        assert_eq!(
            buffer
                .pop_oldest()
                .expect("保持しているので取り出せること")
                .channels,
            1,
            "先に届いた音を先に取り出すこと"
        );
        assert_eq!(
            buffer
                .pop_oldest()
                .expect("保持しているので取り出せること")
                .channels,
            2,
            "後から届いた音を後に取り出すこと"
        );
    }

    #[test]
    fn drop_late_removes_unplayable_audio() {
        let mut buffer = Fixture::new();
        let now_us = 1_000_000;
        buffer.push_at(0, 1, now_us);
        let delay_us = buffer
            .target_delay_us()
            .expect("観測したので目標遅延が決まること");
        let play_at_us = now_us + delay_us;
        assert_eq!(
            buffer.drop_late(play_at_us),
            0,
            "鳴らす時刻ちょうどでは捨てないこと"
        );
        assert_eq!(
            buffer.drop_late(play_at_us + AUDIO_PLAYOUT_MAX_LATENESS_US),
            0,
            "閾値ちょうどでは捨てないこと"
        );
        assert_eq!(
            buffer.drop_late(play_at_us + AUDIO_PLAYOUT_MAX_LATENESS_US + 1),
            1,
            "閾値を超えて遅れた音は捨てること"
        );
        assert_eq!(buffer.dropped_late(), 1, "捨てた数を計上すること");
        assert!(buffer.is_empty(), "捨てたら空になること");
    }

    #[test]
    fn quiet_arrival_decreases_target_delay() {
        let mut buffer = Fixture::new();
        let mut timestamp_us = 0;
        let mut arrival_us = 10_000_000;
        buffer.push_at(timestamp_us, 1, arrival_us);
        let initial_delay_us = buffer
            .target_delay_us()
            .expect("観測したので目標遅延が決まること");
        // 揺らぎの無いまま 4 秒ぶん加える。目標遅延は学習値へ向けて下がること
        for _ in 0..100 {
            timestamp_us += 40_000;
            arrival_us += 40_000;
            buffer.push_at(timestamp_us, 1, arrival_us);
        }
        let quiet_delay_us = buffer
            .target_delay_us()
            .expect("観測したので目標遅延が決まること");
        assert!(
            quiet_delay_us < initial_delay_us,
            "揺らぎが無いまま時間が進むと目標遅延を縮めること"
        );
        assert!(quiet_delay_us > 0, "目標遅延は正の値のままであること");
    }

    #[test]
    fn arrival_jitter_increases_target_delay() {
        let mut buffer = Fixture::new();
        // 40 ms 間隔で届く音声を 2 秒ぶん加える (揺らぎは無い)
        let mut timestamp_us = 0;
        let mut arrival_us = 10_000_000;
        for _ in 0..50 {
            buffer.push_at(timestamp_us, 1, arrival_us);
            timestamp_us += 40_000;
            arrival_us += 40_000;
        }
        let quiet_delay_us = buffer
            .target_delay_us()
            .expect("観測したので目標遅延が決まること");
        // 200 ms 遅れて届く音声を混ぜ、学習の区間 (500 ms) を 1 つ超えるまで加える
        buffer.push_at(timestamp_us, 2, arrival_us + 200_000);
        for _ in 0..20 {
            timestamp_us += 40_000;
            arrival_us += 40_000;
            buffer.push_at(timestamp_us, 3, arrival_us);
        }
        let jittery_delay_us = buffer
            .target_delay_us()
            .expect("観測したので目標遅延が決まること");
        assert!(
            jittery_delay_us > quiet_delay_us,
            "揺らぎが増えたら目標遅延を伸ばすこと quiet={quiet_delay_us} jittery={jittery_delay_us}"
        );
        // 伸びた目標遅延でも、届いた順のまま取り出せること
        let mut previous_pts_us = None;
        let mut released = 0;
        while let Some(frame) = buffer.pop_releasable(i64::MAX / 2, 0) {
            if let Some(previous_pts_us) = previous_pts_us {
                assert!(
                    frame.pts_us > previous_pts_us,
                    "取り出す順が届いた順のままであること"
                );
            }
            previous_pts_us = Some(frame.pts_us);
            released += 1;
        }
        assert!(released > 0, "取り出せる音があること");
    }

    #[test]
    fn overflow_drops_oldest_audio() {
        let mut buffer = Fixture::new();
        let base_us = 1_000_000;
        for i in 0..=MAX_PENDING_FRAMES as i64 {
            // TIMESTAMP と到着時刻を同じ間隔で進め、鳴らす時刻の順を到着順に揃える
            buffer.push_at(i * 20_000, 1, base_us + i * 20_000);
        }
        assert_eq!(
            buffer.len(),
            MAX_PENDING_FRAMES,
            "上限を超えた分は保持しないこと"
        );
        assert_eq!(buffer.dropped_overflow(), 1, "上限超過を計上すること");
        assert_eq!(
            buffer
                .pop_oldest()
                .expect("保持しているので取り出せること")
                .pts_us,
            20_000,
            "最も早い音から捨てること"
        );
    }
}
