# 未対応 request の拒否で §8.9 の REGISTER を適用する

- Created: 2026-10-12
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-unsupported-request-auth-token-register
- Polished: 2026-10-14

## 目的

draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression) は "The receiver of a message
carrying an Authorization Token with Alias Type REGISTER that does not result in a Session error
MUST register the Token Alias in the token cache, even if the message fails for other reasons,
including Unauthorized." と定める。未対応 request を NOT_SUPPORTED で拒否する経路がこの MUST を
満たしていない。あわせて、同じメッセージの重複トークンを MALFORMED_AUTH_TOKEN で拒否する MUST
(同節 "The receiver of a message containing a well-formed Token structure that is otherwise
invalid MUST reject that message with an MALFORMED_AUTH_TOKEN error.") も満たしていない。

## 現状

- `Session::handle_unsupported_request` は PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE /
  SUBSCRIBE_TRACKS を REQUEST_NOT_SUPPORTED (control GOAWAY を送信済みなら REQUEST_GOING_AWAY)
  で拒否する。本体を解析しないため AUTHORIZATION_TOKEN の REGISTER を token cache へ登録しない。
- `ControlMessage::Unsupported` は本体を生バイト列 (`body`) として持つが、構造は持たない。
  `Session::recv_request` は `body` を捨てて `handle_unsupported_request(type_id, request_id)` を
  呼ぶ。
- 受理する request の REGISTER 適用は、受理経路が各ハンドラの `apply_peer_message_auth_tokens`
  (SUBSCRIBE 系 / PUBLISH / FETCH / TRACK_STATUS)、拒否経路が `accept_peer_request` の
  `apply_peer_message_auth_token_registers_only` (一意性違反 / GOAWAY 拒否) が担う
  (REQUEST_UPDATE は `handle_peer_request_update`)。未対応 request はどちらも通らない。
- 同じメッセージに重複した (Token Type, Token Value) がある場合の MALFORMED_AUTH_TOKEN 拒否も、
  未対応 request では行われない (本体を解析しないため)。
- `accept_peer_request` は、§8.9 のメッセージ拒否が MUST であり §9.2 の GOAWAY 拒否が MAY で
  あることから、MALFORMED_AUTH_TOKEN の判定を GOAWAY 拒否より先に行う。未対応 request でも
  同じ順序が要る。

## 設計方針

- `Session::recv_request` から `handle_unsupported_request` へ `body` を渡し、Session 側で本体を
  解析する。`ControlMessage::Unsupported` の構造は変えない (message.rs の decode は型を知らない
  ため、3 種類の構造を知る Session 側で読む)。
- 3 種類 (PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS) の共通構造
  (Request ID → Track Namespace Prefix → Number of Parameters → Parameters) に従って Request ID
  と Track Namespace Prefix を読み進め、残りを `MessageParameters::decode` に渡す。読み進めた
  Request ID は `handle_unsupported_request` が既に受け取っている値と一致することを確かめる。
  `ControlMessage::decode` を通った場合は同じバイト列に由来するため必ず一致する。公開 enum を
  手組みして食い違わせた場合は PROTOCOL_VIOLATION でセッションを閉じる。
- 解析の順序は次のとおりとする。セッション終了を招く検証 (§9.20 / §9.20.1 の MUST) を先に
  置き、そのうえで §8.9 のメッセージ単位の拒否 (MUST) を §9.2 の GOAWAY 拒否 (MAY) と
  §1.6 の NOT_SUPPORTED (SHOULD) より先に置く。0222 の `accept_peer_request` と同じ整理で
  あり、セッション終了の検証がメッセージ単位の reject に隠れないようにするためである。
  1. Request ID の parity と重複 (`validate_peer_request_id`)
  2. パラメータ部の decode (§9.20 / §8.3 の MUST。未知のパラメータ型・重複・フレーミングは
     `MessageParameters::decode` が拒否する)
  3. パラメータのスコープ検証 (§9.20.1 の MUST)。3 種類それぞれの許可パラメータ集合を定数と
     して持つ。同節はメッセージの型ごとに許されるパラメータを定める MUST であり、未対応
     request でもパラメータを解析する以上は適用される。「NOT_SUPPORTED の判断は変えない」とは、
     対応する request として受理しないことを意味する。違反時は PROTOCOL_VIOLATION で
     セッションを閉じる (受理経路も decode の時点で検証しており、同じ優先順になる)
  4. AUTHORIZATION_TOKEN の一意性 (§8.9 の MUST)。違反時は
     `apply_peer_message_auth_token_registers_only` を適用したうえで REQUEST_ERROR
     (MALFORMED_AUTH_TOKEN) を返し、セッションは維持する (`accept_peer_request` と同じ。
     ただし REGISTER の適用自体が失敗した場合は下記のとおり)
  5. REGISTER の token cache への適用 (§8.9 の MUST)。GOAWAY 拒否の場合も適用する
  6. REQUEST_GOING_AWAY (control GOAWAY を送信済み) または REQUEST_NOT_SUPPORTED
- 解析に失敗した場合は、原因に応じた終了コードでセッションを閉じる (REQUEST_ERROR は送らない)。
  写像は現行の `session_error_from_data_message` と `examples/tokio-moq` の `session_error_code`
  に従う。
  - Track Namespace の書式不正 (§8.7) → PROTOCOL_VIOLATION
  - トークンの内部直列化が宣言長と一致しない (§8.3) → KEY_VALUE_FORMATTING_ERROR
  - 未知のパラメータ型・重複・body の切り詰め (§9.20 / §9 Table 5) → PROTOCOL_VIOLATION
  - パラメータブロックを読み切った後の余剰バイト (§9 Table 5 の Length の不一致) →
    PROTOCOL_VIOLATION (受理経路の decode 層と同じ扱い)
  - 宣言長が残りを超える場合、§8.3 の MUST は KEY_VALUE_FORMATTING_ERROR だが、現行の codec は
    `UnexpectedEof` を返して PROTOCOL_VIOLATION に写す。この挙動の修正は 0238 で扱う
    (0238 は codec 層を直し、写像は変えない) ため、本 issue では現行の写像のままとする
- 自側が MAX_AUTH_TOKEN_CACHE_SIZE を宣言していない (既定 0) セッションでは、REGISTER を適用
  した時点で AUTH_TOKEN_CACHE_OVERFLOW でセッションを閉じる (§9.1.3 / §8.9)。
  `apply_peer_message_auth_token_registers_only` と同じ挙動であり、REQUEST_ERROR は送らない。
- 0222 の `apply_peer_message_auth_token_registers_only` を再利用する。
- 「本体を解析しない」と書いた doc コメントと `docs/moqt.md` の記述を実装に合わせて更新する。
  `message.rs` の「本体は解析せず生バイト列のまま保持する」は、構造を変えない方針のため
  そのまま正しい (対象外)。
- 後方互換は不要 (破壊的変更を恐れない)。

## 完了条件

- 自側が MAX_AUTH_TOKEN_CACHE_SIZE を宣言したセッションで、未対応 request に REGISTER 付き
  トークンが載る場合、REQUEST_NOT_SUPPORTED を返しつつ alias が token cache に登録されること。
- 自側が MAX_AUTH_TOKEN_CACHE_SIZE を宣言したセッションで、control GOAWAY を送信済みの場合は
  REQUEST_GOING_AWAY を返しつつ、同じく alias が登録されること。
- 自側が MAX_AUTH_TOKEN_CACHE_SIZE を宣言したセッションで、同じメッセージに重複した
  (Token Type, Token Value) がある場合、MALFORMED_AUTH_TOKEN で拒否され、セッションは
  維持されること (REQUEST_GOING_AWAY / REQUEST_NOT_SUPPORTED より優先されること)。
- 登録済み alias の再登録は DUPLICATE_AUTH_TOKEN_ALIAS でセッションを閉じること (受理経路の
  `accept_peer_request` と同じ)。
- 自側が MAX_AUTH_TOKEN_CACHE_SIZE を宣言していないセッションで REGISTER 付きの未対応 request
  を受けた場合、AUTH_TOKEN_CACHE_OVERFLOW でセッションを閉じ、REQUEST_ERROR を送らないこと。
- body が共通構造に従わない場合、原因に応じた終了コード (PROTOCOL_VIOLATION /
  KEY_VALUE_FORMATTING_ERROR) でセッションを閉じること。
- パラメータが 3 種類それぞれの許可集合に無い場合、PROTOCOL_VIOLATION でセッションを閉じる
  こと。
- 上記がテストで固定されていること。`tests/test_session/request_stream.rs` の既存 3 テストは
  body を共通構造 (Request ID → Track Namespace Prefix → Number of Parameters) に更新する。
- `docs/moqt.md` と doc コメントが実装に合っていること。
