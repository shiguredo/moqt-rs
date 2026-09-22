# PUBLISH の送信側が subscriber の FIN を受けると PUBLISH_DONE を送れない

- Created: 2026-09-22
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-publish-sender-peer-fin-termination
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §3.1 (Subscriptions) は subscription の終端手段を役割ごとに定める。

> The subscriber terminates a subscription in the Pending (Subscriber) or Established
> states by sending STOP_SENDING.  The publisher terminates a subscription in the
> Pending (Publisher) or Established states by sending PUBLISH_DONE and closing the stream.

publisher が PUBLISH_DONE を送れない状態に陥ってはならない。§6.4.2.2 (Graceful Request Stream Closure) は PUBLISH の送信者を「メッセージ送信直後の FIN」の例外とするため、subscriber の FIN は違反動作なしに届く。

> A requester, with the exception of the sender of PUBLISH, MAY FIN immediately
> after sending a message if it will not send a REQUEST_UPDATE.

現状は、PUBLISH を送った側 (publisher 役) が subscriber の FIN を受けると subscription が `Terminated` に遷移し、以後 `Session::send_publish_done` が必ず失敗する。publisher は §3.1 の MUST を果たせず、subscriber は購読終了を知らないまま残る。

## 現状

- `src/session/core.rs` の `Session::is_local_requester` は `RequestKind::Publish` に対して常に false を返す (`RequestTable::Subscription` の分岐で `RequestKind::Subscribe` 以外を false にする)
- `src/session/core.rs` の `Session::recv_request_stream_closed` は、自側が SUBSCRIBE / FETCH の responder のときだけ peer FIN を記録して終端を遅延させる。`RequestKind::Publish` はこの分岐の対象外である
- そのため `Session::close_subscription_on_stream_end` に到達し、`SubscriptionState::Terminated` が代入される
- `src/session/subscription/send.rs` の `Session::send_publish_done` は `Subscription::is_pending_publisher()` または `SubscriptionState::Established` を要求するため、`Terminated` からの送信は `SESSION_PROTOCOL_VIOLATION` になる
- この組合せは `Session::is_local_requester` が PUBLISH を常に false とする既存判断の帰結であり、`Session::recv_request_stream_closed` の doc にも「PUBLISH 起点で自側が PUBLISH を送った側 (publisher 役) の組合せは未対応である」と記載している
- 実測での再現: PUBLISH 送信 → peer の REQUEST_OK 受信 (Established) → peer FIN の通知 → `subscription.state` が `Terminated` になり、`Session::send_publish_done` が `SESSION_PROTOCOL_VIOLATION` (reason: "publish_done requires Pending(Publisher) or Established state") を返す
- `Session::recv_request_stream_closed` は `SessionEvent::FinishRequestStream` も発行しない (`Session::is_local_requester` が false のため)。publisher 側の送信方向は half-open のまま残る
- 0116 は PUBLISH 起点 subscription の subscriber responder が PUBLISH_DONE を受けた後に FIN しない問題であり、本 issue とは方向が逆である。`src/session/subscription/recv.rs` の `Session::handle_peer_publish_done` の経路であり、本 issue の `Session::recv_request_stream_closed` の経路とは重ならない

## 設計方針

- peer FIN を request の終端として扱うかどうかを、終端の方向ではなく自側の役割と購読の終端手段で分岐する。§3.1 が購読の終端を publisher の PUBLISH_DONE と subscriber の STOP_SENDING に限るため、subscriber の FIN は購読の終端ではない
- 自側が PUBLISH を送った側 (`Subscription::my_role` が `TrackRole::Publisher` かつ `Subscription::initiator` が `SubscriptionInitiator::Publisher`) の peer FIN は、`RequestKind::Subscribe` / `RequestKind::Fetch` の responder と同じく終端を遅延させる
- 終端の確定は 0108 で導入した `Session::peer_fin_received` / `Session::local_fin_sent` と `Session::finish_request_on_fin_exchange` をそのまま使う。PUBLISH_DONE は `fin: true` で送るため `local_fin_sent` に記録され、両方向が閉じた時点で `SessionEvent::RequestTerminated { reason: PeerStreamFin }` が発行される
- 自側が PUBLISH を受けた側 (subscriber 役) の peer FIN は従来どおり終端する。publisher の FIN は PUBLISH_DONE を送った後の完了通知である (§6.4.2.2 "the publisher of an Established subscription MUST send PUBLISH_DONE, before sending a FIN")
- `Pending(Publisher)` (REQUEST_OK 未受信) での peer FIN は従来どおり終端する。§6.4.2.2 "An endpoint that receives a FIN before all required messages have arrived treats the request as failed." に従う
- `RequestStreamEnd::Reset` は従来どおり即時に終端する。cancel は §6.4.2.3 (Request Cancellation and Rejection) が RESET_STREAM / STOP_SENDING と定める
- 自側が publisher 役の subscription は open 中の outgoing fill fetch stream を持ちうるため、終端時は `Session::reset_open_fill_streams` を呼ぶ (0108 で維持した挙動)
- `Session::send_publish_done` の送信条件と `SubscriptionState` の遷移は変更しない。peer FIN の時点で終端しないことが本 issue の修正である
- `Session::recv_request_stream_closed` の doc にある「PUBLISH 起点で自側が PUBLISH を送った側の組合せは未対応」の記述を、対応後の内容に更新する

## 完了条件

- PUBLISH を送った側が peer subscriber の FIN を受けた後も `SubscriptionState::Established` を維持し、`Session::send_publish_done` を送れることがテストで固定されていること
- その PUBLISH_DONE の送信時点で `SessionEvent::RequestTerminated { reason: PeerStreamFin }` が発行されることがテストで固定されていること
- 自側が PUBLISH を送った側で peer FIN が先、自側の PUBLISH_DONE が先の両順序で同じ終端結果になることがテストで固定されていること
- PUBLISH_DONE を送る前に peer FIN を受けた場合、`Session::recv_request_stream_closed` が `SessionEvent::FinishRequestStream` を発行しないことがテストで固定されていること (PUBLISH_DONE が最終メッセージであり、その送信が FIN を伴う)
- 自側が PUBLISH を受けた側 (subscriber 役) の peer FIN 終端と、`Pending(Publisher)` での peer FIN 終端の既存挙動が維持されていることがテストで固定されていること
- 自側が publisher 役の subscription で peer FIN の終端時に open 中の fill fetch stream を reset する既存挙動が維持されていること
- `cargo test --workspace` が通ること
