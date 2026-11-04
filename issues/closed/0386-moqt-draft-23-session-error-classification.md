# セッション終了コードを draft 23 の具体的な分類へ移行する

- Created: 2026-11-03
- Completed: 2026-11-03
- Branch: develop
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §8.3、§8.7、§8.9、§9、§11、§12.2、§16.12.1](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/error.rs` の定数と `normalize_session_termination_code` は draft 22 のコード集合である。
`src/session/`、各 codec、`crates/tokio-moq/src/error.rs` の `session_error_code` とそれを使う `moqt_client.rs` の handshake 終了処理は、多くの異なる違反を PROTOCOL_VIOLATION にまとめている。
KEY_VALUE_FORMATTING_ERROR の旧名も公開されている。

## 設計方針

INVALID_STREAM_HEADER / MALFORMED_OBJECT / INVALID_OBJECT_STATUS / TRUNCATED_OBJECT、MALFORMED_MESSAGE / INVALID_FIELD / INVALID_TRACK_NAMESPACE / UNEXPECTED_MESSAGE / ROLE_VIOLATION /
INVALID_PARAMETER / CLOSED_CONTROL_STREAM、DELTA_ENCODING_OVERFLOW / FIELD_LENGTH_EXCEEDED を定義し、原因を失わず各受信経路から送出する。
`0x06` を MALFORMED_KEY_VALUE に改称する。
認証構造の不正は MALFORMED_AUTH_TOKEN とし、delta overflow と長さ超過が他の分類に優先する規則を実装する。
汎用 PROTOCOL_VIOLATION が明示される条件は保つ。

## 完了条件

- 定義済みコードが正規化で INTERNAL_ERROR へ落ちず、未知コードの扱いを保つこと。
- データ・制御・共有構造の各違反について wire 入力と直接 Session API 入力を検証すること。
- control stream の閉鎖、重複応答、wrong role、パラメータ違反、宣言長超過・delta overflow のコードを確認すること。
- 既存の 0311 は入れ子 Property などの分類整合性を扱う別課題として参照し、draft 23 の追加コードと対応表を本 issue で扱うこと。

## 依存・関連

- 関連: [0311](0311-draft-23-error-classification-consistency.md)

## 解決方法

### セッション終了コードの定義

`src/error.rs` に draft-ietf-moq-transport-23 §12.2 (Session Termination Codes) / §16.12.1 (Session Termination Error Codes) の Table 20 に載る全コードを定義した。

- `SESSION_KEY_VALUE_FORMATTING_ERROR` を `SESSION_MALFORMED_KEY_VALUE` (`0x6`) に改称した。
- 追加したコード: `SESSION_INVALID_STREAM_HEADER` (`0x200`) / `SESSION_MALFORMED_OBJECT` (`0x201`) / `SESSION_INVALID_OBJECT_STATUS` (`0x202`) / `SESSION_TRUNCATED_OBJECT` (`0x203`) /
  `SESSION_INVALID_TRACK_NAMESPACE` (`0x302`) / `SESSION_UNEXPECTED_MESSAGE` (`0x303`) / `SESSION_ROLE_VIOLATION` (`0x304`) / `SESSION_INVALID_PARAMETER` (`0x305`) / `SESSION_CLOSED_CONTROL_STREAM`
  (`0x306`) / `SESSION_DELTA_ENCODING_OVERFLOW` (`0x400`)。
- `KNOWN_SESSION_TERMINATION_CODES` を上記の全コードへ更新した。`normalize_session_termination_code` は定義済みコードをそのまま返し、未割り当て値と GREASE 値だけを `SESSION_INTERNAL_ERROR` に正規化する (§13 (Grease) の MUST)。
- `MessageError::session_termination_code` を追加し、codec の variant からセッション終了コードへの写像を 1 箇所に集約した。`MessageError::data_plane_session_termination_code` はデータプレーン用で、`UnexpectedEof` を MALFORMED_OBJECT に写す点だけが異なる。
- `MessageError` に `InvalidStreamHeader` / `MalformedObject` / `InvalidObjectStatus` / `InvalidTrackNamespace` / `DeltaEncodingOverflow` / `RoleViolation` を追加し、`KeyValueFormattingError` を
  `MalformedKeyValue` に改称した。`AuthorizationToken::decode` の失敗は §8.9 の MUST に従い `MalformedAuthToken` に変更した。

### 違反 → 終了コードの対応表

- KVP の Length/Value が型の定義と一致しない (未知 Type の skip 失敗を含む): 根拠 §8.3、検出 `MalformedKeyValue`、終了コード `SESSION_MALFORMED_KEY_VALUE` (`0x6`)
- Authorization Token の構造不正 (未知 Alias Type・フィールド欠落・余剰バイト): 根拠 §8.9、検出 `AuthorizationToken::decode`、終了コード `SESSION_MALFORMED_AUTH_TOKEN` (`0x16`)
- 未知のストリーム型 / データグラム型、不正な Type Flags: 根拠 §6.4.1 / §11 / §11.2.1 / §11.3.1、検出 `InvalidStreamHeader`、終了コード `SESSION_INVALID_STREAM_HEADER` (`0x200`)
- Object の直列化を復号できない (未知の FETCH Serialization Flags、prior Object 不在参照、Properties Length と実データ長の不一致): 根拠 §11.4.1 / §11.4.2.2 / §11.1.2、検出 `MalformedObject`、終了コード `SESSION_MALFORMED_OBJECT` (`0x201`)
- 未知の Object Status、非 Normal Status への Properties、Status 付き payload、zero-length Normal: 根拠 §11.1.1 / §11.1.2、検出 `validate_object_status` / `validate_properties_blob`、終了コード
  `SESSION_INVALID_OBJECT_STATUS` (`0x202`)
- シリアライズ途中の Object での FIN: 根拠 §11.3、検出 `Session::report_mid_object_fin`、終了コード `SESSION_TRUNCATED_OBJECT` (`0x203`)
- 制御メッセージを解析できない (本文長の不一致・本文の途中終端・必須 Error Detail の欠落・本体 Request ID の不一致): 根拠 §9 / §9.4.2.1、検出 `MalformedMessage` / `UnexpectedEof`、終了コード `SESSION_MALFORMED_MESSAGE` (`0x300`)
- 制御メッセージのフィールド値逸脱 (`MAX_CONTROL_MESSAGE_SIZE` の重複・型不一致・最小値未満、FETCH_OK の End < Start、client GOAWAY の非空 URI、REQUEST_OK の非空 Track Properties): 根拠 §9.1.8 / §9.3 / §9.2、検出 `InvalidField` /
  `validate_max_control_message_size`、終了コード `SESSION_INVALID_FIELD` (`0x301`)
- Track Namespace の空フィールド・33 フィールド以上: 根拠 §8.7、検出 `InvalidTrackNamespace`、終了コード `SESSION_INVALID_TRACK_NAMESPACE` (`0x302`)
- 未知のメッセージ型、制御ストリームに置けないメッセージ、順序違反、重複応答、重複 SETUP / GOAWAY、request stream 先頭に置けない型: 根拠 §6.3 / §9 / §12.2、検出 `InvalidMessageType` / `Session::recv_control` / `Session::recv_stream_message`、終了コード
  `SESSION_UNEXPECTED_MESSAGE` (`0x303`)
- peer の role では送れないメッセージ・フィールド (SETUP の DELETE / USE_ALIAS、server 側の非空 Redirect URI、publisher 側での FETCH_HEADER / SUBSCRIBE_OK / REQUEST_ERROR / PUBLISH_DONE / PUBLISH_STATE_NOTIFY 受信、subscriber からの
  REQUEST_UPDATE、TRACK_STATUS の応答側での応答受信): 根拠 §9.1.4 / §9.4.2.1 / §9.5 / §9.10 / §9.13、検出 `RoleViolation` / `Session::validate_peer_redirect`、終了コード `SESSION_ROLE_VIOLATION` (`0x304`)
- Message Parameter の未知・重複・不許可・値域外、FILL_PARAMETERS の内側スコープ違反・入れ子、LOCATION_FILTER の未知 Type: 根拠 §9.19、検出 `InvalidParameter`、終了コード `SESSION_INVALID_PARAMETER` (`0x305`)
- 制御ストリームの FIN / RESET_STREAM: 根拠 §6.3、検出 `Session::recv_control_stream_closed`、終了コード `SESSION_CLOSED_CONTROL_STREAM` (`0x306`)
- デルタ溢出 (KVP Type、Parameter Type、Subgroup Object ID、FETCH の Group / Object / Subgroup ID、LOCATION_FILTER の EndGroupDelta): 根拠 §8.3 / §9.19 / §11.3.1 / §11.4、検出 `DeltaEncodingOverflow`、終了コード
  `SESSION_DELTA_ENCODING_OVERFLOW` (`0x400`)
- 長さ超過 (KVP 値長 2^16-1、Track Namespace / Full Track Name 4,096、Reason Phrase 1,024、MAX_CONTROL_MESSAGE_SIZE 超過、GOAWAY URI 8,192、Properties Length の不一致): 根拠 §8.3 / §8.5 / §8.7 / §9 / §9.2、検出
  `FieldLengthExceeded` / `PayloadTooLong` / `ReasonPhraseTooLong`、終了コード `SESSION_FIELD_LENGTH_EXCEEDED` (`0x401`)

より具体的なコードが定められていない違反だけが `PROTOCOL_VIOLATION` (`0x3`) のままになる (例: 不正な Range Filter を REQUEST_ERROR で拒否する経路、§12.1 の Malformed Track を理由に購読だけを cancel する経路、`Session` の送信 API の誤用)。

### 優先規則の実装

draft-ietf-moq-transport-23 §12.2 の "When more than one code applies, an endpoint uses DELTA_ENCODING_OVERFLOW or FIELD_LENGTH_EXCEEDED in preference to the other codes." を、codec の検証順序で実装した。長さ超過とデルタ溢出を、構造・値域・順序の検証より先に判定する。

- `MessageDecoder::try_decode_message` は宣言長を受信上限と比較してからメッセージ型と本文を解釈する (未知の型でも長さ超過が優先される)。
- `MessageParameters::decode` / `SetupOptions::decode` / `MessageParameters::decode_value` は KVP の値長を値の解釈より先に検証する。
- `TrackNamespace::decode_from` は全フィールドを読んで総バイト長を確定させ、4,096 バイト超過をフィールド数・空フィールドより先に判定する。フィールド数の早期拒否は優先規則を破るため行わない。
- `SubgroupObject::decode` / `ObjectDatagram::decode` / `FetchStreamEntry::decode` は Properties Length と実データ長の整合を他の Object 検証より先に確認する。
- `FetchStreamDecoder` は Group / Object / Subgroup ID の delta 溢出を、Group Order の順序検証より先に判定する。
- LOCATION_FILTER の `StartGroup + EndGroupDelta` は §9.19.9 の MUST に従い `DELTA_ENCODING_OVERFLOW` とし、Range Filter の delta 溢出は §8.6 の MUST に従い REQUEST_ERROR (`INVALID_FILTER`) のままとした (セッションを閉じない文脈のため優先規則の対象外)。

### 受信経路の写像

- 制御プレーン: `Session::recv_control` (UNEXPECTED_MESSAGE)、`Session::recv_control_stream_type` (INVALID_STREAM_HEADER)、`Session::recv_control_stream_closed` (CLOSED_CONTROL_STREAM)、`Session::recv_request`
  / `Session::handle_unsupported_request` (UNEXPECTED_MESSAGE / MALFORMED_MESSAGE / INVALID_PARAMETER / INVALID_TRACK_NAMESPACE / DELTA_ENCODING_OVERFLOW /
  FIELD_LENGTH_EXCEEDED)、`Session::recv_stream_message` (UNEXPECTED_MESSAGE / ROLE_VIOLATION)、`Session::handle_peer_request_ok` (INVALID_FIELD / INVALID_PARAMETER /
  UNEXPECTED_MESSAGE)、`Session::handle_peer_setup` (UNEXPECTED_MESSAGE / ROLE_VIOLATION)、`Session::validate_peer_redirect` (MALFORMED_MESSAGE / ROLE_VIOLATION) を更新した。
- データプレーン: `Session::recv_data_stream_type` / `Session::recv_subgroup_header` / `Session::recv_subgroup_object` / `Session::recv_fetch_header` / `Session::recv_datagram` /
  `Session::recv_object_datagram` の peer 起因の違反を INVALID_STREAM_HEADER / MALFORMED_OBJECT / INVALID_OBJECT_STATUS / UNEXPECTED_MESSAGE / ROLE_VIOLATION に分類し、`Session::report_mid_object_fin` を
  TRUNCATED_OBJECT に変更した。`session_error_from_data_message` は `MessageError::data_plane_session_termination_code` を使って原因を保つ。
- 共有構造: `src/message/common.rs` (Track Namespace)、`src/kvp.rs` (Type Delta)、`src/message_parameter.rs` (Parameter / Authorization Token / FILL_PARAMETERS / LOCATION_FILTER)、`src/parameter.rs` (Setup
  Options)、`src/message.rs` (Error Details の 4,096 バイト境界)、`src/object_properties.rs` / `src/track_properties.rs` / `src/stream.rs` / `src/stream/*.rs` (Properties と Object の直列化) を更新した。
- I/O 層: `crates/tokio-moq/src/error.rs` の `session_error_code` を `MessageError::session_termination_code` への委譲に変更した。`crates/tokio-moq/src/moqt_client.rs` の SETUP
  交換中の終了処理は同関数を使うため、そのまま新しい分類に追随する。`crates/moq-sub/src/pipeline.rs` の LOC Properties の終了依頼は `data_plane_session_termination_code` を使うよう変更した。
- `crates/moq-pub` / `crates/moq-sub` の encode 失敗経路は同じ `session_error_code` を通るため、追加の変更は不要だった。

### 別課題との切り分け

`ObjectProperties::decode_exact` は失敗要因を `ProtocolViolation` に畳むのをやめ、`decode` の原因を保ったまま返すようにした。これにより Object scope の必須 Track Property は MalformedTrack、Properties Length の不一致は MALFORMED_OBJECT
として観測できる。入れ子 IMMUTABLE_PROPERTIES と重複 Property の分類整合性は 0311 の範囲として据え置き、本 issue では draft-23 の追加コードと対応表だけを扱った。

### ドキュメント

`docs/moqt.md` にセッション終了コードの分類表を追加し、draft-23 のコードへ記述を更新した。`skills/shiguredo-moqt/SKILL.md` の節参照を draft-23 に更新した。

### 追加・更新したテスト

- `tests/test_session/error_codes.rs` (新規): wire 入力 9 件と Session API 入力 12 件の終了コード検証。
- `pbt/tests/prop_error.rs` (新規): 正規化の性質 3 件。
- `pbt/tests/prop_session/error_codes.rs` (新規): Object Status と Subgroup Header Type Flags の全数分類 2 件。
- `tests/test_error.rs`: Table 20 の全コード値、全コードの正規化恒等性、未割り当て値の INTERNAL_ERROR 正規化を追加。
- 既存テストの期待値を新しい分類へ更新した (`tests/test_message.rs` / `tests/test_message_parameter.rs` / `tests/test_parameter.rs` / `tests/test_stream/*.rs` / `tests/test_session/*` / `crates/tokio-moq/src/peer_harness.rs` / `crates/moq-sub/src/pipeline.rs`)。

## 完了条件の検証

- 定義済みコードが正規化で INTERNAL_ERROR へ落ちず、未知コードの扱いを保つこと。
  - `tests/test_error.rs` の `session_termination_codes_match_iana_registry` が §16.12.1 の全コード値を検証する。
  - `tests/test_error.rs` の `unknown_error_code_normalization::all_defined_session_codes_are_known`
    が全定義済みコードの正規化恒等性を、`unknown_error_code_normalization::unassigned_session_codes_are_normalized_to_internal_error` と `unknown_error_code_normalization::grease_codes_are_normalized_to_internal_error`
    が未知コードの INTERNAL_ERROR 正規化を検証する。
  - `pbt/tests/prop_error.rs` の `defined_session_codes_are_never_normalized_to_internal_error` / `normalization_result_is_identity_or_internal_error` / `unassigned_neighbors_are_normalized_to_internal_error` が任意入力で同じ性質を検証する。
- データ・制御・共有構造の各違反について wire 入力と直接 Session API 入力を検証すること。
  - `tests/test_session/error_codes.rs` の `wire_malformed_kvp_maps_to_malformed_key_value` / `wire_invalid_track_namespace_maps_to_invalid_track_namespace` /
    `wire_oversized_track_namespace_maps_to_field_length_exceeded` / `wire_unknown_message_type_maps_to_unexpected_message` / `wire_parameter_delta_overflow_maps_to_delta_encoding_overflow` /
    `wire_control_message_length_exceeded_maps_to_field_length_exceeded` / `wire_object_datagram_invalid_status_maps_to_invalid_object_status` /
    `wire_subgroup_header_invalid_flags_map_to_invalid_stream_header` / `wire_fetch_unknown_flags_map_to_malformed_object` が wire 入力を検証する。
  - `tests/test_session/error_codes.rs` の `api_parameter_scope_violation_maps_to_invalid_parameter` / `api_duplicate_response_maps_to_unexpected_message` /
    `api_request_update_from_publisher_maps_to_role_violation` / `api_closed_control_stream_maps_to_closed_control_stream` / `api_client_goaway_with_non_zero_uri_maps_to_invalid_field` /
    `api_object_datagram_invalid_status_maps_to_invalid_object_status` / `api_object_datagram_properties_on_non_normal_status_maps_to_invalid_object_status` /
    `api_unknown_datagram_type_maps_to_invalid_stream_header` / `api_truncated_datagram_maps_to_malformed_object` / `api_fetch_header_for_unknown_request_maps_to_unexpected_message` /
    `api_mid_object_fin_maps_to_truncated_object` / `api_invalid_track_namespace_maps_to_invalid_track_namespace` が decode 済みメッセージを Session API へ直接渡す経路を検証する。
  - 共有構造の codec 単体の分類は `tests/test_message.rs` / `tests/test_message_parameter.rs` / `tests/test_parameter.rs` / `tests/test_kvp.rs` / `tests/test_object_properties.rs` / `tests/test_track_properties.rs` / `tests/test_stream/*.rs` / `pbt/tests/prop_session/error_codes.rs` で検証する。
- control stream の閉鎖、重複応答、wrong role、パラメータ違反、宣言長超過・delta overflow のコードを確認すること。
  - control stream の閉鎖: `tests/test_session/error_codes.rs` の `api_closed_control_stream_maps_to_closed_control_stream`、`tests/test_session/setup.rs` の `control_stream_fin_closes_established_session`
    / `control_stream_reset_closes_established_session` / `control_stream_close_in_local_setup_sent_closes_session`。
  - 重複応答: `tests/test_session/error_codes.rs` の `api_duplicate_response_maps_to_unexpected_message`、`tests/test_session/fetch/after_fin.rs` の `fetch_ok_twice_before_fin_fails_session` /
    `fetch_ok_twice_after_fin_fails_session`、`tests/test_session/goaway.rs` の `goaway_on_request_stream_duplicate_receive_closes_session`。
  - wrong role: `tests/test_session/error_codes.rs` の `api_request_update_from_publisher_maps_to_role_violation`、`tests/test_session/subscription/publish_state_notify.rs` の
    `notify_received_on_publisher_side_closes_session`、`tests/test_session/track_status.rs` の `responder_receiving_request_ok_closes_session` /
    `responder_receiving_request_error_closes_session`、`tests/test_session/request_stream.rs` の `server_receives_redirect_nonzero_connect_uri_closes_session`。
  - パラメータ違反: `tests/test_session/error_codes.rs` の `api_parameter_scope_violation_maps_to_invalid_parameter`、`tests/test_session/parameter_rules.rs` の
    `recv_request_with_disallowed_parameter_closes_session` / `recv_request_duplicate_fill_parameters_closes_session` / `recv_request_fill_parameters_inner_scope_violation_closes_session`。
  - 宣言長超過: `tests/test_session/error_codes.rs` の `wire_control_message_length_exceeded_maps_to_field_length_exceeded` /
    `wire_oversized_track_namespace_maps_to_field_length_exceeded`、`tests/test_session/request_stream.rs` の `unsupported_request_kvp_length_errors_close_with_mapped_code`。
  - delta overflow: `tests/test_session/error_codes.rs` の `wire_parameter_delta_overflow_maps_to_delta_encoding_overflow`、`tests/test_stream/decoder.rs` の
    `test_subgroup_rejects_object_id_delta_overflow` / `test_fetch_rejects_group_id_delta_overflow_in_ascending_mode` / `test_fetch_rejects_object_id_delta_overflow` /
    `test_fetch_rejects_subgroup_id_previous_plus_one_overflow`、`tests/test_message_parameter.rs` の `location_filter_update::overflow_is_err`。
- 0311 を別課題として参照し、draft 23 の追加コードと対応表を本 issue で扱うこと。
  - 対応表は上記「違反 → 終了コードの対応表」に記載した。入れ子 IMMUTABLE_PROPERTIES と重複 Property の分類整合性は 0311 の範囲として据え置き、本 issue では draft-23 の追加コードの定義・正規化・受信経路の写像・テストを扱った。
- `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace` がすべて成功することを確認した。
