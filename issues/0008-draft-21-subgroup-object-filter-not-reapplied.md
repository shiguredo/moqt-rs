# 受信 Subgroup Object に Object 単位フィルタを再適用する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-subgroup-object-filter-not-reapplied

## 目的

draft-ietf-moq-transport-21 §3.1 (Subscriptions) の「subscriber は受信 Object がどの subscription のものか判別するためフィルタを再適用する」規則と §3.3.3 (Combining Filters) の Pass 評価を、受信 subgroup 経路でも満たす。フィルタを通過しない Object が Application へ渡るのを防ぐ。

## 現状

`src/session/data.rs` の `recv_subgroup_object` は、Object 単位のフィルタを評価していない。受信 subgroup 経路ではヘッダ受信時の `header_passes_filters` (`src/session/data.rs` → `src/session/subscription/validation.rs`) だけで候補を絞る。この関数は SUBGROUP_FILTER / PRIORITY_FILTER と Location Filter の Group
部分しか評価せず、OBJECTID_FILTER / OBJECT_PROPERTY_FILTER / Location Filter の Object 部分は評価対象外である。

対して datagram 経路の `recv_object_datagram` は `object_passes_filters` を呼び、`TrackDataAcceptance` を返して破棄を表現している。送信側の `send_subgroup_object` も `object_passes_filters` を呼ぶ。受信 subgroup 経路だけが抜けている。

根拠 (draft-ietf-moq-transport-21 §3.1):

> "Because subscriptions can share a Track Alias, the subscriber re-applies each subscription's filter to determine which subscription a received Object belongs to."

実害は、同一 Track Alias に複数 subscription を張った場合や、非同期なフィルタ更新で購読範囲外の Object が届いた場合に、フィルタ不通過の Object を Application へ渡すこと。Subgroup 経路の Object 単位フィルタを検証するテストは存在しない。

## 設計方針

`recv_subgroup_object` で `object_passes_filters` を評価し、不通過なら subscription 状態 (Largest 記録、tracker、`record_object_status_end` 等) を更新せず破棄する。破棄を表現するため、戻り値を datagram と同様の `TrackDataAcceptance` にする。公開 API の変更になるため、影響範囲と後方互換の扱いを `CHANGES.md` に明記する。

## 完了条件

- 受信 subgroup 経路で OBJECTID_FILTER / OBJECT_PROPERTY_FILTER / Location Filter の Object 部分が評価されること
- フィルタ不通過 Object が subscription 状態を更新しないこと
- 不通過の破棄が呼び出し側へ伝わること
- Subgroup 経路の Object 単位フィルタのテストが `tests/` / `pbt/` に追加されていること
