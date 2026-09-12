# SubgroupObject::encode の properties 検証経路を fuzz でカバーする

- Created: 2026-09-12
- Completed: {YYYY-MM-DD}
- Branch: feature/test-subgroup-encode-fuzz-coverage
- Polished: {YYYY-MM-DD}

## 目的

`SubgroupObject::encode` の properties 検証 (`validate_properties_blob`) に fuzz から到達できるようにし、不一致 blob の拒否経路を任意入力に対して回帰検知できるようにする。

## 現状

`fuzz/fuzz_targets/fuzz_roundtrip.rs` は decode 成功時に properties 生バイトを捨て、`properties_data = None` 固定で再 encode している。

- `SubgroupObject::decode(data, true)` の第 2 戻り値 (`properties_bytes`) を `_` で捨て、`object.encode(true, None, &mut buf)` を呼ぶため、`validate_properties_blob` に到達しない。
- Fetch 側は `fuzz/fuzz_targets/fuzz_fetch_stream_encoder.rs` が任意の `properties` を `encode_object` に渡すため検証経路に到達するが、Subgroup 側の encode は fuzz から到達できない。
- decode が返す properties は定義上つねに長さ整合のため、decode 結果を encode に渡すだけでは不一致・途中切れ varint の拒否経路には到達しない。

## 設計方針

- `fuzz_roundtrip.rs` で decode の properties 生バイトを encode に渡し、property blob の再エンコード経路を通す。
- 任意の `(has_properties, properties_data)` を生成して `SubgroupObject::encode` を呼ぶ専用 fuzz ターゲット (例: `fuzz/fuzz_targets/fuzz_subgroup_object_encode.rs`) を追加し、不一致長・途中切れ varint の拒否経路を到達可能にする。fuzz は panic しないことのみを検証する。

## 完了条件

- `fuzz_roundtrip.rs` が decode した properties を encode に渡していること
- Subgroup 側 encode の properties 検証経路に到達する fuzz ターゲットが追加されていること
- `cargo check --manifest-path fuzz/Cargo.toml` が通ること
