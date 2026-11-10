# TRACK_STATUS_OK を FIN 付きで送ったら request stream の GOAWAY deadline を解除する

- Created: 2026-11-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-track-status-ok-goaway-deadline
- Polished: {YYYY-MM-DD}

## 目的

`Session::send_request_ok` は TRACK_STATUS への応答 (TRACK_STATUS_OK) を FIN 付きで送るが、request stream の GOAWAY の reset deadline を解除しない。
自側が当該 request stream に GOAWAY を送っていて、peer の FIN が届く前に deadline が満了すると、送信方向を FIN で閉じた stream に `ResetRequestStream(GOING_AWAY)` が発行される。
reset が TRACK_STATUS_OK の配送より先に効くと応答が失われ、draft-ietf-moq-transport-23 §9.12 (TRACK_STATUS) の "The bidi stream is closed with a FIN after TRACK_STATUS_OK or TRACK_STATUS_ERROR are sent." を満たせない。

## 現状

- `src/session/core.rs` の `send_request_ok` は TRACK_STATUS の分岐で `fin = true` とし、`SendOnStream { fin: true }` を積んでから `mark_send_direction_closed_with_fin` を呼ぶ。`clear_request_stream_goaway_deadline` は呼ばない。
- 同じく最終メッセージを FIN 付きで送る `send_request_error` と `emit_request_error_with_details`、PUBLISH_DONE を送る `src/session/data.rs` の `maybe_flush_pending_publish_done` は `clear_request_stream_goaway_deadline` を呼ぶ。
- deadline は `finish_request_on_fin_exchange` (両方向の FIN が揃ったとき) と `forget_track_status` でも解除されるが、peer の FIN が届く前には解除されない。
- `src/session/goaway.rs` の deadline 満了の処理は、満了した request stream ごとに `SessionEvent::ResetRequestStream { error_code: STREAM_GOING_AWAY }` を無条件に積み、`terminate_request_on_goaway_timeout` が終端していない request なら `RequestTerminated { reason: GoawayTimeout }` も積む。
  送信方向を FIN で閉じたかどうか (`local_fin_sent`) は見ない。
- TRACK_STATUS の responder は request の receiver として `send_goaway_on_request_stream` を呼べる (`is_request_receiver` は `track_status_requests` を参照する)。Timeout が 0 より大きければ deadline が登録される。

## 設計方針

- `send_request_ok` は TRACK_STATUS_OK を FIN 付きで送るとき、`send_request_error` と同じく `clear_request_stream_goaway_deadline` を呼ぶ。最終メッセージを送った後は reset する理由が無いため。
- 呼ぶ位置は `SendOnStream` を積む前後のどちらでもよいが、`send_request_error` と揃える。

## 完了条件

- 自側が request stream に GOAWAY を送った後に TRACK_STATUS_OK を FIN 付きで送ると、deadline が満了しても `ResetRequestStream` と `RequestTerminated { reason: GoawayTimeout }` が発行されないこと。
- 上記を `tests/test_session/` (GOAWAY の deadline のテスト群) で固定すること。修正前の実装では失敗すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
