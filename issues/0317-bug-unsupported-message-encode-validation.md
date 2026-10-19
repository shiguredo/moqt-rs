# ControlMessage::Unsupported の encode で type_id と request_id を検証する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-unsupported-message-encode-validation
- Polished: 2026-10-20

## 目的

`ControlMessage::Unsupported` は公開 enum の variant で、フィールドを自由に構築できる。encode は `type_id` と `body` を無検証で書き出し、`request_id` を参照しないため、decode で別メッセージになる wire や `UNSUPPORTED_MESSAGE_TYPES` に無い型の wire を生成できる。公開 API の契約として、encode が成功した `Unsupported` は decode で同じ `type_id` / `request_id` / `body` に戻ることを保証する。

## 現状

- `src/message.rs` の `ControlMessage::encode_message_body` の `Unsupported` 分岐は `payload.extend_from_slice(body)` と `*type_id` だけを使い、`request_id` を参照しない。
- `Unsupported { type_id: 0x03, request_id: None, body: vec![] }` のような既知型・不整合な組み合わせを encode できる。この wire は SUBSCRIBE として decode できず、`MessageError::UnexpectedEof` になる。
- `Unsupported { type_id: 0x03, request_id: None, body: <SUBSCRIBE の本体> }` のように対応済みの型の本体を `body` に置くと、encode した wire は `ControlMessage::Subscribe` として decode され、`Unsupported` に戻らない。
- request 型で `request_id: Some(5)` と body 先頭の varint が一致しない値も encode でき、decode は body 先頭の varint を `request_id` として返す。
- `tests/test_message.rs` の `unsupported_control_messages_roundtrip` は、request 型で body 先頭と `request_id` を一致させた入力と応答専用型の正しい入力だけを固定している。
- `examples/tokio-moq/src/peer_harness.rs` の `setup_head` は、`SetupOptions` の型付き API では表現できない不正な SETUP を送るために、`ControlMessage::Unsupported { type_id: SETUP_STREAM_TYPE, request_id: None, body }` を `encode_control_stream_setup` に渡している。encode に型の検証を入れるとこの経路がエラーになる。
- `Session::handle_unsupported_request` は、encode を通らずに `Session::recv_request` へ直接渡された手組みの `Unsupported` について、body 先頭の Request ID と `request_id` の一致を検証している（`tests/test_session/request_stream.rs` の `unsupported_request_body_decode_errors_close_session`）。

## 設計方針

- encode 時に次の 3 点を検証し、違反は `MessageError::ProtocolViolation` を返す。フィールドを private にする案は採らない。enum variant のフィールドは private にできず、variant の形を変える大きな破壊的変更になるためである。本 issue の目的は encode が生成する wire の roundtrip を保証することであり、encode 時の検証は既存の `Goaway` などと同じ境界検証のパターンで、公開 API の構築方法を変えずにこの契約を守れる。
  `Session::recv_request` は encode を通らない手組みメッセージも受けるため、Session 側の body と `request_id` の整合性検証は別途残す。

  1. `type_id` が `UNSUPPORTED_MESSAGE_TYPES` に含まれること。
  2. `request_id` の有無が型と一致すること。`UNSUPPORTED_MESSAGE_TYPES` のうち `UNSUPPORTED_MESSAGE_TYPES_WITH_REQUEST_ID` に含まれる型は `Some`、含まれない型は `None` でなければならない。
  3. `request_id` が `Some` のとき、`body` の先頭の varint が `request_id` と一致すること。`body` が空、または varint が途中で切れている場合も `ProtocolViolation` とする。`request_id` が `None` のときは body 先頭を Request ID として解釈しない（応答専用の型の本体は Track Namespace Suffix などで、先頭 varint は Request ID ではない）。

- encode は body 全体の構造までは検証しない。未対応 request の本体解析とパラメータ検証は `Session::handle_unsupported_request` が引き続き行う。encode 検証を理由に `Session` 側の body と `request_id` の整合性検証を削除しない（encode を通らない手組みメッセージがあるため）。
- `tests/test_message.rs` に不正な組み合わせの encode テストを追加する。対象は、`UNSUPPORTED_MESSAGE_TYPES` に無い型、request 型で `request_id` が `None`、応答専用型で `request_id` が `Some`、request 型で body が空、body 先頭の varint が `request_id` と不一致、body 先頭の varint が途中で切れている、の各ケースである。正しい `Unsupported` の roundtrip テストは維持する。
- `examples/tokio-moq/src/peer_harness.rs` の `setup_head` は、stream type prefix + SETUP メッセージの type (`0x2F00`) + u16 ビッグエンディアン長 + body を直接組み立てるヘルパーに置き換える。`ControlMessage::Unsupported` を SETUP の生バイト列に使う経路は残さない。`SETUP_STREAM_TYPE` と SETUP メッセージの type はどちらも `0x2F00` だが、別の名前空間であることに注意する。
- `ControlMessage::Unsupported` と `ControlMessage::encode` の doc に、encode 時の検証内容と返すエラー（`PayloadTooLong` と `ProtocolViolation`）を明記する。
- `issues/0276-test-pbt-unsupported-control-message.md` の PBT 生成器は、request 型の `request_id` と body 先頭の varint を一致させる必要がある。0276 の実装時に本 issue の検証へ追随すること。

## 完了条件

- `UNSUPPORTED_MESSAGE_TYPES` に無い型、`request_id` の有無が型と不一致、request 型で body が空または varint が切り詰め、body 先頭の varint が `request_id` と不一致の各ケースで、`ControlMessage::Unsupported` の encode が `MessageError::ProtocolViolation` を返すこと。
- 正しい `Unsupported` の encode → decode が同じ `type_id` / `request_id` / `body` を返すこと。
- 上記が `tests/test_message.rs` で固定されていること。
- `examples/tokio-moq/src/peer_harness.rs` の `setup_head` が生バイト列を直接組み立て、SETUP の不正形を送る次の 3 テストが通ること。
  - `initial_setup_with_unknown_token_alias_closes_with_key_value_formatting_error`
  - `initial_setup_with_duplicate_tokens_closes_with_malformed_auth_token`
  - `initial_setup_with_delete_token_closes_with_protocol_violation`
- `Session::handle_unsupported_request` の body と `request_id` の整合性検証が残り、`tests/test_session/request_stream.rs` の `unsupported_request_body_decode_errors_close_session` が通ること。
- `ControlMessage::Unsupported` と `ControlMessage::encode` の doc が実装と一致していること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
