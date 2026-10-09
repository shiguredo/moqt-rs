//! 音声の再生に関する処理
//!
//! 音声の再生に使う純粋な処理 (時間圧縮・伸長、目標遅延の学習と閉ループの調整、鳴らす
//! 時刻の決定、表示時刻に合わせたフレームの選択、A/V 同期の遅延制御、再生の観測値など) を
//! 置く。I/O・出力デバイス・タイマーには触れず、時刻やサンプルは呼び出し側が引数で渡す。

pub mod buffer;
pub mod delay;
pub mod feedback;
pub mod scheduler;
pub mod stretch;
pub mod timeline;
pub mod timing;
