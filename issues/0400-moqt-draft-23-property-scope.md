# 対応済み Property のスコープ違反を malformed Track にする

- Created: 2026-11-03
- Completed: 2026-11-03
- Branch: develop
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §8.4、§12.1 条件 8](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/object_properties.rs` の `decode_kv_pairs` は Mandatory Track Property の範囲を Object で拒否するが、通常の Track-only 型 `MAX_CACHE_DURATION` / `DEFAULT_PUBLISHER_PRIORITY` / `DYNAMIC_GROUPS` などのスコープを一括検証しない。
`src/track_properties.rs` の decoder も Object-only の既知型をスコープで検証する仕組みを持たない。

## 設計方針

このライブラリが意味を理解する Property ごとに Track / Object の許可スコープを照合し、受信時の違反を MalformedTrack として返す。
IMMUTABLE_PROPERTIES の内側にも同じスコープ検証を行う。
未知 Property をスコープ推測で拒否せず、ローカル encode 入力の不正と受信 Track の検出を区別する。
Track Properties のスコープ検証は構造 decode と分離する。
`TrackProperties::decode` / `ControlMessage::decode` はスコープ違反だけを理由に失敗せず、Properties を保持したメッセージを `Session` へ渡す。
PUBLISH / SUBSCRIBE_OK / FETCH_OK / TRACK_STATUS_OK の受信ハンドラが Request ID と Full Track Name を得て検証し、`MalformedTrack` を同じ Track の subscription / FETCH の cancel・エラー通知へ変換する。
構造や直列化が壊れた KVP のエラー分類は維持する。
初回 PUBLISH では subscription 登録前でも、受信メッセージの Request ID / Full Track Name から新規 request と同じ Track の既存要求を取消対象として解決し、アプリケーションへ正常な受信として通知しない。
Object 側は `ObjectProperties::decode_exact` が `MalformedTrack` を `ProtocolViolation` へ畳まず透過させ、既存の data plane の取消経路へ渡す。関連する 0311 のエラー写像と揃える。ローカル encode のスコープ違反は入力検証エラーとし、受信 Track の取消は発生させない。

## 完了条件

- Track-only の既知型が Object に現れた場合と、対応する Object-only 型が Track に現れた場合を検証すること。
- IMMUTABLE_PROPERTIES 内も同じ規則になり、未知型と正当な両スコープ型は受理されること。
- 違反で session 全体を不必要に閉じず、既存の malformed Track の cancel・通知経路へ渡すこと。
- Track Properties を含む制御メッセージの wire 入力と直接 Session API の双方で、初回 PUBLISH も含めて Track 文脈を失わず取消へ進むこと。Object 側も `decode_exact` 経由で `MalformedTrack` が失われないこと。

## 依存・関連

- 関連: [0311](0311-draft-22-error-classification-consistency.md)
- 関連: [0401](0401-moqt-draft-23-property-duplicates.md)

## 解決方法

- スコープ表を内部モジュール `src/property_scope.rs` に集約した。draft-ietf-moq-transport-23 §16.8 (Properties) Table 15 の 9 型
  (0x02 / 0x04 / 0x06 / 0x0B / 0x0E / 0x22 / 0x30 / 0x3C / 0x3E) と、provisional 登録の Table 16 のうち本実装が `src/loc.rs` で解釈する 6 型
  (0x08 / 0x09 / 0x0C / 0x0D / 0x0F / 0x10) の Track / Object / Both を `PropertyScope` で表す。解釈しない型と GREASE 値は `None` とし、
  §8.4 の未知 Property 転送 MUST と §16.8 の "Endpoints MUST ignore unknown Property types" に従って拒否しない。`is_object_only` /
  `is_track_only` の 2 述語を `track_properties` と `object_properties` の双方から使うことで、判定表が 2 箇所に分かれないようにした。
- `src/track_properties.rs` に `TrackProperties::validate_scope` を追加した。draft-ietf-moq-transport-23 §8.4 の "An endpoint that supports a
  Property and receives it outside its scope … MUST treat the Track as malformed" / §12.1 (Malformed Tracks) 条件 8 を受け、Object 専用の
  既知 Property を mutable リストと IMMUTABLE_PROPERTIES の内側 (§10.7 (Immutable Properties) の "MUST search both") の両方から検出して
  `MalformedTrack` を返す。`TrackProperties::decode` はスコープ違反だけを理由に失敗しない (§12.1 の cancel には Request ID と Full Track Name
  が必要であり、decode 層はどちらも持たない)。`encode` はローカル入力の検証であるため `ProtocolViolation` で拒否し、受信 Track の
  malformed 判定と区別する。IMMUTABLE_PROPERTIES の内側も encode 時に同じ規則で検証する。
- `src/object_properties.rs` の `decode_kv_pairs` に Track 専用既知型 (0x04 / 0x0E / 0x22 / 0x30 と、§3.7 (Mandatory to Understand Track
  Properties) の 0x4000-0x7FFF) の検出を追加し、mutable リストと IMMUTABLE_PROPERTIES の内側の両方で `MalformedTrack` を返すようにした。
  `ObjectProperties::encode` は同じ型を `ProtocolViolation` で拒否し、内側のスコープ違反も decode の `MalformedTrack` から
  `ProtocolViolation` へ写す。`decode_exact` は `MalformedTrack` を `ProtocolViolation` へ畳まず透過する (0311 の写像と同じ)。構造・直列化の
  壊れた KVP の分類 (`MalformedKeyValue` / `FieldLengthExceeded` / `UnexpectedEof` / `ProtocolViolation` / `MalformedObject`) は変更していない。
- `src/session/data.rs` の malformed Track 終端を、Track 文脈の解決 (`malformed_track_of`)・sibling 列挙 (`terminate_malformed_track_siblings`)・
  1 件の終端 (`terminate_request_for_malformed_track`) に分けた。Track 文脈の解決に TRACK_STATUS を加え、`terminate_request_for_malformed_track`
  は TRACK_STATUS を `TrackStatusResponse::Error` として確定して cancel と `RequestTerminated { reason: MalformedTrack }` を発行する。
  初回 PUBLISH 用に `terminate_malformed_track_for_publish` を追加した。受信メッセージの Request ID と Full Track Name から同じ Track の
  既存 subscription / FETCH を sibling として cancel し、購読登録前の PUBLISH 自身には `terminate_unregistered_request_for_malformed_track`
  で STOP_SENDING / RESET_STREAM (`STREAM_MALFORMED_TRACK`) と `RequestTerminated` を発行する。`rejected_request_ids` に記録するため、
  以後の stream close は no-op で吸収される。PUBLISH を購読として登録しないため、アプリは正常な受信として観測しない。
- `src/session/subscription/recv.rs` の `handle_peer_publish` / `handle_peer_subscribe_ok`、`src/session/fetch.rs` の `handle_peer_fetch_ok`、
  `src/session/track_status.rs` の `handle_ok_for_track_status` (呼び出し元 `src/session/core.rs` の `handle_peer_request_ok` から
  TrackProperties を渡す) がスコープ検証を行う。違反はセッション終了ではなく同じ Track の subscription / FETCH の cancel と
  `RequestTerminated` へ渡す。FETCH_OK はデータストリーム FIN 後の `Terminated` でも `response_received` を立てる。
  未知 Mandatory Track Property (§3.7) の扱いを先に判定するのは、draft が両条件の優先順位を定めておらず、既存の request 拒否
  (PUBLISH は REQUEST_ERROR、SUBSCRIBE_OK / FETCH_OK は cancel) を維持するためである。
- PBT の正常系サンプラー (`pbt/src/common.rs`、`pbt/tests/prop_object_properties.rs`) から、スコープ違反になる型の生成を除外した
  (Track 側の 0x09 / 0x0C、Object 側の 0x04 / 0x0E / 0x22 / 0x30)。利用者向けドキュメント (README / docs / SKILL) の更新は、draft 23 の
  仕様参照をまとめて更新する 0404 の範囲とする。

## 完了条件の検証

- Track-only の既知型が Object に現れた場合と、対応する Object-only 型が Track に現れた場合を検証すること:
  `tests/test_object_properties.rs` の `property_scope::track_only_property_is_malformed` が 0x04 / 0x0E / 0x22 / 0x30 を、
  `tests/test_track_properties.rs` の `property_scope::validate_scope_detects_object_only_types` が 0x3C / 0x3E / 0x10 / 0x09 / 0x0C を
  `MalformedTrack` にする。`src/property_scope.rs` の単体テスト `moqt_properties_match_table_15` / `loc_properties_match_registry` /
  `unknown_properties_have_no_scope` / `grease_values_have_no_scope` が表と未知型の扱いを固定する。PBT は
  `pbt/tests/prop_track_properties.rs` の `validate_scope_matches_independent_scope_table` と `pbt/tests/prop_object_properties.rs` の
  `object_scope_rejects_track_only_property` が、仕様の表を独立に書き下した期待値と比較する。
- IMMUTABLE_PROPERTIES 内も同じ規則になり、未知型と正当な両スコープ型は受理されること:
  `tests/test_track_properties.rs` の `property_scope::validate_scope_searches_inside_immutable` /
  `encode_rejects_object_only_property_inside_immutable` / `validate_scope_accepts_track_and_both_scope_and_unknown`、
  `tests/test_object_properties.rs` の `property_scope::track_only_property_inside_immutable_is_malformed` /
  `immutable_inner_both_scope_and_unknown_accepted` / `encode_rejects_track_only_property_inside_immutable` が、内側の違反検出と、
  未知型・GREASE・両スコープ型 (0x02 / 0x06 / 0x08 / 0x0D / 0x0F) の受理を固定する。Session 経由では
  `tests/test_session/property_scope.rs` の `publish_with_object_only_property_in_immutable_is_cancelled` /
  `subgroup_object_with_track_only_property_in_immutable_terminates_subscription` が内側の違反を取消へ渡す。
- 違反で session 全体を不必要に閉じず、既存の malformed Track の cancel・通知経路へ渡すこと:
  `tests/test_session/property_scope.rs` の `publish_with_object_only_property_is_not_registered_and_is_cancelled` /
  `subscribe_ok_with_object_only_property_cancels_subscription` / `fetch_ok_with_object_only_property_cancels_fetch` /
  `track_status_ok_with_object_only_property_cancels_related_requests` が、`Established` の維持、`STREAM_MALFORMED_TRACK` による
  STOP_SENDING → RESET_STREAM と `RequestTerminated { reason: MalformedTrack }`、購読・fetch の `Terminated` を検証する。
  初回 PUBLISH の購読未登録と、同じ Track の既存 subscription の取消は
  `publish_with_object_only_property_cancels_existing_subscription_for_same_track` が固定する。Object 側は
  `subgroup_object_with_track_only_property_terminates_subscription` / `datagram_with_track_only_property_terminates_subscription` が
  data plane の `terminate_malformed_track` 経路 (data stream の reset を含む) を検証する。
- Track Properties を含む制御メッセージの wire 入力と直接 Session API の双方で、初回 PUBLISH も含めて Track 文脈を失わず取消へ進むこと。
  Object 側も `decode_exact` 経由で `MalformedTrack` が失われないこと:
  `tests/test_session/property_scope.rs` の `publish_with_object_only_property_on_wire_is_cancelled` /
  `subscribe_ok_with_object_only_property_on_wire_cancels_subscription` / `fetch_ok_with_object_only_property_on_wire_cancels_fetch` /
  `track_status_ok_with_object_only_property_on_wire_cancels_track_status` が、`ControlMessage::decode` がスコープ違反だけでは失敗せず、
  受信ハンドラが Request ID と Full Track Name から取消へ進むことを検証する。`TrackProperties::decode` の非拒否は
  `decode_keeps_object_only_property_and_validate_scope_detects_it` が固定する。`decode_exact` の透過は
  `tests/test_object_properties.rs` の `decode_exact_keeps_malformed_track` (`ObjectPropertyTracker::observe_object` 経由) と
  `tests/test_video_decode_order.rs` の `prior_object_id_gap_of_keeps_malformed_track_for_scope_violation` が検証する。
- 検証コマンド: `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace` が
  すべて成功する。
