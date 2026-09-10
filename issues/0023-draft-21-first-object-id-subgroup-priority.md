# FirstObjectId Subgroup の Publisher Priority を Subgroup 単位で記録する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-first-object-id-subgroup-priority
- Polished: 2026-09-10

## 目的

Subgroup ID モードが FirstObjectId のとき、優先度の異なる並行 Subgroup を受信しても Malformed Track を誤検出しないようにする。Publisher Priority は Subgroup 単位の値であり、購読単位の直近値で判定してはならない。

## 現状

- `src/session/data.rs` の `recv_subgroup_header` は、購読単位の `subscription.publisher_priority` をヘッダ受信ごとに上書きする。`IncomingDataStream::Subgroup` (`src/session/data.rs`) は priority を保持しない。
- FirstObjectId モードだけは priority の記録を先頭 Object 受信時まで遅延し、`subscription.effective_publisher_priority()` を参照する。同一購読で優先度の異なる FirstObjectId Subgroup が並行すると、後から来たヘッダの優先度が先の Subgroup の先頭 Object の記録に使われる。
- 誤検出は並行 2 本を受信しただけでは発火しない。`SubgroupTracker::record_priority` は同一 `(track_alias, group_id, subgroup_id)` の 2 回目以降の値不一致だけを検出するため、同じ Subgroup キーが再評価されて初めて発火する。再現手順:
  1. stream A: FirstObjectId / priority 10 のヘッダを受理し、先頭 Object (ID 5) はまだ処理しない
  2. stream B: FirstObjectId / priority 99 のヘッダを受理する (`subscription.publisher_priority = 99` に上書き)
  3. A の先頭 Object ID 5 を処理すると、subgroup 5 の priority が 99 として記録される (正しくは 10)
  4. STOP_SENDING / RESET 後に subgroup 5 を priority 10 で再オープンすると、記録済み 99 と比較して §12.1 (Malformed Tracks) 条件 1 を誤検出する。逆に 99 で再オープンされた場合は一致扱いになり検出漏れになる
- 同根の問題が重複 Object 検証にもある。`recv_subgroup_object` の `observe_object_fields` に渡す priority も `subscription.effective_publisher_priority()` を参照しており、Subgroup 単位ではなく直近ヘッダの値になる。

根拠 (draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 1):

> "An Object with a particular Subgroup ID is received, but its Publisher Priority is different from that of the previous Object with the same Subgroup ID."

§7.1 (Caching Relays) も重複 Object の Priority 不一致を Malformed と規定する。

FirstObjectId の並行 priority 差を扱うテストは存在しない (`tests/test_session/data_stream.rs` の `condition1_priority_mismatch_terminates_subscription` は Explicit モード)。

## 設計方針

- `IncomingDataStream::Subgroup` にヘッダ時点で解決済みの publisher priority (`u8`) を保持する。解決規則は現行の `resolve_header_publisher_priority` と同じ (ヘッダの値、無ければ Track Property の DEFAULT_PUBLISHER_PRIORITY、無ければ 128)。
- `recv_subgroup_object` は、`record_priority` (§12.1 条件 1) と `observe_object_fields` (§7.1 / §12.1 条件 6/7 の重複 Object 検証) の両方で、`subscription.effective_publisher_priority()` ではなく stream に保持した値を使う。Subgroup 単位の priority を正とする。
- `Subscription.publisher_priority` は公開フィールドで既存テストの対象であるため、ヘッダ受信ごとの更新は維持する。doc を「直近に受信した subgroup header の解決済み priority」に明確化し、Subgroup 単位の検証では stream 保持値を使うと書き分ける。
- datagram 経路の priority 解決は変更しない (本 issue の対象外)。
- 変更は内部型 (`pub(super)`) と内部ロジックに閉じるため公開 API の変更はない。

## 完了条件

- FirstObjectId Subgroup の先頭 Object 解決時に、購読単位の直近値ではなく、その stream のヘッダ時点の priority が `record_priority` と `observe_object_fields` の両方で使われること
- 上記再現手順 1-4 で subgroup 5 を priority 10 で再オープンしても Malformed にならないこと
- 同じ手順で subgroup 5 を本来と異なる priority 99 で再オープンした場合は §12.1 条件 1 の Malformed として該当 subscription が Terminated になること
- 回帰テストが `tests/test_session/data_stream.rs` に追加され、`cargo test --workspace` が通ること
