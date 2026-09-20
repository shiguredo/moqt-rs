//! MoQT サブスクライバーの中核 (lib ターゲット)
//!
//! バイナリ (`main.rs`) と E2E テストの双方から使う。tracing の初期化、Ctrl+C の
//! 待ち受け、tokio ランタイムの構築、SDL プレイヤーの実行といったバイナリ固有の処理は
//! ここには置かない。
//!
//! 呼び出し側は [`cli::Config`] を組み立て、デコード済みフレームを受け取るチャネル、
//! `tokio_metrics::TaskMonitor`、`tokio_utils::ShutdownMonitor`、表示待ちフレーム数、
//! 再生側の終了通知を用意して [`pipeline::run`] を起動する。再生を行わない呼び出し側は
//! 受け取ったフレームを破棄すればよい (SDL プレイヤーは lib に含まれない)。
//!
//! draft-ietf-moq-transport-22、draft-ietf-moq-loc-04、draft-ietf-moq-msf-01 に準拠。

// 呼び出し側が設定を組み立て、デコード済みフレームを受け取るために公開する
pub mod cli;
pub mod error;
pub mod pipeline;

// 呼び出し側がチャネルの型として必要とするため、クレート直下へ再公開する
pub use decoder::{DecodedAudioFrame, DecodedVideoFrame};

// パイプラインの内部実装
mod decoder;
mod mp4;
mod stream_reader;
