# STOP_SENDING 後の Subgroup 再オープンを Forward 0→1 更新時のみ許可する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-stop-sending-subgroup-reopen

## 目的

draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams) の SHOULD NOT に従う。STOP_SENDING を受けた Subgroup への再オープンを、Forward State が 0 から 1 へ変わった場合に限定する。

## 現状

`src/subgroup_tracker.rs` の `can_reopen` は `StoppedByPeer` を無条件に再オープン可能として扱う。`open` 呼び出し元 (`src/session/data.rs`) にも `REQUEST_UPDATE` の Forward State 0→1 を確認する gating がない。`src/subgroup_tracker.rs` のコメントは「仕様は reset の理由で区別していない」としているが、STOP_SENDING については正確ではない。

根拠 (draft-ietf-moq-transport-21 §11.3.2):

> "A publisher that receives a STOP_SENDING on a Subgroup stream SHOULD NOT attempt to open a new stream to deliver additional Objects in that Subgroup. However, if the publisher subsequently receives a REQUEST_UPDATE that changes the Forward State from 0 to 1, it MAY open a new stream..."

## 設計方針

STOP_SENDING 由来の再オープンは、当該 request の Forward State が 0 から 1 へ更新された場合のみ許可する。`Subscription` は `forward_state` を保持しているため gating 可能。reset 由来と STOP_SENDING 由来を区別できる状態を `SubgroupTracker` に持たせる。

## 完了条件

- STOP_SENDING 後の再オープンが Forward 0→1 更新時のみ許可されること
- Forward 更新なしの再オープンが抑止されること
- 回帰テストが `tests/test_subgroup_tracker.rs` / `tests/test_session/data_stream.rs` に追加されていること
