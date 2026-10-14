//! playout モジュールのプロパティベーステスト
//!
//! 表示時刻に合わせたフレームの選択 (buffer)、受信側の音声の追いつき (catch_up)、目標遅延の
//! 学習 (delay) と閉ループの調整 (feedback)、鳴らす時刻の決定 (scheduler)、波形の周期に
//! よる時間圧縮・伸長 (stretch)、共通の時間軸 (timeline)、音声の再生の観測値 (timing) の
//! プロパティを検証する。

mod buffer;
mod catch_up;
mod delay;
mod feedback;
mod scheduler;
mod stretch;
mod timeline;
mod timing;
