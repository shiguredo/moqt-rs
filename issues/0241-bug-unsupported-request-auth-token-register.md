# 未対応 request の拒否で §8.9 の REGISTER を適用する

- Created: 2026-10-12
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-unsupported-request-auth-token-register
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression) は "The receiver of a message
carrying an Authorization Token with Alias Type REGISTER that does not result in a Session error
MUST register the Token Alias in the token cache, even if the message fails for other reasons,
including Unauthorized." と定める。未対応 request を NOT_SUPPORTED で拒否する経路が
この MUST を満たしていない。

## 現状

- `Session::handle_unsupported_request` は PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE /
  SUBSCRIBE_TRACKS を REQUEST_NOT_SUPPORTED (control GOAWAY を送信済みなら REQUEST_GOING_AWAY)
  で拒否する。本体を解析しないため AUTHORIZATION_TOKEN の REGISTER を token cache へ登録しない。
- 受理する request の REGISTER 適用は、受理経路が各ハンドラの `apply_peer_message_auth_tokens`
  (SUBSCRIBE 系 / FETCH)、拒否経路が `accept_peer_request` の
  `apply_peer_message_auth_token_registers_only` (一意性違反 / GOAWAY 拒否) が担う
  (REQUEST_UPDATE は `handle_peer_request_update`)。未対応 request はどちらも通らない。
- 同じメッセージに重複した (Token Type, Token Value) がある場合の MALFORMED_AUTH_TOKEN 拒否も
  未対応 request では行われない (本体を解析しないため)。

## 設計方針

- 未対応 request でもパラメータ部を解析し、REGISTER を token cache へ適用する。本体の意味解析は
  しない (NOT_SUPPORTED の判断は変えない)。
- `ControlMessage::Unsupported` は本体 (パラメータ部) をデコードした構造を持たないため、3 種類
  (PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS) の共通構造
  (Request ID → Track Namespace Prefix → Number of Parameters → Parameters) に従って
  パラメータ部まで読み進める。読み進められない場合は §8.3 / §8.9 の MUST に従って
  セッションを閉じる。
- 0222 の `apply_peer_message_auth_token_registers_only` (拒否するメッセージでも REGISTER だけは
  適用する) を再利用する。

## 完了条件

- 未対応 request に REGISTER 付きトークンが載る場合、REQUEST_NOT_SUPPORTED を返しつつ
  alias が token cache に登録されること。
- 登録済み alias の再登録は従来どおり DUPLICATE_AUTH_TOKEN_ALIAS でセッションを閉じること。
- 上記がテストで固定されていること。
