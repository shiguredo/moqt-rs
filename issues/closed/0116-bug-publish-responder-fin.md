# PUBLISH 起点 subscription の responder が FIN しない

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-publish-responder-fin
- Polished: 2026-09-21

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
- `Session::send_request_ok` は PUBLISH_OK を `fin: false` で発行する。PUBLISH_OK 直後に FIN しない判断自体は、publisher (requester) が REQUEST_UPDATE を送りうるため妥当である
- I/O 層 (`examples/moqt-transport/src/moqt_client.rs`) が bidi 送信方向を FIN するのは `SessionEvent::SendOnStream { fin: true }`、`SessionEvent::FinishRequestStream`、`cleanup_closed_requests` の 3 経路であり、`SessionEvent::RequestTerminated` では FIN しない。本 issue で増やす必要があるのは `SessionEvent::FinishRequestStream` 経路である
- publisher 側の `Session::send_publish_done` は PUBLISH_DONE を `fin: true` で送るため、SUBSCRIBE 起点の publisher responder は FIN する。欠けているのは PUBLISH 起点の subscriber responder である

## 設計方針

- FIN を発行する契機は `Session::handle_peer_publish_done` に一本化する。`Session::is_local_requester` と `Session::recv_request_stream_closed` は変更しない
- responder 用の判定を `Session::recv_request_stream_closed` に足すと、`tests/test_session/subscription/handshake.rs` の `responder_does_not_get_finish_request_stream` が固定する「responder は peer FIN で FIN しない」という 0097 / 0108 の設計に反する
- FIN を発行するのは、自側が必須応答 (PUBLISH_OK / REQUEST_ERROR) を送り終えている場合に限る
- §6.4.2.2 は "An endpoint MUST NOT send a FIN on a direction of a request stream until it has sent all required messages on that direction for its request type." と定めている
- `Session::handle_peer_publish_done` は `Pending(Publisher)` (PUBLISH_OK 未送信) からの PUBLISH_DONE も受理するため、そこで FIN すると peer に request 失敗と扱われる
- 具体的な条件は、`Terminated` へ遷移させる前の state が `Established` であり、かつ自側が PUBLISH 起点の subscriber responder であること。`Pending(Publisher)` では発行しない
- state が既に `Terminated` の経路 (§9.5.1 の MUST に基づく PUBLISH_DONE(UPDATE_FAILED) を受理する経路) では発行しない。この経路は SUBSCRIBE 起点 requester の事象であり、requester 側の FIN は responder の FIN を受けた `Session::recv_request_stream_closed` の既存経路が担う
- 発行するイベントは `SessionEvent::FinishRequestStream { request_id }` である。request は既に `Terminated` かつ `PublishDoneReceived` の発行済みであり、この時点で自側が送るべきメッセージは残っていない
- `SessionEvent::FinishRequestStream` の doc は現在「requester 側で bidi request stream の送信方向を FIN で閉じる」と requester 専用に読める。responder 側でも PUBLISH_DONE 受信後の FIN に使うことを追記する
- PUBLISH_OK の時点では FIN しない。publisher (requester) が REQUEST_UPDATE を送りうるためである (§6.4.2.2 "will not need to respond to a future REQUEST_UPDATE")
- responder かどうかは `Subscription::my_role` が `TrackRole::Subscriber` であり、かつ `Subscription::initiator` が `SubscriptionInitiator::Publisher` であることに対応する (`RequestKind::Publish` で作られた subscription)
- `Session::handle_peer_publish_done` は `my_role != TrackRole::Subscriber` を拒否するため、この経路は常に PUBLISH 起点の subscriber responder である
- SUBSCRIBE 起点の requester が responder の FIN を受けて FIN する既存経路 (`Session::recv_request_stream_closed` の `Session::is_local_requester` 分岐) は変更しない
- `Session::is_local_requester` が PUBLISH を常に false とする判断 (requester 側の FIN 規則を PUBLISH に適用しない) は維持し、responder 側の FIN を別経路として足す
- 他の `Terminated` 遷移経路 (REQUEST_ERROR / STOP_SENDING / supersede) には FIN を追加しない。REQUEST_ERROR 後の PUBLISH_DONE(UPDATE_FAILED) は SUBSCRIBE 起点 requester の事象であり、STOP_SENDING による cancel は §6.4.2.3 のとおり FIN ではなく RESET_STREAM / STOP_SENDING で表現され、supersede は SUBSCRIBE 起点の requester 側の事象である

## 完了条件

- PUBLISH 起点 subscription の subscriber が `Established` のまま PUBLISH_DONE を受信した後、`SessionEvent::FinishRequestStream` が発行されることがテストで固定されていること。実際に FIN するのは I/O 層の責務であり、Session のテストではイベント発行までを固定する
- `Pending(Publisher)` (PUBLISH_OK 未送信) で PUBLISH_DONE を受信した場合は `SessionEvent::FinishRequestStream` が発行されないことがテストで固定されていること
- 既に `Terminated` の subscription が PUBLISH_DONE(UPDATE_FAILED) を受信した場合は `SessionEvent::FinishRequestStream` が発行されないことがテストで固定されていること
- PUBLISH_OK の送信直後には `SessionEvent::FinishRequestStream` が発行されないことがテストで固定されていること
- `SessionEvent::FinishRequestStream` の doc が requester 専用ではなく responder 側でも発行されることを示す形に更新されていること
- SUBSCRIBE 起点 subscription の既存 FIN 経路が変わらないことがテストで固定されていること
- `cargo test --workspace` が通ること
