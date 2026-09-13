# PUBLISH 起点 subscription の REQUEST_UPDATE ハンドシェイクを実装する

- Created: 2026-09-13
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-publish-originated-request-update
- Polished: 2026-09-13

## 目的

PUBLISH で確立した subscription でも REQUEST_UPDATE / REQUEST_OK のハンドシェイクを行えるようにする。draft-ietf-moq-transport-21 §9.5 (REQUEST_UPDATE) は request の sender が REQUEST_UPDATE を送れることに加えて "A subscriber can also send REQUEST_UPDATE to modify parameters of a subscription established with PUBLISH." と規定しており、
PUBLISH 起点の subscription では publisher と subscriber の両方が REQUEST_UPDATE を送れる。現状は PUBLISH 起点で publisher (initiator) が送った REQUEST_UPDATE への REQUEST_OK の送受信は動作するが、subscriber (responder) が送った REQUEST_UPDATE への REQUEST_OK の受信と、publisher が peer subscriber の REQUEST_UPDATE に返す REQUEST_OK の送信ができない。

## 現状

subscription の REQUEST_UPDATE の送信可否は `src/session/subscription/send.rs` の `send_update_for_subscription` が「initiator 自身または自側 subscriber」、受信可否は `src/session/subscription/recv.rs` の `handle_update_for_subscription` が「peer が initiator または自側 publisher」で判定しており、PUBLISH 起点の両方向を受理する。しかし応答側は
`is_initiator_self()` による二者択一のガードになっており、PUBLISH 起点の経路が欠けている。

- `src/session/subscription/dispatch.rs` の `send_ok_for_subscription` は `is_initiator_self()` の購読を `request_ok (subscription) can only be sent by responder` で拒否する。PUBLISH を送った側 (自側 publisher、initiator) は peer subscriber の REQUEST_UPDATE に REQUEST_OK を返せない。`src/session/core.rs` の
  `stopped_outgoing_subgroups` の doc には、この制限により PUBLISH 起点では Forward 0→1 の REQUEST_UPDATE による再オープン解除が行われない既知の制限が記載されている (SUBSCRIBE 起点では動作する)。
- `src/session/subscription/dispatch.rs` の `handle_ok_for_subscription` は `!is_initiator_self()` の購読を `REQUEST_OK received on responder side` として `self.fail` でセッションクローズする。PUBLISH を受けた側 (自側 subscriber、responder) は `send_update_for_subscription` が REQUEST_UPDATE の送信を許可するのに、peer publisher からの
  REQUEST_OK を受信するとセッションが閉じる。
- 上記経路を有効化したときに `handle_ok_for_subscription` が発火する `SessionEvent::RequestOkReceived` の `request_kind` は `my_role` で決めており、PUBLISH 起点の subscriber 役では `RequestKind::Publish` ではなく `RequestKind::Subscribe` になる (`RequestKind` は bidi request stream の開始メッセージ種別)。

根拠 (draft-ietf-moq-transport-21 §9.5 (REQUEST_UPDATE)):

> The sender of a request (SUBSCRIBE, PUBLISH, FETCH, PUBLISH_NAMESPACE, SUBSCRIBE_NAMESPACE, SUBSCRIBE_TRACKS) can later send a REQUEST_UPDATE on the same bidi stream as the request to modify it.  A subscriber can also send REQUEST_UPDATE to modify parameters of a subscription established
> with PUBLISH.

## 設計方針

- `send_ok_for_subscription` の許可条件を「Established かつ peer が REQUEST_UPDATE を送れる購読 (peer が initiator、または自側が Publisher)」に合わせ、PUBLISH 起点で自側 publisher 役 (initiator) の Established 応答を許可する。
  拒否条件は `is_initiator_self() && !(state == Established && my_role == Publisher)` とし、Pending は現行どおり `is_initiator_self()` を拒否する。
  - `Pending(Publisher)` 分岐は PUBLISH を受けた自側 subscriber が PUBLISH_OK (REQUEST_OK) を返す経路であり、状態による限定を外すと PUBLISH 起点の自側 publisher がここへ入り、自分が送った PUBLISH を自分の応答で確立扱いにしてしまう。
- `handle_ok_for_subscription` の許可条件を「Established かつ自側が REQUEST_UPDATE を送れる購読 (自側が initiator、または自側が Subscriber)」に合わせ、PUBLISH 起点で自側 subscriber 役 (responder) の Established REQUEST_UPDATE_OK を許可する。
  拒否条件は `!is_initiator_self() && !(state == Established && my_role == Subscriber)` とし、Pending は現行どおり `!is_initiator_self()` を拒否する (自側 initiator の `Pending(Publisher)` は PUBLISH_OK 待ちとして維持する)。
  - Pending まで緩めると、PUBLISH を受けた自側 subscriber が peer publisher から REQUEST_OK を受信したときに `Pending(Publisher)` 分岐へ入り、応答を送っていない購読を確立扱いにしてしまう。
- ガードの条件は、`send_ok_for_subscription` が `handle_update_for_subscription` と、`handle_ok_for_subscription` が `send_update_for_subscription` と、それぞれ対称 (受理する REQUEST_UPDATE には REQUEST_OK を返せる / 自分が送った REQUEST_UPDATE の REQUEST_OK は受理する) になるように整理し、その対応をコメントに残す。2 つのガードが拒否する組合せは同じではないため、単一の共通述語には統合しない。
- `RequestOkReceived` の `request_kind` は購読の開始メッセージ種別を返すように直す。判定は `Subscription::initiator` を一次情報とし、`SubscriptionInitiator::Subscriber` なら SUBSCRIBE 起点で `RequestKind::Subscribe`、`SubscriptionInitiator::Publisher` なら PUBLISH 起点で `RequestKind::Publish` とする。`my_role` は自側が担う役割であり開始メッセージ種別ではないため使わない。
- `SessionEvent::RequestOkReceived` の doc にある「`request_kind` で context を判別する (Publish = 旧 PUBLISH_OK 相当)」は、PUBLISH 起点では PUBLISH_OK と REQUEST_UPDATE_OK の両方が `RequestKind::Publish` になるため誤りである。
  `request_kind` が購読の開始メッセージ種別を表し、PUBLISH_OK と REQUEST_UPDATE_OK の区別は応答時点の subscription state (`Pending(Publisher)` への初回応答か Established の REQUEST_UPDATE_OK か) で決まることを読み取れる内容へ直す。
  同じ doc コメントの `parameters` の例示と FORWARD の記述は 0083 が扱う。
- `src/session/core.rs` の `stopped_outgoing_subgroups` の doc にある「PUBLISH 起点 (自側 publisher) の subscription では peer subscriber の REQUEST_UPDATE に応答する経路がなく、Forward 0→1 による解除は行われない」既知の制限の記述を、当該経路が有効になった内容へ更新する。
- `src/session/core.rs` の `Session::send_request_ok` の doc にある「REQUEST_OK を送信する (responder 側のみ)」は、PUBLISH 起点 Established の自側 publisher (initiator) からの応答を許可すると偽になる。
  初回応答は responder、REQUEST_UPDATE への応答は REQUEST_UPDATE の受信側が送れるという実装に合う文言へ更新する。
- `send_ok_for_subscription` の `"request_ok (subscription) can only be sent by responder"` と `handle_ok_for_subscription` の `"REQUEST_OK received on responder side"` は、変更後の条件と一致する文言へ更新する。
- 有効化する経路で Established 分岐の既存処理 (EXPIRES / LARGEST_OBJECT の適用、`pending_update_params` の適用、Forward 0→1 の STOP_SENDING 解除) が動くことをテストで確認する。

## 完了条件

- PUBLISH 起点で自側 subscriber 役 (responder) が送った REQUEST_UPDATE に対する peer publisher からの REQUEST_OK の受信がセッションクローズにならず、`RequestOkReceived` が発火すること
- PUBLISH 起点で自側 publisher 役 (initiator) が peer subscriber の REQUEST_UPDATE に REQUEST_OK を返せること。Forward 0→1 の受理で `stopped_outgoing_subgroups` の解除が行われること
- 新たに受理する経路で Established 分岐の既存処理が動くこと
  - REQUEST_UPDATE_OK の EXPIRES 適用と LARGEST_OBJECT の `largest_location` への保存 (自側 subscriber 役のとき)
  - REQUEST_UPDATE_OK への LARGEST_OBJECT の注入
  - 受理した REQUEST_UPDATE の `pending_update_params` の適用
- PUBLISH 起点で自側 subscriber 役 (responder) が Established の REQUEST_UPDATE_OK を受信したときの `SessionEvent::RequestOkReceived` の `request_kind` が `RequestKind::Publish` であること (元から `RequestKind::Publish` になる既存経路ではなく、新たに受理する経路で `RequestOkReceived` を捕捉して検証する)
- Pending の既存到達経路が変わらないこと
  - `Pending(Publisher)` の自側 subscriber (responder) が `send_request_ok` で PUBLISH_OK を送ると Established になること
  - `Pending(Publisher)` の自側 publisher (initiator) が `send_request_ok` を呼ぶと従来どおり拒否されること
  - `Pending(Publisher)` の自側 subscriber (responder) が peer publisher から REQUEST_OK を受信すると従来どおりセッションクローズになること
- SUBSCRIBE 起点の既存挙動 (Established の REQUEST_UPDATE_OK 送受信と Pending の SUBSCRIBE_OK) が変わらないこと
- `SessionEvent::RequestOkReceived`、`stopped_outgoing_subgroups`、`Session::send_request_ok` の doc が実装と一致していること
- 回帰テストが `tests/test_session/subscription/request_update.rs` などに追加され、`cargo test --workspace` が通ること
- `CHANGES.md` の `## develop` に `[FIX]` エントリが追加されていること
