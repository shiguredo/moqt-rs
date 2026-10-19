# FETCH の cancel で bidi request stream の受信方向に STOP_SENDING を指示する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-fetch-cancel-stop-sending
- Polished: 2026-10-19

## 目的

draft-ietf-moq-transport-22 §3.2.4 (Fetch State Management) は "If the data stream is already open, the subscriber wishing to cancel the FETCH MAY send STOP_SENDING for the data stream as well as the bidi request stream. It MUST send STOP_SENDING for the bidi request stream." と定める。
draft-ietf-moq-transport-22 §6.4.2.3 (Request Cancellation and Rejection) も、cancel は開いている方向を「送信方向は RESET_STREAM、受信方向は STOP_SENDING」で打ち切ると定める。
現状のアプリ起点の FETCH cancel (`Session::send_fetch_stop_sending`) は bidi request stream の受信方向に STOP_SENDING を発行しない。

## 現状

- `src/session/fetch.rs` の `send_fetch_stop_sending` は `SessionEvent::ResetRequestStream` (自側送信方向の RESET_STREAM) だけを発行し、`SessionEvent::StopSendingRequestStream` を発行しない。
- `src/session/data.rs` の `send_data_stream_stop_sending` は通常の FETCH 応答 stream の STOP_SENDING 通知を `send_fetch_stop_sending` に dispatch するため、example が `send_data_stream_stop_sending` を呼ぶ経路でも
  bidi の STOP_SENDING が欠ける (`examples/moq-sub/src/pipeline.rs` の `handle_stream_body` の `StreamType::Fetch` 分岐)。
- `src/session/fetch.rs` の `handle_peer_fetch_ok` は、未知の Mandatory Track Property を含む FETCH_OK の受信経路で `StopSendingRequestStream` → `ResetRequestStream` を発行しており、この経路は既に対応済みである。欠けているのはアプリ起点の `send_fetch_stop_sending` 経路である。
- publisher 側の `fetch_stop_sending_received` は STOP_SENDING 受信で bidi request stream の送信方向を reset する経路であり、STOP_SENDING を送る側ではないため本 issue の対象外である。
- `tests/test_session/fetch/unified.rs` の `send_fetch_stop_sending_transitions_to_terminated` テストは `ResetRequestStream` だけを検証しており、欠落を固定していない。

## 設計方針

- `send_fetch_stop_sending` で `StopSendingRequestStream { error_code: STREAM_CANCELLED }` を `ResetRequestStream` より先に発行する。両イベントの `error_code` は `STREAM_CANCELLED` (0x1) とする (§12.5)。`StopSendingRequestStream` は bidi request stream の受信方向、`ResetRequestStream` は送信方向の cancel である (§6.4.2.3)。
- `SessionEvent` の順序契約を doc に明記する。同じ cancel 処理で両イベントを発行する場合は、受信方向の STOP_SENDING → 送信方向の RESET_STREAM の順とする。GOAWAY timeout のように `ResetRequestStream` のみを発行する経路は対象外である。
- 次の doc を新挙動に合わせて更新する。
  - `src/session/fetch.rs` の `send_fetch_stop_sending` doc: 本関数が bidi request stream の受信方向に `StopSendingRequestStream`、送信方向に `ResetRequestStream` を発行することを明記し、要約 (「FETCH データストリームに対して STOP_SENDING を送出した」) と「I/O 層が QUIC STOP_SENDING を送出したうえで本関数を呼ぶ」という記述を修正する。
  - `src/session/types.rs` の `SessionEvent` enum doc: 上記の順序契約を明記する。
  - `src/session/types.rs` の `StopSendingRequestStream` doc: 発行経路に `send_fetch_stop_sending` を追加し、「次の 2 経路」を「次の 3 経路」に更新する。
  - `src/session/types.rs` の `ResetRequestStream` doc: 発行経路に `send_fetch_stop_sending` を追加する。既存の記載漏れである `fetch_stop_sending_received` も併せて追加する。
  - `src/session/data.rs` の `send_data_stream_stop_sending` doc: 通常の FETCH 応答 stream の dispatch が bidi の STOP_SENDING を伴うことを明記する。
  - `examples/tokio-moq/src/moqt_client.rs` の `SessionEvent::StopSendingRequestStream` ハンドラのコメント: 発行経路にアプリ起点の FETCH cancel (`send_fetch_stop_sending`) を追加する。
- データストリームが既に開いている場合の data stream への STOP_SENDING は §3.2.4 の MAY であり、本 issue の必須範囲には含めない。fill fetch stream の cancel は subscription の cancel ではないため対象外とする。
- publisher 側の `fetch_stop_sending_received` は STOP_SENDING を受けて送信方向を reset する経路であり、STOP_SENDING を送る側ではないため対象外とする。`send_fetch_stop_sending` は `RequestTerminated` を発行しない (既存挙動を維持する)。
- テストを更新する。
  - `tests/test_session/fetch/unified.rs` の `send_fetch_stop_sending_transitions_to_terminated` を、`StopSendingRequestStream` → `ResetRequestStream` の両イベントと順序を検証する形に更新する。
  - `send_data_stream_stop_sending` 経由の通常 FETCH 応答 stream の cancel でも同じイベントが同じ順序で発行されることを `tests/test_session/fetch/` のテストで固定する。

## 完了条件

- subscriber のアプリ起点 cancel (`Session::send_fetch_stop_sending`) で `StopSendingRequestStream` と `ResetRequestStream` の両方が `STREAM_CANCELLED` (0x1) で発行されること。
- `send_data_stream_stop_sending` 経由の通常 FETCH 応答 stream の cancel でも同じ 2 イベントが同じ順序で発行されること。
- 両イベントを発行する cancel 経路では `StopSendingRequestStream` → `ResetRequestStream` の順であることをテストで固定すること。
- publisher 側の `fetch_stop_sending_received` が発行する cancel イベント (`ResetRequestStream` のみ) は変更しないこと。
- FETCH cancel 経路で、データストリームが既に開いている場合の data stream への STOP_SENDING (MAY) を新設しないこと。
- `send_fetch_stop_sending` は `RequestTerminated` を新たに発行しないこと。
- `send_fetch_stop_sending` / `StopSendingRequestStream` / `ResetRequestStream` / `SessionEvent` / `send_data_stream_stop_sending` の doc と `examples/tokio-moq/src/moqt_client.rs` の `StopSendingRequestStream` ハンドラのコメントが新挙動と順序契約を反映していること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
