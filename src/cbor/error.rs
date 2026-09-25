//! CBOR デコード時のエラー型を定義するモジュール

use core::fmt;

/// CBOR デコード時に発生するエラーの種別
///
/// 各種別が RFC 8949 のどの well-formedness 条件に対応するかを doc コメントに記す。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecodeErrorKind {
    /// 入力がデータ項目の途中で終了している
    ///
    /// RFC 8949 Appendix F の well-formedness error kind 2 (too little data) に対応する。
    /// 発生位置は入力の末尾 (不足している先頭バイトの位置) になる。
    UnexpectedEnd,
    /// 単一のデータ項目をデコードした後に入力が残っている
    ///
    /// RFC 8949 Appendix F の well-formedness error kind 1 (too much data) に対応する。
    /// 発生位置は残った入力の先頭になる。
    TrailingData,
    /// additional information 28..=30 が使われている
    ///
    /// RFC 8949 Section 3 で予約された値であり、well-formed ではない。
    ReservedAdditionalInformation,
    /// major type 0, 1, 6 で additional information 31 が使われている
    ///
    /// RFC 8949 Table 2 のとおり、これらの major type で indefinite-length は使えない。
    IndefiniteLengthNotAllowed,
    /// データ項目が期待される位置に break stop code (0xff) が出現した
    ///
    /// RFC 8949 Section 3.2.1 のとおり、break は indefinite-length 項目の内部だけで使える。
    UnexpectedBreak,
    /// 0xf8 に続くバイトが 32 未満の simple value 表現
    ///
    /// RFC 8949 Section 3.3 のとおり、この 2 バイト表現は well-formed ではない。
    InvalidSimpleValue,
    /// indefinite-length 文字列のチャンクが同じ major type の definite-length 文字列でない
    ///
    /// RFC 8949 Section 3.2.3 のとおり、indefinite-length 文字列のチャンクには
    /// 同じ major type の definite-length 文字列しか置けない。
    InvalidIndefiniteStringChunk,
    /// indefinite-length マップでキーの直後が break stop code になっている
    ///
    /// RFC 8949 Section 3.2.2 のとおり、キーに対応する値が必要である。
    MissingMapValue,
    /// テキスト文字列が UTF-8 として不正
    ///
    /// RFC 8949 Section 3.1 のとおり、UTF-8 として不正なテキスト文字列は
    /// well-formed だが invalid である。このライブラリは invalid なデータ項目を
    /// エラーとして扱う (RFC 8949 Section 5.3.1)。
    InvalidUtf8,
    /// ネストの深さが上限を超えた
    ///
    /// RFC 8949 Section 10 の resource exhaustion への対策として深さを制限している。
    DepthLimitExceeded,
}

/// CBOR デコード時のエラー
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeError {
    /// エラーの種別
    kind: DecodeErrorKind,
    /// エラーを検出した入力中のバイト位置 (0 始まり)
    position: usize,
}

impl DecodeError {
    /// エラーの種別を返す
    pub fn kind(&self) -> DecodeErrorKind {
        self.kind
    }

    /// エラーを検出した入力中のバイト位置 (0 始まり) を返す
    ///
    /// 位置の指す先は種別ごとに異なる。先頭バイトが原因のエラー
    /// (予約された additional information、break の位置違反など) では
    /// その先頭バイトの位置、入力不足では入力の末尾、UTF-8 違反では
    /// 違反したバイトの位置になる。
    pub fn position(&self) -> usize {
        self.position
    }

    /// エラーを生成する
    pub(crate) fn new(kind: DecodeErrorKind, position: usize) -> Self {
        Self { kind, position }
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self.kind {
            DecodeErrorKind::UnexpectedEnd => "unexpected end of input",
            DecodeErrorKind::TrailingData => "trailing data after the data item",
            DecodeErrorKind::ReservedAdditionalInformation => {
                "reserved additional information (28..=30)"
            }
            DecodeErrorKind::IndefiniteLengthNotAllowed => {
                "indefinite length is not allowed for this major type"
            }
            DecodeErrorKind::UnexpectedBreak => "unexpected break stop code",
            DecodeErrorKind::InvalidSimpleValue => "invalid simple value encoding",
            DecodeErrorKind::InvalidIndefiniteStringChunk => {
                "invalid indefinite-length string chunk"
            }
            DecodeErrorKind::MissingMapValue => "missing map value",
            DecodeErrorKind::InvalidUtf8 => "invalid UTF-8 text string",
            DecodeErrorKind::DepthLimitExceeded => "maximum nesting depth exceeded",
        };
        write!(f, "{message} at offset {}", self.position)
    }
}

impl core::error::Error for DecodeError {}
