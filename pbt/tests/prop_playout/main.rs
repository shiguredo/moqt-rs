//! playout モジュールのプロパティベーステスト
//!
//! 目標遅延の学習 (delay)、波形の周期による時間圧縮・伸長 (stretch)、
//! A/V 同期の遅延制御 (sync) のプロパティを検証する。

mod delay;
mod stretch;
mod sync;
