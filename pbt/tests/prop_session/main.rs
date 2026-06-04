//! Session 状態機械のプロパティベーステスト
//!
//! - AuthTokenCache のラウンドトリップ系プロパティ
//! - Client / Server ハンドシェイクのプロパティ

mod auth_token;
mod common;
mod datagram;
mod fetch;
mod goaway;
mod handshake;
mod namespace;
mod request_id;
mod subscription;
