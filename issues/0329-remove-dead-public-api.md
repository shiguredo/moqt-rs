# 不要な公開 API、到達不能な検証、重複したテスト、薄いラッパーと固定値判定を削除する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/remove-dead-public-api
- Polished: 2026-10-20

## 目的

リポジトリ内で利用実績が無い、または重複している公開 API、到達不能な検証、重複したテスト、実体のない薄いラッパー、常に同じ値を返す判定を削除し、公開面と実装の齟齬をなくす。公開 API は互換性の約束になるため、利用実績の無いものや同じ意味の別名を漫然と残さない。private の薄いラッパーも、実体が 1 行なら呼び出し側に畳んで間接層を残さない。

## 現状

- `src/error.rs` の `MessageError::malformed_track_reason` は、`src/`, `tests/`, `examples/` のどこからも呼ばれていない。`MalformedTrack` の説明文は `reason()` でも取れるため、variant を判別する必要がなければ `reason()` で代替できる。
- `src/object_properties.rs` の `ObjectProperties::len` / `iter` は、`src/`, `tests/`, `pbt/`, `examples/` のどこからも呼ばれていない。`as_slice()` が公開されているため、`as_slice().len()` / `as_slice().iter()` で代替できる。
- `src/message_parameter.rs` の `MessageParameters::track_namespace_prefix` は、`src/`, `tests/`, `examples/` のどこからも呼ばれていない。`PARAM_TRACK_NAMESPACE_PREFIX` (type 0x34) を設定する request は未実装である。
  未対応 request (`PUBLISH_NAMESPACE` / `SUBSCRIBE_NAMESPACE` / `SUBSCRIBE_TRACKS`) の本体は `src/session/core.rs` の `decode_unsupported_request_body` が Track Namespace Prefix フィールドを `TrackNamespace::decode_from` で読み飛ばすだけで、getter は通らない。
- `src/track_properties.rs` の private な `is_known_mandatory` は引数を無視して常に `false` を返す。`has_unknown_mandatory` の判定からこの呼び出しを外しても、draft-ietf-moq-transport-22 時点では 0x4000-0x7FFF に既知プロパティが無いため挙動は変わらない。`tests/test_track_properties.rs` のコメントは `is_known_mandatory` を名指ししているので、削除に合わせて更新する。
- `src/stream/decoder.rs` の `SubgroupStreamDecoder::resolved_subgroup_id` と `FetchStreamDecoder::set_subgroup_final_object`、`src/pending_subgroup_buffer.rs` の `PendingSubgroupBuffer::take_ready`、`src/subgroup_tracker.rs` の `SubgroupTracker::get` は、`src/` と `examples/` から呼ばれず、テストと PBT だけが使っている。
  - `set_subgroup_final_object` が設定する `FetchValidationState::subgroup_final_objects` は、この setter 以外に設定経路が無い。setter を削除すると、`FetchValidationState` の最終 Object 検証と prune も到達不能になる。
  - `take_ready` は `take_ready_for` と並ぶ公開 API だが、`examples/moq-sub` は `take_ready_for` だけを使っている。`src/pending_subgroup_buffer.rs` のモジュール doc と各メソッドの doc は `take_ready` を前提に書かれている。
  - `get` は `pbt/tests/prop_subgroup_tracker.rs` のモデル検査だけが使っている。`pub(crate)` に下げると PBT からは見えない。
  - `resolved_subgroup_id` は getter だけがテスト専用で、フィールドは `SubgroupStreamDecoder` の内部で使い続ける。
- `src/playout/delay.rs` の `JitterDelayManager::earliest_arrival` は、`src/`, `tests/`, `examples/` のどこからも呼ばれていない。
  - `AudioDelayManager` は `pub type AudioDelayManager = JitterDelayManager;` の alias で、`src/playout/timeline.rs`、`tests/test_playout/delay.rs`、`pbt/tests/prop_playout/delay.rs`、`skills/shiguredo-moqt/SKILL.md` が alias 名を使っている。
  - `AUDIO_DELAY_FEEDBACK_MAX_US` (300 ms) は `AudioDelayFeedback::upper_us` で `applied_ceiling_us()` と `min` される。`applied_ceiling_us()` は 100 ms (`AUDIO_DELAY_FEEDBACK_MAX_TARGET_US`) から 160 ms (`AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US`) の範囲に収まるため、300 ms は常に緩く、実際の上限として効いていない。
    `src/playout/feedback.rs` の doc は「絶対の上限として残す」と書いているが、`applied_ceiling_us()` が 160 ms 以下であるため、その説明は実装と合っていない。
  - `JITTER_BUFFER_MAX_QUEUED_FRAMES` (24) は `src/playout/buffer.rs` の定数で、`TimelineConfig::video_queue_limit` (既定 24) と同じ値を別に持っている。example の `examples/moq-sub/src/main.rs` は `timeline.config().video_queue_limit` を `PlayoutBuffer::new` に渡しており、`JITTER_BUFFER_MAX_QUEUED_FRAMES` はテストと PBT だけが使っている。
- `src/message/common.rs` の `TrackNamespace::encode_to` の 4096 バイト検査は、`TrackNamespace::new` と `TrackNamespace::decode_from` が同じ `MAX_TRACK_NAME_LENGTH` を検査し、`fields()` は不変参照だけを返すため、型の不変条件により到達不能である。
  - `src/stream/fetch.rs` の `FetchStreamObject::decode_after_flags` の `flags >= 128` 再検査は、唯一の呼び出し元 `FetchStreamEntry::decode_with_properties` が同じ検査を済ませているため到達不能である。関数 doc は「将来の単独利用に備えた防御」と書いているが、現在の呼び出し経路では通らない。
- `tests/test_parameter.rs` の `missing_returns_none` は、`mod` 内とトップレベルに同名で重複している。
  - `src/loc.rs` のインラインテストのうち、`tests/test_loc.rs` と重複するのは `even_id_with_bytes_error` / `odd_id_with_varint_error` / `timestamp_max_accepted` / `audio_level_over_8bit_rejected_on_encode` / `video_frame_marking_empty_rejected_on_encode` / `video_frame_marking_5_bytes_rejected_on_encode` である。
  - `audio_config_roundtrip`、`timestamp_max_accepted_on_decode`、`audio_level_max_accepted`、`audio_level_over_8bit_rejected_on_decode`、`video_frame_marking_empty_rejected_on_decode`、`video_frame_marking_5_bytes_rejected_on_decode` は `tests/test_loc.rs` にも `pbt/tests/prop_loc.rs` にも無い。
  - `duplicate_property_id_rejected_on_encode` / `_on_decode`、`video_frame_marking_declared_len_out_of_range_beats_truncation`、`video_frame_marking_truncated_within_valid_declared_len` も `tests/test_loc.rs` にも `pbt/tests/prop_loc.rs` にも無い。
  - `all_known_properties_roundtrip` は PBT のランダム roundtrip と重なるが、全既知プロパティを同時に含む固定ケースは PBT では確率的にしか踏まないため残す。`video_frame_marking_roundtrip` は `tests/test_loc.rs` の 1 バイトケースより広い 1-4 バイトを固定しており残す。
  - `pbt/tests/prop_loc.rs` は roundtrip と accessor をランダム入力で検証し、空の encode だけは固定ケースを持つ。エラーケースは固定していない。

## 設計方針

- 削除する。公開 API の削除は破壊的変更として扱い、CHANGES.md の扱い (現方針では更新しない) に従う。`skills/shiguredo-moqt/SKILL.md` は利用者向けの公開 API 一覧なので、削除・改名に追随させる。`docs/` と `README.md` に該当する記述は現状無いが、実装時に `rg` で確認する。
- テスト専用 API は、`pub(crate)` に下げて `src` の `#[cfg(test)]` へテストを移すか、テストごと削除する。統合テスト / PBT から `pub(crate)` へはアクセスできないため、可視性を下げるだけにしない。
- `resolved_subgroup_id`: getter を `pub(crate)` に下げ、公開 API からは消す。フィールドは内部で使い続ける。`DecodedSubgroupObject` は `subgroup_id` を公開しないため、getter を見ている `tests/test_stream/decoder.rs` のテストは `src/stream/decoder.rs` の `#[cfg(test)]` へ移す。
- `set_subgroup_final_object`: setter と `FetchValidationState::subgroup_final_objects` フィールド、その検証・prune を削除する。`tests/test_stream/decoder.rs` の `test_fetch_rejects_object_beyond_known_final_subgroup_object` は削除する。最終 Object の検証は production の経路が無いため、残すと dead code になる。
- `take_ready`: 削除し、モジュール doc と各メソッドの doc を `take_ready_for` 中心に書き換える。`tests/test_pending_subgroup_buffer.rs` と `pbt/tests/prop_pending_subgroup_buffer.rs` の `take_ready` 専用ケースは削除し、`take_ready_for` で同じ性質を検証できるものだけ書き換える。
  PBT の `Command::Take` とモデルの `take_ready` も削除する。バッファ全体の引き取り順序とタイムアウト確定の一部は `take_ready_for` では表現できないため、その検証は失われる。
  `examples/moq-sub/src/pipeline.rs` のコメントも追随させる。
- `get`: `pub(crate)` に下げ、`pbt/tests/prop_subgroup_tracker.rs` の `get` を使うモデル照合は削除する。モデル検査は `src/subgroup_tracker.rs` の `#[cfg(test)]` へ移すが、`noprop` は root の dev-dependency ではないため、固定の操作列で書く。ランダム操作列による検出力は落ちるが、テスト専用の公開 API を残さない方を優先する。
- `AudioDelayManager`: alias を削除し、`JitterDelayManager` に統一する。`src/playout/timeline.rs`、`tests/test_playout/delay.rs`、`pbt/tests/prop_playout/delay.rs`、`skills/shiguredo-moqt/SKILL.md` を置き換える。
- `AUDIO_DELAY_FEEDBACK_MAX_US`: 削除し、`AudioDelayFeedback::upper_us` を
  `self.ceiling_us.unwrap_or(i64::MAX).min(self.applied_ceiling_us())` に置き換える。
  `tests/test_playout/feedback.rs` の `target_stays_between_the_floor_and_the_ceiling` にある `AUDIO_DELAY_FEEDBACK_MAX_US` を使う表明は削除する (直前の `applied_ceiling_us` の表明で 100 ms に固定済み)。
  `the_adaptive_ceiling_constants_are_fixed` の `AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US < AUDIO_DELAY_FEEDBACK_MAX_US` の表明は削除し、`AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US > AUDIO_DELAY_FEEDBACK_MAX_TARGET_US` の表明は残す。
  `pbt/tests/prop_playout/feedback.rs` の `target_limits` と境界値サンプラーも `ceiling_us.unwrap_or(i64::MAX).min(applied_ceiling_us)` と `AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US` を上限に置き換える。
  `src/playout/feedback.rs` の「絶対の上限として残す」という doc も削除する。
- `JITTER_BUFFER_MAX_QUEUED_FRAMES`: 削除し、テストと PBT は `TimelineConfig::default().video_queue_limit` または `TimelineConfig::video_queue_limit` を使う。`skills/shiguredo-moqt/SKILL.md` の記述も追随させる。
- `TrackNamespace::encode_to`: 4096 バイト検査を削除する。検査を外すと関数は常に `Ok(())` を返すため、戻り値を `()` に変える。
  `src/message.rs` の `encode_request_id_and_namespace` は末尾で `Ok(())` を返し、`Redirect::encode_to` と `Fetch::encode_message_body`、`src/message_parameter.rs` の `encode_value` から `?` を外す。
  `TrackNamespace::new` / `decode_from` の検査は残す。
- `decode_after_flags`: `flags >= 128` の再検査を削除し、関数 doc の「将来の単独利用に備えた防御」という記述も削除する。呼び出し元の検査は残す。
- 薄いラッパー: `validate_immutable_inner` は `decode_kv_pairs(bytes, false)?` を呼び出し側へ直接書く。`decode_hex_any_case` は `hex_nibble` を呼び出し側へ直接書き、RFC 3986 §2.1 の大文字小文字の根拠コメントを呼び出し側へ移す。`encode_control_stream_prefix` は `encode_control_stream_setup` の中へ畳む。
- `is_known_mandatory`: 削除し、`has_unknown_mandatory` の判定を `(MANDATORY_TRACK_PROPERTY_MIN..=MANDATORY_TRACK_PROPERTY_MAX).contains(&prop_type) && !crate::grease::is_grease(prop_type)` にする。`tests/test_track_properties.rs` のコメントを更新する。
- `malformed_track_reason`, `ObjectProperties::len` / `iter`, `track_namespace_prefix`, `earliest_arrival`: 削除する。
- 重複テスト: `tests/test_parameter.rs` の `missing_returns_none` はトップレベルの重複を削除する。`src/loc.rs` は上記の重複 6 本だけを削除し、それ以外のインラインテストは残す。`pbt/tests/prop_loc.rs` は変更しない。
- `CHANGES.md` は更新しない。`skills/shiguredo-moqt/SKILL.md` は更新する。

## 完了条件

- 上記の各項目が方針どおりに処理され、`src/` の `pub` に削除対象のシンボルが残っていないこと (`rg` で確認)。`resolved_subgroup_id` / `get` は `pub(crate)` として残る。
- `skills/shiguredo-moqt/SKILL.md` から削除・改名した公開 API の記述が消え、残存する API の記述が実装と一致していること。
- `src/loc.rs` の重複でないインラインテスト、`tests/test_loc.rs` のテスト、`pbt/tests/prop_loc.rs` が残っていること。
- `take_ready` / `get` / `set_subgroup_final_object` のテストの整理が、それぞれ `take_ready_for`、`src` の単体テスト、最終 Object 検証の削除と対応していること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` が通ること。
- ソースコードに issue 番号を書かないこと。コメントは日本語、ログは英語、テストのログは日本語であること。
