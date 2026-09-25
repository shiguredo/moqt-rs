//! RFC 8949 (CBOR) と RFC 8742 (CBOR Sequences) のコーデック
//!
//! CBOR (Concise Binary Object Representation) のエンコードとデコードを行う。
//! 依存クレートはなく、`core` と `alloc` のみで動作する。
//!
//! # 特徴
//!
//! - 単一のデータ項目 ([`crate::cbor::decode::decode`]) と CBOR シーケンス ([`crate::cbor::decode::decode_all`], [`crate::cbor::decode::Decoder`]) のデコード
//! - 引数を最短形式で出力するエンコード ([`crate::cbor::value::Value::to_bytes`])
//! - RFC 8949 Section 4.2.1 の core deterministic encoding ([`crate::cbor::value::Value::to_canonical_bytes`])
//! - RFC 8949 Section 8 の診断記法 ([`crate::cbor::value::Value`] の [`core::fmt::Display`])
//!
//! # 使い方
//!
//! ```
//! use shiguredo_moqt::cbor::decode::decode;
//! use shiguredo_moqt::cbor::value::Value;
//!
//! // Value を構築してエンコードする
//! let value = Value::Array(vec![
//!     Value::Unsigned(1),
//!     Value::TextString(String::from("spam")),
//!     Value::Bool(true),
//! ]);
//! let bytes = value.to_bytes();
//! assert_eq!(bytes, vec![0x83, 0x01, 0x64, 0x73, 0x70, 0x61, 0x6d, 0xf5]);
//!
//! // バイト列をデコードする
//! let decoded = decode(&bytes).expect("デコードに失敗するはずがない (実装バグ)");
//! assert_eq!(decoded, value);
//! assert_eq!(decoded.to_string(), "[1, \"spam\", true]");
//! ```
//!
//! # 設計上の判断
//!
//! - 浮動小数点値は [`f64`] に統一する。半精度・単精度でエンコードされた値は
//!   デコード時に正確に [`f64`] へ拡張される。通常のエンコードは常に倍精度
//!   (0xfb) で出力し、決定論的エンコードは値を保つ最短の表現で出力する。
//!   (RFC 8949 Section 2.1 では浮動小数点値の幅はエンコードの詳細であり、
//!   データモデル上の値は 1 種類である)
//! - indefinite-length 表現はデコード時に同じ値の definite-length 表現へ
//!   実体化する。エンコードは常に definite-length を使う。
//! - マップの重複キーは許容し、全てのエントリを出現順に保持する。
//!   重複キーの検出は行わないため、必要ならアプリケーション側で行うこと
//!   (RFC 8949 Section 5.6 の 3 方式のうち「全てのエントリをアプリケーションに渡す」方式)。
//! - テキスト文字列は UTF-8 として検証し、不正な場合は
//!   [`crate::cbor::error::DecodeErrorKind::InvalidUtf8`] エラーにする。UTF-8 として不正な
//!   テキスト文字列は well-formed だが invalid なデータだからである
//!   (RFC 8949 Section 3.1, Section 5.3.1)。
//! - タグ内容の妥当性は検証しない。既知のタグも解釈せず、[`crate::cbor::value::Value::Tag`] として
//!   そのまま保持する (RFC 8949 Section 5.3.2)。
//! - デコード時のネストの深さはデフォルトで [`crate::cbor::decode::DEFAULT_MAX_DEPTH`] 段に制限する
//!   (RFC 8949 Section 10 の resource exhaustion 対策)。
//! - エンコード時の simple value は 0..=19 と 32..=255 だけを扱う。20..=23 は
//!   [`crate::cbor::value::Value::Bool`] / [`crate::cbor::value::Value::Null`] / [`crate::cbor::value::Value::Undefined`] に
//!   対応する表現、24..=31 は予約されており表現が存在しないため、後者を指定すると
//!   パニックする。

pub mod decode;
mod diagnostic;
mod encode;
pub mod error;
mod half;
pub mod value;
