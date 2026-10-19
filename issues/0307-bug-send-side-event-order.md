# 最終応答の SendOnStream を RequestTerminated より先に発行する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-send-side-event-order
- Polished: 2026-10-19

## 目的

peer FIN 受信済みの request に最終応答を送る場合、`Session` は `SendOnStream { fin: true }` を発行した後に `RequestTerminated` を発行しなければならない。逆順だと、アプリは request の終端を観測してから最終応答の送信を指示され、I/O 層が閉じた stream へ書き込もうとする。現状は経路によって順序が逆転している。

## 現状

- `src/session/core.rs` の `send_request_ok` は `mark_send_direction_closed_with_fin` を `SendOnStream` の push より先に呼ぶ。peer FIN 受信済みの TRACK_STATUS では `RequestTerminated` が `SendOnStream` より先に出る。
- `src/session/subscription/send.rs` の `send_publish_done` と `src/session/core.rs` の `emit_request_error` は `SendOnStream` を先に push してから `mark_send_direction_closed_with_fin` を呼ぶ。
- `tests/test_session.rs` の `take_send_on_stream_with_fin` は `RequestTerminated` を読み飛ばすため、`tests/test_session/track_status.rs` のテストでは順序を検証していない。
- `SessionEvent` の doc にイベント順序の契約が書かれていない。

## 設計方針

- `send_request_ok` を「`SendOnStream` push → `mark_send_direction_closed_with_fin`」の順に統一する。
- `SessionEvent` の doc に「最終応答の `SendOnStream` は `RequestTerminated` より先」と明記する。
- 順序を検証するテストを追加する。

## 完了条件

- peer FIN 受信済みの TRACK_STATUS で `SendOnStream` が `RequestTerminated` より先に `poll_event` から出ること。
- イベント順序の契約が doc とテストで固定されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
