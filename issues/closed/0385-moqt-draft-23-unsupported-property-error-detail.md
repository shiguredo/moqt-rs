# UNSUPPORTED_EXTENSION に未対応 Property の詳細を付ける

- Created: 2026-11-03
- Completed: 2026-11-03
- Branch: develop
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §3.7、§9.4.2.2](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/track_properties.rs` の `TrackProperties::has_unknown_mandatory` は bool のみを返す。
`src/session/subscription/recv.rs` と `src/session/fetch.rs` の未知 Mandatory Track Property に対する拒否処理は、相手へ未対応の Property Type を通知する表現を持たない。

## 設計方針

Error Details 対応を前提に、Type `0x02` の UNSUPPORTED_PROPERTY を扱う。
UNSUPPORTED_EXTENSION の拒否時に未知 Mandatory Track Property の型を列挙して通知できるようにする。
複数 Detail を許し、IMMUTABLE_PROPERTIES 内の対象も調べ、GREASE は対象にしない。
任意の上流転送やリレー機能の追加は対象にしない。

## 完了条件

- 複数の未知 Mandatory Track Property について、それぞれ Type 値を持つ Detail を生成・受信できること。
- GREASE と既知 Property を未対応として報告しないこと。
- UNSUPPORTED_EXTENSION 以外に届いた Detail を、Error Details の適用規則どおり無視すること。

## 依存・関連

- 先行: [0384](0384-moqt-draft-23-request-error-details.md)
- 関連: [0402](0402-moqt-draft-23-track-status-mandatory-properties.md)

## 解決方法

- `src/message.rs` に Type 0x02 の `UnsupportedProperty` を追加した。値は Mandatory Track
  Property の Property Type (vi64) 1 つであり、Type 0x02 が偶数型のため KVP の Length を
  持たない (draft-ietf-moq-transport-23 §9.4.2.2 (UNSUPPORTED_PROPERTY) /
  §8.3 (Key-Value-Pair Structure))。`ErrorDetail` に `UnsupportedProperty` variant を
  追加し、`applies_to` は Error Code = UNSUPPORTED_EXTENSION のときだけ true を返す。
- 複数インスタンスを許す型として扱った (§9.4.2.2 (UNSUPPORTED_PROPERTY) が明示的に許可
  する)。同じ Error Detail Type が繰り返されるため 2 個目以降の Delta Type は 0 になり、
  encode の重複拒否 (REDIRECT の単一インスタンス規則) の対象外にして、decode でも重複を
  ProtocolViolation にしない。同じ Property Type の重複も型の定義が許すため拒否しない。
- `ErrorDetails` に `unsupported_properties()` を追加し、保持している Detail の
  Property Type を出現順に返す。`redirect()` は新 variant を読み飛ばして最初の REDIRECT を
  返す形に直した。decode は既存の適用規則に従い、UNSUPPORTED_EXTENSION 以外の Error Code
  では値を読み進めるだけで保持しない (draft-ietf-moq-transport-23 §9.4.2 (Error Details)
  の MUST ignore)。encode は適用外の Detail を ProtocolViolation で拒否する (0384 の
  REDIRECT と同じ方針で、decode で落ちるワイヤを送信前に防ぐ)。
- `src/track_properties.rs` に `unknown_mandatory_types()` を追加した。§3.7 の
  0x4000-0x7FFF のうち既知型 (§16.8 (Properties)) と GREASE 予約値 (§13 (Grease)) を除いた
  型を、mutable リストと IMMUTABLE_PROPERTIES の内側 (§10.7 (Immutable Properties) の
  "MUST search both") の両方から集めて昇順・重複なしで返す。同じ型が mutable と内側の
  両方に現れても Detail は 1 つで足りるため重複を除去する。`has_unknown_mandatory` は
  この列挙が空でないことに委譲し、bool 判定と列挙が同じ規則に従うことを保証した。
- `src/session/core.rs` に `emit_request_error_with_details` を追加し、既存の
  `emit_request_error` は Error Details 空でこれに委譲する形にした。適用規則の検証は
  encode 層に集約し、適用外 Detail を含むメッセージは encode で ProtocolViolation になる
  (Session 側で握りつぶさない)。
- `src/session/subscription/recv.rs` の PUBLISH 拒否 (`handle_peer_publish`) は、
  `unknown_mandatory_types()` の型ごとに UNSUPPORTED_PROPERTY Detail を 1 つ生成して
  REQUEST_ERROR (UNSUPPORTED_EXTENSION) で返す (§9.4.2.2 の SHOULD)。拒否後の購読状態・
  セッション維持・`emit_request_error` の副作用 (拒否済み Request ID の記録、request stream
  の FIN) は従来どおりである。
- SUBSCRIBE_OK / FETCH_OK の未知 Mandatory Track Property 経路
  (`handle_peer_subscribe_ok` / `handle_peer_fetch_ok`) は、§3.7 (Mandatory to Understand
  Track Properties) が cancel を求める経路であり、相手へエラーを返さない。このため付加
  できる Error Detail が無く、`has_unknown_mandatory` による cancel をそのまま維持した。
  §3.7 が relay に対して求める downstream への UNSUPPORTED_EXTENSION 伝播と §9.4.2.2 の
  relay 転送は、リレー機能を持たない本方針 (CODEBASE.md) のため追加していない。
- 受信側は decode で保持した Detail が `SessionEvent::RequestErrorReceived` の
  `error_details` にそのまま載る (適用外 Detail の除去は 0384 の `applicable` の経路を
  共有する)。アプリは `error_details.unsupported_properties()` で未対応の型を参照できる。
- ドキュメントは `docs/moqt.md` のコントロールメッセージ節と
  `skills/shiguredo-moqt/SKILL.md` の variant 表・イベント表を更新した。
  `crates/tokio-moq` は Error Details をアプリへ渡さない既存方針
  (`docs/missing-features.md` の REQUEST_ERROR 節) のままで変更していない。

## 完了条件の検証

- 複数の未知 Mandatory Track Property について、それぞれ Type 値を持つ Detail を生成・
  受信できること:
  `tests/test_session/parameter_rules.rs` の
  `publish_with_multiple_unknown_mandatory_properties_reports_each_type` が、mutable の
  0x4000 / 0x5001 と IMMUTABLE 内側の 0x6000 に対して 3 個の Detail が生成され、wire を
  経由しても同じ型列が保たれることを検証する。
  `client_receives_unsupported_property_details_for_rejected_publish` が、拒否された
  PUBLISH の publisher 側で `RequestErrorReceived` の `error_details` から型列を取得できる
  ことを検証する。codec は `tests/test_message.rs` の
  `request_error_details::unsupported_property_golden_bytes` (Length を持たない vi64 の
  ゴールデンバイト列) と `multiple_unsupported_property_details_golden_bytes` (2 個目の
  Delta Type が 0)、`duplicate_unsupported_property_detail_allowed` (同一型の重複許容)。
  PBT は `pbt/tests/prop_message.rs` の `unsupported_property_details_roundtrip` が 0〜8 個の
  Detail を往復させる。実装は `UnsupportedProperty` / `ErrorDetail::UnsupportedProperty` と
  `ErrorDetails::encode` / `decode_from` / `unsupported_properties`、および
  `handle_peer_publish` の Detail 生成。
- GREASE と既知 Property を未対応として報告しないこと:
  `tests/test_session/parameter_rules.rs` の
  `publish_with_multiple_unknown_mandatory_properties_reports_each_type` が、既知の 0x0E と
  GREASE の 0x401D を Detail に含めないことを固定する。列挙の単体テストは
  `tests/test_track_properties.rs` の
  `mandatory_properties::unknown_mandatory_types_lists_sorted_unique_types` /
  `unknown_mandatory_types_empty_when_no_unknown` /
  `mandatory_range_scan_matches_grease_exclusion` (0x4000-0x7FFF 全域で GREASE 除外と一致)
  と `immutable_properties::unknown_mandatory_types_searches_inside_immutable` /
  `unknown_mandatory_types_deduplicates_across_mutable_and_immutable` /
  `unknown_mandatory_types_survives_roundtrip`。PBT は
  `pbt/tests/prop_track_properties.rs` の
  `unknown_mandatory_types_matches_independent_filter` が、仕様の 3 条件 (範囲・GREASE 除外・
  既知型) を独立に書き下した期待値と比較する。実装は
  `TrackProperties::unknown_mandatory_types` と `is_unknown_mandatory`。
- UNSUPPORTED_EXTENSION 以外に届いた Detail を、Error Details の適用規則どおり無視すること:
  `tests/test_message.rs` の
  `request_error_details::unsupported_property_detail_for_other_error_code_is_ignored` が
  decode の非保持を、`encode_unsupported_property_for_other_error_code_is_protocol_violation`
  が送信側の拒否を検証する。Session 経路は
  `tests/test_session/request_stream.rs` の
  `server_receives_inapplicable_unsupported_property_detail_accepted` が、セッションを閉じず
  イベントの `error_details` からも落ちることを検証する。PBT は
  `pbt/tests/prop_message.rs` の `inapplicable_unsupported_property_details_are_ignored` が
  任意の非適用 Error Code と 0〜4 個の Detail の組み合わせで固定する。実装は
  `ErrorDetail::applies_to`、`ErrorDetails::decode_from` の条件付き保持と `applicable`。
- 検証コマンド: `cargo fmt --all -- --check` /
  `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace` が
  すべて成功する。
