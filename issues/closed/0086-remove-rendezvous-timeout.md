# relay 専用の RENDEZVOUS_TIMEOUT を削除する

- Created: 2026-09-15
- Completed: 2026-09-15
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

## 解決方法

relay 専用の RENDEZVOUS_TIMEOUT parameter を codec 層と session 層の両方から削除した。

- `src/message_parameter.rs` から `PARAM_RENDEZVOUS_TIMEOUT` と `MessageParameters::rendezvous_timeout`、`value_encoding` の該当分岐を削除した。
- `src/message.rs` の `SUBSCRIBE_ALLOWED_PARAMS` から該当要素を削除した。
- `src/session/types.rs` の `Subscription::subscriber_rendezvous_timeout_ms` を削除し、値を組み立てていた `src/session/subscription/{send,recv,delivery}.rs` の該当箇所を削除した。このフィールドは削除前からライブラリ内部で読まれておらず、書き込み専用だった。
- 削除後は当該パラメータが未知の型として拒否される。`tests/test_message.rs` に型 0x04 を含む SUBSCRIBE が `ProtocolViolation` で拒否されることを固定するテストを追加した。
- 保持を検証していた 3 テスト (`rendezvous_timeout_is_preserved_for_both_subscribe_sides` / `rendezvous_timeout_zero_is_not_normalized_away` / `subscribe_with_rendezvous_timeout_is_accepted`) と、`tests/test_session.rs` の `rendezvous_timeout_params` ヘルパを削除した。
  - `tests/test_session/parameter_rules.rs` のスコープ外パラメータ検証は、代表を TRACK_NAMESPACE_PREFIX に差し替えて検証を維持した。
- `docs/IMPLEMENTATION.md` のパラメータ一覧と `skills/shiguredo-moqt/SKILL.md` のパラメータ型定数表から該当行を削除し、`CHANGES.md` の `## develop` に `[CHANGE]` エントリを追加した。

検証は `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` がすべて通ることを確認した。削除規模は 15 ファイル、120 行削除である。
