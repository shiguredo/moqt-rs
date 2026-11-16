# MSF カタログの depends に renderGroup 参照を追加する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §5.2.15 (Dependencies) は、`depends` の要素に track name (JSON String) だけでなく renderGroup (JSON Number) を取れるようにした。

- String 要素はカタログ内のちょうど 1 つの他の track object の Track Name に一致しなければならない (MUST)。
- Number 要素はカタログ内のちょうど 1 つの renderGroup の識別子に一致しなければならない (MUST)。
- 解決できない dependency を含むカタログは invalid である (MUST)。
- v01 の「依存先の namespace は依存元と同一と仮定する」は削除された。

現行実装は String 専用で、v02 のカタログにある Number 要素を型エラーとして拒否する。

## 現状

- `MsfTrack.depends: Vec<String>`、`MsfCloneTrack.depends: Option<Vec<String>>`、`MsfRemoveTrack` 周辺の受け渡しも `String` 前提である。
- `decode_common_track_fields` は `depends` の各要素を文字列として読むため、JSON Number を `MessageError::InvalidCatalog` にする。
- `validate_full_track` は media timeline / event timeline の `depends` が非空かどうかだけを見て、参照先が解決できるかを検証しない。
- `validate_track_name_uniqueness` は namespace 付きの Full Track Name で一意性を見る。同名別 namespace を許す既存テスト `same_name_different_namespace_allowed` がある。
- renderGroup は `MsfTrack.render_group: Option<u64>` に保持される。

## 設計方針

- `depends` の要素を表す公開 enum (例: `MsfDependency::TrackName(String)` / `MsfDependency::RenderGroup(u64)`) を追加し、`DisplayJson` で String / Number を出し分ける。
- `MsfTrack.depends` と `MsfCloneTrack.depends` の型をこの enum の列に変更する。公開型の変更を伴う破壊的変更として doc に明記する。
- カタログ適用後に dependency の解決検証を追加する。track name 参照はカタログ内の track と一致すること、renderGroup 参照はカタログ内のいずれかの track が持つ renderGroup の識別子と一致することを検証し、解決できない場合は `MessageError::InvalidCatalog` にする。renderGroup は複数の track が共有してよい。
- media timeline / event timeline の「depends 非空」の検証は v02 で条件が変わるため、別 issue の timeline catalog 要件で扱う。本 issue では型と解決検証に限定する。

## 完了条件

- `"depends": ["video"]` と `"depends": [1]` の両方が decode / encode で往復一致すること。
- 存在しない track name と存在しない renderGroup の参照が `MessageError::InvalidCatalog` で拒否されること。同じ renderGroup を複数の track が共有している場合も、その renderGroup への参照が解決されること。
- 既存の String のみの `depends` の挙動が退行しないこと。
- 上記を `tests/test_msf/catalog_decode.rs` と `tests/test_msf/delta_apply.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
