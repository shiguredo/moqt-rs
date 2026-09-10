# fuzz_session に server role と送信 API を追加する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/test-fuzz-session-server-role

## 目的

`Session` の server role と送信側の状態遷移を fuzzing の対象に含め、受信系だけでは検出できない不整合を拾う。

## 現状

`fuzz/fuzz_targets/fuzz_session.rs` は `Session::new_client` のみを使い、`recv_*` / `tick` / `close` のみを呼ぶ。`Op` 列挙に送信 API が無い。server role の Request ID parity 検証や送信側の状態遷移は fuzz の対象外。

`pbt/tests/prop_session/common.rs` は両 role を扱うため完全な欠落ではない。

## 設計方針

`fuzz_session` に `new_server` バリアントと、`send_subscribe` / `send_publish` / `send_fetch` 等の送信 API を呼ぶ `Op` を追加する。入力長が増えすぎないよう、操作列の生成は既存の方式に合わせる。

## 完了条件

- `fuzz_session` が client / server 両 role を生成すること
- 送信 API が fuzz の操作列に含まれること
- `cargo check --manifest-path fuzz/Cargo.toml` が通ること
- クラッシュが検出されないこと (既知の未解決クラッシュがあれば issue 化する)
