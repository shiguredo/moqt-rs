# ControlMessage::Unsupported の encode で type_id と request_id を検証する

- Created: 2026-10-19
- Completed: 2026-11-20
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
- `crates/tokio-moq/src/peer_harness.rs` の `setup_head` は、`SetupOptions` の型付き API では表現できない不正な SETUP を送るために、`ControlMessage::Unsupported { type_id: SETUP_STREAM_TYPE, request_id: None, body }` を `encode_control_stream_setup` に渡している。encode に型の検証を入れるとこの経路がエラーになる。
- `Session::handle_unsupported_request` は、encode を通らずに `Session::recv_request` へ直接渡された手組みの `Unsupported` について、body 先頭の Request ID と `request_id` の一致を検証している（`tests/test_session/request_stream.rs` の `unsupported_request_body_decode_errors_close_session`）。

## 設計方針

- encode 時に次の 3 点を検証し、違反は `MessageError::ProtocolViolation` を返す。フィールドを private にする案は採らない。enum variant のフィールドは private にできず、variant の形を変える大きな破壊的変更になるためである。本 issue の目的は encode が生成する wire の roundtrip を保証することであり、encode 時の検証は既存の `Goaway` などと同じ境界検証のパターンで、公開 API の構築方法を変えずにこの契約を守れる。
  `Session::recv_request` は encode を通らない手組みメッセージも受けるため、Session 側の body と `request_id` の整合性検証は別途残す。

  1. `type_id` が `UNSUPPORTED_MESSAGE_TYPES` に含まれること。
  2. `request_id` の有無が型と一致すること。`UNSUPPORTED_MESSAGE_TYPES` のうち `UNSUPPORTED_MESSAGE_TYPES_WITH_REQUEST_ID` に含まれる型は `Some`、含まれない型は `None` でなければならない。
  3. `request_id` が `Some` のとき、`body` の先頭の varint が `request_id` と一致すること。`body` が空、または varint が途中で切れている場合も `ProtocolViolation` とする。`request_id` が `None` のときは body 先頭を Request ID として解釈しない（応答専用の型の本体は Track Namespace Suffix などで、先頭 varint は Request ID ではない）。

- encode は body 全体の構造までは検証しない。未対応 request の本体解析とパラメータ検証は `Session::handle_unsupported_request` が引き続き行う。encode 検証を理由に `Session` 側の body と `request_id` の整合性検証を削除しない（encode を通らない手組みメッセージがあるため）。
- `tests/test_message.rs` に不正な組み合わせの encode テストを追加する。対象は、`UNSUPPORTED_MESSAGE_TYPES` に無い型、request 型で `request_id` が `None`、応答専用型で `request_id` が `Some`、request 型で body が空、body 先頭の varint が `request_id` と不一致、body 先頭の varint が途中で切れている、の各ケースである。正しい `Unsupported` の roundtrip テストは維持する。
- `crates/tokio-moq/src/peer_harness.rs` の `setup_head` は、stream type prefix + SETUP メッセージの type (`0x2F00`) + u16 ビッグエンディアン長 + body を直接組み立てるヘルパーに置き換える。`ControlMessage::Unsupported` を SETUP の生バイト列に使う経路は残さない。`SETUP_STREAM_TYPE` と SETUP メッセージの type はどちらも `0x2F00` だが、別の名前空間であることに注意する。
- `ControlMessage::Unsupported` と `ControlMessage::encode` の doc に、encode 時の検証内容と返すエラー（`PayloadTooLong` と `ProtocolViolation`）を明記する。
- `issues/0276-test-pbt-unsupported-control-message.md` の PBT 生成器は、request 型の `request_id` と body 先頭の varint を一致させる必要がある。0276 の実装時に本 issue の検証へ追随すること。

## 完了条件

- `UNSUPPORTED_MESSAGE_TYPES` に無い型、`request_id` の有無が型と不一致、request 型で body が空または varint が切り詰め、body 先頭の varint が `request_id` と不一致の各ケースで、`ControlMessage::Unsupported` の encode が `MessageError::ProtocolViolation` を返すこと。
- 正しい `Unsupported` の encode → decode が同じ `type_id` / `request_id` / `body` を返すこと。
- 上記が `tests/test_message.rs` で固定されていること。
- `crates/tokio-moq/src/peer_harness.rs` の `setup_head` が生バイト列を直接組み立て、SETUP の不正形を送る次の 3 テストが通ること。
  - `initial_setup_with_unknown_token_alias_closes_with_key_value_formatting_error`
  - `initial_setup_with_duplicate_tokens_closes_with_malformed_auth_token`
  - `initial_setup_with_delete_token_closes_with_protocol_violation`
- `Session::handle_unsupported_request` の body と `request_id` の整合性検証が残り、`tests/test_session/request_stream.rs` の `unsupported_request_body_decode_errors_close_session` が通ること。
- `ControlMessage::Unsupported` と `ControlMessage::encode` の doc が実装と一致していること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

本 issue が対象とする `ControlMessage::Unsupported` は、draft-23 対応のコミット `11ae45a` (0431 draft-23 の
client/server 向け低レイヤー API を完成させる) で削除された。したがって、本 issue の「現状」と「設計方針」が
前提とする `Unsupported` 分岐、`UNSUPPORTED_MESSAGE_TYPES` / `UNSUPPORTED_MESSAGE_TYPES_WITH_REQUEST_ID`、
`Session::handle_unsupported_request` は現行のソースに存在せず、encode 時の検証を追加する対象自体が無い。

- `src/message.rs` の `ControlMessage` enum は 18 個の型付き variant だけを持ち、`Unsupported` variant は無い。
  `ControlMessage::encode_message_body` にも `Unsupported` の分岐は無く、各 variant が自身の構造体の
  `encode_message_body` に委譲する。`ControlMessage::decode_message_body` は未知の type_id を
  `MessageError::InvalidMessageType(type_id)` で拒否するため、decode が `Unsupported` を返すこともない。
- かつて `Unsupported` が担当していた 6 型 (PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS /
  NAMESPACE / NAMESPACE_DONE / PUBLISH_SKIPPED) は、`PublishNamespace` / `SubscribeNamespace` /
  `SubscribeTracks` / `Namespace` / `NamespaceDone` / `PublishSkipped` として型付きで実装された。
  `tests/test_message.rs` の `namespace_control_messages_roundtrip` がこの 6 型の encode → decode の一致を
  固定している。本 issue が維持を求めた `unsupported_control_messages_roundtrip` はこのテストに置き換えられている。
- `crates/tokio-moq/src/peer_harness.rs` の `setup_head` は、本 issue の設計方針どおり、stream type prefix +
  SETUP メッセージの type + Length + body を生バイト列で組み立てている。`ControlMessage::Unsupported` を
  SETUP の生バイト列に使う経路は残っていない。ただし、本 issue が書く「u16 ビッグエンディアン長」は
  draft-22 時点の記述であり、現行は draft-23 の vi64 Length を使う (`ControlMessage::encode` も vi64 で
  Length を書く)。
- `Session::handle_unsupported_request` は現行の `src/session/core.rs` に存在しない。`Session::recv_request`
  は 7 種の request を型付き variant で直接受け、namespace 系 request も `handle_peer_publish_namespace`
  などが処理する。request_id は型付き variant のフィールドであり、body 先頭の varint は encode が
  request_id から生成するため、本 issue が求めた「Session 側の body と request_id の整合性検証」は
  構造上不要になった。decode を通らない手組みメッセージに対するパラメータスコープ検証は
  `Session::recv_request` の doc にあるとおり引き続き session 層が行う。
- 本 issue の「`ControlMessage::Unsupported` と `ControlMessage::encode` の doc に ... `PayloadTooLong` と `ProtocolViolation` を明記する」は、現行の `ControlMessage::encode` の doc では `FieldLengthExceeded` になっており、`Unsupported` の doc 自体が無い。これも draft-22 時点の記述である。

以下は本 issue の記述と現行ソースの不一致のうち、状態遷移の根拠となる逐語引用である。

- 「`src/message.rs` の `ControlMessage::encode_message_body` の `Unsupported` 分岐は `payload.extend_from_slice(body)` と `*type_id` だけを使い、`request_id` を参照しない。」: 現行の `encode_message_body` に `Unsupported` 分岐は無い。
- 「`tests/test_message.rs` の `unsupported_control_messages_roundtrip` は、request 型で body 先頭と `request_id` を一致させた入力と応答専用型の正しい入力だけを固定している。」: 現行の `tests/test_message.rs` にこのテストは無く、`namespace_control_messages_roundtrip` が 6 型を固定している。
- 「`crates/tokio-moq/src/peer_harness.rs` の `setup_head` は、`SetupOptions` の型付き API では表現できない不正な SETUP を送るために、`ControlMessage::Unsupported { type_id: SETUP_STREAM_TYPE, request_id: None, body }` を `encode_control_stream_setup` に渡している。encode に型の検証を入れるとこの経路がエラーになる。」: 現行の `setup_head` は生バイト列を直接組み立てている。
- 「`Session::handle_unsupported_request` は、encode を通らずに `Session::recv_request` へ直接渡された手組みの `Unsupported` について、body 先頭の Request ID と `request_id` の一致を検証している（`tests/test_session/request_stream.rs` の `unsupported_request_body_decode_errors_close_session`）。」: 現行の `src/session/core.rs` にこの関数は無い。
- 「`Session::handle_unsupported_request` の body と `request_id` の整合性検証が残り、`tests/test_session/request_stream.rs` の `unsupported_request_body_decode_errors_close_session` が通ること。」: 関数もテストも現行には無い。
- 「この wire は SUBSCRIBE として decode できず、`MessageError::UnexpectedEof` になる。」: 現行の `ControlMessage::decode` は本文解析中の `UnexpectedEof` を `MalformedMessage` に写すため、同じ wire でも返るエラーは異なる。
- 「stream type prefix + SETUP メッセージの type (`0x2F00`) + u16 ビッグエンディアン長 + body を直接組み立てるヘルパーに置き換える。」: 現行の `setup_head` は vi64 の Length を書く。
- 「返すエラー（`PayloadTooLong` と `ProtocolViolation`）」: 現行の `ControlMessage::encode` は `FieldLengthExceeded` を返す。
- 完了条件の 3 テスト名のうち、`initial_setup_with_unknown_token_alias_closes_with_key_value_formatting_error`
  は `initial_setup_with_unknown_token_alias_closes_with_malformed_auth_token` に、
  `initial_setup_with_delete_token_closes_with_protocol_violation` は
  `initial_setup_with_delete_token_closes_with_role_violation` に改名され、期待する終了コードも
  `SESSION_KEY_VALUE_FORMATTING_ERROR` から `SESSION_MALFORMED_AUTH_TOKEN`、
  `SESSION_PROTOCOL_VIOLATION` から `SESSION_ROLE_VIOLATION` に変わっている。
  `initial_setup_with_duplicate_tokens_closes_with_malformed_auth_token` は現行にも存在する。
  いずれも `setup_head` の生バイト列経由で SETUP の不正形を送るテストである。

検証:

- `CARGO_TARGET_DIR=/tmp/polish-0317-target cargo test -p shiguredo_moqt --test test_message namespace_control_messages_roundtrip` を実行し、`1 passed; 0 failed` を確認した。
- `CARGO_TARGET_DIR=/tmp/polish-0317-target cargo test -p tokio-moq --lib initial_setup_with` を実行し、`7 passed; 0 failed` を確認した。改名後の `initial_setup_with_unknown_token_alias_closes_with_malformed_auth_token` / `initial_setup_with_delete_token_closes_with_role_violation` も含まれる。
- `cargo fmt --all -- --check` は成功した。
- `cargo test --workspace` は実行環境に `pkg-config` / `libpulse` が無く、`shiguredo_audio_device` のビルドで失敗した。本 issue の対象モジュール (`shiguredo_moqt` と `tokio-moq`) は個別に実行して通ることを確認した。

以上から、本 issue の目的だった「`Unsupported` の encode が roundtrip を壊す」問題は、`Unsupported` を廃止して全メッセージを型付き variant に移行したことで解消済みである。encode 時の追加検証を実装する必要はなく、完了条件も現行のシンボル・テストに存在しないため、closed とする。

なお、`issues/0276-test-pbt-unsupported-control-message.md` も `Unsupported` を前提にしているため、同じ理由で前提が変わっている。0276 の扱いは本 issue では変更せず、0276 側で判断する。
