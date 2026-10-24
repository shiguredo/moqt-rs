# 未対応 request の USE_ALIAS を §8.9 に沿って扱うかを決める

- Created: 2026-10-15
- Completed: {YYYY-MM-DD}
- Branch: feature/change-unsupported-request-use-alias
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression) は "The receiver of a message
referencing an Alias that is not currently registered MUST reject the message with
UNKNOWN_AUTH_TOKEN_ALIAS." と MUST を課す。未対応 request (PUBLISH_NAMESPACE /
SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS) の拒否経路でこの MUST をどう扱うかを決め、判断と根拠を
コードとテストに残す。

## 現状

- `src/session/core.rs` の `Session::handle_unsupported_request` は拒否の前に `apply_peer_message_auth_token_registers_only` を呼ぶ。draft §8.9 の REGISTER 適用 MUST だけを満たし、DELETE / USE_ALIAS / USE_VALUE は「失敗するメッセージのパラメータ」として適用しない。
- そのため未登録 alias を参照する USE_ALIAS を含む未対応 request は、`UNKNOWN_AUTH_TOKEN_ALIAS` (0x17、draft §12.2 (Session Termination Codes)) ではなく `REQUEST_ERROR` の `NOT_SUPPORTED` で拒否され、セッションは維持される。
- 同じ方針は control GOAWAY 送信後の拒否経路でも採っており、`tests/test_session/goaway.rs` の `goaway_sent_side_use_alias_after_goaway_does_not_kill_session` が先例である。受理する request では `apply_peer_message_auth_tokens` が `UNKNOWN_AUTH_TOKEN_ALIAS` でセッションを閉じるため、受理経路と拒否経路で非対称になっている。
- 現行方針は `tests/test_session/request_stream.rs` の `unsupported_request_with_unknown_alias_is_rejected_with_not_supported` でテストに固定されているが、「`UNKNOWN_AUTH_TOKEN_ALIAS` を返さない」と判断した根拠は `apply_peer_message_auth_token_registers_only` の doc にしかない。

## 設計方針

次のどちらを採るかを決め、選んだ理由を `apply_peer_message_auth_token_registers_only` の doc と issue の「解決方法」に記録する。

- (a) 現行方針を維持する。失敗するメッセージのパラメータは適用しないため、未登録 alias の USE_ALIAS も解決せず `NOT_SUPPORTED` で拒否する。draft §1.6 (Modularity) の NOT_SUPPORTED (SHOULD) が先に成立し、§8.9 の MUST は「受理するメッセージ」に対する規定として読む。
- (b) §8.9 の MUST を文字どおり適用する。USE_ALIAS を含む未対応 request は alias を解決し、未登録なら `UNKNOWN_AUTH_TOKEN_ALIAS` でセッションを閉じる。受理経路と対称になる一方、未対応 request でセッションを落とすことになる。

どちらを採る場合も、alias 解決後も含めた (Token Type, Token Value) の一意性検証は `issues/0237` の範囲であり、本 issue では扱わない。0237 の実装時に `handle_unsupported_request` が追従すべき箇所があれば、その整理も本 issue の判断に含める。

## 完了条件

- 採った方針と根拠がコードの doc に書かれていること。
- 方針がテストで固定されていること。(b) を採る場合は `UNKNOWN_AUTH_TOKEN_ALIAS` でセッションが閉じるケースを追加し、(a) を採る場合は現行テストに「`NOT_SUPPORTED` であり `UNKNOWN_AUTH_TOKEN_ALIAS` を返さない」ことを明記する。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
