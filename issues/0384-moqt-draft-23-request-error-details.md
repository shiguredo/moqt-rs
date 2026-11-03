# REQUEST_ERROR に構造化された Error Details を追加する

- Created: 2026-11-03
- Completed: 2026-11-03
- Branch: develop
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §9.4.1、§9.4.2、§9.4.2.1、§16.11](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/message.rs` の `RequestError` は `redirect: Option<Redirect>` を持ち、REDIRECT の場合だけ独自の構造を本文末尾へ直結する。
他のエラーコードの末尾データは長さ不一致として拒否する。
`src/session/core.rs` の `validate_peer_redirect` とエラー送信 API もこの表現を使う。

## 設計方針

Error Reason の後を Delta Type 付き KVP の Error Details 列に変更する。
REDIRECT は Type `0x01` の length-prefixed Error Detail とし、REDIRECT error では必須にする。
未知 Type と、既知だが当該 Error Code に適用しない Detail を無視する。
未知 Type の重複は許可し、既知 Type の多重可否は定義に従う。
namespace 用 REDIRECT の空 Track Name と、server の非空 Connect URI 拒否を新形式でも保つ。
公開送信 API とイベントに Detail を渡す手段を用意する。

## 完了条件

- REDIRECT の golden バイト列と、必須 Detail 欠落時の MALFORMED_MESSAGE を検証すること。
- 未知 Detail・重複未知 Detail・適用外の既知 Detail が不当にセッションを閉じないこと。
- Detail の KVP 境界・全体消費量と、REDIRECT の role / field 検証を確認すること。

## 解決方法

- `src/message.rs` の `RequestError` から `redirect: Option<Redirect>` を削除し、
  `error_details: ErrorDetails` を追加した。ワイヤ上は Error Reason の後ろからメッセージ
  末尾までを Delta Type 付き KVP 列 (Error Details) が占める
  (draft-ietf-moq-transport-23 §9.4.1 (REQUEST_ERROR Message Format) /
  §8.3 (Key-Value-Pair Structure))。
- `ErrorDetail` は Type 0x01 の REDIRECT だけを型付きで持つ。Type 0x02 の
  UNSUPPORTED_PROPERTY の型付き対応は [0385](0385-moqt-draft-23-unsupported-property-error-detail.md)
  の範囲とし、本 issue では未知 Type として読み飛ばす (受信側は未知 Type を無視する MUST
  のため、この状態でも仕様に適合する)。Error Details の値長上限 2^16-1 バイトを超える
  Length は FIELD_LENGTH_EXCEEDED で拒否する。
- REDIRECT Detail は KVP の Length が覆う範囲だけを `Redirect::decode_from` で読み、
  構造が必要とするバイト数と Length が一致しない場合 (不足・余剰) は MALFORMED_KEY_VALUE
  にした。Length の外へフィールドをはみ出させると後続 KVP の境界が壊れるため、はみ出しは
  読み進めずに拒否する。
- 未知 Type と、既知でも当該 Error Code に適用されない Type は、偶数 Type は vi64・
  奇数 Type は Length + バイト列という KVP の形式だけで読み飛ばして保持しない
  (draft-ietf-moq-transport-23 §9.4.2 (Error Details) の MUST ignore)。未知 Type の
  重複は許可する (同節の MUST allow)。既知 Type は REDIRECT の単一インスタンス定義に
  従い、重複を ProtocolViolation で拒否する。
- Error Code = REDIRECT で REDIRECT Detail が無い場合は MALFORMED_MESSAGE にした
  (§9.4.2.1 の MUST)。エンコード側は、適用外 Detail・単一インスタンス型の重複・
  REDIRECT Detail の欠落を ProtocolViolation で拒否し、decode で落ちるワイヤを生成しない。
- Session 側は `Session::send_request_error` の引数を `ErrorDetails` に変更し、
  `SessionEvent::RequestErrorReceived` の `redirect` を `error_details` に置き換えた。
  `Session::validate_peer_redirect` は Error Code = REDIRECT のときだけ REDIRECT Detail を
  検証し、欠落は MALFORMED_MESSAGE (decode 済みメッセージを直接受け取る API 経路の防御)、
  server が非ゼロ Connect URI を受けた場合は従来どおり PROTOCOL_VIOLATION で閉じる。
  draft 23 が求める ROLE_VIOLATION への細分化は
  [0386](0386-moqt-draft-23-session-error-classification.md) で扱う。
  イベントへ載せる前に `ErrorDetails::applicable` で適用外 Detail を落とし、ワイヤ経由の
  decode と同じ解釈に揃えた。
- namespace-scoped request 向けの空 Track Name は引き続き表現・受理でき、track-scoped
  request では Track Name の制約を課さない。namespace-scoped request 自体を本ライブラリは
  実装しないため、その文脈での Track Name 検証は行わない。
- 追随した利用側: `src/session/subscription/recv.rs` の拒否経路、
  `crates/tokio-moq/src/moqt_client.rs` の `MoqtClient::send_request_error`、
  `tests/`・`pbt/` の REQUEST_ERROR 構築箇所。ドキュメントは `docs/moqt.md`、
  `docs/missing-features.md`、`skills/shiguredo-moqt/SKILL.md` の API とイベント表を
  更新した。

## 完了条件の検証

- REDIRECT の golden バイト列:
  `tests/test_message.rs` の `request_error_details::redirect_golden_bytes` が、
  空の REDIRECT を持つ REQUEST_ERROR の全 10 バイト (Type / Length / Error Code /
  Retry Interval / Error Reason 長 / Delta Type / REDIRECT 値の Length / Connect URI 長 /
  Track Namespace フィールド数 / Track Name 長) と `consumed == encoded.len()` を固定する。
  `redirect_golden_bytes_with_content` が非空フィールドで Length と各フィールドの配置を
  固定する。実装は `ErrorDetails::encode` と `encode_redirect_detail`。
- 必須 Detail 欠落時の MALFORMED_MESSAGE:
  `request_error_details::redirect_missing_detail_is_malformed_message` がワイヤの decode を、
  `tests/test_session/request_stream.rs` の
  `server_receives_redirect_missing_detail_closes_session` が decode 済みメッセージを直接
  渡す API 経路を検証する (SESSION_MALFORMED_MESSAGE と Closing)。送信側は
  `request_error_details::encode_redirect_without_detail_is_protocol_violation`。
  実装は `ErrorDetails::decode_from` と `Session::validate_peer_redirect`。
- 未知 Detail・重複未知 Detail・適用外の既知 Detail が不当にセッションを閉じないこと:
  `request_error_details::unknown_details_are_ignored` /
  `duplicate_unknown_details_are_ignored` /
  `redirect_detail_for_other_error_code_is_ignored` が codec の受理と非保持を、
  `client_receives_error_with_unknown_details_accepted` (未知 Type の重複を含むワイヤ) と
  `server_receives_inapplicable_redirect_detail_accepted` (適用外 REDIRECT Detail と
  イベントの空 Error Details) が Session の受理を検証する。PBT は
  `pbt/tests/prop_message.rs` の `unknown_error_details_are_skipped` が 0〜8 個の未知
  Detail (偶数・奇数 Type、重複を含む) を生成して受理を固定する。
  実装は `skip_error_detail_value` と `ErrorDetails::decode_from` / `applicable`。
- 既知 Type の多重可否が定義に従うこと:
  `request_error_details::duplicate_redirect_detail_is_protocol_violation` が重複 REDIRECT の
  decode を、`encode_duplicate_redirect_detail_is_protocol_violation` が encode を拒否する。
- Detail の KVP 境界・全体消費量:
  `redirect_detail_length_exceeds_remaining_bytes_is_key_value_formatting_error` (Length が
  残りバイトを超える)、
  `redirect_detail_length_smaller_than_fields_is_key_value_formatting_error` (Length が
  フィールドを覆い切れない)、
  `redirect_detail_trailing_bytes_inside_value_is_key_value_formatting_error` (Length が
  構造より長い)、`detail_length_exceeding_maximum_is_field_length_exceeded` (2^16-1 超過)、
  `trailing_garbage_after_details_is_malformed_message` (Detail 列を読み切った後の余剰) が
  境界を固定し、golden テストがメッセージ末尾までの消費量を固定する。
- REDIRECT の role / field 検証:
  `server_receives_redirect_nonzero_connect_uri_closes_session` (server + 非ゼロ Connect URI は
  PROTOCOL_VIOLATION で close)、`client_receives_redirect_nonzero_connect_uri_accepted`
  (client は受理しイベントに Connect URI が載る)、
  `server_receives_redirect_zero_connect_uri_accepted` (空 Connect URI は受理)、
  `redirect_empty_track_name_accepted` (空 Track Name の受理)、
  `redirect_nonempty_track_name_for_subscribe_accepted` /
  `redirect_nonempty_track_name_for_fetch_accepted` (track-scoped の非空 Track Name は受理)。
  `tests/test_message.rs` の `redirect_full_track_name::redirect_at_limit_round_trips` /
  `redirect_over_limit_is_rejected_on_encode` / `redirect_over_limit_is_rejected_on_decode` が
  Redirect target の Full Track Name 4,096 バイト境界を固定する。
- 検証コマンド: `cargo fmt --all -- --check` /
  `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace` が
  すべて成功する。
