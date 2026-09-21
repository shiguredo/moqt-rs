# PUBLISH 起点 subscription の responder が FIN しない

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-publish-responder-fin
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §6.4.2.2 (Graceful Request Stream Closure) は、送るものが無くなった方向を速やかに FIN することを求め、responder の FIN を request 完了の合図と定義する。

> An endpoint SHOULD send a FIN promptly after a message when it has
> nothing further to send on that direction and will not need to
> respond to a future REQUEST_UPDATE.

> A FIN sent by the responder after its response and any subsequent
> messages for the request signals that the request is complete; if it
> has not already done so, the requester SHOULD then send a FIN on its
> direction, gracefully closing the stream.

PUBLISH は publisher が request を開始するため、PUBLISH を受けた subscriber が responder になる。現状この responder は PUBLISH_DONE を受けて subscription が `Terminated` になっても FIN せず、bidi request stream の自側送信方向が half-open のまま残る。responder が FIN しないため requester 側も request 完了の合図を受け取れない。

## 現状

- `src/session/core.rs` の `Session::is_local_requester` は `RequestTable::Subscription` の分岐で `RequestKind::Subscribe` 以外を false にするため、`RequestKind::Publish` では常に false になる。その結果 `Session::recv_request_stream_closed` は `SessionEvent::FinishRequestStream` を発行しない。
- `src/session/subscription/recv.rs` の `Session::handle_peer_publish_done` は subscription を `Terminated` に遷移させ `SessionEvent::PublishDoneReceived` を発行するのみで、FIN を発行する経路を持たない。
- `Session::send_request_ok` は PUBLISH_OK を `fin: false` で発行する。PUBLISH_OK 直後に FIN しない判断自体は §6.4.2.2 の "will not need to respond to a future REQUEST_UPDATE" に照らして妥当である。
- I/O 層 (`examples/moqt-transport/src/moqt_client.rs`) が FIN するのは `SessionEvent::FinishRequestStream` を受けたときだけであり、`SessionEvent::RequestTerminated` では FIN しない。
- publisher 側の `Session::send_publish_done` は PUBLISH_DONE を `fin: true` で送るため、SUBSCRIBE 起点の publisher responder は FIN する。欠けているのは PUBLISH 起点の subscriber responder である。

## 設計方針

- PUBLISH_DONE の受信で subscription が `Terminated` になった時点で、自側が responder なら FIN する経路を追加する。`Session::handle_peer_publish_done` から `SessionEvent::FinishRequestStream` を発行するか、`Session::is_local_requester` とは別に responder 用の判定を設けるかは実装側で決める。
- responder かどうかは `Subscription::initiator` (`SubscriptionInitiator::Publisher`) と `Subscription::my_role` (`TrackRole::Subscriber`) で判定できる。`RequestKind::Publish` で作られた subscription がこれにあたる。
- PUBLISH_OK の時点では FIN しない。publisher が REQUEST_UPDATE を送りうるためである (§6.4.2.2)。
- SUBSCRIBE 起点の requester が responder の FIN を受けて FIN する既存経路 (`Session::recv_request_stream_closed` の `Session::is_local_requester` 分岐) は変更しない。
- `Session::is_local_requester` が PUBLISH を常に false とする判断 (requester 側の FIN 規則を PUBLISH に適用しない) は維持し、responder 側の FIN を別経路として足す。

## 完了条件

- PUBLISH 起点 subscription の subscriber が PUBLISH_DONE を受信した後、`SessionEvent::FinishRequestStream` が発行される (bidi request stream の自側送信方向が FIN される) ことがテストで固定されていること。
- PUBLISH_OK の送信直後には `SessionEvent::FinishRequestStream` が発行されないことがテストで固定されていること。
- SUBSCRIBE 起点 subscription の既存 FIN 経路が変わらないことがテストで固定されていること。
- `cargo test --workspace` が通ること。
