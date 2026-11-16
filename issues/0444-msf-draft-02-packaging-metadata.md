# MSF カタログに packagingMetadata を追加する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §5.2.5 (Packaging metadata) は、packaging 固有のメタデータを運ぶ `packagingMetadata` フィールドを新設した。`loc` では `streamMapping` キーで Object から Stream への割り当てモードを通知する (§4.1.1)。

現行実装はこのフィールドを持たず、v02 のカタログが指定する `streamMapping` を読めない。本 issue はフィールドの追加と構文検証、`streamMapping` を取り出せる accessor までを対象とする。

## 現状

- `MsfTrack` / `MsfCloneTrack` / `TrackJsonParts` / `CommonTrackFields` に `packagingMetadata` に相当するフィールドがない。
- `decode_common_track_fields` は未知の JSON メンバーを無視するため、v02 カタログの `packagingMetadata` は黙って捨てられる。`write_track_json` も出力しない。
- v02 §5.2.5 は、`;` 区切りの `key=value` とし、key と value は case-sensitive、key と value に whitespace・`=`・`;` を含めてはならない (MUST NOT)、未知の key は無視する (MUST ignore)、同じ key が複数ある場合は最初の出現を使うと定める。
- `MsfCloneTrack::into_track` は明示値があれば上書きし、なければ親から継承する。`packagingMetadata` も同じ規則にする。
- `streamMapping` の値の解釈と、値がない場合の既定 `group` の扱いは 0459 が担当する。本 issue では raw 値の保持・構文検証・accessor の提供までにする。

## 設計方針

- `MsfTrack.packaging_metadata: Option<String>` と `MsfCloneTrack.packaging_metadata: Option<String>` を追加し、wire 上の `packagingMetadata` を保持する。
- 構文検証を `src/msf.rs` に追加する。`;` で分割し、各要素を最初の `=` で key と value に分ける。空要素、`=` を含まない要素、空の key、空の value、key / value 中の whitespace・`=`・`;` を `MessageError::InvalidCatalog` で拒否する。
- 構文検証を先に行い、その後に未知の key を無視する。既知の key (`streamMapping`) だけを accessor から返す。
- `MsfTrack` に `streamMapping` を取り出す公開 accessor を追加する。名前は `packaging_metadata_value(&self, key: &str) -> Option<&str>` とし、同じ key が複数ある場合は最初の値を返す。
- decode と encode の両方で構文検証を行う。`validate_full_track`、`validate_clone_track_fragment`、encode 前の検証に追加し、手組みの公開型から不正な metadata を出力できないようにする。
- `streamMapping` の値の検証と既定 `group` の解釈は 0459 が担当する。0459 は「フィールドなし」と「フィールドはあるが `streamMapping` キーなし」の両方で `group` になることを固定する。実施順序は 0444 → 0459 とする。

## 完了条件

- `"packagingMetadata": "streamMapping=object"` を持つカタログが decode / encode で往復一致すること。
- `key1=value;key2=value` が 2 つの key に解釈され、`packaging_metadata_value` で値を取得できること。
- 同じ key が 2 回現れた場合に最初の値を返すこと。未知の key が無視され、エラーにならないこと。
- 空要素 (`"a=b;;c=d"`)、`=` を含まない要素 (`"key"`)、空の key (`"=value"`)、空の value (`"key="`)、whitespace・`=`・`;` を含む key / value が `MessageError::InvalidCatalog` で拒否されること。
- `MsfTrack` / `MsfCloneTrack` を手組みして不正な `packagingMetadata` を持つカタログを encode した場合に `MessageError::InvalidCatalog` になること。
- `MsfCloneTrack` の `packagingMetadata` が、明示値があれば上書き、なければ親から継承されること。
- `streamMapping` の既定 `group` の解釈は 0459 の完了条件で固定されることを doc に明記すること。
- 上記を `tests/test_msf/catalog_decode.rs` と `tests/test_msf/encode_decode_roundtrip.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
