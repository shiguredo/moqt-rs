# 未到達 Request ID の保持上限超過で INVALID_REQUEST_ID にしない

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-remove-out-of-order-request-id-limit
- Polished: 2026-09-21

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

- 仕様が要求しないセッション終了条件を外す。`RequestIdTracker::accept` が `SessionError` を返すのは parity 違反と重複の 2 条件のみにする
- 保持上限は撤廃する。`MAX_OUT_OF_ORDER_REQUEST_IDS` を削除し、`above` への挿入を無条件に受理する。公開定数の削除なので `CHANGES.md` の `## develop` に `[CHANGE]` を記載する
- 上限超過時に記録を省略して受理する案は採用しない。記録しなかった Request ID の再受信を重複として検出できなくなり、§6.4.2.1 の MUST (重複で `INVALID_REQUEST_ID` でセッションを閉じる) を意図的に満たせない状態を作るためである
- 前縁を進めて追跡を打ち切る方式も採用しない。未到達 ID の初回受信を重複と誤判定し、仕様に無い `INVALID_REQUEST_ID` を新たに生むためである
- 撤廃後のメモリは doc コメントに明記する。仕様に準拠した peer は Request ID を 2 ずつ連番で使うため、前縁が埋まるたびに `above` から吸収され、保持数は同時に未到達な request 数で抑えられる。前縁が埋まらないまま飛び ID を送り続ける非準拠 peer では `above` が増え続けるが、§6.4.2.1 は parity 違反と重複以外でのセッション終了を許しておらず、上限超過で正当な peer を落とす方が interop への影響が大きいため、この増加は許容する
- `MAX_OUT_OF_ORDER_REQUEST_IDS` を消すことに伴い、`src/session/request_id.rs` の定数 doc と `RequestIdTracker` の doc、`src/session/core.rs` の `Session::recv_request` doc にある「実装保護として上限超過で `INVALID_REQUEST_ID` に閉じる」記述を実装に合わせて直す
- `src/session/tests.rs` の `peer_request_tracker_rejects_too_many_out_of_order_ids` は、上限超過を拒否する挙動を固定しているため置き換える

## 完了条件

- 前縁より先の Request ID を旧 `MAX_OUT_OF_ORDER_REQUEST_IDS` (1024) を大きく超える件数 (4096 件など) `RequestIdTracker::accept` に渡しても `Ok` を返し、その状態の `Session` がセッションを閉じないことがテストで固定されていること
- 前縁が埋まったときに `above` が吸収され、重複判定が変わらないことがテストで固定されていること
- parity 違反と重複が引き続き `INVALID_REQUEST_ID` で拒否されることがテストで固定されていること
- 撤廃後のメモリ特性 (準拠 peer では同時未到達 request 数で抑えられること、非準拠 peer では増え続けることを許容する根拠) が doc コメントに明記されていること
- `cargo test --workspace` が通ること
