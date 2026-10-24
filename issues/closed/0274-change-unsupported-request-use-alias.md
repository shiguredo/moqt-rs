# 未対応 request の USE_ALIAS を §8.9 に沿って扱うかを決める

- Created: 2026-10-15
- Completed: 2026-10-25
- Branch: feature/change-unsupported-request-use-alias
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression) は "The receiver of a message
referencing an Alias that is not currently registered MUST reject the message with
UNKNOWN_AUTH_TOKEN_ALIAS." と MUST を課す。未対応 request (PUBLISH_NAMESPACE /
SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS) の拒否経路でこの MUST をどう扱うかを決め、判断と根拠を
コードとテストに残す。

## 現状

- `src/session/core.rs` の `Session::handle_unsupported_request` は拒否の前に `apply_peer_message_auth_token_registers_only` を呼ぶ。この関数は draft §8.9 の REGISTER 適用 MUST だけを満たし、DELETE / USE_ALIAS / USE_VALUE は「失敗するメッセージのパラメータ」として適用しない。
- そのため、他の拒否条件に該当しない場合、未登録 alias を参照する USE_ALIAS を含む未対応 request は、`UNKNOWN_AUTH_TOKEN_ALIAS` (0x17、draft §12.2 (Session Termination Codes)) ではなく `REQUEST_ERROR` の `NOT_SUPPORTED` で拒否され、セッションは維持される。control GOAWAY を送信済みなら `GOING_AWAY` で拒否される。
- `UNKNOWN_AUTH_TOKEN_ALIAS` (0x17) は draft §12.2 (Session Termination Codes) にのみ定義され、§12.3 (Request Error Codes) には無い。そのため `UNKNOWN_AUTH_TOKEN_ALIAS` を返す場合はセッション終了になり、メッセージ単位の `REQUEST_ERROR` としては使えない。
- 同じ方針は control GOAWAY 送信後の拒否経路でも採っており、`tests/test_session/goaway.rs` の `goaway_sent_side_use_alias_after_goaway_does_not_kill_session` が先例である。受理する request では `apply_peer_message_auth_tokens` が `UNKNOWN_AUTH_TOKEN_ALIAS` でセッションを閉じるため、受理経路と拒否経路で非対称になっている。
- 現行方針は `tests/test_session/request_stream.rs` の `unsupported_request_with_unknown_alias_is_rejected_with_not_supported` でテストに固定されている。判断の根拠は `apply_peer_message_auth_token_registers_only` の doc にあり、テストのコメントも同じ判断を要約している。
- alias 解決後も含めた (Token Type, Token Value) の一意性検証は `issues/closed/0237-bug-auth-token-alias-uniqueness.md` で実装済みである。`Session::validate_peer_message_auth_token_uniqueness` が `handle_unsupported_request` でも呼ばれ、重複時は `MALFORMED_AUTH_TOKEN` でメッセージ単位に拒否する。本 issue はこの一意性検証を変更しない。

## 設計方針

次のどちらを採るかを決め、選んだ理由を `apply_peer_message_auth_token_registers_only` と `Session::handle_unsupported_request` の doc と issue の「解決方法」に記録する。本 issue の変更対象は未対応 request の拒否経路 (`Session::handle_unsupported_request`) であり、control GOAWAY 送信後の supported request の拒否経路 (`Session::accept_peer_request`) は先例として挙動を確認するが変更しない。

- (a) 現行方針を維持する。失敗するメッセージのパラメータは REGISTER を除いて適用しないため、未登録 alias の USE_ALIAS も解決せず `NOT_SUPPORTED` で拒否する。draft §1.6 (Modularity) の NOT_SUPPORTED (SHOULD) は未対応メッセージ全般への応答を定める。§8.9 の REGISTER 適用 MUST は "even if the message fails for other reasons" と失敗するメッセージにも及ぶことを明記する一方、USE_ALIAS の MUST にはこの限定が無い。したがって USE_ALIAS の MUST は受理するメッセージに対する規定として読む。
- (b) §8.9 の USE_ALIAS に対する MUST を文字どおり適用する。USE_ALIAS を含む未対応 request は alias を解決し、未登録なら `UNKNOWN_AUTH_TOKEN_ALIAS` でセッションを閉じる。`UNKNOWN_AUTH_TOKEN_ALIAS` は §12.3 に無いため、メッセージ単位の `REQUEST_ERROR` ではなくセッション終了になる。受理経路と対称になる一方、未対応 request でセッションを落とすことになる。DELETE / USE_VALUE の扱いは現行どおり (REGISTER のみ適用) とし、本 issue では変更しない。

どちらを採る場合も、alias 解決後も含めた (Token Type, Token Value) の一意性検証は 0237 で実装済みであり、本 issue では扱わない。0237 は `Session::validate_peer_message_auth_token_uniqueness` を追加し、`handle_unsupported_request` でも重複を `MALFORMED_AUTH_TOKEN` で拒否する経路を実装済みである。本 issue は未登録 alias の扱いだけを決める。

## 完了条件

- 採った方針と根拠が `apply_peer_message_auth_token_registers_only` と `Session::handle_unsupported_request` の doc に書かれていること。
- 方針がテストで固定されていること。(b) を採る場合は `UNKNOWN_AUTH_TOKEN_ALIAS` でセッションが閉じるケースを追加し、既存の `unsupported_request_with_unknown_alias_is_rejected_with_not_supported` も `UNKNOWN_AUTH_TOKEN_ALIAS` でセッションが閉じる期待に更新する。(a) を採る場合は現行テストに「`NOT_SUPPORTED` であり `UNKNOWN_AUTH_TOKEN_ALIAS` を返さない」ことを明記する。
- `docs/moqt.md` の §8.9 の記述が採った方針と一致していること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

現行方針 (a) を維持する。未対応 request の拒否経路では、失敗するメッセージのパラメータを
REGISTER 以外適用しないため、未登録 alias を参照する USE_ALIAS も解決せず
`REQUEST_ERROR` の `NOT_SUPPORTED` で拒否し、セッションを維持する。control GOAWAY を
送信済みなら `GOING_AWAY` で拒否する。

- draft §1.6 (Modularity) の NOT_SUPPORTED (SHOULD) は未対応メッセージ全般への応答を定める。
  §8.9 の REGISTER 適用 MUST は "even if the message fails for other reasons" と失敗する
  メッセージにも及ぶことを明記する一方、USE_ALIAS の MUST にはこの限定が無い。したがって
  USE_ALIAS の MUST は受理するメッセージに対する規定として読む。
- `UNKNOWN_AUTH_TOKEN_ALIAS` (0x17) は §12.2 (Session Termination Codes) にのみ定義され
  §12.3 (Request Error Codes) に無いため、メッセージ単位の `REQUEST_ERROR` ではなく
  セッション終了になる。
- 現行コードは既に (a) を実装している。`src/session/core.rs` の
  `Session::handle_unsupported_request` は `apply_peer_message_auth_token_registers_only` を
  呼び、REGISTER だけを適用する。`apply_peer_message_auth_token_registers_only` の doc は
  DELETE / USE_ALIAS / USE_VALUE を「失敗した request のパラメータ」として適用しない理由を
  記録している。
- `tests/test_session/request_stream.rs` の
  `unsupported_request_with_unknown_alias_is_rejected_with_not_supported` は、未登録 alias を
  参照する USE_ALIAS を含む未対応 request が `REQUEST_NOT_SUPPORTED` で拒否され、
  セッションが `Established` のままであることを固定している。
  `tests/test_session/goaway.rs` の
  `goaway_sent_side_use_alias_after_goaway_does_not_kill_session` は GOAWAY 拒否経路の先例である。
- alias 解決後も含めた (Token Type, Token Value) の一意性検証は
  `issues/closed/0237-bug-auth-token-alias-uniqueness.md` で実装済みであり、本 issue では
  変更しない。`Session::validate_peer_message_auth_token_uniqueness` が
  `handle_unsupported_request` でも呼ばれ、重複時は `REQUEST_MALFORMED_AUTH_TOKEN` で
  メッセージ単位に拒否する。
- `docs/moqt.md` の §8.9 の記述は「未登録 alias の扱いは経路ごとの現行挙動のまま変えない」
  としており、(a) と一致している。
- `cargo fmt --all -- --check`、`cargo test -p shiguredo_moqt --lib`、
  `cargo clippy -p shiguredo_moqt --all-targets -- -D warnings` は通った。
  `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` は
  この環境に libpulse が無いため `examples/moq-pub` のビルドで止まるが、コード変更は無い。

現行コードが (a) を実装済みであり、doc とテストも (a) を固定しているため、本 issue は
実装済みとして closed にする。コード変更は不要である。
