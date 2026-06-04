// no_std 環境向けライブラリ
#![no_std]

//! shiguredo_moqt: draft-ietf-moq-transport-21 に基づく sans I/O な MoQ ライブラリ
//!
//! I/O を持たない純粋なコーデック層と、1 本の MoQT Transport Session に閉じた
//! sans I/O なセッション状態機械を提供する。

extern crate alloc;

/// shiguredo_moqt: draft-ietf-moq-transport-21 に基づく sans I/O MoQ ライブラリ
///
/// WebTransport over HTTP/2 ・ WebTransport over HTTP/3 ・ QUIC 上で動作する前提で、
/// I/O を持たない純粋なコーデック層と、1 本の `MOQT Transport Session` に閉じた
/// sans I/O なセッション状態機械 [`session::core::Session`] を提供する。
/// [`session::core::Session`] は peer / endpoint 視点の protocol state を扱い、relay 全体の
/// routing / fan-out / cache / policy は対象外とする。
pub mod decoder;
/// エラー型と終了コード定義 (draft-ietf-moq-transport-21 §16.11 (Error Codes))
pub mod error;
pub mod grease;
/// KVP (Key-Value-Pair) の delta-key 骨格を集約する内部モジュール (外部公開しない)
pub(crate) mod kvp;
pub mod loc;
pub mod message;
/// メッセージ パラメータ群 (draft-ietf-moq-transport-21 §9.20 (Control Message Parameters))
pub mod message_parameter;
/// MOQT Streaming Format のカタログとタイムラインのコーデック (draft-ietf-moq-msf-01)
pub mod msf;
pub mod name;
pub mod object_properties;
/// SETUP 用の Setup Options (draft-ietf-moq-transport-21 §9.1 (SETUP))
pub mod parameter;
pub mod session;
pub mod stream;
pub mod subgroup_tracker;
/// トラック プロパティ群 (draft-ietf-moq-transport-21 §8.4 (Track and Object Properties))
pub mod track_properties;
/// 可変長整数 (Variable-Length Integer) のコーデック (draft-ietf-moq-transport-21 §8.1 (Variable-Length Integers))
pub mod varint;
