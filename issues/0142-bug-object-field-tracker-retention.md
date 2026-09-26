# ObjectFieldTracker の保持量に上限を設ける

- Created: 2026-09-22
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-object-field-tracker-retention
- Polished: 2026-09-25

## 目的

`Session` が subscription ごとに保持する `ObjectFieldTracker` の記録が、subscription の生存中ずっと増え続ける。受信 Object 1 件あたり最大 65535 バイトの IMMUTABLE_PROPERTIES (0x0B) を保持するため、peer が送るバイト量に比例してメモリが増える。上限を設けて、長時間の subscription でも保持量が有界になるようにする。

## 現状

- `Session` は `peer_object_fields: HashMap<u64, ObjectFieldTracker>` を request_id ごとに保持する (`src/session/core.rs`)
- `ObjectFieldTracker` は `(group_id, object_id)` ごとに `ObjectFieldRecord` を持つ。record は immutables の生バイト列 (`Option<Vec<u8>>`、上限 65535 バイト) と payload_key を保持する (`src/object_properties.rs`)
- `ObjectFieldTracker::prune_past_groups` は存在するが、crate 内で呼んでいるのは `fuzz/fuzz_targets/fuzz_object_trackers.rs` だけである。`Session` は subscription を forget するまで 1 件も破棄しない
- 同じ性質の記録は `ObjectPropertyTracker` (`peer_object_properties`) にもあり、group 単位の gap 記録が forget まで残る
- 前例として `Session` は datagram の object header 提供時刻に `pub const MAX_DATAGRAM_TRACKING_ENTRIES_PER_SUBSCRIPTION: usize = 100_000` の上限を設け、超過時は最も古い Group から破棄している (`src/session/core.rs`)

## 設計方針

`ObjectFieldTracker` の保持量を 2 段階で有界にする。

- group の変化を検出した時点で `ObjectFieldTracker::prune_past_groups` を呼ぶ。判定と発火は tracker 自身が行い、
  `ObjectFieldTracker` に「実効 group 順序 (`ascending`)」と「直前の観測 group」を保持させる。
  `Session` は tracker の生成時に `Subscription::effective_publisher_group_order`
  (draft-ietf-moq-transport-21 §10.5 (DEFAULT PUBLISHER GROUP ORDER)) から `ascending` を設定する
  - tracker を生成するのは `accept_subgroup_object` / `recv_object_datagram` の 2 箇所である (`entry(...).or_default()`
    を `entry(...).or_insert_with(...)` に置き換える)。`ObjectFieldTracker::new(ascending: bool)` で生成し、
    `ascending` は `effective_publisher_group_order()` が `DEFAULT_PUBLISHER_GROUP_ORDER_ASCENDING` (0x1) のとき true、
    0x2 (descending) のとき false とする。未宣言は §10.5 の既定により 0x1 に解決されるため true 側になる
    (`effective_publisher_group_order()` の `unwrap_or` と同じ扱い。descending を表す公開定数は現状無いため
    数値で比較する)
  - `ObjectFieldTracker::new()` は `tests/test_object_properties.rs` / `pbt/tests/prop_object_tracker.rs` /
    `fuzz/fuzz_targets/fuzz_object_trackers.rs` にも呼び出しがあるため、引数追加に追従する (`Default` は
    Ascending のままにする)
  - §10.5 の group order は publisher の選好であり、順序の保証ではない。prune は選好に合わせた best-effort の
    保持量削減であり、Object の到着順が選好と食い違っても機能を壊さない
  - `ascending` は tracker が保持して `prune_past_groups` へ渡す (Session 側で二重管理しない)
- 件数上限 `ObjectFieldTracker::MAX_RECORDS = 1_000` を設け、prune の後も上限を超える場合は記録を破棄する
  - 値の根拠: 1 レコードは IMMUTABLE_PROPERTIES の生バイト列 (最大 65535 バイト。draft-ietf-moq-transport-21 §8.3
    (Key-Value-Pair Structure) の "The maximum length of a value is 2^16-1 bytes") と payload_key を保持するため、
    datagram の `MAX_DATAGRAM_TRACKING_ENTRIES_PER_SUBSCRIPTION` (100_000) と同じ値は使えない (最悪 6.5 GB)。
    1_000 件なら最悪 65.5 MB に収まり、30 fps の映像では約 33 秒分の検出窓になる
  - 破棄は次の 2 段階を、件数が上限以下になるまで繰り返す。group が複数あるときは最も古い group
    (`ascending` なら最小の group_id、そうでなければ最大の group_id) の記録を group 単位でまとめて破棄する。
    1 group しか無い場合は、その group の最も古い object_id から 1 件ずつ破棄して上限に収める
    (最新 group だけで上限を超える場合は 1 段階目で他 group を消した後に 2 段階目が働く)
  - 破棄は観測 (比較) を確定させた後に行う。破棄された Object を再観測しても記録が無いため比較されず、
    `ObjectFieldMismatch` を返さない (見逃し側に倒す)
  - 破棄により §12.1 (Malformed Tracks) 条件 6 と §7.1 (Caching Relays) の重複検出を失うことは known limitation
    として doc と `CHANGES.md` に明記する (§7.1 は Forwarding Preference / Subgroup ID / Priority / Payload が
    異なる重複 Object を MUST で Malformed とするが、本実装は relay 非対応で endpoint として §12.1 条件 6 を扱う)
- 記録数の検査は `records` が private のため、診断用の `pub(crate) fn len(&self) -> usize` を追加し、
  `src/object_properties.rs` の `#[cfg(test)] mod tests` と `src/session/tests.rs` から行う
  (`tests/` は公開 API にだけ書く規約のため、件数は crate 内の単体テストで、公開挙動は `tests/` で検証する)
- immutables の保持量そのものを減らす案 (ダイジェスト等の固定長に丸める) は、比較キーの算出を呼び出し側の責務とする現行設計を変えるため、本 issue では扱わない
- `prune_past_groups` の ascending / descending の意味論をテストで固定する (`tests/` または `pbt/` に現在 1 件も無い)
- `Session` の `peer_object_properties` の gap 記録は **本 issue の対象外** とする。保持するのは group / object の
  id のみで 1 件あたりのサイズが小さく (ObjectFieldRecord の最大 65535 バイトとは桁が違う)、上限値の設計は
  別の判断になる。必要になった時点で別 issue とする
- [0143](0143-bug-object-after-track-end-boundary.md) は「終端宣言と同じ位置の重複を §12.1 条件 6 の比較に委ねる」
  設計である。本 issue の prune / 破棄の後はその比較が成立しない (見逃し側) ため、0143 の実装時にこの前提を
  前提条件として明記すること

## 完了条件

- `ObjectFieldTracker` の記録数が `MAX_RECORDS` を超えないことを固定するテストが
  `src/object_properties.rs` の `#[cfg(test)] mod tests` に追加されていること (診断用の `pub(crate) fn len`)
- group が複数あるときは最も古い group が丸ごと破棄され、新しい group の記録が残ることを固定するテストが
  追加されていること (ascending と descending の両方)
- 1 group しか無い状態で上限を超えた場合は、その group の最も古い object_id から破棄され、件数が上限に収まる
  ことを固定するテストが追加されていること
- 最新 group だけで上限を超え、かつ他 group も存在する場合に、他 group の破棄後も最新 group の古い記録が
  上限まで破棄されることを固定するテストが追加されていること
- group の変化を検出した時点で `prune_past_groups` が呼ばれること (ascending と descending の両方) と、
  その境界が単体テストで固定されていること
- 破棄後に同じ (group, object) を再観測しても `ObjectFieldMismatch` を返さない (見逃し側に倒れる) ことを
  固定するテストが追加されていること
- `Session` の 2 箇所で `effective_publisher_group_order` から `ascending` が設定されることを固定するテストが
  `src/session/tests.rs` に追加されていること (未宣言が Ascending として扱われる場合を含む)
- `ObjectFieldTracker` と `Session` の doc から「prune を呼ぶか tracker 自体を破棄するまで減らない」という
  記述が消え、上限と破棄の規則 (group 単位 → 同一 group 内は object_id 順) および known limitation
  (§12.1 条件 6 / §7.1 の重複検出を失う) が書かれていること
- `CHANGES.md` の `## develop` の既存エントリ (`Session` は現状 `prune_past_groups` を呼ばない) を実装後の
  内容に更新し、上限による破棄を載せること
- `fuzz/fuzz_targets/fuzz_object_trackers.rs` の既存呼び出しが新しい生成 API に追従していること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること

## 解決方法

未着手。
