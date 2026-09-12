# PUBLISH 起点 subscription の REQUEST_UPDATE ハンドシェイクを実装する

- Created: 2026-09-13
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-publish-originated-request-update
- Polished: {YYYY-MM-DD}

## 目的

PUBLISH で確立した subscription でも REQUEST_UPDATE / REQUEST_OK のハンドシェイクを行えるようにする。draft-ietf-moq-transport-21 §9.5 (REQUEST_UPDATE) は request の sender が REQUEST_UPDATE を送れることに加えて "A subscriber can also send REQUEST_UPDATE to modify parameters of a subscription established with PUBLISH." と規定しており、
PUBLISH 起点の subscription では publisher と subscriber の両方が REQUEST_UPDATE を送れる。現状は SUBSCRIBE 起点の方向しか通らず、PUBLISH 起点では送信済み REQUEST_UPDATE への REQUEST_OK の受信、および peer subscriber の REQUEST_UPDATE への REQUEST_OK 応答ができない。

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

- `send_ok_for_subscription` の拒否条件を「peer が REQUEST_UPDATE を送れる購読か」に合わせ、PUBLISH 起点で自側 publisher 役 (initiator) の Established 応答を許可する。Pending の PUBLISH_OK / SUBSCRIBE_OK 待ちの扱いは変えない。
- `handle_ok_for_subscription` の拒否条件を「自側が REQUEST_UPDATE を送れる購読か」に合わせ、PUBLISH 起点で自側 subscriber 役 (responder) の Established REQUEST_UPDATE_OK を許可する。Pending は PUBLISH_OK の受信と区別するため変えない。
- ガードの条件は `send_update_for_subscription` / `handle_update_for_subscription` の判定と対称になるように整理し、両者が一致していることをコメントに残す。
- `RequestOkReceived` の `request_kind` は購読の開始メッセージ種別 (SUBSCRIBE 起点なら `RequestKind::Subscribe`、PUBLISH 起点なら `RequestKind::Publish`) を返すように直す。
- 有効化する経路で Established 分岐の既存処理 (EXPIRES / LARGEST_OBJECT の適用、`pending_update_params` の適用、Forward 0→1 の STOP_SENDING 解除) が動くことをテストで確認する。

## 完了条件

- PUBLISH 起点で自側 subscriber 役の REQUEST_UPDATE に対する REQUEST_OK の受信がセッションクローズにならず、`RequestOkReceived` が発火すること
- PUBLISH 起点で自側 publisher 役が peer subscriber の REQUEST_UPDATE に REQUEST_OK を返せること。Forward 0→1 の受理で `stopped_outgoing_subgroups` の解除が行われること
- PUBLISH 起点の `SessionEvent::RequestOkReceived` の `request_kind` が `RequestKind::Publish` であること
- SUBSCRIBE 起点の既存挙動と Pending の PUBLISH_OK / SUBSCRIBE_OK の扱いが変わらないこと
- 回帰テストが `tests/test_session/subscription/request_update.rs` などに追加され、`cargo test --workspace` が通ること
- `CHANGES.md` の `## develop` に `[FIX]` エントリが追加されていること
