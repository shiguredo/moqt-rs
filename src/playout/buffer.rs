//! 復号した映像フレームを、共有の時間軸が決めた表示時刻に合わせて選ぶキュー
//!
//! 到着のタイミングのままフレームを出し入れすると、経路の到着の揺らぎがそのまま表示間隔の
//! 揺らぎ (かくつき) になる。表示時刻は [`crate::playout::timeline::PlayoutTimeline`] が
//! LOC の TIMESTAMP と共有の基準の遅れ・再生遅延から決める (音声と同じ式)。学習 (基準の
//! 遅れ、再生遅延、フレーム間隔) は時間軸が持つ。
//!
//! - 呼び出し側が表示の機会ごとに [`PlayoutBuffer::select_nearest`] を 1 回呼び、目標表示
//!   時刻に最も近いフレームを 1 枚選ぶ (moqt-rs に vsync は無く、表示の機会は呼び出し側の
//!   ループ周期である。1 ms ループなら 1〜3 ms 程度で回る)
//! - 目標表示時刻から [`MAX_PRESENTATION_LAG_US`] より古いフレームは提示せず、捨てる
//!   フレームとして返す。この上限は「表示できるフレームが無いときだけ遅れて提示する逃げ道」
//!   の上限であり、通常の表示の遅れ (呼び出し側のループ周期) とは別である。逃げ道でも
//!   提示しないフレームは捨てる (表示を止めないが、遅れたフレームを溜めない)
//! - 提示しないフレームは呼び出し側が破棄し、計器へ記録する
//! - 表示時刻を決められないフレーム (壁時計の TIMESTAMP を持たない、または時間軸が
//!   そのトラックの TIMESTAMP を使わない) は、届いた順に 1 回の選択で 1 枚ずつ描く
//!
//! 時刻は呼び出し側が引数で渡す (すべてマイクロ秒)。現在時刻と時間軸は呼び出し側が渡し、
//! ブラウザ API には依存しない。moqt-js の `src/playout/buffer.ts` に対応する
//! (moqt-js はキューが時間軸を保持するが、本実装は呼び出し側が引数で渡す)。

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::playout::timeline::{PlayoutTimeline, Track};

/// 表示待ちのキューの上限 (枚)
///
/// 保持しているフレームは復号器のメモリを占める。30 fps で再生遅延の上限 (500 ms) を
/// 保持できる枚数に余裕を足した値にする。
pub const JITTER_BUFFER_MAX_QUEUED_FRAMES: usize = 24;

/// 表示の機会に表示できるフレームが無いときに限って、表示時刻を過ぎたフレームを遅れて
/// 提示してよい上限 (マイクロ秒)
///
/// 目標表示時刻に合うフレームがキューに無く、かつ遅れがこの範囲なら、1 枚だけ遅れて提示する。
/// 表示を止めないための逃げ道の上限であり、通常は目標に最も近いフレームを提示する。
///
/// moqt-js は rAF (vsync) の周期と提示遅延から `max(20 ms, 表示周期) + 提示遅延` へ広げるが、
/// moqt-rs に vsync は無い。表示の機会は呼び出し側のループ周期 (1 ms ループなら 1〜3 ms
/// 程度) であり、通常の表示の遅れはその周期になる。呼び出し側が周期を測れるなら、この値を
/// 周期を下限として広げてよい (この値が決めるのは逃げ道の上限だけである)。
pub const MAX_PRESENTATION_LAG_US: i64 = 20_000;

/// 表示待ちのフレーム
struct QueuedFrame<T> {
    /// フレーム
    item: T,
    /// 壁時計の TIMESTAMP (Unix epoch マイクロ秒)。壁時計として使えないフレームは `None`
    /// で、届いた順に 1 枚ずつ表示する
    timestamp_us: Option<i64>,
    /// 積んだときの時間軸の世代。基準を取り直すと表示時刻を決められなくなる
    generation: u64,
}

/// 1 回の選択の結果
#[derive(Debug)]
pub struct PlayoutNearestSelection<T> {
    /// 描くフレーム (無ければ `None`)
    pub draw: Option<T>,
    /// 提示せずに捨てるフレーム
    ///
    /// 目標表示時刻から `max_late_us` より古いフレームと、同じ目標表示時刻に重なったため
    /// 選ばれなかったフレームである。呼び出し側が破棄し、計器へ記録する。
    pub dropped: Vec<T>,
    /// 描くフレームの表示時刻 (受信側の壁時計、マイクロ秒)。表示時刻を決めずに届いた順に
    /// 描く (壁時計の TIMESTAMP を持たない) フレームと、描くフレームが無いときは `None`
    pub draw_presentation_us: Option<i64>,
    /// 目標表示時刻を過ぎたフレームを、表示できるフレームが無いため遅れて提示したか
    pub draw_late: bool,
}

/// 復号したフレームを積み、目標表示時刻に最も近い 1 枚を選ぶ
///
/// フレームは積んだ順に並べたまま扱い、並べ替えない (復号の出力は TIMESTAMP の順である)。
pub struct PlayoutBuffer<T> {
    /// 表示待ちのキューの上限 (枚)。超えたら古い方から捨てる
    max_queued_frames: usize,
    /// 表示時刻を決める相手のトラック
    track: Track,
    /// 表示待ちのフレーム
    queue: VecDeque<QueuedFrame<T>>,
}

impl<T> PlayoutBuffer<T> {
    /// 表示待ちのキューの上限とトラックを指定して作る
    ///
    /// 上限を超えたフレームは [`PlayoutBuffer::enqueue`] が古い方から返す。
    pub fn new(max_queued_frames: usize, track: Track) -> Self {
        Self {
            max_queued_frames,
            track,
            queue: VecDeque::new(),
        }
    }

    /// 表示待ちのフレーム数
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// 表示待ちのフレームが無いかどうか
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// 復号したフレームを積む
    ///
    /// `timestamp_us` は壁時計の TIMESTAMP (Unix epoch マイクロ秒)。壁時計として使えない
    /// フレーム (Timescale あり / TIMESTAMP 無し) は `None` を渡す。世代は [`PlayoutTimeline`]
    /// から読む。戻り値はキューの上限を超えたため捨てるフレーム (古い方から)。
    pub fn enqueue(
        &mut self,
        item: T,
        timestamp_us: Option<i64>,
        timeline: &PlayoutTimeline,
    ) -> Vec<T> {
        self.queue.push_back(QueuedFrame {
            item,
            timestamp_us,
            generation: timeline.generation(),
        });
        let mut overflow = Vec::new();
        while self.queue.len() > self.max_queued_frames {
            let Some(frame) = self.queue.pop_front() else {
                break;
            };
            overflow.push(frame.item);
        }
        overflow
    }

    /// 目標表示時刻に最も近いフレームを 1 枚選ぶ (表示の機会ごとに呼ぶ)
    ///
    /// 先頭が表示時刻を決められないフレームなら、それを描く (届いた順に 1 枚ずつ)。
    /// 表示時刻を決められるフレームでは、目標表示時刻から許容幅以内で最も近いものを描く。
    /// 許容幅を超えて古いフレームでも、遅れが `max_late_us` 以内なら 1 枚だけ遅れて描く
    /// ([`PlayoutNearestSelection::draw_late`])。それより古いフレームは捨てるフレームとして
    /// 返す。許容幅より未来のフレームしかない場合は何も描かない。
    ///
    /// 選んだフレームより前にあるフレームは、この表示の機会でも次の機会でも提示できない
    /// ため、捨てるフレームとして返す。
    ///
    /// moqt-js は非有限の引数と負の許容幅・負の上限を例外で拒否する。本実装の引数は i64 で
    /// あり非有限の値は表現できないが、負の値は呼び出し側の誤りである。debug ビルドでは
    /// `debug_assert!` で停止する。release ビルドではフレームを落としすぎないよう 0 として
    /// 扱うが、これは事故を防ぐ最後の砦であり、意図した値ではない。特に負の `max_late_us`
    /// はすべてのフレームを捨てる対象にするため、渡さないこと。
    ///
    /// # 引数
    ///
    /// - `target_us`: この選択で表示したい時刻 ([`PlayoutTimeline::present_us`] と同じ軸の
    ///   受信側の壁時計、Unix epoch マイクロ秒)
    /// - `tolerance_us`: 目標表示時刻からの許容幅 (マイクロ秒)。負は呼び出し側の誤り
    /// - `max_late_us`: 遅れて提示してよい上限 (マイクロ秒)。負は呼び出し側の誤り。指定
    ///   しない場合に相当する値は [`MAX_PRESENTATION_LAG_US`] であり、表示の機会の周期が
    ///   分かるならその周期を下限として広げてよい
    /// - `timeline`: 表示時刻を決める共有の時間軸
    pub fn select_nearest(
        &mut self,
        target_us: i64,
        tolerance_us: i64,
        max_late_us: i64,
        timeline: &PlayoutTimeline,
    ) -> PlayoutNearestSelection<T> {
        // moqt-js は負の許容幅と遅れの上限を例外で拒否する。i64 では非有限を表現できない
        // ため、負の値は呼び出し側の誤りとして debug ビルドでは停止する。release ビルドでは
        // フレームを落としすぎないよう 0 として扱う (事故を防ぐ最後の砦)
        debug_assert!(
            tolerance_us >= 0,
            "tolerance_us must be non-negative, got {tolerance_us}"
        );
        debug_assert!(
            max_late_us >= 0,
            "max_late_us must be non-negative, got {max_late_us}"
        );
        let tolerance_us = tolerance_us.max(0);
        let max_late_us = max_late_us.max(0);
        let oldest_allowed_us = target_us.saturating_sub(max_late_us);
        let newest_allowed_us = target_us.saturating_add(tolerance_us);

        let Some(head) = self.queue.front() else {
            return PlayoutNearestSelection {
                draw: None,
                dropped: Vec::new(),
                draw_presentation_us: None,
                draw_late: false,
            };
        };
        if self.frame_presentation_us(head, timeline).is_none() {
            let frame = self
                .queue
                .pop_front()
                .expect("front frame exists because the queue was checked");
            return PlayoutNearestSelection {
                draw: Some(frame.item),
                dropped: Vec::new(),
                draw_presentation_us: None,
                draw_late: false,
            };
        }

        // 表示時刻を過ぎて久しいフレームは、目標表示時刻に合うフレームが無いときの逃げ道
        // (max_late_us) にも使えないため、先に捨てる
        let mut dropped = Vec::new();
        while let Some(frame) = self.queue.front() {
            let Some(presentation_us) = self.frame_presentation_us(frame, timeline) else {
                break;
            };
            if presentation_us >= oldest_allowed_us {
                break;
            }
            let frame = self
                .queue
                .pop_front()
                .expect("front frame exists because the queue was checked");
            dropped.push(frame.item);
        }

        // 許容幅の中から目標表示時刻に最も近いフレームを探す。距離が同じなら先に積んだ
        // フレームを選ぶ (moqt-js と同じ)
        let mut best_index = None;
        let mut best_distance_us = i64::MAX;
        let mut best_presentation_us = None;
        for (index, frame) in self.queue.iter().enumerate() {
            let Some(presentation_us) = self.frame_presentation_us(frame, timeline) else {
                break;
            };
            if presentation_us > newest_allowed_us {
                break;
            }
            let distance_us = presentation_us.saturating_sub(target_us).saturating_abs();
            if distance_us < best_distance_us {
                best_index = Some(index);
                best_distance_us = distance_us;
                best_presentation_us = Some(presentation_us);
            }
        }
        let (Some(best_index), Some(best_presentation_us)) = (best_index, best_presentation_us)
        else {
            // 目標表示時刻に合うフレームは無い。未来のフレームだけなら待つ
            return PlayoutNearestSelection {
                draw: None,
                dropped,
                draw_presentation_us: None,
                draw_late: false,
            };
        };

        // 選んだフレームより前のフレームは、この表示の機会でも次の機会でも提示できない
        let mut skipped: Vec<T> = self
            .queue
            .drain(..best_index)
            .map(|frame| frame.item)
            .collect();
        dropped.append(&mut skipped);
        let drawn = self
            .queue
            .pop_front()
            .expect("frame to draw exists because the selection was made");
        let draw_late = best_presentation_us < target_us.saturating_sub(tolerance_us);
        PlayoutNearestSelection {
            draw: Some(drawn.item),
            dropped,
            draw_presentation_us: Some(best_presentation_us),
            draw_late,
        }
    }

    /// 壁時計の TIMESTAMP のフレームの表示時刻 (受信側の壁時計、マイクロ秒)
    ///
    /// まだ基準が無いときと、このトラックの TIMESTAMP を使わないときは `None`。
    pub fn presentation_time_us(
        &self,
        timestamp_us: Option<i64>,
        timeline: &PlayoutTimeline,
    ) -> Option<i64> {
        let timestamp_us = timestamp_us?;
        timeline.present_us(self.track, timestamp_us)
    }

    /// いま使っている jitter buffer の遅延 (マイクロ秒)。まだ観測していなければ `None`
    ///
    /// moqt-js の `PlayoutBuffer.playoutDelayMs` は映像専用のキューであり、時間軸の
    /// `videoDelayMs` へ委譲する。このキューはトラックを指定できるため、映像なら映像の
    /// jitter buffer の遅延 ([`PlayoutTimeline::video_delay_us`]) を返す。音声では
    /// moqt-js に対応する呼び出しが無く (`videoDelayMs` を返すのは意味が違う)、音声の
    /// 表示の遅れのうち基準を除いた分
    /// ([`PlayoutTimeline::presentation_extra_delay_us`]) を返す。
    pub fn playout_delay_us(&self, timeline: &PlayoutTimeline) -> Option<i64> {
        match self.track {
            Track::Audio => timeline.presentation_extra_delay_us(),
            Track::Video => timeline.video_delay_us(),
        }
    }

    /// 表示待ちのフレームをすべて取り出す
    pub fn clear(&mut self) -> Vec<T> {
        self.queue.drain(..).map(|frame| frame.item).collect()
    }

    /// 積んだときの世代が今と同じフレームの表示時刻
    ///
    /// 基準を取り直した後に残っているフレームは、新しい基準では表示時刻が飛びの分だけ
    /// 未来になる。決められないものとして `None` を返し、届いた順に描く
    /// (取り直し前の [`PlayoutBuffer`] が積んでいたフレームの TIMESTAMP を消していたのと
    /// 同じ扱い)。
    fn frame_presentation_us(
        &self,
        frame: &QueuedFrame<T>,
        timeline: &PlayoutTimeline,
    ) -> Option<i64> {
        if frame.generation != timeline.generation() {
            return None;
        }
        self.presentation_time_us(frame.timestamp_us, timeline)
    }
}
