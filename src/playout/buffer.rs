//! 復号したフレームを表示時刻に合わせて選ぶキュー
//!
//! 到着のタイミングのままフレームを出し入れすると、経路の到着の揺らぎがそのまま表示間隔の
//! 揺らぎになる。表示時刻は [`crate::playout::timeline::PlayoutTimeline`] が LOC の
//! TIMESTAMP と共有の基準の遅れ・表示の遅れから決める。学習 (基準の遅れ、表示の遅れ、
//! フレーム間隔) は時間軸が持つ。
//!
//! 時計もデバイスも触らず、現在時刻と時間軸は呼び出し側が引数で渡す。

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::playout::timeline::{PlayoutTimeline, Track};

/// 表示時刻を過ぎたフレームを捨てずに描く、表示時刻からの遅れの上限 (マイクロ秒)
///
/// 60 Hz の表示周期 (16.7 ms) 程度にする。この範囲の遅れは目で分からず、配信 fps と
/// 表示周期が近いときに位相や取得の間隔の揺れで重なったフレームを捨てずに、後の周期で
/// 追いつける。30 fps では 2 枚が表示時刻を過ぎると古い方は 1 フレーム (33.3 ms) 遅れて
/// いるため捨て、最新を描く。
pub const MAX_PRESENTATION_LAG_US: i64 = 20_000;

/// 表示待ちのキューの上限 (枚)
///
/// 保持しているフレームは復号器のメモリを占める。30 fps で表示の遅れの上限 (500 ms) を
/// 保持できる枚数に余裕を足した値にする。
pub const JITTER_BUFFER_MAX_QUEUED_FRAMES: usize = 24;

/// 表示待ちのフレーム
struct QueuedFrame<T> {
    /// フレーム
    item: T,
    /// 壁時計の TIMESTAMP (Unix epoch マイクロ秒)。壁時計として使えないフレームは `None`
    timestamp_us: Option<i64>,
    /// 積んだときの時間軸の世代。基準を取り直すと表示時刻を決められなくなる
    generation: u64,
}

/// 1 回の選択の結果
#[derive(Debug)]
pub struct PlayoutSelection<T> {
    /// 描くフレーム (無ければ `None`)
    pub draw: Option<T>,
    /// 表示時刻を過ぎたが、より新しいフレームが 2 枚以上表示時刻を過ぎていたため捨てるフレーム
    pub late: Vec<T>,
    /// 描くフレームの表示時刻 (受信側の壁時計、マイクロ秒)。表示時刻を決めずに届いた順に
    /// 描くフレームと、描くフレームが無いときは `None`
    pub draw_presentation_us: Option<i64>,
}

/// 復号したフレームを積み、表示時刻に合わせて選ぶ
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

    /// 表示するフレームを選ぶ (表示周期ごとに呼ぶ)
    ///
    /// 先頭が表示時刻を決められないフレームなら、それを描く (届いた順に 1 枚ずつ)。
    /// 先頭が表示時刻前なら何も描かずに待つ。
    pub fn select(&mut self, now_us: i64, timeline: &PlayoutTimeline) -> PlayoutSelection<T> {
        let Some(head) = self.queue.front() else {
            return PlayoutSelection {
                draw: None,
                late: Vec::new(),
                draw_presentation_us: None,
            };
        };
        if self.frame_presentation_us(head, timeline).is_none() {
            let frame = self
                .queue
                .pop_front()
                .expect("front frame exists because the queue was checked");
            return PlayoutSelection {
                draw: Some(frame.item),
                late: Vec::new(),
                draw_presentation_us: None,
            };
        }

        // 表示時刻を過ぎた最後のフレームを探す。表示時刻を決められないフレームと、まだ
        // 表示時刻前のフレームで止める
        let mut last_due = None;
        for (index, frame) in self.queue.iter().enumerate() {
            match self.frame_presentation_us(frame, timeline) {
                Some(presentation_us) if presentation_us <= now_us => last_due = Some(index),
                _ => break,
            }
        }
        let Some(last_due) = last_due else {
            return PlayoutSelection {
                draw: None,
                late: Vec::new(),
                draw_presentation_us: None,
            };
        };

        // 表示時刻から上限を超えて遅れたフレームを捨てる (最新の 1 枚は残す)
        let mut draw_index = 0;
        while draw_index < last_due {
            let Some(frame) = self.queue.get(draw_index) else {
                break;
            };
            match self.frame_presentation_us(frame, timeline) {
                Some(presentation_us)
                    if presentation_us < now_us.saturating_sub(MAX_PRESENTATION_LAG_US) =>
                {
                    draw_index += 1;
                }
                _ => break,
            }
        }
        let late: Vec<T> = self
            .queue
            .drain(..draw_index)
            .map(|frame| frame.item)
            .collect();
        let drawn = self
            .queue
            .pop_front()
            .expect("frame to draw exists because it is due");
        let draw_presentation_us = self.frame_presentation_us(&drawn, timeline);
        PlayoutSelection {
            draw: Some(drawn.item),
            late,
            draw_presentation_us,
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

    /// いま使っている表示の遅れ (マイクロ秒)。基準がまだ無ければ `None`
    pub fn playout_delay_us(&self, timeline: &PlayoutTimeline) -> Option<i64> {
        timeline.presentation_delay_us(self.track)
    }

    /// 表示待ちのフレームをすべて取り出す
    pub fn clear(&mut self) -> Vec<T> {
        self.queue.drain(..).map(|frame| frame.item).collect()
    }

    /// 積んだときの世代が今と同じフレームの表示時刻
    ///
    /// 基準を取り直した後に残っているフレームは、新しい基準では表示時刻が飛びの分だけ
    /// 未来になる。決められないものとして `None` を返し、届いた順に描く。
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
