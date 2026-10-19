# codec / data stream のテストの穴と誤りを埋める

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/test-codec-test-gaps
- Polished: 2026-10-20

## 目的

draft-ietf-moq-transport-22 の MUST / MAY に対応するにもかかわらずテストが無い codec / data stream の経路と、構造上つねに成功してしまうテスト、仕様と食い違う期待値をまとめて直す。テストが通っていても違反を検出できない状態を解消する。

## 現状

- `src/loc.rs` の `LocProperties::encode` / `LocProperties::decode` は、奇数 ID のバイト列が 65535 バイトを超える場合にエラーを返すが、65535 バイト (上限) と 65536 バイト (超過) の境界を検証するテストが無い。encode は `MessageError::PayloadTooLong`、decode は `MessageError::ProtocolViolation` を返す。
- `src/stream/decoder.rs` の `FetchStreamDecoder::resolve_object` は Descending Group Order で `prev_group.checked_sub(id).and_then(|v| v.checked_sub(1))` により Group ID Delta を解決する。`prev_group == id + 1` の成功境界と `prev_group < id + 1` のアンダーフロー境界を検証するテストが無い。
- `pbt/tests/prop_decoder.rs` の `decoder_split_invariance` は、`MessageDecoder::push` が `self.buf.extend_from_slice(data)` のみであるため、2 回に分けて push してもバッファ内容が一括 push と同一になり、`try_decode_message` の結果も構造上つねに一致する。
  `decoder_garbage_never_panics` は、panic しないことと push 分割で成否が変わらないことだけを検証する PBT で、shiguredo-rust の「PBT に任意入力でパニックしないことだけを検証するテストを書かないこと (fuzzing の役割)」に反する。
- `pbt/tests/prop_stream/decoder.rs` の `sample_subgroup_stream` は `length_prefixed_properties(&noprop::sample_bytes_vec(ctx, 0))` により Properties 長を常に 0 にし、`sample_fetch_stream` は `has_properties: false` 固定であるため、非空 Properties の切り出し (`properties_bytes`) を検証できない。
- `pbt/tests/prop_varint.rs` の `leading_ones_indicate_length` は `expected_leading_ones` の 8 バイト長分岐を `6u32` にしており、同じ行のコメント (`leading_ones=7`) と矛盾する。さらに 8 バイト長の分岐は `leading_ones` を assert せず `buf[0] == 0xFE` だけを確認している。
- `pbt/tests/prop_message.rs` の `SUBSCRIBE_PARAMS` は `PARAM_RENDEZVOUS_TIMEOUT` を欠き、`sample_parameter_from` にも `PARAM_RENDEZVOUS_TIMEOUT` の arm が無い。
  draft-ietf-moq-transport-22 §9.20.6 (RENDEZVOUS TIMEOUT Parameter) は "The RENDEZVOUS_TIMEOUT parameter (Parameter Type 0x04) MAY appear in a SUBSCRIBE message." と規定し、`src/message.rs` の `SUBSCRIBE_ALLOWED_PARAMS` は同型を許可している。
- `tests/test_varint.rs` の `spec_test_vectors` は draft-ietf-moq-transport-22 §8.1 Table 4 (Example Integer Encodings) の 4 バイトベクトル `0xed7f3e7d` (226,442,877) と 7 バイトベクトル `0xfc8998abc66bc0` (151,288,809,941,952) を欠き、Table 4 に無い `0xf01d7f3e7d` (494,878,333) を含む。
- `tests/test_pending_subgroup_buffer.rs` の `caller_dropped_objects_are_not_retained` は、チャンクを push せずに `add` / `remove` し、`stream_count` / `total_bytes` が 0 であることと `take_ready(i64::MAX)` が None であることだけを見ている。チャンクを保持していないことを実データで検証していない。
- 陳腐化したテストコメントがある。
  - `pbt/tests/prop_message.rs` の `SUBSCRIBE_PARAMS` 直前のコメントは "Range Filters (0x25-0x29) は draft-19 §3.4 で追加された。" と書くが、draft-19 は `refs/` に無く、現在の根拠は draft-ietf-moq-transport-22 §3.3.2 (Range Filters) である。
  - `tests/test_message_parameter.rs` の `range_filter_multiple_appearance` の doc は "その他 4 種 (0x25-0x28) を FETCH / SUBSCRIBE / SUBSCRIBE_TRACKS / PUBLISH_OK / REQUEST_UPDATE ... で MAY appear multiple times" と書くが、
    draft-ietf-moq-transport-22 §3.3.2 の列挙に PUBLISH_OK は含まれない (PUBLISH_OK は §1.5 (Response Message Naming) で REQUEST_OK の別名と定義される)。
  - `tests/test_message.rs` の `subscribe_ok_with_fill_parameters_reports_value_error_first` の doc は "(CHANGES.md に記載した受信挙動の変更)" と書くが、CHANGES.md にその追記は無い (CODEBASE.md は変更履歴を CHANGES.md に残さない方針)。
  - `tests/test_object_properties.rs` の `decode_error_variants_are_typed` の doc は "PBT の `decode_never_panics` が任意バイト列で確認していた..." と書くが、`decode_never_panics` という PBT は存在しない。任意バイト列の panic 耐性は `fuzz/fuzz_targets/fuzz_decode_object_properties.rs` が担当する。
  - `tests/test_stream/encoder.rs` の `test_subgroup_change_with_object_id_delta` のコメントは "decoder は 5 + 10 + 1 = 16 と誤解釈した" と書くが、FETCH の同 Group 内 Object ID Delta は `prior + delta` で +1 しないため、正しくは `5 + 10 = 15` である。
  - `tests/test_stream/decoder.rs` の `test_fetch_rejects_object_id_delta_overflow` のコメントは "同 Group の object_id はデルタ (prev_object + delta + 1)" と書くが、
    FETCH の同 Group 内 Object ID Delta は `prev_object + delta` で +1 しない。同じファイルの `test_fetch_rejects_group_id_delta_overflow_in_ascending_mode` の "prev_group + delta + 1" は Group ID の正しい式なので変更しない。
- `tests/test_decoder.rs` の `///` モジュール doc は `use` の後に置かれ、`tests/test_subgroup_tracker.rs` の `///` モジュール doc はファイル先頭に置かれている。どちらもモジュール doc は `//!` でファイル先頭に書く必要がある。

## 設計方針

- MUST に対応するエラーパスを単体テストで固定する。PBT は生成モデルを修正し、実装を変えたら落ちる性質に変える。任意入力の panic 耐性は fuzz に任せ、panic しないことだけを検証する PBT は削除する。
- `consume_payload` の 4 経路テスト (正常系・長さ不一致・状態違い・バッファ非空) は 0314 の担当とする。0314 の契約変更 (外部読み出し専用、呼び出し時 `buf` が空、`length == remaining`、エラー時状態不変) に合わせたテストを 0314 が追加するため、本 issue では consume_payload のテストを追加しない。
- `decoder_split_invariance` は、各 push の後に `try_decode_message` を呼び、その結果列 (メッセージ / None / Err) が一括 push と一致することを検証する。push の分割自体ではなく、インクリメンタルなデコード状態の遷移を検証対象にする。
- `decoder_garbage_never_panics` は削除する。`fuzz/fuzz_targets/fuzz_message_decoder.rs` が任意入力の panic 耐性を担当する。
- `sample_subgroup_stream` / `sample_fetch_stream` は非空 Properties を生成し、`DecodedSubgroupObject::properties_bytes` / `DecodedFetchObject::properties_bytes` が Properties Length varint と Properties データをそのまま保持することを検証する。
- `leading_ones_indicate_length` は 8 バイト長の `expected_leading_ones` を `7u32` にし、8 バイト長でも `leading_ones` を assert する。
- `SUBSCRIBE_PARAMS` に `PARAM_RENDEZVOUS_TIMEOUT` を追加し、`sample_parameter_from` の VarInt arm で生成する。
- `spec_test_vectors` は Table 4 の 8 ベクトルをすべて含め、Table 4 に無い `0xf01d7f3e7d` は別のテストに移すか削除する。
- `caller_dropped_objects_are_not_retained` は、チャンクを push した entry を remove し、`total_bytes` が 0 に戻り、`take_ready(i64::MAX)` が None になることを検証する。保持していたチャンクが引き取れないことを実データで固定する。
- 重複テストの整理 (`pbt/tests/prop_loc.rs` の固定単体ケース、`tests/test_parameter.rs` の `missing_returns_none` 重複) は 0329 が担当するため、本 issue では扱わない。
- 陳腐化したコメントは、上記の現状に挙げた各ファイルで、現在の draft-22 の節番号・実装の式・実在するテスト名に合わせて修正する。
- `tests/test_decoder.rs` / `tests/test_subgroup_tracker.rs` のモジュール doc は `//!` でファイル先頭に置く。
- 実装の挙動は変えない。テストの追加・修正で実装の不足が見つかった場合は、別 issue として切り出す。
- 各テストが「実装を変えたら落ちる」ことを確認してから完了とする。

## 完了条件

- `LocProperties` の 65535 バイト境界のテストが encode / decode の両方に追加され、65535 バイトが受理され、65536 バイトが拒否されること (encode は `PayloadTooLong`、decode は `ProtocolViolation`)。
- FETCH の Descending Group Order で、`prev_group == Group ID Delta + 1` の成功境界と `prev_group < Group ID Delta + 1` のアンダーフロー境界のテストが追加されていること。
- `pbt/tests/prop_decoder.rs` の `decoder_split_invariance` が、各 push 後の `try_decode_message` の結果列を比較する fail し得る性質になっていること。
- `pbt/tests/prop_decoder.rs` の `decoder_garbage_never_panics` が削除され、任意入力の panic 耐性が fuzz に任されていること。
- `pbt/tests/prop_stream/decoder.rs` の `sample_subgroup_stream` / `sample_fetch_stream` が非空 Properties を生成し、`properties_bytes` の切り出しを検証していること。
- `pbt/tests/prop_varint.rs` の `leading_ones_indicate_length` が 8 バイト長で `expected_leading_ones = 7u32` を使い、8 バイト長でも `leading_ones` を assert していること。
- `pbt/tests/prop_message.rs` の `SUBSCRIBE_PARAMS` が `PARAM_RENDEZVOUS_TIMEOUT` を含み、`sample_parameter_from` が同型を生成できること。
- `tests/test_varint.rs` の `spec_test_vectors` が Table 4 の 8 ベクトルをすべて含み、Table 4 に無い `0xf01d7f3e7d` が `spec_test_vectors` から分離されていること。
- `tests/test_pending_subgroup_buffer.rs` の `caller_dropped_objects_are_not_retained` が、チャンクを push した entry の remove 後に `total_bytes` が 0 に戻り、`take_ready(i64::MAX)` が None になることを検証していること。
- 上記の陳腐化したコメントが修正され、`tests/test_decoder.rs` / `tests/test_subgroup_tracker.rs` のモジュール doc が `//!` になっていること。
- 追加・修正した各テストが、対応する実装を変えると落ちることを確認していること (PBT は実装を変えると落ちる性質になっていること)。
- `src/` の実装の挙動を変えていないこと (テストの追加・修正で実装の不足が見つかった場合は別 issue に切り出していること)。
- `cargo test --workspace` / `cargo test -p pbt` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
