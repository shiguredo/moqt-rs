# validate_peer_request と recv_request の併用契約を明確にする

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-validate-peer-request-recv-request

## 目的

公開 API の `validate_peer_request` と `recv_request` を続けて呼んでも、正当な request が重複として拒否されないようにする。または両者の併用禁止を API 契約として明確にする。

## 現状

`src/session/core.rs` の `validate_peer_request` は内部で `accept_peer_request` を呼び、`RequestIdTracker` を進める。その後 Application が `recv_request` に同じ request_id を渡すと、`handle_peer_*` の先頭で再度 `accept_peer_request` が走り、重複として `INVALID_REQUEST_ID` でセッションを閉じる。

`tests/test_session/setup.rs` は `validate_peer_request` 単体しか検証しておらず、`recv_request` との併用契約が未定義。両 API の doc にも相互排他の記載が無い。

## 設計方針

次のいずれかに統一する。

- 低レベル API を `pub(crate)` 化して `recv_request` に一本化する
- `validate_peer_request` を read-only の peek 検証にする
- 併用してはならない旨を両 API の doc に明記する

## 完了条件

- 併用時の挙動が定義され、doc と実装が一致すること
- 併用契約を検証するテストが `tests/test_session/setup.rs` に追加されていること
