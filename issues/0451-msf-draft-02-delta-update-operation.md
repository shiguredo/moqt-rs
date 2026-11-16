# MSF delta update に update 操作を追加する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §5.1.6 (Delta update) は、既存 track のフィールドを変更する `"update"` 操作を追加した。update の track object は `parentName` を MUST、`parentNamespace` を MAY で持ち、宣言した属性が既存値を上書きする。

現行実装は `add` / `remove` / `clone` だけを扱い、`"update"` を未知の操作として拒否する。v02 のカタログ更新を適用できない。

## 現状

- `src/msf.rs` の `MsfDeltaOperation` は `Add` / `Remove` / `Clone` のみで、`decode_delta_operation` は `"update"` を `MessageError::InvalidCatalog` にする。
- `MsfCatalog::apply_delta_in_place` は add の既存名、remove の未知 track、clone の未知の親・既存名を拒否する。update の適用経路はない。
- `validate_after_delta` は一意性・initRef・グループの検証を行うが、マージ後の track を `validate_full_track` に通さない。add と clone は追加時点で `validate_full_track` / `validate_clone_track_fragment` を通る。
- §5.2.3 (Track name) は track object の `name` を Required: Yes とする。§5.1.6 は update に `parentName` を MUST とする。§5.6.4 の例は update で `name` を省略しているが、§5.6 の冒頭は例を非規範と明記している。
- update で `name` や `namespace` を変更できるか、`parentNamespace` を省略したときの既定をどうするかは、仕様に明示がない。

## 設計方針

- update 用の部分更新型 `MsfUpdateTrack` を追加する。`parent_name` を必須、`parent_namespace` を任意、`name` を必須 (§5.2.3)、`namespace` とその他の属性を任意とする。
- `name` は `parent_name` と一致することを要求する。`namespace` を指定する場合は `parent_namespace`、または `parent_namespace` 省略時の catalog namespace と一致することを要求する。update で Full Track Name を変更する経路は作らない。
- `parentNamespace` を省略した場合は catalog namespace を使う (clone と同じ規則)。
- 適用時は `parentName` と解決後の namespace で対象 track を探す。存在しない場合は illogical operation として `MessageError::InvalidCatalog` にする。
- マージ後の `MsfTrack` を `validate_full_track` に通す。例えば isLive が true の track に `trackDuration` を追加する update や、`loc` / `video` で `framerate` を欠く状態になる update を拒否する。
- `isLive` の false から true への変更は §5.2.8 の MUST に従って拒否する。
- encode 側でも `MsfUpdateTrack` を検証し、`validate_delta_for_encode` に update 分岐を追加する。
- §5.6.4 の `name` 省略は非規範例として扱い、`name` 必須の判断を doc に明記する。

## 完了条件

- `"update"` を含む delta update が decode / encode で往復一致すること。
- 存在する track の属性を update で変更でき、変更後のカタログが期待どおりになること。
- 存在しない `parentName` を指す update が `MessageError::InvalidCatalog` で拒否されること。
- `name` が `parentName` と一致しない update、`namespace` が対象と一致しない update が拒否されること。
- `isLive` を false から true に変える update が拒否されること。
- マージ後の track が `validate_full_track` に反する update (例: isLive true で trackDuration、loc / video で framerate 欠如) が拒否されること。
- `MsfUpdateTrack` を手組みして不正な状態を作った場合に encode が `MessageError::InvalidCatalog` になること。
- 上記を `tests/test_msf/delta_apply.rs`、`tests/test_msf/catalog_decode.rs`、`tests/test_msf/encode_decode_roundtrip.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
