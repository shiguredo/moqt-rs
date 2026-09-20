# requester が responder の FIN を受けたら送信方向を FIN で閉じる

- Created: 2026-09-16
- Completed: 2026-09-16
- Branch: feature/add-requester-fin
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §6.4.2.2 (Graceful Request Stream Closure) は、responder が応答と
後続メッセージを送り終えて FIN したら、requester も自分の方向を FIN で閉じることを SHOULD と
している。

> A FIN sent by the responder after its response and any subsequent messages for the request
> signals that the request is complete; if it has not already done so, the requester SHOULD
> then send a FIN on its direction, gracefully closing the stream.

現状は `SessionEvent::RequestTerminated` を発行するだけで、requester 側の送信方向を閉じる手段が
無い。I/O 層とアプリが独自に FIN するしかなく、relay 側は request stream の完了を検知できない。

## 現状

- `src/session/core.rs` の `recv_request_stream_closed` は request を `Terminated` にして
  `SessionEvent::RequestTerminated` を発行するのみで、requester の送信方向を閉じる指示を
  出さない。
- `SessionEvent` には送信方向を FIN するための variant が無い (`SendOnStream` はメッセージを
  伴う送信、`ResetRequestStream` は RESET_STREAM であり FIN ではない)。
- `examples/moqt-transport` の `MoqtClient` は `cleanup_closed_requests` で
  `bidi_sends` の残りを FIN するが、これはアプリが `forget_subscription` を呼んだ後の話で、
  FIN のタイミングはアプリ依存である。

## 設計方針

- `SessionEvent::FinishRequestStream { request_id }` を追加し、responder の FIN を受信したときに
  自側が requester である request について発行する。I/O 層は対応する bidi stream の送信方向を
  FIN で閉じる。既に FIN 済みの request への本イベントは I/O 層で無視する。
- PUBLISH 起点の subscription は対象外とする。publisher は購読の終了時に PUBLISH_DONE を送って
  から FIN する必要があり (同節 "the publisher of an Established subscription MUST send
  PUBLISH_DONE, before sending a FIN")、responder の FIN を受けた時点で送信方向を閉じると
  PUBLISH_DONE を送れなくなるためである。実際に PUBLISH_DONE と共に FIN する。
- 対象は SUBSCRIBE 起点の subscription (自側が subscriber)、FETCH (自側が subscriber)、
  TRACK_STATUS (自側が送信側) である。
- `examples/moqt-transport` の `drain_events` に本イベントの処理を追加し、送信半を FIN する。

## 完了条件

- responder の FIN を受けた requester に `SessionEvent::FinishRequestStream` が発行されること
- 自側が responder の場合は発行されないこと
- PUBLISH 起点の subscription では発行されないこと
- example が本イベントで bidi request stream の送信方向を FIN すること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること
- relay 経由の相互運用 (catalog FETCH / SUBSCRIBE / 映像受信) が従来どおり成立すること

## 解決方法

- `SessionEvent::FinishRequestStream { request_id }` を追加した。I/O 層は対応する bidi stream の
  送信方向を FIN で閉じる。既に FIN 済みの request への本イベントは I/O 層で無視する。
- `recv_request_stream_closed` で、終端が FIN かつ自側が requester の request にだけ本イベントを
  発行する。判定は `is_local_requester` で行い、SUBSCRIBE 起点 (自側 subscriber) / FETCH
  (自側 subscriber) / TRACK_STATUS (自側が送信側) を対象とする。
- PUBLISH 起点の subscription は対象外とした。publisher は PUBLISH_DONE を送ってから FIN する
  必要があり (draft-ietf-moq-transport-21 §6.4.2.2)、responder の FIN を受けた時点で送信方向を
  閉じると PUBLISH_DONE を送れなくなるためである。実際に `send_publish_done` が FIN する。
- `examples/moqt-transport` の `drain_events` に本イベントの処理を追加し、送信半を FIN する。

検証:

- `tests/test_session/subscription/handshake.rs` に 2 件追加した。
  `requester_gets_finish_request_stream_on_responder_fin` は responder の FIN で requester に
  本イベントが発行されることを、
  `responder_does_not_get_finish_request_stream` は自側が responder の場合に発行されないことを
  固定する。
- relay 経由の相互運用 (catalog FETCH / SUBSCRIBE / 映像 100 フレーム描画) が
  従来どおり成立することを確認した。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ることを確認した。
