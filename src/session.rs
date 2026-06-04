//! MoQT セッション状態機械 (sans-I/O)
//!
//! draft-ietf-moq-transport-21 の §6 (Sessions), §3 (Publishing and Receiving Tracks), §4 (Namespace Discovery), §9 (Control Messages) に基づく。draft 由来の
//! 実装のため、将来のバージョンで変更される可能性がある。
//!
//! # 位置づけ
//!
//! [`Session`] は draft が定義する 1 本の `Transport Session` (QUIC connection
//! または WebTransport session) に対応する。
//!
//! 本モジュールが扱うのは、この endpoint から見た peer との protocol state であり、
//! relay 全体の routing / fan-out / cache / policy は対象外である。relay を
//! 実装する場合でも、複数 peer との間に張った個々の `Transport Session` ごとに
//! [`Session`] を使い分けることを想定する。
//!
//! [`Role`] は `Transport Session` における endpoint の役割 (`Client` / `Server`)
//! を表し、[`TrackRole`] は各 request / track における protocol role
//! (`Publisher` / `Subscriber`) を表す。1 本の [`Session`] の中で SUBSCRIBE /
//! PUBLISH はどちらも並行して存在しうる。
//!
//! # 設計方針
//!
//! - I/O を持たない純粋な状態機械
//! - 入力: [`Session::recv_control`] / [`Session::recv_request`] /
//!   [`Session::send_subgroup_header`] / [`Session::send_subgroup_object`] /
//!   [`Session::send_data_stream_closed`] / [`Session::recv_data_stream_stop_sending`] /
//!   [`Session::recv_data_stream_type`] / [`Session::recv_object_datagram`] /
//!   [`Session::close`] 等のアプリ要求
//! - 出力: [`Session::poll_event`] で [`SessionEvent`] を取り出す
//! - 呼び出し側は [`SessionEvent::SendControl`] を I/O 層に渡して送信し、
//!   [`SessionEvent::CloseSession`] で QUIC CONNECTION_CLOSE や WebTransport
//!   session close を発行する
//!
//! # 実装範囲
//!
//! SETUP / Request ID 管理 / SUBSCRIBE / PUBLISH / REQUEST_UPDATE / PUBLISH_DONE /
//! STOP_SENDING / FETCH / Namespace 系 / TRACK_STATUS / GOAWAY / tick 駆動の
//! タイムアウト判定に加え、data plane の送受信 state
//! (`send_subgroup_header` / `send_subgroup_object` / `send_data_stream_closed` /
//! `recv_data_stream_stop_sending` / `recv_data_stream_type` / `recv_subgroup_header` /
//! `recv_subgroup_object` / `recv_fetch_header` / `recv_object_datagram`) を実装済み。
//!
//! # Stream 抽象化
//!
//! MoQT は制御ストリーム (SETUP 用 uni stream ペア) と request stream (bidi、
//! SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS / PUBLISH_NAMESPACE /
//! SUBSCRIBE_NAMESPACE のいずれかで始まる) と data stream (uni、FETCH_HEADER /
//! SUBGROUP_HEADER で始まる) を区別する。
//! 応答メッセージ (SUBSCRIBE_OK / PUBLISH_OK / REQUEST_ERROR 等) は wire
//! format に Request ID を含まないため、bidi stream のコンテキストから特定する。
//!
//! 本モジュールではこれを以下の API / Event で表現する:
//!
//! - `recv_control(msg)` / `recv_control_stream_closed(end)` /
//!   `SessionEvent::SendControl` — 制御ストリーム用 (SETUP / GOAWAY / transport close)
//! - `recv_request(msg)` / `SessionEvent::SendRequest { request_id, message }` —
//!   bidi request stream の最初のメッセージ (Request ID を含む)
//! - `recv_stream_message(request_id, msg)` / `SessionEvent::SendOnStream { request_id, message, fin }` —
//!   bidi request stream 上の応答 (`fin` が true の応答がその stream の最終メッセージ)
//! - `send_subgroup_header(...)` / `send_subgroup_object(...)` /
//!   `send_data_stream_closed(stream_id, end)` / `recv_data_stream_stop_sending(stream_id)` /
//!   `recv_data_stream_type(stream_id, type_id)` / `recv_subgroup_header(...)` /
//!   `recv_subgroup_object(...)` / `recv_fetch_header(...)` /
//!   `recv_data_stream_closed(stream_id, end)` — uni data stream 用
//! - `recv_object_datagram(datagram)` — datagram 用
//!
//! # サブモジュール構成
//!
//! - `types`: 型定義 (Session 除く)
//! - `auth_token_cache`: AUTHORIZATION_TOKEN Alias Cache
//! - `request_id`: Request ID 採番と検証
//! - `core`: `Session` 構造体 + ライフサイクル + SETUP / 共通ディスパッチ /
//!   REQUEST_OK / REQUEST_ERROR の送受信
//! - `data`: data stream / datagram の送受信 state
//! - `subscription`: SUBSCRIBE / PUBLISH / REQUEST_UPDATE / PUBLISH_DONE /
//!   STOP_SENDING 関連
//! - `fetch`: FETCH 関連
//! - `namespace`: Namespace 系 / TRACK_STATUS 関連
//! - `goaway`: GOAWAY / tick 関連

pub mod auth_token_cache;
pub mod core;
pub mod data;
pub mod fetch;
pub mod goaway;
pub mod namespace;
pub mod request_id;
pub mod subscription;
pub mod types;

#[cfg(test)]
mod tests;
