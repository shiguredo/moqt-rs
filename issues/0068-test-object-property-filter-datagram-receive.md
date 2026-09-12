# OBJECT_PROPERTY_FILTER の datagram 受信経路テストを追加する

- Created: 2026-09-13
- Completed: {YYYY-MM-DD}
- Branch: feature/test-object-property-filter-datagram-receive
- Polished: {YYYY-MM-DD}

## 目的

OBJECT_PROPERTY_FILTER (PARAM_OBJECT_PROPERTY_FILTER) を受信 Object に適用する挙動を datagram 経路でもテストに固定する。subgroup 経路には `tests/test_session/subgroup_object_filter.rs` の `object_property_filter_routes_each_received_object` があるが、`Session::recv_object_datagram` 経路には OBJECT_PROPERTY_FILTER を組み合わせたテストがなく、
回帰を検出できない。

## 現状

- `Session::recv_object_datagram` は `resolve_peer_track_alias` で候補 subscription を解決し、候補ごとに `object_passes_filters` を評価して最初に通過した subscription に紐づける。Object Properties は `ObjectFilterInput` の `properties_bytes` として渡される。
- subgroup 経路の受信側テストは `object_property_filter_routes_each_received_object` があるが、datagram 経路のフィルタテストは Object ID / Location / Priority 系のみで、OBJECT_PROPERTY_FILTER を検証するテストがない。
- 複数 subscription が alias を共有する場合の候補ごとの再適用、フィルタ不合格時の `FilteredOut`、キャンセル由来候補のみ合格時の `Discarded` は datagram 経路にも同じ規則が適用されるが、OBJECT_PROPERTY_FILTER では固定されていない。

## 設計方針

- `tests/test_session/subgroup_object_filter.rs` の `object_property_filter_routes_each_received_object` と同じ手順で、OBJECT_PROPERTY_FILTER を持つ subscription を確立し、datagram を注入して次を検証する。
  - プロパティがフィルタに一致する datagram が当該 subscription に紐づくこと
  - 一致しない datagram が `FilteredOut` になり、subscription スコープの状態 (`largest_received_location` など) を更新しないこと
  - 共有 alias で候補が複数ある場合に、最初に通過した subscription に紐づくこと
- 既存の datagram 受信テストの流儀に合わせ、モックやスタブは使わない。

## 完了条件

- OBJECT_PROPERTY_FILTER と datagram 受信を組み合わせたテストが追加され、`cargo test --workspace` が通ること
- フィルタ一致 / 不一致 / 候補複数の各ケースで帰属と状態更新が固定されていること
