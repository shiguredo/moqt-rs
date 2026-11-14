# draft-23 の client/server 向け低レイヤー API を完成させる

- Created: 2026-11-14
- Completed: {YYYY-MM-DD}
- Branch: develop (CODEBASE.md の規約により develop に直接コミットする)
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-23 で client / server の endpoint 同士が使う低レイヤー API の欠落を解消する。
namespace 発見・告知の 6 メッセージ (`PUBLISH_NAMESPACE` / `SUBSCRIBE_NAMESPACE` /
`SUBSCRIBE_TRACKS` / `NAMESPACE` / `NAMESPACE_DONE` / `PUBLISH_SKIPPED`) は、以前に
「relay 専用」と判断されて削除されたが、§4.1 は publisher → subscriber と
subscriber → publisher の双方が使うと定めており、client / server の endpoint 同士でも使う。
また §3.3.3 は publisher に「通過する Object だけを転送する」ことを MUST とするが、
FETCH 応答の Object 系 Range Filter は評価していない。

## 現状

- `src/session/core.rs` の `recv_request` は namespace 系 3 メッセージを
  `REQUEST_ERROR` の `NOT_SUPPORTED` で拒否し、`ControlMessage::Unsupported` で本体だけを
  解析していた。
- `src/session/namespace.rs` は存在せず、namespace 系 request の状態・応答 stream の
  メッセージ・`REQUEST_UPDATE` の prefix 更新を扱う経路が無い。
- `src/session/data.rs` の `send_fetch_object` は Location Filter だけを検証し、
  FETCH 要求の `SUBGROUP_FILTER` / `OBJECTID_FILTER` / `PRIORITY_FILTER` /
  `OBJECT_PROPERTY_FILTER` を評価しない。
- `SessionEvent` に namespace 系の受信イベントが無く、`MoqtClient` からも送信できない。

## 設計方針

- 6 メッセージを `src/message.rs` の `ControlMessage` に追加し、`Unsupported` と
  未対応 request の拒否経路を削除する。
- `src/session/namespace.rs` に namespace 系 request の状態
  (`Pending` / `Established` / `Terminated`)、応答 stream の送受信、
  `REQUEST_UPDATE` の `TRACK_NAMESPACE_PREFIX` 更新、`SUBSCRIBE_TRACKS` と PUBLISH の
  相関検証を実装する。
- `Fetch` に Range Filter を保持し、`send_fetch_object_with_context` で
  Subgroup ID / Publisher Priority / Object Properties を使って Pass 判定する。
- `MoqtClient` に送信 API とイベント配送を追加し、docs / SKILL.md / PBT を追随させる。

## 完了条件

- draft-23 の制御メッセージ 19 種すべてを client / server の endpoint 同士で送受信できる。
- namespace 系 request の状態遷移、`REQUEST_UPDATE`、`NAMESPACE_DONE` の順序、
  `PUBLISH_SKIPPED` と PUBLISH の相関がテストで検証されている。
- FETCH 応答の Object 系 Range Filter がテストで検証されている。
- `cargo fmt` / `cargo clippy` / `cargo test` が通る。
