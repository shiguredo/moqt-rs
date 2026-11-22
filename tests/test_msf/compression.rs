//! MSF_COMPRESSION property の値のテスト
//!
//! draft-ietf-moq-msf-02 §12.1.1 (Compression Signaling) / §12.1.2 の値 0 / 1 と、
//! 未サポート値 (2 以上) の扱いを固定する。

use super::*;

/// 0 と 1 は MUST support の範囲であり、2 以上は未サポートとして拒否すること
#[test]
fn compression_values_are_parsed() {
    assert_eq!(MsfCompression::try_from(0), Ok(MsfCompression::None));
    assert_eq!(MsfCompression::try_from(1), Ok(MsfCompression::Gzip));
    assert_eq!(
        MsfCompression::try_from(2),
        Err(MessageError::UnsupportedCompression(2)),
        "未定義の値 2 は UnsupportedCompression になること"
    );
    assert_eq!(
        MsfCompression::try_from(127),
        Err(MessageError::UnsupportedCompression(127)),
        "Standards Action の範囲でも未実装値は未サポートになること"
    );
    assert_eq!(
        MsfCompression::try_from(128),
        Err(MessageError::UnsupportedCompression(128)),
        "private use の範囲でも未実装値は未サポートになること"
    );
}

/// 圧縮方式の値を property の値へ戻せること
#[test]
fn compression_values_roundtrip() {
    assert_eq!(MsfCompression::None.value(), 0);
    assert_eq!(MsfCompression::Gzip.value(), 1);
}

/// UnsupportedCompression はセッションを閉じないこと
///
/// draft-ietf-moq-msf-02 §12.1.1 / §12.1.2 は payload を処理しないことを求めるが、
/// MOQT のセッション終了コードは規定しない。
#[test]
fn unsupported_compression_does_not_terminate_the_session() {
    let error = MessageError::UnsupportedCompression(2);
    assert_eq!(error.session_termination_code(), None);
    assert_eq!(error.data_plane_session_termination_code(), None);
    assert_eq!(error.reason(), "unsupported MSF compression value");
}
