# MSF カタログに packagingMetadata を追加する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/add-msf-packaging-metadata
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §5.2.5 (Packaging metadata) は、packaging 固有のメタデータを運ぶ `packagingMetadata` フィールドを新設した。`loc` では `streamMapping` キーで Object から Stream への割り当てモードを通知する (§4.1.1)。
現行実装はこのフィールドを持たず、v02 のカタログが指定する streamMapping を読めない。既定値 `group` 以外のモードを扱うには、まずこのフィールドを表現できる必要がある。

## 現状

- `MsfTrack` / `MsfCloneTrack` / `TrackJsonParts` / `CommonTrackFields` に `packagingMetadata` に相当するフィールドがない。
- `decode_common_track_fields` は未知の JSON メンバーを無視するため、v02 カタログの `packagingMetadata` は黙って捨てられる。`write_track_json` も出力しない。
- v02 §5.2.5 は、`;` 区切りの `key=value` とし、key と value は case-sensitive、key と value に whitespace・`=`・`;` を含めてはならない (MUST NOT)、未知の key は無視する (MUST ignore)、同じ key が複数ある場合は最初の出現を使うと定める。
- `loc` の `streamMapping` の値は §4.1.1 が定義する。フィールドがない、またはキーがない場合は `group` と仮定する (MUST)。

## 設計方針

- `MsfTrack.packaging_metadata: Option<String>` を追加し、wire 上の `packagingMetadata` をそのまま保持する。
- `;` 区切りと `key=value` の構文、空白・`=`・`;` の禁止、重複 key の先頭優先、未知 key の無視を検証・解釈する関数を `src/msf.rs` に追加する。不正な構文は `MessageError::InvalidCatalog` にする。
- `streamMapping` の値の解釈と検証は、stream mapping 本体の issue で扱う。本 issue では値を取り出せる accessor を用意するに留める。
- `MsfCloneTrack` にも同じフィールドを追加し、clone 時の継承規則は他のフィールドと同じく明示値があれば上書き、なければ継承とする。

## 完了条件

- `"packagingMetadata": "streamMapping=object"` を持つカタログが decode / encode で往復一致すること。
- `key1=value;key2=value` が 2 つの key に解釈され、`key1=a=b` と空白を含む値が `MessageError::InvalidCatalog` で拒否されること。
- 同じ key が 2 回現れた場合に最初の値を返すこと。未知の key が無視され、エラーにならないこと。
- `packagingMetadata` を持たない track で `streamMapping` が `group` として扱われること。
- 上記を `tests/test_msf/catalog_decode.rs` と `tests/test_msf/encode_decode_roundtrip.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
