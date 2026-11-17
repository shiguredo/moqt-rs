# MSF initDataList に track-property / object-property 型を追加する

- Created: 2026-11-16
- Completed: 2026-11-17
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §5.1.7 (Initialization Data List) は `initDataList` の `type` に `"track-property"` と `"object-property"` を追加した。
これらの `data` は初期化データの Base64 ではなく、property type の hex notation を持つ。初期化データ自体は §12.2 (Initialization properties) の `VIDEO_CONFIG` (0x0D) / `AUDIO_CONFIG` (0x0F) に raw binary として載る。

現行実装は `"inline"` だけを受理するため、v02 のカタログが持つ property 型の初期化データを読めない。

## 現状

- `MsfInitDataKind` は `Inline` のみで、`from_init_data_kind_str` は `"inline"` 以外を `MessageError::InvalidCatalog` にする。
- `MsfInitData.data` は種別に依存しない `String` で、`validate_init_refs` は `initRef` の id が存在するかだけを見る。`decode_init_data` は `data` の形式を検証しない。
- `"inline"` の `data` が Base64 であることの検証は 0342 が担当しており、未完了である。
- §12.2 は、値が immutable なら Track property と `initRef` (type `track-property`)、mutable なら Object property と `initRef` (type `object-property`) を要求する。`0x0D` / `0x0F` は `PropertyScope::Both` である。
- `initRef` は id への文字列だけで、カタログは property を Track と Object のどちらとして使うかを表さない。そのため、カタログの情報だけから `initRef` の type と property の scope の整合を検証することはできない。

## 設計方針

- `MsfInitDataKind` に `TrackProperty` / `ObjectProperty` を追加し、`from_init_data_kind_str` と `DisplayJson` の両方向で扱う。
- `type` ごとに `data` の妥当性を変える。`inline` は 0342 の Base64 検証に委ねる。`track-property` / `object-property` は property type の hex notation として検証する。
- hex notation の文法は仕様が定義していないため、本ライブラリの受理形式を次のように定めて doc に明記する: `0x` 接頭辞に続けて 1〜16 桁の hex digit (大文字小文字を問わない) で、値は MOQT の property type が表現できる範囲 (2^62 - 1 以下) に収まること。例は `0x0D`、`0x78`。`0D`、`0x`、`0xZZ`、範囲外の値は拒否する。
- decode と encode の両方で `data` を検証し、手組みの公開型から不正な `data` を出力できないようにする。
- `initRef` の type と property の scope の整合検証は、カタログから scope を特定できないため本 issue では行わない。§12.2 の wire 上の signaling は、初期化 property を扱う別の対応に委ねる。
- `"inline"` の挙動と Base64 検証は 0342 の担当とし、本 issue では非退行だけを確認する。

## 完了条件

- `"type": "track-property"` と `"type": "object-property"` を持つ `initDataList` が decode / encode で往復一致すること。
- `"data": "0x0D"` と `"data": "0x78"` が受理され、`"0D"`、`"0x"`、`"0xZZ"`、2^62 以上の値が `MessageError::InvalidCatalog` で拒否されること。
- 手組みの `MsfInitData` に不正な hex を入れて encode した場合に `MessageError::InvalidCatalog` になること。
- `"inline"` の `data` の扱いが変わらないこと (Base64 検証は 0342)。
- `initRef` の type と property の scope の整合検証は行わないことを doc に明記すること。
- 上記を `tests/test_msf/catalog_decode.rs` と `tests/test_msf/encode_decode_roundtrip.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- `MsfInitDataKind` に `TrackProperty` / `ObjectProperty` を追加し、`from_init_data_kind_str` と `DisplayJson` の両方向で `track-property` / `object-property` を扱うようにした。
- `validate_init_data` を追加し、property 型の `data` を `0x` + 1〜16 桁の hex digit (2^62 - 1 以下) として検証する。decode と full encode の両方から呼ぶ。`inline` の Base64 検証は 0342 に委ね、挙動を変えていない。
- `initRef` の type と property scope の整合は catalog から判定できないため検証しないことを doc に明記した。
- `tests/test_msf/catalog_decode.rs` / `encode_decode_roundtrip.rs` に、property 型の受理・往復、不正 hex、範囲境界、encode 拒否のテストを追加した。`docs/msf.md` も更新した。

確認コマンド: `cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` が通ることを確認した。
