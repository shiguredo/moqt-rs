//! MoQT パブリッシャーの中核 (lib ターゲット)
//!
//! バイナリ (`main.rs`) と E2E テストの双方から使う。tracing の初期化、Ctrl+C の
//! 待ち受け、tokio ランタイムの構築といったバイナリ固有の処理はここには置かない。
//!
//! 呼び出し側は [`cli::Config`] を組み立て、[`pipeline::run`] に
//! `tokio_metrics::TaskMonitor` と `tokio_utils::ShutdownMonitor` を渡して起動する。
//!
//! draft-ietf-moq-transport-22、draft-ietf-moq-loc-04、draft-ietf-moq-msf-01 に準拠。

// 呼び出し側が設定を組み立ててパイプラインを起動するために公開する
pub mod cli;
pub mod error;
pub mod pipeline;

// パイプラインの内部実装
mod audio_capture;
mod capture;
mod catalog;
mod datagram_writer;
mod decoder;
mod encoder;
mod fake_audio_capture;
mod fake_capture;
mod mp4;
mod stream_writer;
