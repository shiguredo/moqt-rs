# relay 専用の RENDEZVOUS_TIMEOUT を削除する

- Created: 2026-09-15
- Completed: {YYYY-MM-DD}
- Branch: feature/remove-rendezvous-timeout
- Polished: {YYYY-MM-DD}

## 目的

RENDEZVOUS_TIMEOUT は relay が publisher の出現を待つためのパラメータであり、relay を実装しない moqt-rs では使い道がない。書き込み専用の未使用フィールドを抱えている状態を解消し、ライブラリを client / server の endpoint 専用に絞る。

## 現状

- draft-ietf-moq-transport-21 §9.20.7 (RENDEZVOUS TIMEOUT Parameter): "It is the duration in milliseconds the subscriber is willing to wait for a publisher to become available. This applies when a relay receives a SUBSCRIBE for a Track that has no current publisher." と定義され、動作主体は relay である。
- `src/message_parameter.rs` の `PARAM_RENDEZVOUS_TIMEOUT` と `MessageParameters::rendezvous_timeout`、`SUBSCRIBE_ALLOWED_PARAMS` の要素として実装されている。
- `src/session/types.rs` の `Subscription::subscriber_rendezvous_timeout_ms` は `src/session/subscription/send.rs` の `send_subscribe` と `src/session/subscription/recv.rs` の `handle_peer_subscribe` が値を書き込むが、ライブラリ内部で読む箇所がない (書き込み専用)。
- セッション層に RENDEZVOUS_TIMEOUT を待つ処理はなく、期限管理は `tick` の対象にもなっていない。

## 設計方針

- `Subscription::subscriber_rendezvous_timeout_ms` フィールドと、それを組み立てる全箇所を削除する。
- `PARAM_RENDEZVOUS_TIMEOUT` と `MessageParameters::rendezvous_timeout`、`SUBSCRIBE_ALLOWED_PARAMS` の該当要素を削除する。
- draft に定義されたパラメータを codec 層から消す判断であるため、削除後に SUBSCRIBE に RENDEZVOUS_TIMEOUT が載った場合は `SUBSCRIBE_ALLOWED_PARAMS` のスコープ検証で拒否される。この挙動をテストで固定する。
- `src/error.rs` のエラーコード定数は削除対象としない。

## 完了条件

- `PARAM_RENDEZVOUS_TIMEOUT` と `MessageParameters::rendezvous_timeout`、`Subscription::subscriber_rendezvous_timeout_ms` が削除されていること
- SUBSCRIBE のパラメータスコープ検証で RENDEZVOUS_TIMEOUT が拒否されることがテストで固定されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
- `docs/IMPLEMENTATION.md` のパラメータ表から RENDEZVOUS_TIMEOUT の記述が削除され、削除後の実装と一致していること
- `CHANGES.md` の `## develop` に `[CHANGE]` エントリが追加されていること
