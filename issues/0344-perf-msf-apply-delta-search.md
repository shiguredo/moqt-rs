# MSF の delta 適用の探索と複製を減らす

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-msf-apply-delta-search
- Polished: 2026-11-21

## 目的

`apply_delta` は Add の重複判定に `Vec::contains` と `find_track_index` の線形探索を使い、Remove / Clone / Update も毎回カタログ全体を線形探索する。さらに毎回カタログ全体を clone する COW になっている。巨大なカタログでは、delta 適用の CPU 時間がトラック数と delta 件数の積で増え、delta ごとのカタログ全体の clone がメモリ確保を伴う。

## 現状

- `src/msf.rs` の `apply_delta_in_place` (`apply_delta` の本体) は `batch_keys: Vec<(Option<TrackNamespace>, Vec<u8>)>` の `contains` と `find_track_index` で重複を調べる。キーは `MsfCatalog::track_key` が返す解決済みの `(Option<TrackNamespace>, Vec<u8>)` である。
- Remove / Clone / Update も `find_track_index` による線形探索である。Update は closed の [0451](closed/0451-msf-draft-02-delta-update-operation.md) で追加された操作で、`parentName` / `parentNamespace` から親トラックを線形探索する。
- `apply_delta` は呼び出しごとにカタログ全体を clone する (`let mut working = self.clone();`)。
- 影響: delta の件数 N と track 数 M で、Add は既存カタログの走査 O(M) とバッチ内重複判定 O(N) を各トラックに行うため O(N*(M+N))、Remove / Clone / Update は O(N*M)。delta を連続適用するとカタログ全体の clone によるメモリ確保も増える。
- `apply_delta` の原子性 (Err 時にカタログを変更しない) は closed の [0050](closed/0050-update-apply-delta-atomic.md) で copy-on-write により保証されており、`tests/test_msf/delta_apply.rs` の `apply_delta_is_atomic_when_later_operation_fails` が固定している。
- `removed_tracks` は Remove のたびに `insert` で上書きされる (closed の [0453](closed/0453-msf-draft-02-delta-remove-immutability.md) で `or_insert` から変更された)。そのため undo ログは新規挿入だけでなく、上書き前の値の復元も必要である。

## 設計方針

- `apply_delta_in_place` の開始時に `hashbrown::HashMap<(Option<TrackNamespace>, Vec<u8>), usize>` (解決済みキー→`tracks` の index) を構築し、Add / Remove / Clone で追加・削除・index 更新を行い、Update は既存のキーと index をそのまま使う。
  `TrackNamespace` と `Vec<u8>` は `Hash` / `Eq` を実装している。Add の重複判定は既存カタログとバッチ内の両方を O(1) で行い、Remove / Clone / Update の対象探索も O(1) にする。
  - バッチ内の重複は一時的な `hashbrown::HashSet<(Option<TrackNamespace>, Vec<u8>)>` で判定し、Add 操作内の全トラックを検証してから追加する現行の部分適用防止を維持する。
  - `Vec::remove` による要素移動は O(M) のまま残るが、これは探索ではない。削除後は削除位置より後ろの index を更新するか、HashMap を再構築して整合を保つ。`tracks` の順序は現行どおり維持する (`swap_remove` は使わない)。
  - Update は Full Track Name を変更しない (`name` は `parentName` と一致し、`namespace` も変更できない) ため、HashMap のキーと index はそのまま使える。
  - `publish_tracks` との重複は現行どおり `validate_after_delta` で検出する。Add の重複判定を `publish_tracks` に広げない (エラー経路を変えない)。
- `apply_delta` の成功経路で `MsfCatalog` 全体の clone を行わない。原子性は、変更前の状態を undo ログとして記録し、Err 時に記録の逆順で巻き戻すことで維持する。undo ログには次を含める。
  - Add / Clone の各操作で追加した track (追加した件数でもよい。追加は `tracks` の末尾に行うため、逆順の巻き戻しで末尾から取り除ける)
  - Remove で削除した track と削除前の index。`tracks.remove` の直後に記録し、その後の `track_key` が失敗しても巻き戻せるようにする
  - Remove で `removed_tracks` に上書きしたキーの旧値 (`insert` は既存キーの値を変更するため、新規挿入だけでなく上書き前の値も記録する)
  - Update で置き換えた track と置き換え前の track (index と旧 track)
  - `generated_at` の旧値
- `apply_delta_in_place` を残す場合は、同関数内で undo ログを管理し、Err 時に逆順で巻き戻す。呼び出し側の `apply_delta` は複製を作らない。`apply_delta_in_place` を削除する場合は `apply_delta` が同じ責務を持つ。
- `apply_delta` の公開 doc から copy-on-write の記述を外し、失敗時にカタログを変更しない原子性の記述に更新する。`apply_delta_in_place` を残す場合は undo ログによる巻き戻しを記述する。`crates/moq-sub/src/catalog.rs` の `CatalogState::apply` と `tests/test_msf/delta_apply.rs` の `apply_delta_applies_all_operations_on_success` にある copy-on-write を前提にした記述も更新する。
- `validate_after_delta` の一括検証 (少なくとも O(M)、depends / group の検証は O(M^2) になり得る) は、delta ごとに 1 回の検証であり本 issue の削減対象外とする。
- `MsfCatalogDocument` の所有権設計や example の `CatalogState` が保持する `MsfCatalog` の clone (Full カタログ置換時) は本 issue の対象外とする。
- 巨大カタログ (例: 10,000 tracks) に対する add / remove / clone / update の回帰テストを `tests/test_msf/delta_apply.rs` に追加する。ベンチマークは必須としない (追加する場合は criterion を使う)。

## 完了条件

- `apply_delta` の Add / Remove / Clone / Update の対象探索が `find_track_index` / `Vec::contains` の線形探索でなくなっていること (`validate_after_delta` の一括検証は対象外)。
- `tracks` の順序が現行どおり維持されること。
- `apply_delta` の成功経路で `MsfCatalog` 全体の clone を行わないこと。
- Err 時に `tracks` / `removed_tracks` / `generated_at` を含めてカタログが呼び出し前と一致すること。
  Add の後に失敗するケース (既存の `apply_delta_is_atomic_when_later_operation_fails`) に加え、
  Remove の後に `validate_after_delta` で失敗するケース (depends の参照先を削除するなど)、
  Update の後に失敗するケース、Clone の後に失敗するケース、
  既存の削除履歴がある状態で Remove により履歴を上書きした後に失敗するケースの原子性テストが
  `tests/test_msf/delta_apply.rs` に追加されていること。
- `apply_delta` の公開 doc が失敗時にカタログを変更しない原子性の記述に更新されていること (`apply_delta_in_place` を残す場合は undo ログによる巻き戻しが記述されていること)。`crates/moq-sub/src/catalog.rs` と `tests/test_msf/delta_apply.rs` の copy-on-write を前提にした記述も更新されていること。
- 巨大カタログ (例: 10,000 tracks) の add / remove / clone / update を適用する回帰テストが `tests/test_msf/delta_apply.rs` に追加され、通ること。
- 既存の delta 適用テストが変わらず通ること。
- `cargo test --workspace` / `cargo test -p pbt` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` が通ること。
