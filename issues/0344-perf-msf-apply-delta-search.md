# MSF の delta 適用の探索と複製を減らす

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-msf-apply-delta-search
- Polished: 2026-10-21

## 目的

`apply_delta` は Add の重複判定に `Vec::contains` と `find_track_index` の線形探索を使い、Remove / Clone も毎回カタログ全体を線形探索する。さらに毎回カタログ全体を clone する COW になっている。巨大なカタログでは、delta 適用の CPU 時間がトラック数と delta 件数の積で増え、delta ごとのカタログ全体の clone がメモリ確保を伴う。

## 現状

- `src/msf.rs` の `apply_delta_in_place` (`apply_delta` の本体) は `batch_keys: Vec<(Option<String>, String)>` の `contains` と `find_track_index` で重複を調べる。
- Remove / Clone も `find_track_index` による線形探索である。
- `apply_delta` は呼び出しごとにカタログ全体を clone する (`let mut working = self.clone();`)。
- 影響: delta の件数 N と track 数 M で、Add は既存カタログの走査 O(M) とバッチ内重複判定 O(N) を各トラックに行うため O(N*(M+N))、Remove / Clone は O(N*M)。delta を連続適用するとカタログ全体の clone によるメモリ確保も増える。
- `apply_delta` の原子性 (Err 時にカタログを変更しない) は closed の [0050](closed/0050-update-apply-delta-atomic.md) で copy-on-write により保証されており、`tests/test_msf/delta_apply.rs` の `apply_delta_is_atomic_when_later_operation_fails` が固定している。

## 設計方針

- `apply_delta` の開始時に `hashbrown::HashMap<(Option<String>, String), usize>` (解決済みキー→`tracks` の index) を構築し、Add / Remove / Clone で更新する。Add の重複判定は既存カタログとバッチ内の両方を O(1) で行い、Remove / Clone の対象探索も O(1) にする。
  - バッチ内の重複は一時的な `hashbrown::HashSet<(Option<String>, String)>` で判定し、Add 操作内の全トラックを検証してから追加する現行の部分適用防止を維持する。
  - `Vec::remove` による要素移動は O(M) のまま残るが、これは探索ではない。削除後は削除位置より後ろの index を更新するか、HashMap を再構築して整合を保つ。`tracks` の順序は現行どおり維持する (`swap_remove` は使わない)。
  - `publish_tracks` との重複は現行どおり `validate_after_delta` で検出する。Add の重複判定を `publish_tracks` に広げない (エラー経路を変えない)。
- `apply_delta` の成功経路で `MsfCatalog` 全体の clone を行わない。原子性は、変更前の状態を undo ログとして記録し、Err 時に巻き戻すことで維持する。undo ログには次を含める。
  - Add で追加した track (追加した件数でもよい。追加は `tracks` の末尾に行うため、巻き戻し時に末尾から取り除ける)
  - Remove で削除した track と削除前の index
  - `removed_tracks` に新規挿入したキー (`or_insert` が既存キーを変更しないため、新規挿入だけを記録する)
  - `generated_at` の旧値
- `apply_delta` の公開 doc から copy-on-write の記述を外し、失敗時にカタログを変更しない原子性の記述に更新する (`apply_delta_in_place` を残す場合は undo ログによる巻き戻しを記述する)。
- `validate_after_delta` の O(M) 走査は、delta ごとに 1 回の一括検証であり本 issue の削減対象外とする。
- `MsfCatalogDocument` の所有権設計や example の `CatalogState` の clone は本 issue の対象外とする。
- 巨大カタログ (例: 10,000 tracks) に対する add / remove / clone の回帰テストを `tests/test_msf/delta_apply.rs` に追加する。ベンチマークは必須としない (追加する場合は criterion を使う)。

## 完了条件

- `apply_delta` の Add / Remove / Clone の対象探索が `find_track_index` / `Vec::contains` の線形探索でなくなっていること (`validate_after_delta` の一括検証は対象外)。
- `tracks` の順序が現行どおり維持されること。
- `apply_delta` の成功経路で `MsfCatalog` 全体の clone を行わないこと。
- Err 時に `tracks` / `removed_tracks` / `generated_at` を含めてカタログが呼び出し前と一致すること。Add の後に失敗するケースに加え、Remove の後に `validate_after_delta` で失敗するケースの原子性テストが `tests/test_msf/delta_apply.rs` に追加されていること。
- `apply_delta` の公開 doc が失敗時にカタログを変更しない原子性の記述に更新されていること (`apply_delta_in_place` を残す場合は undo ログによる巻き戻しが記述されていること)。
- 巨大カタログ (例: 10,000 tracks) の add / remove / clone を適用する回帰テストが `tests/test_msf/delta_apply.rs` に追加され、通ること。
- 既存の delta 適用テストが変わらず通ること。
- `cargo test --workspace` / `cargo test -p pbt` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` が通ること。
