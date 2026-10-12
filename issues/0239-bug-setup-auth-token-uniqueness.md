# 手組み SETUP でも AUTHORIZATION_TOKEN の一意性を検証する

- Created: 2026-10-12
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-setup-auth-token-uniqueness
- Polished: {YYYY-MM-DD}

## 目的

`Session::recv_control` はデコード済みの `ControlMessage` を受理する sans-I/O API である。
アプリが `SetupOptions` を組み立てて `Setup` として渡すと `SetupOptions::decode` の検証を
経由しないため、draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression) の
(Token Type, Token Value) 一意性 MUST が API 経由で抜ける。

## 現状

- `SetupOptions::decode` は AUTHORIZATION_TOKEN の (Token Type, Token Value) を
  `validate_auth_token_uniqueness` で検証し、重複を拒否する (wire 経由の SETUP は守られている)。
- `Session::handle_peer_setup` は duplicate SETUP / role / transport / URI の検証と、
  `register_setup_auth_tokens` による REGISTER alias の登録 (alias 重複は
  DUPLICATE_AUTH_TOKEN_ALIAS) を行うが、(Token Type, Token Value) の一意性は検証しない。
- そのため、同じ (Token Type, Token Value) のトークンを 2 つ持つ手組み SETUP は
  session が受理してしまう。

## 設計方針

- `Session::handle_peer_setup` で AUTHORIZATION_TOKEN の一意性を検証する。SETUP は Request ID を
  持たずメッセージ単位の reject ができないため、重複時は §8.9 に従いセッションを閉じる
  (終了コードは SETUP の decode 失敗と同じ扱いに揃える)。
- 検証ロジックは既存の 2 箇所 (`SetupOptions::encode` と `SetupOptions::decode` のインライン
  検証) と重複させない。`SetupOptions` の公開メソッドとして切り出すか、parameter モジュールの
  共通ヘルパを呼び、encode / decode / session の 3 箇所で共有する。
- 既存の SETUP 検証 (duplicate SETUP / role / URI / REGISTER alias) の挙動は変えない。

## 完了条件

- 同じ (Token Type, Token Value) を持つ手組み SETUP を `Session::recv_control` に渡したとき、
  wire 経由の同じ SETUP と同じ終了コードで session が閉じること。
- wire 経由の SETUP の検証 (decode 時の拒否) の挙動が変わらないこと。
- 上記がテストで固定されていること。
