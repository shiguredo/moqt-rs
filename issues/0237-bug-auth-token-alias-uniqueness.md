# alias 解決後も含めた AUTHORIZATION_TOKEN の一意性を検証する

- Created: 2026-10-12
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-auth-token-alias-uniqueness
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression) は
"An Authorization Token MAY be repeated within a message as long as the combination of
Token Type and Token Value are unique after resolving any aliases." と定め、同節は
"The receiver of a message containing a well-formed Token structure that is otherwise invalid
MUST reject that message with an MALFORMED_AUTH_TOKEN error." と MUST を課す。alias を解決した
結果の (Token Type, Token Value) が重複するメッセージを受理しているため、この MUST を
満たしていない。

## 現状

- `MessageParameters::validate_auth_token_uniqueness` は 1 つのメッセージ内の REGISTER /
  USE_VALUE の (Token Type, Token Value) だけを検証し、USE_ALIAS は対象外である。関数の doc は
  「USE_ALIAS を含む重複 (同一 Alias の USE_ALIAS の繰り返し、同一メッセージ内の REGISTER と
  それを参照する USE_ALIAS の組み合わせ) は alias 解決を要するため本検証の対象外である
  (現状未実装。解決結果を保持するセッション層で判定する必要がある)」と明記している。
- token cache を持つ session 層でも、USE_ALIAS を解決した結果を含めた一意性検証は行っていない。
  そのため、同一メッセージ内の USE_ALIAS と REGISTER / USE_VALUE が同じ (Token Type,
  Token Value) に解決する場合を見逃す。
- 0222 で「構造は正しいが内容が不正なトークンは MALFORMED_AUTH_TOKEN でメッセージ単位に
  拒否する」経路を入れたが、この組み合わせはその経路でも検出できない。

## 設計方針

- peer の token cache で alias を解決したうえで (Token Type, Token Value) の重複を検証する
  (session 層)。重複時は §8.9 に従い MALFORMED_AUTH_TOKEN でメッセージ単位に拒否し、
  セッションを閉じない (0222 の拒否経路と同じ扱い)。
- 解決できない alias (未登録) は既存の UNKNOWN_AUTH_TOKEN_ALIAS の挙動を変えない。
- 未登録 alias によるセッション終了と、alias 解決後の重複によるメッセージ単位の reject が
  同時に成立する場合の優先順位は、0222 の整理 (セッション終了の MUST をメッセージ単位の
  reject に隠さない) に合わせる。実装時はどちらを先に判定するかを doc とテストで固定する。
- SETUP の AUTHORIZATION_TOKEN の検証 (0239) と同じ session 層の検証になるため、共通化の
  可否と実施順を決めてから着手する。
- codec 層の `validate_auth_token_uniqueness` は「alias 解決を要さない範囲の検証」として
  残し、doc の未実装記述を session 層の検証に合わせて更新する。

## 完了条件

- 同一メッセージ内で alias 解決後に (Token Type, Token Value) が重複するワイヤを、session が
  MALFORMED_AUTH_TOKEN の REQUEST_ERROR で拒否し、セッションを維持すること。
- 未登録 alias を参照する既存挙動 (UNKNOWN_AUTH_TOKEN_ALIAS) が変わらないこと。
- 上記がテストで固定されていること。
