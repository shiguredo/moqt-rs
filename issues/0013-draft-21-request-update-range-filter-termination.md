# REQUEST_UPDATE の Range Filter 上限超過で PUBLISH_DONE / FETCH reset / 状態遷移を行う

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-request-update-range-filter-termination

## 目的

draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions) の MUST を満たす。REQUEST_UPDATE 失敗時に対象 request を正しく終端し、subscription の孤立状態を残さない。

## 現状

`src/session/subscription/recv.rs` の `handle_peer_request_update` は、scope 検証の後に `check_incoming_range_filters` を呼ぶ。これが失敗すると `src/session/core.rs` の `check_incoming_range_filters` 内の `emit_request_error` で REQUEST_ERROR + FIN を送るだけになり、次が行われない。

- subscription: `Established` のまま、PUBLISH_DONE(UPDATE_FAILED) を送らない
- fetch: `Established` のまま、FETCH データストリームを reset しない
- どちらも Application に `RequestTerminated` が届かず、孤立した状態が残る

正規経路の `send_request_error` (`src/session/core.rs`) は request 種別ごとに PUBLISH_DONE 保留・データストリーム reset・`Terminated` 遷移を行う。上限超過経路はこれを迂回している。

根拠 (draft-ietf-moq-transport-21 §9.5.1):

> "When a REQUEST_UPDATE is unsuccessful, the publisher MUST also terminate the subscription by sending a PUBLISH_DONE with error code UPDATE_FAILED. When a REQUEST_UPDATE fails for a FETCH, the publisher MUST reset the FETCH data stream."

## 設計方針

`handle_peer_request_update` の Range Filter 検証失敗を、request 種別に応じた終端処理 (`send_request_error` 相当の経路) へルーティングする。`emit_request_error` は未登録 request の初回拒否専用に限定する。

## 完了条件

- REQUEST_UPDATE の Range Filter 上限超過で subscription に PUBLISH_DONE(UPDATE_FAILED) が送られること
- fetch では FETCH データストリームが reset され、`Terminated` に遷移すること
- 上限超過後に同じ request へ送信・状態参照して整合が取れること
- 回帰テストが `tests/test_session/` に追加されていること
