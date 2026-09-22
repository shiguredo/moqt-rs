# ObjectFieldTracker の保持量に上限を設ける

- Created: 2026-09-22
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-object-field-tracker-retention
- Polished: {YYYY-MM-DD}

## 目的

`Session` が subscription ごとに保持する `ObjectFieldTracker` の記録が、subscription の生存中ずっと増え続ける。受信 Object 1 件あたり最大 65535 バイトの IMMUTABLE_PROPERTIES (0x0B) を保持するため、peer が送るバイト量に比例してメモリが増える。上限を設けて、長時間の subscription でも保持量が有界になるようにする。

## 現状

- `Session` は `peer_object_fields: HashMap<u64, ObjectFieldTracker>` を request_id ごとに保持する (`src/session/core.rs`)
- `ObjectFieldTracker` は `(group_id, object_id)` ごとに `ObjectFieldRecord` を持つ。record は immutables の生バイト列 (`Option<Vec<u8>>`、上限 65535 バイト) と payload_key を保持する (`src/object_properties.rs`)
- `ObjectFieldTracker::prune_past_groups` は存在するが、crate 内で呼んでいるのは `fuzz/fuzz_targets/fuzz_object_trackers.rs` だけである。`Session` は subscription を forget するまで 1 件も破棄しない
- 同じ構造は `ObjectPropertyTracker` (`peer_object_properties`) にもあり、group ごとの gap 記録が forget まで残る
- 前例として `Session` は datagram の object header 提供時刻に `pub const MAX_DATAGRAM_TRACKING_ENTRIES_PER_SUBSCRIPTION: usize = 100_000` の上限を設け、超過時は最も古い Group から破棄している (`src/session/core.rs`)

## 設計方針

- 上限の方式を決める。候補は次の 2 つ
  - group 前進時に `ObjectFieldTracker::prune_past_groups(ascending, current_group)` を呼ぶ。`ascending` は `Subscription::effective_publisher_group_order` (Track Property の DEFAULT_PUBLISHER_GROUP_ORDER、既定 Ascending) から解決する。prune の呼び出しは group が変わったときだけ行い、Object ごとに `retain` を回さない (Object 数 × 記録数 の走査を避ける)
  - `MAX_DATAGRAM_TRACKING_ENTRIES_PER_SUBSCRIPTION` と同型の件数上限を tracker に持たせ、超過時に古い group から破棄する
- `prune_past_groups` は同じ group 内の記録を残すため、1 group に属する Object 数が多い Track では group 前進方式だけでは上限にならない。件数上限を併用するか、group 前進方式で十分と判断するかを決める
- immutables の保持量そのものを減らす案 (ダイジェスト等の固定長に丸める) は、比較キーの算出を呼び出し側の責務とする現行設計を変えるため、本 issue では扱わない
- `prune_past_groups` の ascending / descending の意味論をテストで固定する (`tests/` または `pbt/` に現在 1 件も無い)
- 0111 で導入された `peer_object_properties` の gap 記録も同じ性質を持つため、対象に含めるかを決める

## 完了条件

- 同一 subscription で group をまたいで Object を受信し続けても、`peer_object_fields` の記録数が上限を超えないことを固定するテストが追加されていること
- prune の ascending / descending の境界がテストで固定されていること
- `ObjectFieldTracker` と `Session` の doc から「保持量が forget まで単調増加する」という記述を削除できること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること

## 解決方法

未着手。
