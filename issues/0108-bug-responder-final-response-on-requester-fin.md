# requester の FIN で responder が必須応答を送れなくなる

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-responder-final-response-on-requester-fin
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §6.4.2.2 (Graceful Request Stream Closure) は FIN を方向ごとの終端として定義し、request のキャンセルとは区別する。

> "A FIN only indicates that an endpoint will send no further messages in that direction; it is not a request cancellation.  An endpoint MUST NOT send a FIN on a direction of a request stream until it has sent all required messages on that direction for its request type."

requester の送信方向については、PUBLISH の送信者を除いて応答を待たずに FIN してよい。

> "A requester, with the exception of the sender of PUBLISH, MAY FIN immediately after sending a message if it will not send a REQUEST_UPDATE."

同節は、required messages が届く前の FIN を request の失敗として扱うとも定める。

> "An endpoint that receives a FIN before all required messages have arrived treats the request as failed."

ここでいう required messages は FIN を送る側がその方向で送るべきメッセージである。requester が SUBSCRIBE / FETCH の送信直後に FIN する場合、requester が送るべきメッセージは既に届いているため request は失敗ではない。失敗になるのは、responder が応答を送る前に FIN した場合である。

一方、responder には応答の MUST がある。§3.1 (Subscriptions):

> "A publisher MUST send exactly one SUBSCRIBE_OK or REQUEST_ERROR in response to a SUBSCRIBE."

§3.2.1 (Fetch State Management):

> "The publisher MUST send exactly one FETCH_OK or REQUEST_ERROR in response to a FETCH."

現状は requester が SUBSCRIBE / FETCH の送信直後に FIN すると responder 側で request が終端され、SUBSCRIBE_OK / FETCH_OK / REQUEST_ERROR のいずれも送れなくなる。requester の FIN は「requester がもうメッセージを送らない」ことしか意味しないため、responder が応答を送る前の FIN で request を終端してはならない。

## 現状

- `src/session/types.rs` の `RequestStreamEnd` は `Fin` と `Reset` のみを持ち、どちらの方向の終端かを表さない。`terminationreason_from_end` は `Fin` を一律に `TerminationReason::PeerStreamFin` へ写す
- `src/session/core.rs` の `Session::recv_request_stream_closed` は `request_streams` の種別で `Session::close_subscription_on_stream_end` / `Session::close_fetch_on_stream_end` / `Session::close_track_status_on_stream_end` に振り分け、終端の種類にかかわらず `SessionEvent::RequestTerminated` を発行する
- `src/session/subscription/send.rs` の `Session::close_subscription_on_stream_end` は `SubscriptionState::Terminated` を無条件に代入する。`Session::send_subscribe_ok` は publisher 役かつ `is_pending_subscriber()` を要求するため、以後の SUBSCRIBE_OK 送信は `SESSION_PROTOCOL_VIOLATION` で失敗する
- `src/session/fetch.rs` の `Session::close_fetch_on_stream_end` は `FetchState::Terminated` を無条件に代入する。`Session::send_fetch_ok` は `FetchState::Pending` を要求するため、以後の FETCH_OK 送信は `SESSION_PROTOCOL_VIOLATION` で失敗する
- `src/session/subscription/dispatch.rs` の `Session::send_err_for_subscription` と `src/session/fetch.rs` の `Session::send_err_for_fetch` は `Terminated` からの REQUEST_ERROR 送信を拒否する
- `examples/moqt-transport/src/moqt_client.rs` は bidi stream の `StreamRead::Closed(end)` で `Session::recv_request_stream_closed` を呼ぶため、example 経由の実機でも responder は応答を返せない
- `tests/test_session/request_stream.rs` の `request_stream_closed_terminates_subscribe` は requester 自身が送信方向を閉じたことを `recv_request_stream_closed` で通知しており、peer の FIN とローカルの FIN が API 上で区別されていない (通知される理由も `TerminationReason::PeerStreamFin` になる)

## 設計方針

- peer の FIN が「peer の送信方向の終端」であることを API で区別できるようにする。`RequestStreamEnd` に終端した方向を持たせる、peer の FIN 専用の API を分けるなど、形は実装時に決める
- responder が応答を送る前の peer FIN では request を終端しない。応答待ちの状態 (`SubscriptionState::Pending` / `FetchState::Pending`) を維持し、SUBSCRIBE_OK / FETCH_OK / REQUEST_ERROR の送信経路を塞がない
- peer FIN 後の `SessionEvent::RequestTerminated { reason: PeerStreamFin }` は、responder が応答を送り終えて自側の方向を閉じた後の終端として扱う
- `RequestStreamEnd::Reset` は従来どおり request の打ち切りとして即時に終端する
- 自側が requester のときに responder の FIN を受けて `SessionEvent::FinishRequestStream` を発行する既存挙動は変えない
- PUBLISH の送信者は §6.4.2.2 の例外であり、PUBLISH_DONE を送る前に FIN できない。本 issue は SUBSCRIBE / FETCH の responder 応答を対象とし、PUBLISH 起点の subscription の扱いは変えない
- TRACK_STATUS の受信実装は 0109 で扱う。同実装は本 issue の API に従う
- `recv_request_stream_closed` の契約が変わるため、`docs/IMPLEMENTATION.md` と `skills/shiguredo-moqt/SKILL.md` の該当記述を更新する

## 完了条件

- requester が SUBSCRIBE を送った直後に FIN し、その後 responder が SUBSCRIBE_OK を送れることを固定するテストが `src/session/tests.rs` または `tests/test_session/` に追加されていること
- requester が FETCH を送った直後に FIN し、その後 responder が FETCH_OK を送れることを固定するテストが追加されていること
- requester の FIN 後に responder が REQUEST_ERROR を送れることを固定するテストが追加されていること
- peer の RESET_STREAM では従来どおり request が `Terminated` になり `SessionEvent::RequestTerminated { reason: PeerStreamReset }` が発行されることが維持されていること
- 自側が requester のときに responder の FIN を受けて `SessionEvent::FinishRequestStream` が発行される既存挙動が維持されていること
- `cargo test --workspace` が通ること
