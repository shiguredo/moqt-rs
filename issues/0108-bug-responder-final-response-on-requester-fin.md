# requester の FIN で responder が必須応答を送れなくなる

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-responder-final-response-on-requester-fin
- Polished: 2026-09-21

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

§6.4.2.3 (Request Cancellation and Rejection) は cancel の表現を定めており、FIN は含まれない。

> "Implementations cancel a request by abruptly terminating any directions of the stream that are still open, using RESET_STREAM for a direction they are sending and STOP_SENDING for a direction they are receiving."

現状は requester が SUBSCRIBE / FETCH の送信直後に FIN すると responder 側で request が終端され、SUBSCRIBE_OK / FETCH_OK / REQUEST_ERROR のいずれも送れなくなる。

requester の FIN は「requester がもうメッセージを送らない」ことしか意味しないため、responder が応答を送る前の FIN で request を終端してはならない。cancel の手段が RESET_STREAM / STOP_SENDING と定められている以上、responder が応答を送った後 (Established) の FIN でも同じである。

## 現状

- `src/session/types.rs` の `RequestStreamEnd` は peer の終端種別として `Fin` と `Reset` のみを持つ。`terminationreason_from_end` は `Fin` を一律に `TerminationReason::PeerStreamFin` へ写す
- `src/session/core.rs` の `Session::recv_request_stream_closed` は `request_streams` の種別で `Session::close_subscription_on_stream_end` / `Session::close_fetch_on_stream_end` / `Session::close_track_status_on_stream_end` に振り分ける
- 同関数は終端の種類にかかわらず `SessionEvent::RequestTerminated` を発行し、そのとき自側の役割 (requester か responder か) を見ていない。`Session::is_local_requester` は `SessionEvent::FinishRequestStream` の発行条件にしか使われていない
- `src/session/subscription/send.rs` の `Session::close_subscription_on_stream_end` は `SubscriptionState::Terminated` を無条件に代入する。`Session::send_subscribe_ok` は publisher 役かつ `is_pending_subscriber()` を要求するため、以後の SUBSCRIBE_OK 送信は `SESSION_PROTOCOL_VIOLATION` で失敗する
- `src/session/fetch.rs` の `Session::close_fetch_on_stream_end` は `FetchState::Terminated` を無条件に代入する。`Session::send_fetch_ok` は `FetchState::Pending` を要求するため、以後の FETCH_OK 送信は `SESSION_PROTOCOL_VIOLATION` で失敗する
- `src/session/subscription/dispatch.rs` の `Session::send_err_for_subscription` と `src/session/fetch.rs` の `Session::send_err_for_fetch` は `Terminated` からの REQUEST_ERROR 送信を拒否する
- `examples/moqt-transport/src/moqt_client.rs` は bidi stream の `StreamRead::Closed(end)` で `Session::recv_request_stream_closed` を呼ぶため、example 経由の実機でも responder は応答を返せない
- `Session::recv_request_stream_closed` は peer の終端通知専用の API であり、自側の FIN は通知されない。`examples/moqt-transport/src/transport.rs` の `RecvStream::receive_chunk` は peer の FIN (`Ok(None)`) と `StreamReset` だけを `RequestStreamEnd` に写し、ローカルの FIN は `Session` へ通知されない
- `tests/test_session/request_stream.rs` の `request_stream_closed_terminates_subscribe` はテストコメントに「subscriber 側 (自側 SUBSCRIBE を送った client) が FIN で閉じる」と書いている
- しかし `recv_request_stream_closed` は peer の終端通知なので、このテストが通知しているのは requester が responder の FIN を受けたことである。`TerminationReason::PeerStreamFin` になるのは requester 側の終端として正しい。区別すべきは終端の方向ではなく、通知を受けた側が requester か responder かである
- `tests/test_session/fetch/fill.rs` の `cancel_subscription_resets_open_fill_streams` と `cancel_subscription_resets_all_open_fill_streams` は、Established の subscription で responder が requester の FIN を受けたときに `Terminated` へ遷移することと open 中の fill fetch stream が reset されることを固定している
- draft-ietf-moq-transport-21 §3.4.1 が fill fetch stream の reset を MUST とするのは「subscription が cancel されたとき」であり、FIN は cancel ではない。この 2 テストは現行の誤った挙動を固定している

## 設計方針

- peer の FIN を request の終端として扱うかどうかは、終端の方向ではなく自側の役割で分岐する。SUBSCRIBE / FETCH で自側が responder (`Subscription::my_role` / `Fetch::my_role` が `TrackRole::Publisher`) なら終端しない
- 自側が requester なら従来どおり終端する。`Session::is_local_requester` は `RequestKind::Publish` で常に false を返すため、PUBLISH 起点を従来どおり終端させるには `RequestKind::Subscribe` / `RequestKind::Fetch` に限定した判定が要る
- `RequestStreamEnd` は peer の終端種別 (`Fin` / `Reset`) を表す型として維持し、終端した方向は持たせない。`recv_control_stream_closed` / `recv_fetch_data_stream_closed` / `recv_data_stream_closed` と共用しており、peer の終端通知しか来ないため区別する相手がいない
- responder が peer FIN を受けたときは request を終端しない。応答待ちの `SubscriptionState::Pending` / `FetchState::Pending` だけでなく `Established` でもそのまま維持する
- これにより SUBSCRIBE_OK / FETCH_OK / PUBLISH_DONE / REQUEST_ERROR の送信経路と、確立後の data 送信を塞がない
- draft-ietf-moq-transport-21 §6.4.2.2 が FIN を cancel と区別し、§6.4.2.3 が cancel を RESET_STREAM / STOP_SENDING と定めているため、peer FIN を理由に `Terminated` へ遷移させる根拠がない。ただしこれは requester が SUBSCRIBE / FETCH の直後に FIN してよい場合の話であり、PUBLISH 起点には適用しない
- responder が peer FIN を受信した事実は保持する。不要になった `Terminated` 遷移の代わりに peer FIN 受信済みを記録し、自側が request stream の送信方向を閉じる時点 (`SessionEvent::SendOnStream { fin: true }` を発行する時点。SUBSCRIBE の responder では PUBLISH_DONE 送信) で、記録済みなら `SessionEvent::RequestTerminated { reason: PeerStreamFin }` を発行する
- FETCH の responder は `Session::send_fetch_ok` が `fin: false` で送信方向を閉じる経路を持たないため、peer FIN だけでは終端しない。FETCH の終端は従来どおり `RequestStreamEnd::Reset` / `Session::fetch_stop_sending_received` / FETCH データ stream の終端に委ねる
- `RequestStreamEnd::Reset` は従来どおり request の打ち切りとして即時に終端する
- 自側が requester のときに responder の FIN を受けて `SessionEvent::FinishRequestStream` を発行し `SessionEvent::RequestTerminated { reason: PeerStreamFin }` で終端する既存挙動は変えない
- PUBLISH の送信者は §6.4.2.2 の例外であり、PUBLISH_DONE を送る前に FIN できない。したがって PUBLISH 起点で responder (subscriber 役) が受ける peer FIN は PUBLISH_DONE 受信後の完了通知であり、本 issue の対象外とする
- 自側が PUBLISH 起点の responder でも従来どおり終端し、`tests/test_session/subscription/subscription_limits.rs` の `request_update_rejection_close_goes_through_request_streams` が期待する `SessionEvent::RequestTerminated` の発行と `request_streams` からの除去を維持する
- TRACK_STATUS の受信実装は 0109 で扱う。同実装は本 issue の API に従う
- `tests/test_session/fetch/fill.rs` の `cancel_subscription_resets_open_fill_streams` と `cancel_subscription_resets_all_open_fill_streams` は、キャンセルを `RequestStreamEnd::Fin` で表現している
- FIN は cancel ではないため、`RequestStreamEnd::Reset` でキャンセルする形に更新する。SUBSCRIBE / FETCH の responder で peer FIN による終端を期待しているテストはこの 2 件だけであり、他に同種のテストが見つかった場合は同じ方針で更新する
- `tests/test_session/subscription/subscription_limits.rs` の `request_update_rejection_close_goes_through_request_streams` は PUBLISH 起点なので対象外であり、更新しない
- `recv_request_stream_closed` の契約が変わるため、`skills/shiguredo-moqt/SKILL.md` の `RequestStreamEnd` と `recv_request_stream_closed` の記述を更新する

## 完了条件

- requester が SUBSCRIBE を送った直後に FIN し、その後 responder が SUBSCRIBE_OK を送れることを固定するテストが `src/session/tests.rs` または `tests/test_session/` に追加されていること
- requester が FETCH を送った直後に FIN し、その後 responder が FETCH_OK を送れることを固定するテストが追加されていること
- requester の FIN 後に responder が REQUEST_ERROR を送れることを固定するテストが追加されていること
- `Established` の subscription で responder が requester の FIN を受けても `Terminated` にならず、publisher が PUBLISH_DONE を送れることを固定するテストが追加されていること
- responder が peer FIN を受信済みのまま PUBLISH_DONE を送った時点で `SessionEvent::RequestTerminated { reason: PeerStreamFin }` が発行されることを固定するテストが追加されていること
- peer の RESET_STREAM では従来どおり request が `Terminated` になり `SessionEvent::RequestTerminated { reason: PeerStreamReset }` が発行されることが維持されていること
- 自側が requester のときに responder の FIN を受けて `SessionEvent::FinishRequestStream` が発行される既存挙動が維持されていること
- PUBLISH 起点の subscription で responder (subscriber 役) が peer FIN を受けたときの既存挙動が維持されていること
- `cargo test --workspace` が通ること
