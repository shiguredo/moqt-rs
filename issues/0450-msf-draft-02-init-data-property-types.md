# MSF initDataList に track-property / object-property 型を追加する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §5.1.7 (Initialization Data List) は `initDataList` の `type` に `"track-property"` と `"object-property"` を追加した。これらの `data` は初期化データの Base64 ではなく、property type の hex notation を持つ。初期化データ自体は §12.2 (Initialization properties) の `VIDEO_CONFIG` (0x0D) / `AUDIO_CONFIG` (0x0F) に載る。
現行実装は `"inline"` だけを受理するため、v02 のカタログが持つ property 型の初期化データを読めない。

## 現状

- `MsfInitDataKind` は `Inline` のみで、`from_init_data_kind_str` は `"inline"` 以外を `MessageError::InvalidCatalog` にする。
- `MsfInitData.data` は種別に依存しない `String` で、`validate_init_refs` は `initRef` の id が存在するかだけを見て、`type` を見ない。Base64 としての検証は既存 issue で扱っている。
- `src/loc.rs` に `PROP_VIDEO_CONFIG` (0x0D) と `PROP_AUDIO_CONFIG` (0x0F) があり、`TrackProperties` / `ObjectProperties` で wire に載せられる。`crates/moq-pub` は `VIDEO_CONFIG` / `AUDIO_CONFIG` を Object property として付与するが、カタログの `initRef` を設定していない。
- v02 §12.2 は、値が immutable なら Track property と `initRef` (type `track-property`)、mutable なら Object property と各 Group の最初の Object への付与、`initRef` (type `object-property`) を要求する。

## 設計方針

- `MsfInitDataKind` に `TrackProperty` / `ObjectProperty` を追加し、`from_init_data_kind_str` と `DisplayJson` の両方向で扱う。
- `type` ごとに `data` の妥当性を変える。`inline` は Base64、`track-property` / `object-property` は property type の hex notation として検証する。hex notation の正確な表記は仕様の記述と既存の property 定数の表現を確認して決める。
- `initRef` の解決検証に `type` の整合を加える。参照先 entry が `track-property` なら、参照する property が Track property として使われること、`object-property` なら Object property として使われることを検証する。
- `crates/moq-pub` の catalog 生成で `initRef` を設定する経路は、初期化 property の signaling と合わせて別 issue で扱う。本 issue では codec の型追加と検証までを対象にする。

## 完了条件

- `"type": "track-property"` と `"type": "object-property"` を持つ `initDataList` が decode / encode で往復一致すること。
- `inline` の `data` が Base64 として、`track-property` / `object-property` の `data` が property type の hex として検証されること。不正な形式が `MessageError::InvalidCatalog` で拒否されること。
- `initRef` が参照先 entry の `type` と整合しない場合に拒否されること。
- 既存の `inline` の挙動が退行しないこと。
- 上記を `tests/test_msf/catalog_decode.rs` と `tests/test_msf/encode_decode_roundtrip.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
