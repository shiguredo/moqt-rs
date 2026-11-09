//! tobi-quic の qlog 出力
//!
//! 環境変数 `QLOGDIR` が設定されているときだけ、接続ごとに JSON-SEQ 形式の qlog
//! (`*.sqlog`) を `QLOGDIR` 配下へ書き出すファクトリを提供する。設定が無い場合は
//! qlog を無効化する (draft-ietf-quic-qlog-main-schema-14 §14.2 の「明示的な操作
//! なしに qlog を有効化しない」に沿う)。qlog は接続 ID やフレーム内容を含むため、
//! 機微情報を扱う場合は `shiguredo-no-secrets` スキルを参照すること。

use std::fs::File;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tobi_quic::qlog::JsonSeqQlogWriter;
use tobi_quic::{QlogWriter, QlogWriterFactory};

/// `QLOGDIR` 配下へ JSON-SEQ 形式で qlog を書くファクトリ
pub struct DirFactory {
    /// 出力先ディレクトリ
    dir: String,
    /// 接続ごとのファイル名に付ける通し番号
    seq: AtomicU64,
}

impl DirFactory {
    /// 環境変数 `QLOGDIR` が設定されている場合だけファクトリを作る
    pub fn from_env() -> Option<Arc<dyn QlogWriterFactory>> {
        let dir = std::env::var("QLOGDIR").ok()?;
        if dir.is_empty() {
            return None;
        }
        if let Err(err) = std::fs::create_dir_all(&dir) {
            tracing::warn!("failed to create QLOGDIR {dir}: {err}");
            return None;
        }
        Some(Arc::new(Self {
            dir,
            seq: AtomicU64::new(0),
        }))
    }
}

impl QlogWriterFactory for DirFactory {
    fn create(&self, odcid: &[u8]) -> Option<Box<dyn QlogWriter>> {
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        let hex: String = odcid.iter().map(|b| format!("{b:02x}")).collect();
        let path = format!("{}/client-{:02}-{}.sqlog", self.dir, n, hex);
        tracing::info!("writing qlog to {path}");
        let file = File::create(&path).ok()?;
        // vantage point は client 固定。title も client で統一する
        let writer = JsonSeqQlogWriter::new(Box::new(file), "client", "client").ok()?;
        Some(Box::new(writer))
    }
}
