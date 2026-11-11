# TRACK_STATUS_OK を FIN 付きで送ったら request stream の GOAWAY deadline を解除する

- Created: 2026-11-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-track-status-ok-goaway-deadline
- Polished: 2026-11-10

## 目的

`Session::send_request_ok` は TRACK_STATUS への応答 (TRACK_STATUS_OK) を FIN 付きで送るが、request stream の GOAWAY の reset deadline を解除しない。
自側が当該 request stream に GOAWAY を送っていて、peer の FIN が届く前に deadline が満了すると、送信方向を FIN で閉じた stream に `ResetRequestStream(GOING_AWAY)` が発行される。
送信方向の FIN 済み request への reset は I/O 層で無視される (`SessionEvent::ResetRequestStream` の doc の契約) ため wire 上で応答が失われることはないが、Session は当該 request を `GoawayTimeout` で終端扱いにして `RequestTerminated { reason: GoawayTimeout }` を発行する。応答を FIN で送り終えた request が自側 reset で終端したと報告するのは誤ったシグナルであり、後から届く peer FIN による `RequestTerminated { reason: PeerStreamFin }` が得られなくなる (draft-ietf-moq-transport-23 §9.12 (TRACK_STATUS) の "The bidi stream is closed with a FIN after TRACK_STATUS_OK or TRACK_STATUS_ERROR are sent." は tracker 状態 (TRACK_STATUS entry の状態) と矛盾しないように扱う必要がある)。

## 現状

- `src/session/core.rs` の `send_request_ok` は TRACK_STATUS の分岐で `fin = true` とし、`SendOnStream { fin: true }` を積んでから `mark_send_direction_closed_with_fin` を呼ぶ。`clear_request_stream_goaway_deadline` は呼ばない。
- 同じく最終メッセージを FIN 付きで送る `send_request_error` と `emit_request_error_with_details`、PUBLISH_DONE を送る `src/session/data.rs` の `maybe_flush_pending_publish_done` (と `src/session/subscription/send.rs` の `send_publish_done`) は `clear_request_stream_goaway_deadline` を呼ぶ。clear を呼ばない FIN 付き最終メッセージ経路は `send_request_ok` の TRACK_STATUS 分岐だけである。
- deadline は `finish_request_on_fin_exchange` (両方向の FIN が揃ったとき) と `forget_track_status` でも解除されるが、peer の FIN が届く前には解除されない。
- `src/session/goaway.rs` の deadline 満了の処理は、満了した request stream ごとに `SessionEvent::ResetRequestStream { error_code: STREAM_GOING_AWAY }` を無条件に積み、`terminate_request_on_goaway_timeout` が終端していない request なら `RequestTerminated { reason: GoawayTimeout }` も積む。
  送信方向を FIN で閉じたかどうか (`local_fin_sent`) は見ない。
- TRACK_STATUS の responder は request の receiver として `send_goaway_on_request_stream` を呼べる (`is_request_receiver` は `track_status_requests` を参照する)。Timeout が 0 より大きければ deadline が登録される。

## 設計方針

- `send_request_ok` は TRACK_STATUS_OK を FIN 付きで送るとき、`send_request_error` と同じく `clear_request_stream_goaway_deadline` を呼ぶ。最終メッセージを送った後は reset する理由が無いため。
- 呼ぶ位置は `SendOnStream` を積む前後のどちらでもよいが、`send_request_error` と揃える。
- TRACK_STATUS_OK 送信後の `send_goaway_on_request_stream` による deadline 登録は本 issue の対象外である (送信方向が閉じた request への GOAWAY は I/O 層で届かない)。

## 完了条件

- 自側が request stream に GOAWAY を送った後に TRACK_STATUS_OK を FIN 付きで送り、peer の FIN は届けていない状態で tick により deadline を満了させると、`ResetRequestStream` と `RequestTerminated { reason: GoawayTimeout }` が発行されないこと。
- 上記を `tests/test_session/` (GOAWAY の deadline のテスト群) で固定すること。修正前の実装では失敗すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
