# 未到達 Request ID の保持上限超過で INVALID_REQUEST_ID にしない

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-remove-out-of-order-request-id-limit
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §6.4.2.1 (Request ID) が INVALID_REQUEST_ID でのセッション終了を MUST とするのは次の 2 条件のみである。

> If an endpoint receives a Request ID where the least significant bit
> is incorrect for the sender, or a duplicate Request ID, it MUST close
> the session with INVALID_REQUEST_ID.

現状は実装独自の保持上限を超えた時点で同じ INVALID_REQUEST_ID を返すため、どちらの条件にも該当しない peer をセッション終了させうる。bidi request stream は複数本が並行して届くため、peer が前縁より先の Request ID を先に開くこと自体は仕様違反ではない。仕様に無い条件でセッションを閉じると、正当な peer との interop が壊れる。

## 現状

- `src/session/request_id.rs` の `RequestIdTracker` は `front` (連続受信前縁) と `above` (前縁より先に受信した未到達 ID) を保持する。
- `RequestIdTracker::accept` は `above` への新規挿入前に `above.len() >= MAX_OUT_OF_ORDER_REQUEST_IDS` を検査し、超過時は `SESSION_INVALID_REQUEST_ID` の `SessionError` を返す。`MAX_OUT_OF_ORDER_REQUEST_IDS` は 1024 で、doc コメント自身が上限超過を INVALID_REQUEST_ID としてセッションを閉じさせる実装保護だと説明している。
- `src/session/core.rs` の `Session::validate_peer_request_id` が `accept` の `Err` で `Session::fail` を呼ぶため、`Session::recv_request` 経由でセッションが `Closing` に遷移する。`Session::recv_request` の doc も「draft 由来ではない実装保護として、未到達 Request ID の保持上限 (`MAX_OUT_OF_ORDER_REQUEST_IDS`) 超過でも同じ `INVALID_REQUEST_ID` で閉じる」と明記している。
- `src/session/tests.rs` の `peer_request_tracker_rejects_too_many_out_of_order_ids` が上限超過 = INVALID_REQUEST_ID を固定している。

## 設計方針

- 仕様が要求しないセッション終了条件を外す。`RequestIdTracker::accept` が `SessionError` を返すのは parity 違反と重複の 2 条件のみにする。
- 資源保護を残す場合は、上限超過時にセッションを閉じるのではなく仕様の MUST に触れない方法を選ぶ。候補は次の 2 つ。
  - 保持上限を撤廃する。`above` の大きさは peer が同時に開いている request stream 数で律速され、transport の stream 数上限が実質の上限になる。
  - 上限到達後も新規の飛び ID は受理し、`above` への記録のみを省略する。メモリは有界になるが、記録しなかった ID については重複検出が働かず、§6.4.2.1 の MUST (重複で閉じる) を満たせない場合が生じる。採用する場合はこの劣化範囲を doc コメントに明記する。
- 前縁を進めて追跡を打ち切る方式は採用しない。未到達 ID の初回受信を重複と誤判定し、仕様に無い INVALID_REQUEST_ID を新たに生むためである。
- `MAX_OUT_OF_ORDER_REQUEST_IDS` の値・公開範囲と `src/session/tests.rs` のテストの扱いを決定に合わせて更新し、`Session::recv_request` の doc から実装保護の記述を実装に合わせて直す。

## 完了条件

- 前縁より先の Request ID を `MAX_OUT_OF_ORDER_REQUEST_IDS` を超えて `RequestIdTracker::accept` に渡しても `Ok` を返し、その状態の `Session` がセッションを閉じないことがテストで固定されていること。
- parity 違反と重複が引き続き `INVALID_REQUEST_ID` で拒否されることがテストで固定されていること。
- 資源保護を残す場合、採用方式と重複検出が劣化する範囲が doc コメントに明記されていること。
- `cargo test --workspace` が通ること。
