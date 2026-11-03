# REQUEST_UPDATE の処理順序と fill の非 coalescing を保証する

- Created: 2026-11-03
- Completed: 2026-11-03
- Branch: develop
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §6.4.2.2、§9.5、§9.19.15、§9.19.20](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/session/subscription/recv.rs` の `handle_update_for_subscription` は `Subscription::pending_update_params` へ後続値を merge してイベントを出す。
FILL_PARAMETERS 自体は sticky state から除去するが、fill を含む更新でも前の累積値と coalesce する。
`src/session/core.rs` は主に未応答更新の個数を管理する。

## 設計方針

request stream ごとに受信順の更新を保持し、処理と REQUEST_UPDATE_OK の返却順を保証する。
FILL_PARAMETERS を含む更新は coalescing の境界にし、更新時点のフィルタ・PAUSED で個別に処理する。
通常更新の coalescing は許容しつつ、成功ごとの OK と失敗時の単一 ERROR、クレジット回復を整合させる。
片方向の FIN は取消と区別し、request を維持する経路では未応答の更新とクレジットを保持して必要な応答を順に返す。cancel または request 完了時に残るキューとクレジットを解放する。
TRACK_NAMESPACE_PREFIX も非 coalescing だが、未対応 namespace 機能の新規実装はしない。

## 完了条件

- pause → fill → resume や異なる Location Filter の連続更新で、中間の fill が後続値へ置き換わらないこと。
- 成功応答の順番と個数が受信順に一致すること。request を維持する片方向 FIN の後も未応答更新への必須応答を返し、cancel または request 完了時にはキューとクレジットが残らないこと。
- `src/session/subscription/dispatch.rs` / `send.rs` と fetch の更新応答も含めて検証すること。

## 依存・関連

- 先行: [0380](0380-moqt-draft-23-fill-parameters-count.md)
- 先行: [0381](0381-moqt-draft-23-paused-parameter.md)
- 関連: [0389](0389-moqt-draft-23-subscribe-tracks-parameter-scope.md)

## 解決方法

### 受信順グループのキュー化

`src/session/types.rs` の `Subscription::pending_update_params` を `Option<MessageParameters>` から `VecDeque<PendingUpdateGroup>` へ変更した。
`PendingUpdateGroup` は適用待ちの累積パラメータ `parameters` と、そのグループが FILL_PARAMETERS を含む更新かを表す `has_fill_parameters` を持つ。

`src/session/subscription/recv.rs` の `handle_update_for_subscription` は次のように更新を蓄積する。

- 未応答の全グループと今回のメッセージを先頭から `merge_from` で合体した「受信順に処理した場合の累積表示」を作る。累積 Range Filter の上限検証、fill fetch stream の開設判定、`SessionEvent::RequestUpdateReceived` の通知にはこの表示を使う。
- 通常の更新は末尾グループへ `merge_from` で累積する (後の値が前を上書きする)。
- FILL_PARAMETERS を含む更新は独立したグループとし、直前のグループが fill を含む場合も新しいグループを開始する。これにより fill の前後が coalescing の境界になる (§9.19.15 の "A REQUEST_UPDATE containing FILL_PARAMETERS cannot be coalesced with other REQUEST_UPDATEs.")。
- グループのパラメータからは FILL_PARAMETERS を除去する (§9.19.15 の sticky 対象外)。アプリへ通知する累積表示には残す。

`src/session/subscription/dispatch.rs` の `send_ok_for_subscription` は先頭グループを 1 つ取り出して適用する。
応答は受信順に返す MUST (§9.5 "REQUEST_UPDATE_OKs MUST be returned in the order the REQUEST_UPDATEs were received.") があるため、呼び出し側がどの更新への応答を意図していても先頭グループが適用対象になる。
応答の個数は受信数と一致し、成功応答 1 回につき MAX_REQUEST_UPDATES のクレジットを 1 つ回復する (既存の `restore_incoming_request_update_credit`)。

### fill の非 coalescing

FILL_PARAMETERS は合体禁止のパラメータなので、fill を受信した時点の累積表示 (PAUSED / LOCATION_FILTER) で開設を判定し、後続の更新の値では開設しない。
pause → fill → resume の順に受信すると、fill は PAUSED=1 の時点の更新として扱われ、後続の resume によって fill fetch stream が開設されることはない (§3.1.1 の paused の意味)。
異なる Location Filter の連続更新では、fill を含むグループの応答時点でそのグループのフィルタが適用され、後続グループの値で置き換わらない。

### クレジットと失敗応答

- 応答 1 回ごとに受信側クレジットを 1 つ回復する。合体して保持しても応答は更新ごとに 1 つ必要なので、応答数とクレジット回復数が一致する。
- 失敗応答を返すと購読は Terminated になるため、未応答グループとクレジット会計をすべて解放する。複数の更新を合体して単一の REQUEST_UPDATE_ERROR を返す経路でも残数が残らない。
- FETCH は受信側キューを持たない (SUBSCRIBER_PRIORITY を受信時に適用する) が、応答の順序は呼び出し順で保証し、cancel・完了・失敗でクレジットを解放する。

### 片方向 FIN と解放

`Session::release_request_update_state` を追加した。
cancel (`RESET_STREAM` / `STOP_SENDING`)、両方向 FIN による request の完了、失敗応答の送受信、PUBLISH_DONE の送受信で、`pending_update_params` と `outgoing_request_updates` / `incoming_request_updates` を解放する。
一方、片方向の FIN を request 維持として扱う経路 (`Session::defers_peer_fin`) では解放せず、未応答の更新とクレジットを保持したまま `send_request_ok` で応答できるようにした。

### TRACK_NAMESPACE_PREFIX

TRACK_NAMESPACE_PREFIX も非 coalescing (§9.19.20) だが、SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS は本 Session が request として受理しないため新規実装は行っていない。
将来これらの request を扱う場合は同じ境界規則が必要になることを `PendingUpdateGroup` の doc に記した。

### 追加したテスト

- `src/session/tests.rs`: `request_termination_releases_pending_updates_and_credit` (片方向 FIN での保持、cancel と完了でのキュー・クレジット解放)
- `tests/test_session/subscription/request_update.rs`: `request_updates_are_processed_in_receive_order`、`fill_update_is_not_coalesced_with_later_location_filter`、`peer_fin_keeps_pending_updates_until_responses_are_sent`、`peer_cancel_discards_pending_updates`
- `tests/test_session/subscription/subscription_limits.rs`: `incoming_request_update_credit_recovers_after_each_ok`
- `tests/test_session/fetch/request_update.rs`: `fetch_request_updates_return_ok_in_receive_order`
- `pbt/tests/prop_session/subscription.rs`: `request_update_groups_apply_in_receive_order`
- `pbt/tests/prop_session/request_stream.rs`: `peer_fin_keeps_pending_request_updates_respondable`

## 完了条件の検証

- pause → fill → resume で中間の fill が後続値へ置き換わらないこと
  - `request_updates_are_processed_in_receive_order` が 3 つの更新を 3 グループとして保持し、fill を含むグループだけが境界となり、fill fetch stream が開設されないこと、各 REQUEST_UPDATE_OK で paused が true → true → false と受信順に適用されることを検証する。
  - `request_update_groups_apply_in_receive_order` が任意の更新列でグループ境界と応答ごとの適用状態を検証する。
- 異なる Location Filter の連続更新で中間の fill が後続値へ置き換わらないこと
  - `fill_update_is_not_coalesced_with_later_location_filter` が 1 通目 (FILL + LOCATION_FILTER=X) の応答で filter / filter_end が X の値になり、2 通目の応答で Y になることを検証する。
  - `request_update_groups_apply_in_receive_order` が応答回数ごとの filter を検証する。
- 成功応答の順番と個数が受信順に一致すること
  - `request_updates_are_processed_in_receive_order` が REQUEST_UPDATE 3 通に対し REQUEST_UPDATE_OK が 3 通、順序どおりに返り、`fin` が false であることを検証する。
  - `request_update_groups_apply_in_receive_order` が応答ごとにキューが 1 グループずつ減ることを、`fetch_request_updates_return_ok_in_receive_order` が FETCH でも 3 通の応答が同数・同順で返ることを検証する。
- request を維持する片方向 FIN の後も未応答更新への必須応答を返すこと
  - `peer_fin_keeps_pending_updates_until_responses_are_sent` が FIN 後も Established と 3 グループを保持し、応答が順に返り、その後に PUBLISH_DONE + FIN で `RequestTerminated { PeerStreamFin }` になることを検証する。
  - `peer_fin_keeps_pending_request_updates_respondable` が任意個の更新で FIN 後の応答とキューの減少を検証する。
- cancel または request 完了時にキューとクレジットが残らないこと
  - `peer_cancel_discards_pending_updates` が RESET_STREAM でキューが空になり `RequestTerminated { PeerStreamReset }` が発行されることを検証する。
  - `request_termination_releases_pending_updates_and_credit` が片方向 FIN ではキューとクレジットを保持し、cancel と完了で `pending_update_params` と `incoming_request_updates` が残らないことを検証する。
- `src/session/subscription/dispatch.rs` / `send.rs` と fetch の更新応答も含めた検証
  - `send_ok_for_subscription` (先頭グループの適用) を上記の統合テストが、`send_subscribe_ok` の pending 適用を既存テスト群が通ることで確認する。
  - `incoming_request_update_credit_recovers_after_each_ok` が応答ごとのクレジット回復を、`fetch_request_updates_return_ok_in_receive_order` が FETCH の応答順を検証する。
