# NAMESPACE / NAMESPACE_DONE の未知 request id と publisher 側受信のテストを追加する

- Created: 2026-09-13
- Completed: 2026-09-15
- Branch: feature/test-namespace-receive-error-paths
- Polished: {YYYY-MM-DD}

## 目的

`src/session/namespace/subscribe_namespace.rs` の `Session::handle_peer_namespace` / `Session::handle_peer_namespace_done` にある PROTOCOL_VIOLATION 経路のうち、テストで固定されていない 4 ケースを回帰テストで固定する。

## 現状

実装のエラー経路は次の 4 つを持ち、いずれも `SESSION_PROTOCOL_VIOLATION` を返して `Session::fail` によりセッションを閉じる。

- `handle_peer_namespace`: `NAMESPACE received for unknown request id` / `NAMESPACE received on publisher side`
- `handle_peer_namespace_done`: `NAMESPACE_DONE received for unknown request id` / `NAMESPACE_DONE received on publisher side`

`tests/test_session/namespace/subscribe_namespace.rs` のカバレッジ:

- Pending での受信 (`NAMESPACE received before REQUEST_OK...` / `NAMESPACE_DONE received before REQUEST_OK...`): `peer_namespace_before_request_ok_closes_session` / `peer_namespace_done_before_request_ok_closes_session` で確認済み
- 対応する NAMESPACE なしの NAMESPACE_DONE (`NAMESPACE_DONE received before corresponding NAMESPACE`): `namespace_done_without_namespace_is_violation` で確認済み
- 未知 request id の 2 経路: テストなし
- publisher 側受信 (`my_role == Publisher`) の 2 経路: テストなし

0055 は SUBSCRIBE / PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE の `.session` 拒否テストであり、本 issue とは対象が異なる。

## 設計方針

- `Session::recv_stream_message` に `ControlMessage::Namespace` / `ControlMessage::NamespaceDone` を注入する。
- 未知 request id は SUBSCRIBE_NAMESPACE を開始していない request id に注入する。
- publisher 側受信は、`recv_request` で SUBSCRIBE_NAMESPACE を受けた responder 側 (my_role == Publisher) を `send_request_ok` で Established にしてから注入する。
- 各ケースで `SESSION_PROTOCOL_VIOLATION` が返り、セッションが `Closing` になることを検証する。
- 正常系と既存のエラー経路テストは変更しない。

## 完了条件

- 上記 4 ケースのテストが `tests/test_session/namespace/subscribe_namespace.rs` に追加されていること
- 各ケースで返り値のエラーコードが `SESSION_PROTOCOL_VIOLATION` であり、セッションが `Closing` になっていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること

## 解決方法

relay 専用の namespace 発見・告知機構を削除する対応 (0084) に伴い、本 issue が対象とする `Session::handle_peer_namespace` / `Session::handle_peer_namespace_done` と 4 つの PROTOCOL_VIOLATION 経路が削除されるため、テストで固定する対象そのものが無くなった。NAMESPACE / NAMESPACE_DONE を受信する経路自体が存在しなくなる。
