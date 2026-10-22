# alias 解決後も含めた AUTHORIZATION_TOKEN の一意性を検証する

- Created: 2026-10-12
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-auth-token-alias-uniqueness
- Polished: 2026-10-23

## 目的

draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression) は
"An Authorization Token MAY be repeated within a message as long as the combination of
Token Type and Token Value are unique after resolving any aliases." と定め、同節は
"The receiver of a message containing a well-formed Token structure that is otherwise invalid
MUST reject that message with an MALFORMED_AUTH_TOKEN error." と MUST を課す。alias を解決した
結果の (Token Type, Token Value) が重複するメッセージを受理しているため、この MUST を
満たしていない。

## 現状

- `MessageParameters::validate_auth_token_uniqueness` (`src/message_parameter.rs`) は 1 つの
  メッセージ内の REGISTER / USE_VALUE の (Token Type, Token Value) だけを検証し、USE_ALIAS は
  対象外である (`AuthorizationToken::token_type_value` が USE_ALIAS / DELETE に `None` を
  返す)。関数の doc は「USE_ALIAS を含む重複 (同一 Alias の USE_ALIAS の繰り返し、同一
  メッセージ内の REGISTER とそれを参照する USE_ALIAS の組み合わせ) は alias 解決を要するため
  本検証の対象外である (現状未実装。解決結果を保持するセッション層で判定する必要がある)」と
  明記している。
- session 層も alias 解決後の一意性を検証していない。`Session::apply_peer_message_auth_tokens`
  (`src/session/core.rs`) は alias を解決するが、解決結果の (Token Type, Token Value) の重複は
  判定しない。そのため、USE_ALIAS の解決結果が REGISTER / USE_VALUE と重複するメッセージや、
  同一 alias を 2 回参照するメッセージを受理してしまう。
- 一意性検証の呼び出しは次の 3 関数 5 経路にある。
  - `Session::accept_peer_request` (`src/session/core.rs`): SUBSCRIBE / PUBLISH / FETCH /
    TRACK_STATUS の受理経路
  - `Session::handle_peer_request_update` (`src/session/subscription/recv.rs`):
    REQUEST_UPDATE の受理経路
  - `Session::handle_unsupported_request` (`src/session/core.rs`): PUBLISH_NAMESPACE /
    SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS の拒否経路
- 未登録 alias の扱いは経路で異なる。受理経路は `apply_peer_message_auth_tokens` が
  `UNKNOWN_AUTH_TOKEN_ALIAS` (0x17) でセッションを閉じる。拒否経路 (未対応 request / control
  GOAWAY 送信後の拒否) とローカル終端済み request では、失敗するメッセージの USE_ALIAS を
  適用せずメッセージ単位の拒否で終わり、セッションを維持する。後者は
  `tests/test_session/request_stream.rs` の
  `unsupported_request_with_unknown_alias_is_rejected_with_not_supported` と
  `tests/test_session/goaway.rs` の
  `goaway_sent_side_use_alias_after_goaway_does_not_kill_session` で固定されている。
- draft §8.9 の未登録 alias は "MUST reject the message with UNKNOWN_AUTH_TOKEN_ALIAS" であり、
  セッション終了の MUST ではない。`UNKNOWN_AUTH_TOKEN_ALIAS` (0x17) は §12.2 (Session
  Termination Codes) にのみ定義され §12.3 (Request Error Codes) に無いため、本実装は
  セッション終了コードとして送ることを選択している。この選択の是非は 0274 の範囲であり、
  本 issue は経路ごとの現行挙動を変えない。
- 0222 で「構造は正しいが内容が不正なトークンは MALFORMED_AUTH_TOKEN でメッセージ単位に
  拒否する」経路を入れたが、この経路は USE_ALIAS の解決結果を含む重複を検出できない。

## 設計方針

- session 層 (`Session`) に alias 解決後まで含めた一意性検証を行う非公開メソッドを追加し、
  上記 3 関数 5 経路の `MessageParameters::validate_auth_token_uniqueness` 呼び出しを
  置き換える。重複時は現行どおり REQUEST_ERROR (`MALFORMED_AUTH_TOKEN`) でメッセージ単位に
  拒否し、セッションを維持する。
- alias の解決手順を次に固定する。
  1. `AuthTokenCache` (peer の token cache) を複製した一時キャッシュを作る。
  2. メッセージの AUTHORIZATION_TOKEN をワイヤ順に走査し、REGISTER は一時キャッシュへの登録を
     試み、DELETE は一時キャッシュから削除する。USE_ALIAS はその時点の一時キャッシュから
     解決する。
  3. 解決できた USE_ALIAS と REGISTER / USE_VALUE の (Token Type, Token Value) を集計し、
     重複を判定する。

  実キャッシュは変更しない。登録の失敗 (alias 重複 / 容量超過) は重複判定の妨げにせず、
  session error の判定は従来どおり `apply_peer_message_auth_tokens` /
  `apply_peer_message_auth_token_registers_only` に委ねる。
- 同一メッセージ内の先行 REGISTER / DELETE を解決に反映する。draft は USE_ALIAS を
  "previously registered" と書くが、本実装の受理経路はメッセージ内の Token をワイヤ順に
  適用しているため、一意性判定も同じ解決結果を使う。この解釈は doc とテストで固定する。
- 解決できない alias (未登録) は一意性の判定対象から外す。本検証では新たな session error を
  発生させず、未登録 alias の扱いは既存の経路に委ねる。結果として、未登録 alias と重複が
  同じメッセージに同居する場合は REQUEST_ERROR (`MALFORMED_AUTH_TOKEN`) のメッセージ単位の
  reject が優先される (現行コードの「reject するメッセージの DELETE / USE_ALIAS を適用しない」
  整理と同じ)。この優先順位を doc とテストで固定する。
- 経路ごとの挙動を次に固定する。
  - 受理経路 (`accept_peer_request` / `handle_peer_request_update`): 重複のみなら
    REQUEST_ERROR (`MALFORMED_AUTH_TOKEN`) を送りセッションを維持する。未登録 alias のみなら
    従来どおり適用経路が `UNKNOWN_AUTH_TOKEN_ALIAS` (0x17) でセッションを閉じる。両方なら
    `MALFORMED_AUTH_TOKEN` の reject が優先されセッションを維持する。ローカル終端済み request は
    従来どおり応答せず吸収する。
  - 拒否経路 (`handle_unsupported_request`、GOAWAY 拒否): 重複のみなら
    `MALFORMED_AUTH_TOKEN` の reject を `GOING_AWAY` / `NOT_SUPPORTED` より優先する (現行順序を
    維持)。未登録 alias のみなら従来どおり `GOING_AWAY` / `NOT_SUPPORTED` で拒否しセッションを
    維持する (0274 の判断を先取りしない)。
- 未対応 request 経路を対象に含める (0274 の委譲に応える)。`handle_unsupported_request` の
  一意性検証も alias 解決後まで広げるが、未登録 alias の扱いは現行のまま変えない。
- SETUP の AUTHORIZATION_TOKEN の検証 (0239) とは共通化しない。SETUP は DELETE / USE_ALIAS を
  許さず alias 解決が不要で、入力 (`SetupOptions`) と失敗時の扱い (セッション終了) も異なる。
  0237 は 0239 の完了を待たずに実装する。既存の
  `pub(crate) fn validate_auth_token_uniqueness` は alias 解決を要さない範囲の検証として残す。
- codec 層の `MessageParameters::validate_auth_token_uniqueness` は残し、
  `MessageParameters::encode` の送信値検証で使い続ける。doc の「(現状未実装。...セッション層で
  判定する必要がある)」を、追加する session 層の検証を指す記述に更新する。
- 次の記述を実装に合わせて更新する。
  - `src/session/core.rs` の `Session::accept_peer_request` / `Session::handle_unsupported_request`
    の doc とコメント
  - `src/session/subscription/recv.rs` の `Session::handle_peer_request_update` のコメント
  - `tests/test_message_parameter.rs` の `validate_auth_token_uniqueness_ignores_use_alias` の
    コメント
  - `pbt/tests/prop_message_parameter.rs` の「現状の検証対象外」記述
  - `docs/moqt.md` の未対応 request の §8.9 の記述
  - `docs/missing-features.md` の「認可トークンの alias 解決後の一意性」の行 (実装済みに
    なるため削除する)

## 完了条件

- 同一メッセージ内で alias 解決後に (Token Type, Token Value) が重複する次の組を、session が
  REQUEST_ERROR (`MALFORMED_AUTH_TOKEN`) で拒否し、セッションを維持すること。
  - 同一 alias を参照する USE_ALIAS の繰り返し
  - USE_ALIAS の解決結果と USE_VALUE の一致
  - 同一メッセージ内の先行 REGISTER とそれを参照する USE_ALIAS の一致
  - 異なる alias の USE_ALIAS / REGISTER が同じ (Token Type, Token Value) に解決する併用
- 上記が SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS (受理経路)、REQUEST_UPDATE (受理経路)、
  未対応 request (拒否経路) で固定されていること。ワイヤバイト列の decode を経由する経路でも
  固定すること。
- 一意な USE_ALIAS だけを持つ request が従来どおり受理されること。
- 未登録 alias の挙動が経路ごとに変わらないこと。
  - 受理経路では `UNKNOWN_AUTH_TOKEN_ALIAS` (0x17) でセッションを閉じる。
  - 未対応 request の拒否経路では `GOING_AWAY` / `NOT_SUPPORTED` で拒否しセッションを維持する。
  - GOAWAY 拒否経路でもセッションを維持する。
  - ローカル終端済み request は応答せず吸収する。
- 未登録 alias と重複が同居するメッセージでは、`MALFORMED_AUTH_TOKEN` のメッセージ単位の
  reject が優先されること (doc とテストで固定する)。
- doc と実装が一致していること。「設計方針」に列挙した doc・コメントを更新すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
