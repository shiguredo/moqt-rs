//! MOQT サブスクライバーの中核 (lib ターゲット)
//!
//! バイナリ (`main.rs`) と E2E テストの双方から使う。tracing の初期化、終了シグナルの
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

/// 現在時刻を Unix epoch からのマイクロ秒で返す
///
/// 受信側の時刻と、復号の出力が送られた TIMESTAMP からどれだけ遅れているかの観測に使う。
/// 呼び出し側のプレイヤーも同じ時計を使う。
pub fn wall_clock_us() -> i64 {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX)
}

// パイプラインの内部実装
mod catalog;
mod decoder;
mod mp4;
mod stream_reader;
