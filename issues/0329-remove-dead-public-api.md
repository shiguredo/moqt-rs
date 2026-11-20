# 不要な公開 API、到達不能な検証、重複したテスト、薄いラッパーと固定値判定を削除する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/remove-dead-public-api
- Polished: 2026-11-20
- Refreshed: 2026-11-04

## 目的

リポジトリ内で利用実績が無い、または重複している公開 API、到達不能な検証、重複したテスト、実体のない薄いラッパー、常に同じ値を返す判定を削除し、公開面と実装の齟齬をなくす。公開 API は互換性の約束になるため、利用実績の無いものや同じ意味の別名を漫然と残さない。private の薄いラッパーも、実体が 1 行なら呼び出し側に畳んで間接層を残さない。

## 現状

- `src/error.rs` の `MessageError::malformed_track_reason` は、`src/`, `tests/`, `pbt/`, `crates/` のどこからも呼ばれていない。`MalformedTrack` の説明文は `reason()` でも取れるため、variant を判別する必要がなければ `reason()` で代替できる。
- `src/object_properties.rs` の `ObjectProperties::len` / `iter` は、`src/`, `tests/`, `pbt/`, `crates/` のどこからも呼ばれていない。`as_slice()` が公開されているため、`as_slice().len()` / `as_slice().iter()` で代替できる。
- `src/message_parameter.rs` の `MessageParameters::track_namespace_prefix` は、
  `src/session/namespace.rs` の `send_update_for_namespace_subscription` /
  `send_update_for_track_subscription` と `handle_update_for_namespace_subscription` /
  `handle_update_for_track_subscription` が使っている。以前は未対応 request の読み飛ばし経路しか
  無かったが、PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS の実装後に
  production の経路ができたため、削除対象ではない。
- `src/track_properties.rs` の private な `is_known_mandatory` は引数を無視して常に `false` を返す。
  `is_unknown_mandatory` の判定からこの呼び出しを外しても、draft-ietf-moq-transport-23 §3.7 時点では
  0x4000-0x7FFF に既知プロパティが無いため挙動は変わらない。
  `tests/test_track_properties.rs` のコメントは `is_known_mandatory` を名指ししているので、
  削除に合わせて更新する。
- `src/stream/decoder.rs` の `SubgroupStreamDecoder::resolved_subgroup_id`、
  `FetchStreamDecoder::set_subgroup_final_object`、`src/pending_subgroup_buffer.rs` の
  `PendingSubgroupBuffer::take_ready`、`src/subgroup_tracker.rs` の `SubgroupTracker::get` は、
  production の `src/` と `crates/` からは呼ばれず、テストと PBT だけが使っている。
  - `resolved_subgroup_id` は `tests/test_stream/decoder.rs` と `crates/moq-pub/src/pipeline.rs` の
    `recv_subgroup_with_subgroup_id` (テスト用ヘルパー) が使っている。フィールドは
    `SubgroupStreamDecoder` の内部で使い続ける。
  - `set_subgroup_final_object` が設定する `FetchValidationState::subgroup_final_objects` は、
    この setter 以外に設定経路が無い。setter を削除すると、最終 Object 検証と prune も到達不能になる。
    `subgroup_max_objects` も setter の整合性検査でしか読まれない。
  - `take_ready` は `take_ready_for` と並ぶ公開 API だが、`crates/moq-sub` は `take_ready_for` だけを
    使っている。`src/pending_subgroup_buffer.rs` のモジュール doc と各メソッドの doc は `take_ready` を
    前提に書かれている。
  - `get` は `tests/test_subgroup_tracker.rs`、`src/session/tests.rs`、
    `pbt/tests/prop_subgroup_tracker.rs` が使っている。production の `src/` と `crates/` からは呼ばれない。
- `src/playout/buffer.rs` の `JITTER_BUFFER_MAX_QUEUED_FRAMES` (24) は、`TimelineConfig::video_queue_limit`
  (既定 24) と同じ値を別に持っている。コードではテストと PBT だけが使い、`crates/moq-sub/src/main.rs` は
  `timeline.config().video_queue_limit` を `PlayoutBuffer::new` に渡している。`README.md` と
  `skills/shiguredo-moqt/SKILL.md` にも記述がある。
- `src/message/common.rs` の `TrackNamespace::encode_to` の 4096 バイト検査は、`TrackNamespace::new` と
  `TrackNamespace::decode_from` が同じ `MAX_TRACK_NAME_LENGTH` を検査し、`fields()` は不変参照だけを返すため、
  型の不変条件により到達不能である。
  - `src/stream/fetch.rs` の `FetchStreamObject::decode_after_flags` の `flags >= 128` 再検査は、唯一の
    呼び出し元 `FetchStreamEntry::decode_with_properties` が同じ検査を済ませているため到達不能である。
    関数 doc は「将来の単独利用に備えた防御」と書いているが、現在の呼び出し経路では通らない。
- `tests/test_parameter.rs` の `missing_returns_none` は、`mod` 内とトップレベルに同名で重複している。
  - `src/loc.rs` のインラインテストのうち、`tests/test_loc.rs` と実質重複するのは次の 6 本である
    (`tests/test_loc.rs` 側の対応するテスト名を括弧に示す):
    `even_id_with_bytes_error` (`even_id_with_bytes_value_error`) /
    `odd_id_with_varint_error` (`odd_id_with_varint_value_error`) /
    `timestamp_max_accepted` (`timestamp_u64_max_roundtrip`) /
    `audio_level_over_8bit_rejected_on_encode` (`audio_level_exceeds_range_returns_key_value_formatting_error`) /
    `video_frame_marking_empty_rejected_on_encode` (`video_frame_marking_empty_returns_key_value_formatting_error`) /
    `video_frame_marking_5_bytes_rejected_on_encode` (`video_frame_marking_5_bytes_returns_key_value_formatting_error`)。
  - `audio_config_roundtrip`、`timestamp_max_accepted_on_decode`、`audio_level_max_accepted`、
    `audio_level_over_8bit_rejected_on_decode`、`video_frame_marking_empty_rejected_on_decode`、
    `video_frame_marking_5_bytes_rejected_on_decode` は `tests/test_loc.rs` にも `pbt/tests/prop_loc.rs` にも無い。
  - `duplicate_property_id_rejected_on_encode` / `_on_decode`、
    `video_frame_marking_declared_len_out_of_range_beats_truncation`、
    `video_frame_marking_truncated_within_valid_declared_len` も `tests/test_loc.rs` にも
    `pbt/tests/prop_loc.rs` にも無い。
  - `all_known_properties_roundtrip` は PBT のランダム roundtrip と重なるが、全既知プロパティを同時に含む
    固定ケースは PBT では確率的にしか踏まないため残す。`video_frame_marking_roundtrip` は
    `tests/test_loc.rs` の 1 バイトケースより広い 1-4 バイトを固定しており残す。
  - `pbt/tests/prop_loc.rs` は roundtrip と accessor をランダム入力で検証し、空の encode だけは固定ケースを持つ。
    エラーケースは固定していない。

## 設計方針

- 削除する。公開 API の削除は破壊的変更として扱い、CHANGES.md の扱い (現方針では更新しない) に従う。
  `skills/shiguredo-moqt/SKILL.md` は利用者向けの公開 API 一覧なので、削除・改名に追随させる。
  `README.md` の `JITTER_BUFFER_MAX_QUEUED_FRAMES` の例も追随させる。
  `docs/` には該当する記述は無い (実装時に `rg` で確認する)。
- テスト専用 API は、`pub(crate)` に下げて `src` の `#[cfg(test)]` へテストを移すか、テストごと削除する。
  統合テスト / PBT / 別 crate のテストから `pub(crate)` へはアクセスできないため、可視性を下げるだけにしない。
- `resolved_subgroup_id`: getter を `pub(crate)` に下げ、公開 API からは消す。フィールドは内部で使い続ける。
  `DecodedSubgroupObject` は `subgroup_id` を公開しないため、getter を見ている `tests/test_stream/decoder.rs` の
  テストは `src/stream/decoder.rs` の `#[cfg(test)]` へ移す。
  `crates/moq-pub/src/pipeline.rs` の `recv_subgroup_with_subgroup_id` は `header.subgroup_id` と
  `objects[0].object_id` から同じ値を導くように変更する (`SubgroupIdMode::Zero` は `Some(0)`、
  `Explicit(id)` は `Some(id)`、`FirstObjectId` は最初の Object ID)。
- `set_subgroup_final_object`: setter と `FetchValidationState::subgroup_final_objects` フィールド、
  その検証・prune、setter の整合性検査でしか読まれない `subgroup_max_objects` フィールドとその記録・prune を
  削除する。`tests/test_stream/decoder.rs` の
  `test_fetch_rejects_object_beyond_known_final_subgroup_object` は削除する。
  最終 Object の検証は production の経路が無いため、残すと dead code になる。
- `take_ready`: 削除し、モジュール doc と各メソッドの doc を `take_ready_for` 中心に書き換える。
  `tests/test_pending_subgroup_buffer.rs` と `pbt/tests/prop_pending_subgroup_buffer.rs` の `take_ready` 専用ケースは
  削除し、`take_ready_for` で同じ性質を検証できるものだけ書き換える。
  PBT の `Command::Take` とモデルの `take_ready` も削除する。
  バッファ全体の引き取り順序とタイムアウト確定の一部は `take_ready_for` では表現できないため、その検証は失われる。
  `crates/moq-sub/src/pipeline.rs` のコメントも追随させる。
- `get`: `pub(crate)` に下げる。`tests/test_subgroup_tracker.rs` は `src/subgroup_tracker.rs` の
  `#[cfg(test)]` へ移す。`pbt/tests/prop_subgroup_tracker.rs` のモデル検査も `src/subgroup_tracker.rs` の
  `#[cfg(test)]` へ移すが、`noprop` は root の dev-dependency ではないため、固定の操作列で書く。
  ランダム操作列による検出力は落ちるが、テスト専用の公開 API を残さない方を優先する。
  `src/session/tests.rs` は crate 内のテストなので `pub(crate)` のまま使える。
- `JITTER_BUFFER_MAX_QUEUED_FRAMES`: 削除し、テストと PBT は `TimelineConfig::default().video_queue_limit` または
  `TimelineConfig::video_queue_limit` を使う。`README.md` の例と `skills/shiguredo-moqt/SKILL.md` の記述も追随させる。
- `TrackNamespace::encode_to`: 4096 バイト検査を削除する。検査を外すと関数は常に `Ok(())` を返すため、
  戻り値を `()` に変える。`src/message.rs` の `encode_request_id_and_namespace` は末尾で `Ok(())` を返し、
  `Redirect::encode_to`、`Fetch::encode_message_body`、`Namespace::encode_message_body`、
  `NamespaceDone::encode_message_body`、`PublishSkipped::encode_message_body`、
  `src/message_parameter.rs` の `encode_value` から `?` を外す。
  `NamespaceDone::encode_message_body` は `encode_to` の後に `Ok(())` を返す。
  `TrackNamespace::new` / `decode_from` の検査は残す。
- `decode_after_flags`: `flags >= 128` の再検査を削除し、関数 doc の「将来の単独利用に備えた防御」という記述も
  削除する。呼び出し元の検査は残す。
- 薄いラッパー: `validate_immutable_inner` は `decode_kv_pairs(bytes, false, context)?` を呼び出し側へ直接書く。
  `decode_hex_any_case` は `hex_nibble` を呼び出し側へ直接書き、RFC 3986 §2.1 の大文字小文字の根拠コメントを
  呼び出し側へ移す。`encode_control_stream_prefix` は `encode_control_stream_setup` の中へ畳む。
- `is_known_mandatory`: 削除し、`is_unknown_mandatory` から `Self::is_known_mandatory` の呼び出しを外して
  `(MANDATORY_TRACK_PROPERTY_MIN..=MANDATORY_TRACK_PROPERTY_MAX).contains(&prop_type) && !crate::grease::is_grease(prop_type)`
  にする。これにより `unknown_mandatory_types` と
  `has_unknown_mandatory` の判定は変わらない。`tests/test_track_properties.rs` のコメントを更新する。
- `malformed_track_reason`, `ObjectProperties::len` / `iter`: 削除する。`track_namespace_prefix` は
  `src/session/namespace.rs` で使われているため残す。`earliest_arrival` は `src/playout/delay.rs` の削除に伴い
  既に無い。
- 重複テスト: `tests/test_parameter.rs` の `missing_returns_none` はトップレベルの重複を削除する。
  `src/loc.rs` は上記の重複 6 本だけを削除し、それ以外のインラインテストは残す。
  `pbt/tests/prop_loc.rs` は変更しない。
- `CHANGES.md` は更新しない。`skills/shiguredo-moqt/SKILL.md` と `README.md` は削除・改名に追随させる。
- CODEBASE.md の「Pull-Request やブランチを作らず develop にコミットしていくこと」「1 issue 1 commit 1 push」に
  従い、`Branch:` のブランチは作らず develop に直接コミットし、1 コミットにまとめる。

## 完了条件

- 上記の各項目が方針どおりに処理され、`src/` の `pub` に削除対象のシンボルが残っていないこと (`rg` で確認)。
  `resolved_subgroup_id` / `get` は `pub(crate)` として残る。
- `skills/shiguredo-moqt/SKILL.md` と `README.md` から削除・改名した公開 API の記述が消え、
  残存する API の記述が実装と一致していること。
- `src/loc.rs` の重複でないインラインテスト、`tests/test_loc.rs` のテスト、`pbt/tests/prop_loc.rs` が
  残っていること。
- `take_ready` / `get` / `set_subgroup_final_object` / `resolved_subgroup_id` のテストの整理が、それぞれ
  `take_ready_for`、`src/subgroup_tracker.rs` の `#[cfg(test)]`、最終 Object 検証の削除、
  `src/stream/decoder.rs` の `#[cfg(test)]` と対応していること。
- macOS 上で `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。crates 配下の crate が macOS 専用依存を持つため、
  Linux では workspace 全体をビルドできない。
- 変更したファイルを指定した `prek run --files` が通ること (tombi / markdownlint を含む)。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` が通ること。
- ソースコードに issue 番号を書かないこと。コメントは日本語、ログは英語、テストのログは日本語であること。
