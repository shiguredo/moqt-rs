# FETCH の End of Range を delta 符号化する

- Created: 2026-11-03
- Completed: 2026-11-03
- Branch: develop
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §11.4.2.1、§11.4.2.2](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/stream/fetch.rs` の End of Range は Group ID / Object ID を直接持ち、`src/stream/decoder.rs` / `encoder.rs` の `update_prior_for_end_of_range` は受けた値を絶対 Location として保持する。
範囲指示自身の ID を prior から delta 復元する処理はない。

## 設計方針

FETCH_HEADER 直後の最初の entry が範囲指示なら絶対 ID とする。それ以外の範囲指示は、ストリーム内で最初に現れる範囲指示であっても、直前の Object または範囲指示を基準にする。
Group は Ascending なら加算、Descending なら減算する。
同一 Group の Object は prior + delta + 1、Group が変わる場合は絶対 Object ID にする。
直前の actual Object の Subgroup ID / Priority を範囲指示で消さず保持する。
低水準 codec の raw delta と高水準の絶対 Location の契約を明記する。

## 完了条件

- FETCH_HEADER 直後の範囲指示と、Object の後に初めて現れる範囲指示を区別し、連続する範囲指示、Object との交互配置、昇順・降順・同一 Group を golden バイト列で検証すること。
- 加算 overflow / 減算 underflow が DELTA_ENCODING_OVERFLOW になること。
- actual Object が未出現の prior Subgroup ID / Priority 参照を MALFORMED_OBJECT で拒否し、既出なら保持すること。

## 解決方法

低水準 codec (`src/stream/fetch.rs`) の End of Range の 3 variant が持つフィールドを
`group_id_delta` / `object_id_delta` に変え、wire に現れる生値をそのまま扱う契約にした
(draft-ietf-moq-transport-23 §11.4.2 (End of Range) / §11.4.2.1 (ID Decoding))。
`FetchPriorContext::First` のときだけ両フィールドが絶対 ID で、それ以外は直前の
Object または End of Range の Location からのデルタである。デルタの計算・解決・
加減算の overflow 検証は絶対 Location を扱う高水準のエンコーダ / デコーダの責務とし、
各 doc に明記した。

- `FetchStreamDecoder::resolve_end_of_range` を追加し、prior Location を基準に
  Group は Ascending で加算・Descending で減算、Group ID Delta が 0 なら
  Object ID を prior + delta + 1、0 でなければ Object ID Delta を絶対値として解決する。
  u64 の範囲を外れる場合は `MessageError::DeltaEncodingOverflow` (0x400) を返す。
- `FetchStreamDecoder` の prior 状態を「直前 Location (Group ID / Object ID)」と
  「直前の actual Object の Subgroup ID / Priority」に分離し、後者を `Option` にした。
  End of Range は Location だけを更新して後者を保持し (§11.4.2.2 (Prior Object Fields))、
  actual Object が未出現なら `None` のまま残す。prior Subgroup ID / Priority を参照する
  Object は低水準 codec の `NoPriorActualObject` 検証とデコーダ側の解決の両方で
  MALFORMED_OBJECT にする。
- `FetchStreamEncoder` の prior 状態も `PriorEncodeState { group_id, object_id, actual_object }` に
  分け、End of Range は `actual_object` を保持する。`end_of_range_deltas` が絶対 Location から
  生デルタを計算し、Group Order と逆方向の Group 変化や同一 Group で増加しない Object ID は
  書き込み前に `ProtocolViolation` にする。End of Range の Group ID Delta には
  Object と違って +1 が付かない (§11.4.2.1)。
- Object のデルタ規則 (§11.4.1.1) は本対応の対象外とし、変更していない。

## 完了条件の検証

| 完了条件 | 検証 |
| --- | --- |
| FETCH_HEADER 直後の範囲指示が絶対 ID、Object 後は直前 Location 基準になること | `first_end_of_range_after_header_is_absolute_and_after_object_is_delta`。同じ 4 バイトが FETCH_HEADER 直後では (3, 4)、Object (1, 0) の直後では (4, 4) に解決されること |
| 連続する範囲指示 | 同ファイルの `consecutive_end_of_range_indicators_chain_previous_locations`。4 連続の End of Range が直前の End of Range の Location を基準に (5, 10) → (5, 12) → (8, 0) → (8, 5) に解決されること |
| Object との交互配置・昇順・同一 Group | `ascending_end_of_range_golden_bytes_decode` / `_encode`。End of Range の Group ID Delta に +1 が付かないこと、同一 Group の +1 規則、End of Range 後の Subgroup ID / Priority 継承を golden バイト列で固定 |
| 降順と同一 Group | `descending_end_of_range_golden_bytes_decode` / `_encode`。golden バイト列で Group の減算、Group 変化時の絶対 Object ID、同一 Group の +1 規則を固定 |
| 加算 overflow | `ascending_group_id_addition_overflow_is_delta_encoding_overflow` / `same_group_object_id_addition_overflow_is_delta_encoding_overflow`。2 件目の decode が `DeltaEncodingOverflow` (DELTA_ENCODING_OVERFLOW 0x400) になること |
| 減算 underflow | `descending_group_id_subtraction_underflow_is_delta_encoding_overflow`。0 - 1 が `DeltaEncodingOverflow` になること |
| actual Object 未出現の prior Subgroup ID / Priority 参照を MALFORMED_OBJECT で拒否 | `prior_subgroup_id_without_actual_object_is_malformed_object` / `prior_priority_without_actual_object_is_malformed_object` |
| actual Object 既出なら prior Subgroup ID / Priority を保持 | `end_of_range_preserves_prior_actual_object_fields`。End of Range の前後で Subgroup ID 7 / Priority 200 が保持され、後続 Object が省略形で解決できること |
| 低水準 codec の raw 値と高水準の絶対 Location の契約 | `src/stream/fetch.rs` と `encoder.rs` / `decoder.rs` の doc に明記。`fetch_stream_object.rs` の `end_of_range_raw_fields_do_not_depend_on_prior_context` で 3 文脈の往復を固定 |
| エンコーダが表現できない Location を拒否 | `encoder_rejects_end_of_range_group_id_in_the_wrong_direction` / `encoder_rejects_end_of_range_object_id_not_after_prior_location`。`ProtocolViolation` を返し、失敗時にバッファへ書き込まないこと |
| 分割受信で結果が変わらない | `pbt/tests/prop_stream/encoder.rs` の `encoder_decoder_roundtrip_with_end_of_range`。ランダムな Object / End of Range 列の往復と任意 1 分割の一致 |
| 検証コマンド | `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace` がすべて成功 |
