# OBJECT_PROPERTY_FILTER の datagram 受信経路テストを追加する

- Created: 2026-09-13
- Completed: 2026-09-17
- Branch: feature/test-object-property-filter-datagram-receive
- Polished: 2026-09-17

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

## 解決方法

`Session::recv_object_datagram` 経路の OBJECT_PROPERTY_FILTER 再適用を
`tests/test_session/datagram_object_filter.rs` に 4 テストで固定した。
`tests/test_session.rs` に `datagram_object_filter` モジュールを追加した。

テストは `tests/test_session/subgroup_object_filter.rs` の
`object_property_filter_routes_each_received_object` と同じ手順で、client を subscriber として
OBJECT_PROPERTY_FILTER 付き SUBSCRIBE を確立し、`ObjectDatagram` を注入して
`TrackDataAcceptance` と subscription の状態を検証する。Range Filter を送るために server 側で
MAX_FILTER_RANGES を宣言し、フィルタ対象の Property には `ObjectPropertyTracker` の
gap 検証対象外の OBJECT_DELIVERY_TIMEOUT (0x02) を使う。

- `object_property_filter_accepts_matching_datagram`: Property がフィルタに一致する
  datagram が `Accepted` になり、当該 subscription の `largest_received_location` が
  更新されること (2 通目で位置が前進すること)
- `object_property_filter_filters_out_unmatched_datagram_without_state_update`:
  値が範囲外の datagram と Property を持たない datagram が `FilteredOut` になり、
  `largest_received_location` を更新しないこと。加えて PRIOR_OBJECT_ID_GAP (0x3E) 付きの
  datagram がフィルタ不通過なら gap 追跡に記録されず、その gap に入る位置の datagram が
  Malformed Track にならず受理されること (フィルタ不通過時に状態を更新しないことの裏付け)
- `object_property_filter_routes_datagram_to_first_passing_candidate`: 同一 alias を共有する
  2 subscription で、2 番目の候補だけが合格する datagram がその subscription に紐づき、
  どの候補も通過しない datagram が `FilteredOut` になり、両方が合格する datagram が
  登録順で先の subscription に紐づくこと
- `object_property_filter_evaluates_target_property_in_datagram`: 複数の Property を持つ
  datagram でもフィルタ対象の Property の値で合否が決まること。受理された datagram の gap で
  後続 Object が Malformed Track になることを対照に、フィルタ評価が実際に効いていることを見る

フィルタ評価の無効化 (Property を渡さない)、フィルタ不通過時の状態更新 (gap 追跡)、
フィルタ不通過時の `largest_received_location` 更新の 3 つの変異を一時的にソースへ入れて、
いずれも本テストが失敗する (回帰を検出できる) ことを確認した。

検証:

- `cargo test --workspace` が通る (test_session は 666 件)
- `cargo clippy --workspace --all-targets -- -D warnings` が通る
- `cargo fmt --all -- --check` が通る
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` が通る
