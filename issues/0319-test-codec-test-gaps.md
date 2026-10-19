# codec / data stream のテストの穴と誤りを埋める

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/test-codec-test-gaps
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 の MUST に対応するにもかかわらずテストが無い経路と、構造上つねに成功してしまうテスト、仕様と食い違う期待値をまとめて直す。テストが通っていても違反を検出できない状態を解消する。

## 現状

- `src/stream/decoder.rs` の `consume_payload` (Subgroup / Fetch) は tests / pbt / fuzz から呼ばれていない。
- `src/loc.rs` の 65535 バイト超の decode と encode のエラー経路を検証するテストが無い。
- FETCH の Descending Group Order で Group ID Delta がアンダーフローする境界のテストが無い。
- `pbt/tests/prop_decoder.rs` の `decoder_split_invariance` は push 実装が `extend_from_slice` のみのため構造上つねに一致し、`decoder_garbage_never_panics` は規約が禁じる「panic しないことだけ」の PBT になっている。
- `pbt/tests/prop_stream/decoder.rs` のサンプラーは `sample_bytes_vec(ctx, 0)` で Properties 長を常に 0 にし、`has_properties: false` 固定のものがあるため、非空 Properties の切り出しを検証できない。
- `pbt/tests/prop_varint.rs` の `expected_leading_ones` は 8 バイト長で 6 になっており、コメント (`leading_ones=7`) と矛盾する。
- `pbt/tests/prop_message.rs` の `SUBSCRIBE_PARAMS` は `PARAM_RENDEZVOUS_TIMEOUT` を欠き、`sample_parameter_from` にも対応 arm が無い。
- `tests/test_varint.rs` の `spec_test_vectors` は draft-22 §8.1 Table 4 の 4 バイト・7 バイトベクトルを欠き、Table 4 に無い値を含む。
- `tests/test_pending_subgroup_buffer.rs` の `caller_dropped_objects_are_not_retained` はカウンタが 0 であることしか見ていない。
- `pbt/tests/prop_loc.rs` に固定単体ケースが含まれ、pbt に unittest を書かない規約に反する。
- `tests/test_parameter.rs` に同名の `missing_returns_none` が重複している。
- C4M の `pbt/tests/prop_c4m/dpop.rs` の `resource_consistent` は実装の写経で、fuzz は encode と `CatClaims::validate` を対象にしていない。
- 陳腐化したテストコメントがある。`pbt/tests/prop_message.rs` は削除済みの draft-19 §3.4 を参照し、`tests/test_message_parameter.rs` は存在しない PUBLISH_OK を参照する。`tests/test_message.rs` は存在しない CHANGES.md の追記を参照し、`tests/test_object_properties.rs` は存在しない PBT `decode_never_panics`
  を参照する。`tests/test_stream/encoder.rs` と `tests/test_stream/decoder.rs` の FETCH Object ID Delta のコメントは `+1` を誤って足している。
- `tests/test_decoder.rs` と `tests/test_subgroup_tracker.rs` はファイル先頭の doc を `///` で書いており、`//!` にする必要がある。

## 設計方針

- MUST に対応するエラーパスを単体テストで固定する。PBT は生成モデルを修正し、fail し得る性質に変える。任意入力の panic 耐性は fuzz に任せる。
- 既存の誤った期待値・誤った生成・重複テストを修正する。
- 各テストが「実装を変えたら落ちる」ことを確認してから完了とする。

## 完了条件

- `consume_payload` の正常系・長さ不一致・状態違い、LOC 65535、Descending アンダーフロー、非空 Properties の増分デコード、C4M PBT の独立モデルのテストが追加されていること。
- 上記の誤ったテスト・生成モデル・陳腐化したコメントが修正されていること。
- `cargo test --workspace` / `cargo test -p pbt` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
