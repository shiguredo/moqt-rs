# MSF カタログの名前フィールドを v02 の '/' 区切りとバイト比較に合わせる

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 はカタログ上の名前表現を変更した。

- §5.2.2 (Track namespace): `trackNamespace` は `/` 区切りの string で、各 substring を MOQT Track Namespace Field に対応させる。1〜32 fields、各 field は非空。空文字、先頭・末尾の `/`、連続する `/`、32 fields 超はカタログを invalid にする (MUST)。宣言した namespace は継承した namespace を拡張せず置換する。
- §5.2.3 (Track name): `trackName` は空であってはならない (MUST NOT)。一意性は Full Track Name (namespace + name) で判定する (MUST)。
- §11.2.1〜§11.2.5: カタログ文字列から MOQT バイト列への canonical mapping を定義する。UTF-8 に符号化し、正規化・case folding をせず、未対サロゲートと NUL を禁じ、BOM を禁じる。比較は mapped bytes の完全一致で行い、MOQT の生バイト列と比較するときもバイト列同士で比較する (MUST)。

現行実装はカタログの namespace を生の string として保持し、MOQT 側の Track Namespace と解釈が一致していない。v02 のカタログを受理しても、publisher と subscriber で異なる Track Namespace に解決される。

## 現状

- `MsfTrack.namespace` / `MsfCloneTrack.namespace` / `MsfRemoveTrack.namespace` / `MsfCloneTrack.parent_namespace` は `Option<String>` を保持するだけで、`/` 分割・1〜32 fields 検証・`Track Namespace` への変換を行わない。`decode_common_track_fields` は JSON string であることだけを確認する。
- `MsfTrack.name` / `MsfCloneTrack.name` は空文字を受理する。`decode_track` は `name` の存在だけを要求し、`validate_track_name_uniqueness` は空名 1 件を受理する。
- `crates/moq-sub` の `resolve_track_namespace` はカタログの `namespace` を `src/name.rs` の `parse_namespace` (`-` 区切りの MSF URL 形式) として解釈する。v02 の `"customer/livestream/123"` は `-` を含まない 1 field として扱われ、意図した 3 fields にならない。
- `crates/moq-pub` はカタログの `namespace` に `serialize_namespace` の `-` 区切り形式を書き込む。v02 の `/` 区切りと一致しない。
- NUL の検査はない。`MsfCatalogDocument::encode` は `\u0000` を含む name / namespace をそのまま出力できる。BOM と未対サロゲートは `nojson::RawJson::parse` が拒否するが、その挙動を固定するテストはない。
- カタログ内の比較は `MsfCatalog::track_key` と `validate_track_name_uniqueness` の `String` 等値で行われ、これは UTF-8 バイト等値である。一方、MOQT の生バイト列と比較する API はない。

## 設計方針

- カタログの namespace string を検証して `TrackNamespace` に変換する関数を `src/msf.rs` に追加する。`src/message/common.rs` の `TrackNamespace` が持つ 32 fields 制限を再利用し、空 field と区切り違反を `MessageError::InvalidCatalog` で拒否する。
- カタログの name string を `Vec<u8>` に変換する関数を追加する。空文字と NUL を拒否し、それ以外は UTF-8 バイト列をそのまま返す。
- wire 上の `MsfTrack.namespace` / `name` は string のまま保持する。公開型は変えず、変換 API を追加する形にする。`MsfCloneTrack.parent_namespace` にも同じ検証を適用する。
- `crates/moq-pub` はカタログの namespace に `/` 区切りで書き、`crates/moq-sub` は `/` 区切りとして読むように揃える。MOQT 側の `TrackNamespace` との比較は変換後のバイト列で行う。
- BOM と未対サロゲートの拒否は `nojson` の挙動に依存するため、挙動をテストで固定する。

## 完了条件

- `"customer/livestream/123"` が 3 fields の `TrackNamespace` に変換され、URL 形式 `customer-livestream-123--catalog` と往復一致すること。
- 空文字、`"a//b"`、`"/a"`、`"a/"`、33 fields、NUL を含む field が `MessageError::InvalidCatalog` で拒否されること。
- 空の `trackName` が拒否され、namespace が異なる同名 track は別 track として受理されること。
- BOM 付きカタログと `"\uDEAD"` を含むカタログが `MessageError::InvalidCatalog` で拒否されることがテストで固定されていること。
- `crates/moq-pub` が出力したカタログを `crates/moq-sub` が読んだとき、同じ `TrackNamespace` に解決されること。
- 上記を `tests/test_msf/catalog_decode.rs` と `tests/test_msf/encode_decode_roundtrip.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
