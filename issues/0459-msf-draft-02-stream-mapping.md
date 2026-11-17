# MSF の LOC stream mapping (group / object) に対応する

- Created: 2026-11-16
- Completed: 2026-11-17
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §4.1.1 (LOC to MOQT Stream mapping) は、LOC packaging の Object から MOQT Stream への割り当てモードを新設した。

- publisher は `group` または `object` のいずれかを MUST で選択し、`packagingMetadata` の `streamMapping` キーで per-track に通知する (§5.2.5)。
- subscriber は両モードを消費できなければならない (MUST)。
- `packagingMetadata` がない、またはキーがない場合は `group` と仮定する (MUST)。
- v01 の「各 Object を新しい MOQT Stream に MUST マップする」は削除された。

現行実装は `group` モード固定で、`object` モードの送信も受信もできず、モードを通知する経路もない。

## 現状

- `crates/moq-pub` の `SubgroupWriter::build_header` は `SubgroupIdMode::Zero` を使い、1 Group 内の全 Object を 1 Subgroup に載せる `group` モード固定である。
- `crates/moq-pub/src/cli.rs` の `Config` と `crates/moq-pub/src/catalog.rs` の `VideoTrackParams` にモード指定がなく、`build_catalog` も `packagingMetadata` を出力しない。
- `crates/moq-sub` は 1 Group を 1 Subgroup stream として受信する前提で、stream ごとに復号器を作る (`deliver_video_object` / `wait_until_video_deliverable` の doc)。object モードでは 1 Group が複数の Subgroup stream に分かれるため、この前提では delta の参照フレームを共有できない。
- `VideoDecodeOrder` の module doc も「1 Group の Object を 1 本の Subgroup で送る publisher を前提とする」としている。
- `src/subgroup_tracker.rs` は同じ `(track_alias, group_id, subgroup_id)` の再オープンを `ProtocolViolation` にする。`SubgroupIdMode::Zero` のまま object モードで複数の Subgroup を開くと衝突する。
- カタログに `packagingMetadata` がなく、`streamMapping` を通知できない。フィールド自体は 0444 が追加する。

## 設計方針

- 0444 の後に実施する。0444 が追加する `packaging_metadata_value` を使い、`MsfTrack::stream_mapping() -> StreamMapping` の型付き accessor を追加する。`packagingMetadata` がない場合と `streamMapping` キーがない場合は `Group` を返す。`streamMapping` の値が `group` / `object` 以外の場合は `MessageError::InvalidCatalog` にする。
- publisher は `Config` または `VideoTrackParams` からモードを選べるようにし、`build_catalog` が `packagingMetadata` に `streamMapping=group` または `streamMapping=object` を出力する。
- publisher は `object` モードのとき、1 Object ごとに Subgroup を作り、Subgroup ID が衝突しない方式 (`SubgroupIdMode::FirstObjectId` または `Explicit(object_id)`) を使う。`SubgroupIdMode::Zero` のまま複数 Subgroup を開かない。
- subscriber は `stream_mapping()` を読み、`object` モードでは Group 内の複数 Subgroup を同じ Group として扱う。Group 単位で復号器を共有し、Group 内の Object を Object ID 順に並べ替えるバッファを追加して、同一復号器へ直列に流す。`VideoDecodeOrder` の gap 判定は、Group 内の到着順の入れ替わりを許容するように変更する。
- `group` モードの既存の送受信挙動は変えない。

## 完了条件

- `packagingMetadata` がない track、`streamMapping` キーがない track、`streamMapping=group` の track が `StreamMapping::Group` になること。
- `streamMapping` の不正値が `MessageError::InvalidCatalog` で拒否されること。
- `crates/moq-pub` が `Config` / `VideoTrackParams` の指定に従ってモードを選び、カタログに `packagingMetadata` を出力すること。
- `object` モードで、各 Object がそれぞれの Subgroup に載り、Subgroup ID が衝突しないこと。
- `crates/moq-sub` が両モードを受信でき、`object` モードでは Group 内の複数 Subgroup を Object ID 順に並べ替えて同一の復号器に流すこと。
- `object` モードで Subgroup の到着順が入れ替わっても、Object を取りこぼさずに復号できること。
- `group` モードの既存挙動が変わらないこと。
- 上記を `crates/moq-pub` と `crates/moq-sub` のテストで固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- `MsfTrack::stream_mapping()` を追加し、`packagingMetadata` の `streamMapping=group|object` を解釈するようにした。キーがない場合は `group`、`group` / `object` 以外は `InvalidCatalog` とする (§4.1.1 / §5.2.5)。
- moq-pub に `--stream-mapping group|object` (既定 group) を追加し、`VideoTrackParams` / `CatalogContext` / `build_catalog` へ伝播させた。カタログの `packagingMetadata` には選択したモードを `streamMapping=...` として明示する。
- object モードでは 1 Object ごとに `SubgroupWriter` を作り、`SubgroupIdMode::FirstObjectId` で Subgroup ID を Object ID から決める。Group 内の Object ID は 0 から 1 ずつ増やし、FIRST_OBJECT は Subgroup の最初の Object で true になる。
- カメラ / MP4 パススルー / reencode / peer 購読の共通送信経路を object モードに対応させた。group モードは従来どおり `SubgroupIdMode::Zero` で 1 Group 1 Subgroup のままである。
- moq-sub はカタログの `stream_mapping()` を読み、object モードでは Group ごとに `VideoObjectReorderBuffer` で Object ID 順に並べ替え、購読で 1 つの復号器へ直列に流す。Group 内の複数 Subgroup の到着順が入れ替わっても、キーフレームより先に届いた delta を保留できる。
- `VideoDecodeOrder` は同じ Group の Object ID の欠けで状態を捨てず、欠けた Object が後から届いた場合に復号を続けられるようにした。実際に欠けが解消しなければ後続も捨て、次のキーフレームで再開する。
- `src/video_object_reorder.rs` の単体テスト、PBT、moq-pub / moq-sub の統合テストを追加した。実 peer harness で object モードのカタログを渡し、delta をキーフレームより先に送って 2 フレームが順に復号されることを確認している。`docs/msf.md` も更新した。

確認コマンド: `cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` が通ることを確認した。
