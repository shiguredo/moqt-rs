# 呼び出しの無い公開 API と到達不能な検証・重複テストを削除する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/remove-dead-public-api
- Polished: {YYYY-MM-DD}

## 目的

リポジトリ内から呼ばれない公開 API、到達不能な検証、重複したテストを削除し、公開面と実装の齟齬をなくす。公開 API は互換性の約束になるため、利用実績の無いものを漫然と残さない。

## 現状

- `src/error.rs` の `MessageError::malformed_track_reason` は呼び出しが無く、`reason()` で代替できる。
- `src/object_properties.rs` の `len` / `iter` は呼び出しが無く、`as_slice()` と重複する。
- `src/message_parameter.rs` の `track_namespace_prefix` は呼び出しが無い (TRACK_NAMESPACE_PREFIX を使う request は未実装)。
- `src/track_properties.rs` の `is_known_mandatory` は引数を無視して常に false を返す。
- `src/stream/decoder.rs` の `set_subgroup_final_object` / `resolved_subgroup_id`、`src/pending_subgroup_buffer.rs` の `take_ready`、`src/subgroup_tracker.rs` の `get` はテスト専用である。
- `src/playout/delay.rs` の `earliest_arrival` は参照 0 件。`AudioDelayManager` は `JitterDelayManager` の alias で公開名が 2 つある。`AUDIO_DELAY_FEEDBACK_MAX_US` は実際には上限として効いていない。`JITTER_BUFFER_MAX_QUEUED_FRAMES` は example の `video_queue_limit` と二重定義。
- `src/message/common.rs` の `TrackNamespace::encode_to` の 4096 バイト検査は型不変条件により到達不能。`src/object_properties.rs` の `validate_immutable_inner` は `decode_kv_pairs` を呼ぶだけのラッパー。`src/name.rs` の `decode_hex_any_case` は `hex_nibble` を呼ぶだけのラッパー。`src/stream.rs` の
  `encode_control_stream_prefix` は呼び出し 1 箇所の 5 行関数。`src/stream/fetch.rs` の `decode_after_flags` の `flags >= 128` 再検査は呼び出し元で済み。
- `tests/test_parameter.rs` に同名の `missing_returns_none` が重複。`src/loc.rs` のインラインテストは `tests/test_loc.rs` と PBT に重複。`pbt/tests/prop_loc.rs` に固定単体ケースがある。

## 設計方針

- 削除する。公開 API の削除は破壊的変更として扱い、CHANGES.md の扱い (現方針では更新しない) に従う。
- テスト専用 API は `pub(crate)` に下げるか、テストを公開 API 経由に書き換えて削除する。
- 削除により重複テストも整理する。

## 完了条件

- 上記の死にコード・到達不能検証・重複テストが削除されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` が通ること。
