//! 音声の再生に関する処理
//!
//! 音声の再生に使う純粋な処理 (到着の揺らぎと実出力の先行量からの再生予算、鳴らす時刻の
//! 決定、表示時刻に合わせたフレームの選択、A/V 同期の遅延制御、再生の観測値など) を置く。
//! I/O・出力デバイス・タイマーには触れず、時刻やサンプルは呼び出し側が引数で渡す。

pub mod buffer;
pub mod catch_up;
pub mod expansion;
pub mod gap;
pub mod predictor;
pub mod publish_catch_up;
pub mod scheduler;
pub mod splice_fade;
pub mod stretch;
pub mod timeline;
pub mod timing;
