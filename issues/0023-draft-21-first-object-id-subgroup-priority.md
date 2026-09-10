# FirstObjectId Subgroup の Publisher Priority を Subgroup 単位で記録する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-first-object-id-subgroup-priority

## 目的

Subgroup ID モードが FirstObjectId のとき、優先度の異なる並行 Subgroup を受信しても Malformed Track を誤検出しないようにする。Publisher Priority は Subgroup 単位の値である。

## 現状

`src/session/data.rs` の `recv_subgroup_header` は、購読単位の `subscription.publisher_priority` をヘッダ受信ごとに上書きする。`IncomingDataStream::Subgroup` (`src/session/types.rs`) は priority を保持しない。

FirstObjectId モードだけは priority の記録を先頭 Object 受信時まで遅延し、`subscription.effective_publisher_priority()` を参照する。同一購読で優先度の異なる FirstObjectId Subgroup が並行すると、後から来たヘッダの優先度が先の Subgroup の先頭 Object に記録される。

影響: draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 1「同一 Subgroup ID の前 Object と Publisher Priority が異なる」の検出が誤り、正当な購読を Malformed として終端しうる。逆に検出漏れにもなる。

FirstObjectId の並行 priority 差を扱うテストは存在しない。

## 設計方針

`IncomingDataStream::Subgroup` に解決済みの publisher priority を保持し、FirstObjectId でもヘッダ時点の値を先頭 Object 解決時に使う。Subgroup 単位の priority を正とする。

## 完了条件

- FirstObjectId Subgroup でも Subgroup 単位の priority が記録されること
- 優先度の異なる並行 FirstObjectId Subgroup が Malformed と誤判定されないこと
- 回帰テストが `tests/test_session/data_stream.rs` 等に追加されていること
