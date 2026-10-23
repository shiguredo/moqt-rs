# 長寿命購読で増え続ける SubgroupTracker の追跡状態を有界にする

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-unbounded-state-growth
- Polished: 2026-10-21

## 目的

`SubgroupTracker` は `(track_alias, group_id, subgroup_id)` ごとに終端状態・Publisher Priority・FIN 時の最終 Object ID を保持する。subscription の終了時には `remove_track_alias` で解放されるが、長寿命の subscription では Group が進むたびにキーが増え、subscription の生存中は解放されない。追跡キー数に上限を設け、長時間の配信でもメモリ使用量を有界にする。

## 現状

- `src/subgroup_tracker.rs` の `SubgroupTracker` は次の 3 つの `HashMap` を保持する。
  - `entries`: `(track_alias, group_id, subgroup_id)` → `SubgroupStreamState` (再オープン禁止の判定用)
  - `priorities`: 同じキー → Publisher Priority (draft-ietf-moq-transport-22 §12.1 (Malformed Tracks) 条件 1 の検出用)
  - `fin_last_object_ids`: 同じキー → FIN 時の最終 Object ID (同条件 2/3 の検出用)
- `open` は `entries` にキーを追加し、`mark_fin` / `mark_reset` / `mark_stop_sending` は終端状態を記録するだけでキーを削除しない。`mark_fin` は未知キーでも `fin_last_object_ids` に記録する。
- `Session` は `peer_subgroups` と `my_subgroups` の 2 つの `SubgroupTracker` を保持する (`src/session/core.rs`)。
- subscription の alias 共有が解消されたときに `remove_track_alias` を呼び、alias 単位で 3 つの `HashMap` からキーを削除する (`src/session/subscription.rs` の `remove_alias_holder` / `release_peer_alias` 分岐)。subscription の生存中は Group が進んでもキーは削除されない。
- 影響: 長時間の配信で Group 数に比例して状態が増え続ける。1 Group あたりの Subgroup 数を S、Group 数を G とすると、`entries` / `priorities` / `fin_last_object_ids` のキー数は O(G * S) で増える。
- なお `RequestIdTracker` と `FetchStreamDecoder` は本 issue の対象外とする。
  - `src/session/request_id.rs` の `RequestIdTracker` は、準拠 peer では前縁が埋まるたびに `above` から吸収され、
    保持数は同時に未到達な request 数に比例する。非準拠 peer の増加は draft-ietf-moq-transport-22 §6.4.2.1 (Request ID) が
    `INVALID_REQUEST_ID` でのセッション終了を MUST とする 2 条件 (parity 違反と重複) 以外での終了を許さないため許容する設計であり、
    上限を設けると正当な peer を落とす (`RequestIdTracker` の doc と `src/session/tests.rs` の `peer_request_tracker_accepts_unbounded_out_of_order_ids`)。
    この設計は `issues/closed/0113-bug-remove-out-of-order-request-id-limit.md` で確定している。
  - `src/stream/decoder.rs` の `FetchStreamDecoder` は、`validate_fetch_object` が group の前進時に `prune_past_group_state` を呼び、per-group の `subgroup_priorities` / `subgroup_max_objects` / `subgroup_final_objects` を破棄する。`last_object_ids` という per-group フィールドは存在しない。

## 設計方針

- `SubgroupTracker` に `track_alias` ごとの追跡キー数の上限 `MAX_TRACKED_SUBGROUP_KEYS_PER_TRACK_ALIAS` を設ける。値は `MAX_DATAGRAM_TRACKING_ENTRIES_PER_SUBSCRIPTION` (100,000) と同じ 100,000 とする。1 キーあたりの保持量は小さく (キー 3 つの `u64` と enum / `Option<u64>`)、100,000 件でも数十 MB に収まる。30 fps で 1 Group あたり 1 Subgroup なら約 1 時間分の検出窓になる。
- 追跡キー数は `entries` / `priorities` / `fin_last_object_ids` のキーの和集合の件数とする。
- `open` / `mark_fin` / `mark_reset` / `mark_stop_sending` / `record_priority`、および 0352 で追加される recovery メソッドなど、
  キーを追加するか終端状態にするすべての操作の後、当該 `track_alias` の追跡キー数が上限を超えたら、
  当該 alias の終端済みキーを 3 つの `HashMap` からまとめてすべて破棄する。
- 終端済みキーとは、`entries` の値が `Open` 以外のキーと、`entries` に存在しない `priorities` / `fin_last_object_ids` のキーである。
  `Open` のキーは破棄しない (進行中の stream の追跡を壊さないため)。
- 一度にすべての終端済みキーを破棄するのは、`SubgroupTracker` が Group の完了順序を受け取らず、`track_alias` が複数 subscription で共有されうるためである。Group 順序に依存する prune は別 subscription の追跡状態を壊しうる。全破棄は 1 回の走査で済み、検出窓が上限に達した時点でリセットされる。
- 追跡キー数は `track_alias` ごとのカウンタで保持し、`open` / `mark_fin` / `record_priority` でキーが和集合に新規に加わったときだけ加算し、破棄で減算する。これにより上限判定を全走査なしで行う。
- `Open` のキーだけで上限を超える場合は破棄しない。`Session` 経由の利用では `Open` のキー数は同時に開いている Subgroup stream 数に比例し、transport の同時 stream 上限で抑えられる。
- 破棄は検証を確定させた後に行う。破棄されたキーは未知キーとして扱う。`open` は再オープンを許可し、`check_object_after_fin` は条件 2 を、`record_priority` は条件 1 を検出しない (見逃し側に倒す)。この known limitation を `SubgroupTracker` / `fin_last_object_ids` / 各メソッドの doc に書く。
- `SubgroupTracker` は公開 API だが、関数シグネチャは変えない。上限超過時の終端済みキーの破棄は挙動変更であり、doc に明記する。
- `remove_track_alias` は従来どおり subscription 終了時に 3 つの `HashMap` から alias のキーをすべて削除する。alias ごとの件数カウンタも削除する。
- 上限と破棄の検証は `src/subgroup_tracker.rs` の `#[cfg(test)] mod tests` で `entries` / `priorities` / `fin_last_object_ids` の件数を直接確認する。公開挙動 (破棄後の再オープンと検出漏れ) は `tests/test_subgroup_tracker.rs` で確認する。
- `issues/0352-bug-subgroup-tracker-state.md` (`SubgroupTracker` の状態不変条件) は同じファイルの `ClosedFin` / `mark_fin` / `mark_reset` / `mark_stop_sending` を変更するため、本 issue は 0352 の実装後に着手する。0352 後の終端状態と `mark_fin` の形を前提にする。

## 完了条件

- `SubgroupTracker` に `MAX_TRACKED_SUBGROUP_KEYS_PER_TRACK_ALIAS` が追加され、`track_alias` ごとの追跡キー数が上限を超えないこと (`Open` のキーだけで超える場合を除く)。
- 上限超過時に当該 alias の終端済みキーが `entries` / `priorities` / `fin_last_object_ids` からまとめて削除され、`entries` に `Open` 以外のキーが残らず、`entries` に存在しない `priorities` / `fin_last_object_ids` のキーが残らないこと。
- `Open` のキーは上限超過時も破棄されないこと。
- 破棄されたキーは未知キーとして扱われ、`open` が成功し、`check_object_after_fin` が `None` を返し、`record_priority` が初回記録として成功すること。
- `SubgroupTracker` / `fin_last_object_ids` / `open` / `mark_fin` / `record_priority` / `check_object_after_fin` の doc に、上限・破棄・known limitation が書かれていること。
- `remove_track_alias` が 3 つの `HashMap` から alias のキーをすべて削除し、alias ごとの件数カウンタも削除すること (同じ alias を再利用しても以前の件数が残らないこと)。
- 0352 の実装後の状態で `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
