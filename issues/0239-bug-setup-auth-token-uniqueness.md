# 手組み SETUP でも AUTHORIZATION_TOKEN の一意性を検証する

- Created: 2026-10-12
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-setup-auth-token-uniqueness
- Polished: 2026-10-25

## 目的

`Session::recv_control` はデコード済みの `ControlMessage` を受理する sans-I/O API である。
アプリが `SetupOptions` を組み立てて `Setup` として渡すと `SetupOptions::decode` の検証を
経由しないため、draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression) の
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
- `Session::handle_peer_setup` は duplicate SETUP / role / transport / URI の検証と、
  `register_setup_auth_tokens` による REGISTER alias の登録 (alias 重複は
  DUPLICATE_AUTH_TOKEN_ALIAS) を行うが、(Token Type, Token Value) の一意性は検証しない。
- そのため、同じ (Token Type, Token Value) のトークンを 2 つ持つ手組み SETUP は
  session が受理してしまう。

## 設計方針

- `Session::handle_peer_setup` で AUTHORIZATION_TOKEN の一意性を検証する。重複時は
  `SESSION_MALFORMED_AUTH_TOKEN` (0x16) でセッションを閉じる (§8.9 の MUST を §12.2 の
  セッション終了コードで実現する。wire 経由で `SetupOptions::decode` が
  `MessageError::MalformedAuthToken` を返す場合と同じコード)。
- 検証は既存の SETUP 検証 (duplicate SETUP / role / transport / URI / REGISTER alias) を
  すべて通過した後に行い、既存エラーの優先順位を変えない。既存の検証と重複トークンが
  同居する場合は既存のエラーコードを優先する。
- 検証ロジックは既存の `src/message_parameter.rs` の `pub(crate) fn validate_auth_token_uniqueness`
  を共有する。`SetupOptions::encode` / `decode` は既にこのヘルパを呼んでいる。
  `SetupOptions` の公開メソッドとして薄いラッパーを追加するか、session からヘルパを直接呼び、
  encode / decode / session の 3 箇所で同じ検証を使う。検証ロジックは複製しない。
- 既存の SETUP 検証 (duplicate SETUP / role / transport / URI / REGISTER alias) の挙動は変えない。

## 完了条件

- 同じ (Token Type, Token Value) を持つ手組み SETUP を `Session::recv_control` に渡したとき、
  `SESSION_MALFORMED_AUTH_TOKEN` (0x16) で session が閉じること。wire 経由の同じ SETUP は
  `SetupOptions::decode` が `MessageError::MalformedAuthToken` を返し、I/O 層
  (`examples/tokio-moq/src/error.rs` の `session_error_code`) が同じ 0x16 で閉じる。
- `SetupOptions::encode` / `decode` の重複拒否 (`MessageError::MalformedAuthToken`) の
  挙動が変わらないこと。
- 上記がテストで固定されていること。手組み SETUP の close code と `CloseSession` イベント、
  重複トークンを含む SETUP の生バイト列の decode 拒否を、それぞれテストで固定する。
- 既存の SETUP 検証のテストが引き続き通ること。
