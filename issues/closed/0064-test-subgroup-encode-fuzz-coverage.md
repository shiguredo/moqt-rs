# SubgroupObject::encode の properties 検証経路を fuzz でカバーする

- Created: 2026-09-12
- Completed: 2026-09-17
- Branch: feature/test-subgroup-encode-fuzz-coverage
- Polished: 2026-09-17

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

## 解決方法

`fuzz_roundtrip.rs` の変更と、専用ターゲット `fuzz/fuzz_targets/fuzz_subgroup_object_encode.rs` の
追加で到達可能にした。

- `fuzz_roundtrip.rs` は `SubgroupObject::decode(data, true)` の第 2 戻り値
  (Properties Length varint 込みの生バイト) を `properties_data` として受け取り、
  `object.encode(true, properties_data.as_deref(), &mut buf)` へそのまま渡すようにした。
  これまで `_` で捨てて `None` 固定で再 encode していたため、decode 成功時の再エンコードが
  Properties 検証経路を通っていなかった
- `fuzz/fuzz_targets/fuzz_subgroup_object_encode.rs` を追加し、`fuzz/Cargo.toml` に `[[bin]]` を
  登録した。`has_properties` と `properties_data` (Properties Length varint + Properties データの
  生バイト列) を `Arbitrary` で任意に生成して `SubgroupObject::encode` を呼び、panic しないこと
  だけを検証する
- payload_length と status は encode が要求する契約 (payload_length == 0 なら status 必須、
  payload_length > 0 なら status 禁止) を満たす 3 通りに固定した。ここを任意値にすると契約違反で
  手前の検証に弾かれ、目的の Properties 検証経路に到達しない。status に End of Group /
  End of Track を選ぶ分岐では、非 Normal status に Properties Length > 0 を付けた場合の拒否経路も通る
- `has_properties` が false のときは `properties_data` を渡さない (`then_some`)。
  `fuzz_fetch_stream_encoder.rs` と同じ組み立て方に揃えた

検証:

- `cargo check --manifest-path fuzz/Cargo.toml` が通る
- `cargo +nightly fuzz run fuzz_subgroup_object_encode -- -runs=10000` と
  `cargo +nightly fuzz run fuzz_roundtrip -- -runs=10000` が panic せず完走する
- `cargo +nightly fuzz coverage` と `llvm-cov show` で `validate_properties_blob` への到達を確認した。
  `fuzz_subgroup_object_encode` は 25 回呼び、内訳は Properties Length varint が途中切れ 8 回、
  宣言長と実データ長の不一致 13 回、長さ整合 4 回で、空スライスの拒否と非 Normal status への
  Properties 付与の拒否も通った。`fuzz_roundtrip` は decode した Properties を渡す経路から 61 回呼んだ
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` が通る
