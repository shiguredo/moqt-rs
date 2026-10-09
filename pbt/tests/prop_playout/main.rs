//! playout モジュールのプロパティベーステスト
//!
//! 表示時刻に合わせたフレームの選択 (buffer)、目標遅延の学習 (delay) と閉ループの調整
//! (feedback)、鳴らす時刻の決定 (scheduler)、波形の周期による時間圧縮・伸長 (stretch)、
//! 共通の時間軸 (timeline)、音声の再生の観測値 (timing) のプロパティを検証する。

mod buffer;
mod delay;
mod feedback;
mod scheduler;
mod stretch;
mod timeline;
mod timing;
