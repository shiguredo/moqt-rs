# 手組み SETUP でも AUTHORIZATION_TOKEN の一意性を検証する

- Created: 2026-10-12
- Completed: 2026-11-10
- Branch: develop
- Polished: 2026-11-10
- Refreshed: 2026-11-04

## 目的

`Session::recv_control` はデコード済みの `ControlMessage` を受理する sans-I/O API である。
アプリが `SetupOptions` を組み立てて `Setup` として渡すと `SetupOptions::decode` の検証を
経由しないため、draft-ietf-moq-transport-23 §8.9 (Authorization Token Compression) の
一意性条件が API 経由で抜ける。

§8.9 は "An Authorization Token MAY be repeated within a message as long as the combination of
Token Type and Token Value are unique after resolving any aliases." と定め、整形式だが
それ以外の点で不正な Token を含むメッセージの受信側に "MUST reject that message with an
MALFORMED_AUTH_TOKEN error." を課す。SETUP は Request ID を持たないため、この拒否は
§12.2 (Session Termination Codes) の MALFORMED_AUTH_TOKEN (0x16) によるセッション終了として
実現する。

## 現状

- `SetupOptions::decode` は AUTHORIZATION_TOKEN の (Token Type, Token Value) を
  `src/message_parameter.rs` の `validate_auth_token_uniqueness` で検証し、重複を
  `MessageError::MalformedAuthToken` で拒否する (wire 経由の SETUP は守られている)。
- `Session::handle_peer_setup` は duplicate SETUP (UNEXPECTED_MESSAGE) / role / transport / URI の検証、
  `validate_max_control_message_size` (INVALID_FIELD)、`register_setup_auth_tokens` による REGISTER alias の登録
  (alias 重複は DUPLICATE_AUTH_TOKEN_ALIAS、DELETE / USE_ALIAS は ROLE_VIOLATION) を行うが、(Token Type, Token Value) の一意性は検証しない。
  検証をすべて通過すると `self.auth.peer_token_cache = peer_cache;` → `self.setup.peer = Some(setup);` →
  `self.state = SessionState::Established;` → `SessionEvent::Established` の順に状態を書く。既存の拒否はこれらより前で返るため、
  拒否時は peer の token cache も peer の SETUP も保持しない。
- SETUP の USE_ALIAS / DELETE は先に拒否される (wire では decode 中の `validate_setup_scope`、手組みでは `register_setup_auth_tokens`) ため、
  一意性の判定に残るのは REGISTER と USE_VALUE だけで、どちらも Token Type と Token Value を直接持つ。
  したがって `token_type_value` ベースの `validate_auth_token_uniqueness` の判定は、`register_setup_auth_tokens` の後に置けば
  §8.9 の「alias 解決後の一意性」と一致する。request 用の `Session::validate_peer_message_auth_token_uniqueness` は
  `&MessageParameters` を受けるため SETUP には使えない。
- wire 経由の重複トークン付き SETUP の拒否は、`crates/tokio-moq/src/peer_harness.rs` の
  `initial_setup_with_duplicate_tokens_closes_with_malformed_auth_token` (close code `SESSION_MALFORMED_AUTH_TOKEN`) と、
  `crates/tokio-moq/src/error.rs` の `session_error_code_maps_message_failures` (0x16 への写像) が既に固定している。
- そのため、同じ (Token Type, Token Value) のトークンを 2 つ持つ手組み SETUP は
  session が受理してしまう。

## 設計方針

- `Session::handle_peer_setup` で AUTHORIZATION_TOKEN の一意性を検証する。重複時は
  `SESSION_MALFORMED_AUTH_TOKEN` (0x16) でセッションを閉じる (§8.9 の MUST を §12.2 の
  セッション終了コードで実現する。wire 経由で `SetupOptions::decode` が
  `MessageError::MalformedAuthToken` を返す場合と同じコード)。
- 検証は既存の SETUP 検証 (duplicate SETUP / role / transport / URI / MAX_CONTROL_MESSAGE_SIZE / REGISTER alias) を
  すべて通過した後に行い、既存エラーの優先順位を変えない。既存の検証と重複トークンが
  同居する場合は既存のエラーコードを優先する。
- 挿入位置は `register_setup_auth_tokens` の `if let Err` ブロックの直後で、`self.auth.peer_token_cache = peer_cache;` より前とする。
  重複時は既存の拒否と同じく、peer の token cache (登録しかけた alias) も peer の SETUP も保持せず、`Established` にしない。
  §8.9 が REGISTER の登録を MUST とするのは "that does not result in a Session error" の場合だけであり、alias を残さない実装でよい。
- 検証ロジックは既存の `src/message_parameter.rs` の `pub(crate) fn validate_auth_token_uniqueness`
  を共有する。`SetupOptions::encode` / `decode` は既にこのヘルパを呼んでいる。
  `SetupOptions` の公開メソッドとして薄いラッパーを追加するか、session からヘルパを直接呼び、
  encode / decode / session の 3 箇所で同じ検証を使う。検証ロジックは複製しない。
- 既存の SETUP 検証 (duplicate SETUP / role / transport / URI / REGISTER alias) の挙動は変えない。

## 完了条件

- 同じ (Token Type, Token Value) を持つ手組み SETUP を `Session::recv_control` に渡したとき、
  `SESSION_MALFORMED_AUTH_TOKEN` (0x16) で session が閉じること。USE_VALUE の重複、値の同じ REGISTER 2 件、
  値の同じ REGISTER と USE_VALUE の組み合わせのいずれでも閉じること。
  wire 経由の同じ SETUP は `SetupOptions::decode` が `MessageError::MalformedAuthToken` を返し、I/O 層
  (`crates/tokio-moq/src/error.rs` の `session_error_code`) が同じ 0x16 で閉じる (既存テストで固定済み)。
- 重複で閉じたとき、`SessionEvent::Established` が出ず、`state()` が `Closing` になり、
  `peer_auth_token_cache()` が空で、`peer_control_message_size_limit()` が `PeerControlMessageSizeLimit::BeforeSetup` のままであること。
- `SetupOptions::encode` / `decode` の重複拒否 (`MessageError::MalformedAuthToken`) の
  挙動が変わらないこと。
- 上記がテストで固定されていること。手組み SETUP の close code、`CloseSession` イベントと拒否後の状態を
  `tests/test_session/` のテストで固定し、重複トークンを含む SETUP の生バイト列の `SetupOptions::decode` の拒否を
  `tests/test_parameter.rs` の単体テストで固定する (wire 経由の close code は peer_harness の既存テストが固定している)。
- 既存の SETUP 検証のテストが引き続き通ること。

## 解決方法

`src/session/core.rs` の `Session::handle_peer_setup` で、`register_setup_auth_tokens` の検証の直後、`self.auth.peer_token_cache = peer_cache;` より前に、
`SetupOptions::authorization_tokens()` を `src/message_parameter.rs` の `validate_auth_token_uniqueness` に渡して (Token Type, Token Value) の一意性を検証するようにした。
重複時は `session_error_from_message` で `SESSION_MALFORMED_AUTH_TOKEN` (0x16) の `SessionError` にしてセッションを閉じる。
既存の検証 (duplicate SETUP / role / transport / URI / MAX_CONTROL_MESSAGE_SIZE / REGISTER alias) を通過した後に置くため、既存エラーの優先順位は変わらない。
拒否時は peer の token cache も peer の SETUP も保持せず、`Established` にしない。検証ロジックは encode / decode と同じヘルパを共有し、複製していない。

テストは次のとおり。

- `tests/test_session/setup.rs` に `peer_setup_duplicate_authorization_token_closes_with_malformed_auth_token` を追加した。
  USE_VALUE の重複、値の同じ REGISTER 2 件、値の同じ REGISTER と USE_VALUE の各手組み SETUP で、0x16 で閉じること、`state()` が `Closing`、
  `peer_auth_token_cache()` が空、`peer_control_message_size_limit()` が `BeforeSetup` のままで、`CloseSession` を発行し `Established` を発行しないことを固定する。修正前の実装では失敗する。
- `tests/test_parameter.rs` に `duplicate_token_type_and_value_is_malformed_auth_token` を追加し、重複トークンを含む生バイト列の `SetupOptions::decode` が `MessageError::MalformedAuthToken` になること、値の異なる Token の繰り返しは受理することを固定した。
- wire 経由の close code は `crates/tokio-moq/src/peer_harness.rs` の既存テスト `initial_setup_with_duplicate_tokens_closes_with_malformed_auth_token` が固定している。

`CODEBASE.md` の規定に従い、ブランチと PR を作らず develop に直接コミットし、`CHANGES.md` は更新していない。
