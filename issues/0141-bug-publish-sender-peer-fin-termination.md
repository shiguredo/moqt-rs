# PUBLISH の送信側が subscriber の FIN を受けると PUBLISH_DONE を送れない

- Created: 2026-09-22
- Completed: 2026-09-25
- Branch: feature/fix-publish-sender-peer-fin-termination
- Polished: 2026-09-25

## 目的

draft-ietf-moq-transport-21 §3.1 (Subscriptions) は subscription の終端手段を役割ごとに定める。

> The subscriber terminates a subscription in the Pending (Subscriber) or Established
> states by sending STOP_SENDING.  The publisher terminates a subscription in the
> Pending (Publisher) or Established states by sending PUBLISH_DONE and closing the stream.

publisher が PUBLISH_DONE を送れない状態に陥ってはならない。§6.4.2.2 (Graceful Request Stream Closure) は
「送るものが無くなったら速やかに FIN を送る」SHOULD を定め、PUBLISH の送信者だけをその例外 (FIN を遅らせてよい) とするため、
subscriber の FIN は違反動作なしに届く。

> An endpoint SHOULD send a FIN promptly after a message when it has nothing further to
> send on that direction and will not need to respond to a future REQUEST_UPDATE.  A
> requester, with the exception of the sender of PUBLISH, MAY FIN immediately after
> sending a message if it will not send a REQUEST_UPDATE.

現状は、PUBLISH を送った側 (publisher 役) が subscriber の FIN を受けると subscription が `Terminated` に遷移し、
以後 `Session::send_publish_done` が必ず失敗する。publisher は §6.4.2.2 の次の MUST と §9.9 (PUBLISH_DONE) の次の MUST NOT を果たせない。

> ... the publisher of an Established subscription MUST send PUBLISH_DONE, before sending a
> FIN.

> A sender MUST NOT destroy subscription state until it sends PUBLISH_DONE, though it can
> choose to stop sending objects (and thus send PUBLISH_DONE) for any reason.

subscriber は購読終了を知らないまま残る
(§3.1.1 "A subscriber keeps subscription state until it cancels the request (see Section 6.4.2.3), or until receipt of a PUBLISH_DONE or REQUEST_ERROR.")。

## 現状

- `src/session/core.rs` の `Session::is_local_requester` は `RequestKind::Publish` に対して常に false を返す
  (`RequestTable::Subscription` の分岐で `RequestKind::Subscribe` 以外を false にする)
- `src/session/core.rs` の `Session::recv_request_stream_closed` は、自側が SUBSCRIBE / FETCH / TRACK_STATUS の responder のときだけ
  peer FIN を記録して終端を遅延させる (`RequestKind::Subscribe | RequestKind::Fetch | RequestKind::TrackStatus` と `!Session::is_local_requester`)。
  `RequestKind::Publish` はこの分岐の対象外である
- そのため `Session::close_subscription_on_stream_end` に到達し、`SubscriptionState::Terminated` が代入される
- `src/session/subscription/send.rs` の `Session::send_publish_done` は `Subscription::is_pending_publisher()` または
  `SubscriptionState::Established` を要求するため、`Terminated` からの送信は `SESSION_PROTOCOL_VIOLATION` になる
- この組合せは `Session::is_local_requester` が PUBLISH を常に false とする既存判断の帰結であり、
  `Session::recv_request_stream_closed` の doc にも「PUBLISH 起点で自側が PUBLISH を送った側 (publisher 役) の組合せは未対応である」と記載している
- `Session::recv_request_stream_closed` は `SessionEvent::FinishRequestStream` も発行しない (`Session::is_local_requester` が false のため)。
  PUBLISH の最終メッセージである PUBLISH_DONE の送信が `fin` を伴うため、終端は PUBLISH_DONE の送信時点で確定する
- 実測での再現: PUBLISH 送信 → peer の REQUEST_OK 受信 (Established) → peer FIN の通知 → `subscription.state` が `Terminated` になり、
  `Session::send_publish_done` が `SESSION_PROTOCOL_VIOLATION` (reason: "publish_done requires Pending(Publisher) or Established state") を返す
- 自側が PUBLISH を送った側で open 中の fill fetch stream がある場合の現行挙動は
  `tests/test_session/fetch/fill.rs` の `publish_origin_peer_fin_resets_open_fill_streams` が固定している。
  同テストは peer FIN の時点で `SubscriptionState::Terminated` と fill fetch stream の reset
  (`SessionEvent::ResetDataStream` の `STREAM_CANCELLED`) を期待しており、本 issue の修正で期待値の更新が必要になる
- 0116 は PUBLISH 起点 subscription の subscriber responder が PUBLISH_DONE を受けた後に FIN しない問題であり、本 issue とは方向が逆である。
  `src/session/subscription/recv.rs` の `Session::handle_peer_publish_done` の経路であり、
  本 issue の `Session::recv_request_stream_closed` の経路とは重ならない

## 設計方針

- peer FIN を request の終端として扱うかどうかを、終端の方向ではなく自側の役割と購読の終端手段で分岐する。
  §3.1 が購読の終端を publisher の PUBLISH_DONE と subscriber の STOP_SENDING に限るため、subscriber の FIN は購読の終端ではない
- 遅延の条件は `RequestKind::Publish` について次をすべて満たす場合とする。
  既存の SUBSCRIBE / FETCH / TRACK_STATUS の遅延条件式には手を入れず、TRACK_STATUS の遅延を維持する
  - 自側が PUBLISH を送った側である (`Subscription::initiator` が `SubscriptionInitiator::Publisher` かつ
    `Subscription::my_role` が `TrackRole::Publisher`)
  - `SubscriptionState::Established` である
  - `Session::is_local_requester` は `RequestKind::Publish` で常に false を返すため役割の判別に使えない。
    `RequestKind::Subscribe` / `RequestKind::Fetch` / `RequestKind::TrackStatus` の responder 判定 (`!Session::is_local_requester`) では
    publisher 役と subscriber 役を区別できないため、`Subscription::initiator` と `Subscription::my_role` を明示的に見る
- `Pending(Publisher)` (REQUEST_OK 未受信) での peer FIN は従来どおり終端する。
  この場合は遅延の条件に含めない
  (§6.4.2.2 "An endpoint that receives a FIN before all required messages have arrived treats the request as failed." に従う)
- 自側が PUBLISH を受けた側 (subscriber 役) の peer FIN は従来どおり終端する。
  publisher の FIN は PUBLISH_DONE を送った後の完了通知である
  (§6.4.2.2 "the publisher of an Established subscription MUST send PUBLISH_DONE, before sending a FIN")
- `RequestStreamEnd::Reset` は従来どおり即時に終端する。
  cancel は §6.4.2.3 (Request Cancellation and Rejection) が RESET_STREAM / STOP_SENDING と定める
- 終端の確定は 0108 で導入したフィールド `Session::peer_fin_received` / `Session::local_fin_sent` と
  メソッド `Session::finish_request_on_fin_exchange` をそのまま使う。
  PUBLISH_DONE は `fin: true` で送るため `Session::mark_send_direction_closed_with_fin` が `local_fin_sent` に記録し、
  両方向が閉じた時点で `SessionEvent::RequestTerminated { reason: PeerStreamFin }` が発行される
- 「`Session::peer_fin_received` の id は必ず `request_streams` にも entry を持ち、両者は同時に除去する」という既存の不変条件を維持する。
  `Session::finish_request_on_fin_exchange` が両集合と `request_streams` を同時に除去する経路に PUBLISH も乗せる
- open 中の fill fetch stream は peer FIN では reset しない。
  §6.4.2.2 が FIN を「cancel ではない」と定めており、§3.4.1 (Opening and Closing Fill Fetch Streams) の
  "When the subscription is cancelled, the publisher MUST reset any open fill fetch streams." の対象は
  cancel (STOP_SENDING / RESET_STREAM) に限られる。同節は
  "Resetting or cancelling a fill fetch stream, by either endpoint, does not affect the subscription,
  which continues to deliver objects using subscribe subgroups and datagrams." とも定めており、
  購読が継続する限り fill の配送も継続する
  - したがって `Session::close_subscription_on_stream_end` の `Session::reset_open_fill_streams` はこの経路では呼ばれない。
    reset は購読の終端 (cancel) の経路で従来どおり行う
  - 副作用として、open 中の outgoing stream (fill fetch / subgroup) が残っている間は §9.9 の
    "A sender MUST NOT send PUBLISH_DONE until it has closed all streams it will ever open, and has no further datagrams to send,
    for a subscription." により `Session::send_publish_done` が従来どおり
    `SESSION_PROTOCOL_VIOLATION` ("publish_done requires all outgoing streams closed") で拒否される。
    アプリは fill の完了 (FIN) または `Session::send_fetch_data_stream_closed`、subgroup の `Session::send_data_stream_closed` で
    閉じてから PUBLISH_DONE を送る。この拒否は仕様どおりであり本 issue では変更しない
- `Session::send_publish_done` の送信条件と `SubscriptionState` の遷移は変更しない。peer FIN の時点で終端しないことが本 issue の修正である
- 遅延の導入で新たに到達可能になる経路を塞ぐ。
  `src/session/subscription/dispatch.rs` の `Session::handle_err_for_subscription` は、Established の PUBLISH 起点 publisher 役
  (本 issue の遅延条件と同じ組合せ) で REQUEST_ERROR (REQUEST_UPDATE 失敗応答) を受けると
  PUBLISH_DONE(UPDATE_FAILED) を直接 push する。この分岐は `Session::mark_send_direction_closed_with_fin` を呼んでおらず、
  peer FIN が `Session::peer_fin_received` に記録済みでも終端が確定しない。
  保留側の `Session::maybe_flush_pending_publish_done` は同メソッドを呼んでいるため、直接 push する分岐にも同じ呼び出しを加えて対称にする。
  修正前は peer FIN の時点で subscription が `Terminated` になるためこの組合せは到達せず、本 issue の修正で初めて到達可能になる
- 新しい `SessionEvent` は追加しない。peer FIN は購読の終端要求ではないため、PUBLISH_DONE を送る契機はアプリ自身の判断 (配信の終了) であり、
  peer FIN でイベントを発行しない既存の SUBSCRIBE / FETCH / TRACK_STATUS responder の遅延経路と同じ扱いにする
- 遅延中に同じ peer FIN が再通知されても `Session::peer_fin_received` への記録は冪等で、
  `Session::finish_request_on_fin_exchange` は両方向が揃うまで何もしない。
  終端確定後の再通知の扱いは既存の responder の遅延経路と同じにし、本 issue で新たな挙動を導入しない
- 変更対象は次のとおりである
  - `src/session/core.rs`: `Session::recv_request_stream_closed` の遅延条件と、
    `Session::peer_fin_received` / `Session::finish_request_on_fin_exchange` の doc
    (遅延対象の列挙に PUBLISH 起点の publisher 役を加え、既存の TRACK_STATUS の記載漏れも直す)
  - `src/session/subscription/dispatch.rs`: `Session::handle_err_for_subscription` の PUBLISH_DONE(UPDATE_FAILED) を
    直接 push する分岐に `Session::mark_send_direction_closed_with_fin` の呼び出しを加え、保留側の
    `Session::maybe_flush_pending_publish_done` と対称にする。あわせて同分岐の
    「PUBLISH 起点の request は peer FIN を `peer_fin_received` に記録しない」というコメントを実装に合わせて直す
  - `src/session/types.rs`: `SessionEvent::RequestTerminated` の doc にある `PeerStreamFin` の発行条件の列挙に、
    遅延対象として PUBLISH の送信側 (publisher 役、Established) と TRACK_STATUS を加える
  - `src/session/subscription/send.rs`: `Session::close_subscription_on_stream_end` の doc の
    「自側が SUBSCRIBE の responder のとき」という遅延条件の説明に、TRACK_STATUS と PUBLISH の送信側を加える
  - `tests/test_session/request_stream.rs`: PUBLISH 起点 publisher 役の FIN 交換テストを追加する
  - `tests/test_session/subscription/publish_done.rs`: peer FIN 後の PUBLISH_DONE 送信テストと、
    peer FIN 記録後に PUBLISH_DONE(UPDATE_FAILED) を送る経路のテストを追加する
  - `tests/test_session/fetch/fill.rs`: `publish_origin_peer_fin_resets_open_fill_streams` を
    新しい挙動 (peer FIN では終端も reset もしない) に更新する
  - `skills/shiguredo-moqt/SKILL.md`: `Session::recv_request_stream_closed` と
    `RequestTerminated { reason: PeerStreamFin }` の記述を、終端しない条件に PUBLISH の送信側 (publisher 役、Established) を
    加えた内容に更新する (0108 と同じ契約変更のため)
- `pbt/tests/prop_session/request_stream.rs` の `publish_responder_terminates_on_peer_fin` は
  自側が PUBLISH を受けた側 (subscriber 役) の挙動を固定しており、期待値とテスト名は変更しない。
  ただし同テストの doc コメントにある「requester 側の組合せは本 API の未対応範囲」は本 issue の修正で事実に反するため、
  自側が PUBLISH を送った側は本 issue で対応する旨に更新する

## 完了条件

- PUBLISH を送った側が peer subscriber の FIN を受けた後も `SubscriptionState::Established` を維持し、
  open 中の outgoing stream が無ければ `Session::send_publish_done` を送れることがテストで固定されていること
  (`tests/test_session/request_stream.rs` と `tests/test_session/subscription/publish_done.rs` に追加する)
- その PUBLISH_DONE の送信時点で `SessionEvent::RequestTerminated { reason: PeerStreamFin }` が発行されることがテストで固定されていること
- 自側が PUBLISH を送った側で peer FIN が先、自側の PUBLISH_DONE が先の両順序で同じ終端結果になることがテストで固定されていること
- PUBLISH_DONE を送る前に peer FIN を受けた場合、`Session::recv_request_stream_closed` が
  `SessionEvent::FinishRequestStream` を発行しないことがテストで固定されていること
- 自側が PUBLISH を送った側で open 中の fill fetch stream がある場合の新しい挙動がテストで固定されていること
  - peer FIN の時点で `SubscriptionState::Established` を維持し、fill fetch stream が reset されない
    (`SessionEvent::ResetDataStream` が発行されず、`Session::open_outgoing_fill_stream_count` が減らない)
  - その状態では `Session::send_publish_done` が `SESSION_PROTOCOL_VIOLATION` で拒否される (§9.9 の MUST NOT に対応する既存挙動)
  - `Session::send_fetch_data_stream_closed` (fill) と `Session::send_data_stream_closed` (subgroup) で
    open 中の outgoing stream を閉じた後に `Session::send_publish_done` を送れる
  - この固定は `tests/test_session/fetch/fill.rs` の既存テスト `publish_origin_peer_fin_resets_open_fill_streams` の
    期待値を新しい挙動に更新することで行う
- 自側が PUBLISH を受けた側 (subscriber 役) の peer FIN 終端、`Pending(Publisher)` での peer FIN 終端、
  `RequestStreamEnd::Reset` の即時終端の既存挙動が維持されていることがテストで固定されていること
- SUBSCRIBE / FETCH / TRACK_STATUS の responder が peer FIN で終端しない既存挙動が退行していないこと
  (`pbt/tests/prop_session/request_stream.rs` の `track_status_responder_survives_requester_fin` と
  `publish_responder_terminates_on_peer_fin` を含む)
- peer FIN を記録済みの PUBLISH 起点 publisher 役で REQUEST_ERROR (REQUEST_UPDATE 失敗応答) を受けて
  PUBLISH_DONE(UPDATE_FAILED) を送ったときも、`SessionEvent::RequestTerminated { reason: PeerStreamFin }` が
  発行されることがテストで固定されていること (`tests/test_session/subscription/publish_done.rs` に追加する)
- 本 issue の修正で意味が変わる doc とコメントがすべて実装と一致していること。少なくとも
  `src/session/core.rs` の `Session::recv_request_stream_closed` / `Session::peer_fin_received` / `Session::finish_request_on_fin_exchange`、
  `src/session/types.rs` の `SessionEvent::RequestTerminated`、
  `src/session/subscription/dispatch.rs` の `Session::handle_err_for_subscription` の分岐コメント、
  `src/session/subscription/send.rs` の `Session::close_subscription_on_stream_end`、`skills/shiguredo-moqt/SKILL.md`、
  `pbt/tests/prop_session/request_stream.rs` の `publish_responder_terminates_on_peer_fin` の doc コメントを対象とする
- `cargo test --workspace` が通ること

## 解決方法

`Session::recv_request_stream_closed` の遅延条件を新しい private メソッド `Session::defers_peer_fin` に集約し、PUBLISH を送った側 (publisher 役) の peer FIN を終端として扱わないようにした。

- `src/session/core.rs`: `Session::defers_peer_fin` を追加し、`Session::recv_request_stream_closed` の分岐を
  `matches!(end, RequestStreamEnd::Fin) && self.defers_peer_fin(request_id, kind)` に置き換えた。
  PUBLISH の条件は「`Pending(Publisher)` ではない」「`Subscription::initiator` が Publisher」「`Subscription::my_role` が Publisher」
  「自側の最終メッセージ未送信 (`Session::local_fin_sent` に無い)」である
  - 設計方針では `Established` に限るとしていたが、§9.9 の MUST NOT で保留した PUBLISH_DONE が残っている間は
    `Terminated` でも自側の最終メッセージが未送信であるため、条件を「最終メッセージ未送信」まで広げた。
    先に終端するとアプリが購読を破棄でき、§9.5.1 の MUST を果たせない
- `src/session/subscription/dispatch.rs`: `Session::handle_err_for_subscription` の Pending / Established 分岐で
  `Session::reset_open_fill_streams` を呼び、自側が失敗応答を送る `Session::send_err_for_subscription` と対称にした。
  open 中の fill fetch stream が残ると、保留した PUBLISH_DONE が fill fetch stream の終端では送られず §9.5.1 の MUST を果たせない
- `src/session/subscription/send.rs`: peer FIN を受信済みの subscription への `Session::send_request_update` を
  `SESSION_PROTOCOL_VIOLATION` で拒否する。peer は応答できず、応答待ちのまま `CONTROL_MESSAGE_TIMEOUT` でセッションを閉じるためである
- doc の更新: `Session::recv_request_stream_closed` / `Session::peer_fin_received` /
  `Session::finish_request_on_fin_exchange` / `Session::close_subscription_on_stream_end` /
  `Session::maybe_flush_pending_publish_done` / `Session::send_request_update` /
  `SessionEvent::RequestTerminated` / `Session::clear_request_stream_goaway_deadline` を対応後の内容にし、
  応答の MUST の節番号を SUBSCRIBE (§3.1) / FETCH (§3.2.1) / TRACK_STATUS (§9.13) に書き分けた。
  `skills/shiguredo-moqt/SKILL.md` も追随させた
- 追加・更新したテスト
  - `tests/test_session/request_stream.rs`: peer FIN 後の PUBLISH_DONE 送信と `RequestTerminated { reason: PeerStreamFin }`、
    PUBLISH_DONE が先の場合、遅延中の RESET (cancel) の即時終端、`Pending(Publisher)` の peer FIN 終端、
    peer FIN 後の REQUEST_UPDATE 拒否
  - `tests/test_session/subscription/publish_done.rs`: 自側 REQUEST_UPDATE の失敗応答後に peer FIN が届いても終端が失われないこと
  - `tests/test_session/fetch/fill.rs`: peer FIN では fill fetch stream を reset せず PUBLISH_DONE を保留すること、
    受信 REQUEST_ERROR では §3.4.1 の MUST により reset すること、cancel (RESET_STREAM) では reset すること
  - `tests/test_session.rs`: PUBLISH 起点 publisher 役の共有ヘルパー
- `CHANGES.md` の `## develop` に `[FIX]` エントリを追加した
