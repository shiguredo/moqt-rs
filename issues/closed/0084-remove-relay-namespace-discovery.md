# relay 専用の namespace 発見・告知機構を削除する

- Created: 2026-09-15
- Completed: 2026-09-15
- Branch: feature/remove-relay-namespace-discovery
- Polished: {YYYY-MM-DD}

## 目的

relay を実装する予定がないため、relay 専用の namespace 発見・告知機構 (SUBSCRIBE_NAMESPACE / PUBLISH_NAMESPACE / SUBSCRIBE_TRACKS とその応答) を削除し、ライブラリを client / server の endpoint 専用に絞る。使わない状態機械とテストを保守し続けるコストを落とすことが目的である。

## 現状

codec 層と session 層の両方で 4 種の request をフル実装している。

- draft-ietf-moq-transport-21 §4.1 (Subscribing to Namespaces): SUBSCRIBE_NAMESPACE は「subscriber が publisher/relay へ送る namespace 発見」であり、relay 前提の信頼モデルを持つ。
  - 同節: "By sending SUBSCRIBE_NAMESPACE, the subscriber indicates that it trusts the relay to be authoritative for namespaces matching the requested prefix."
- draft-ietf-moq-transport-21 §4.2 (Publishing Namespaces): NAMESPACE を送るのは relay だけである。
  - 同節: "a relay that has received an authorized PUBLISH_NAMESPACE for that namespace from an upstream publisher" が NAMESPACE を送る。
- draft-ietf-moq-transport-21 §4.3.1 の見出しは「Relay Resource Protection in Large Namespaces」であり、SUBSCRIBE_TRACKS は relay の多テナント資源保護を目的とする。

実装箇所は次のとおり。

- codec: `ControlMessage` の `PublishNamespace` / `SubscribeNamespace` / `SubscribeTracks` / `Namespace` / `NamespaceDone` と `PublishSkipped` (すべて `src/message.rs`)
- セッション: `src/session/namespace.rs` と `src/session/namespace/{publish_namespace,subscribe_namespace,track_subscription}.rs` (合計 2,634 行)
- 状態と dispatch: `src/session/core.rs` の `NamespaceState` / `track_subscriptions` / `pending_prefix_updates` / `track_prefix_history` / `RequestTable` の `NamespacePublication` / `NamespaceSubscription` / `TrackSubscription`
- dispatch とイベント: `src/session/subscription/{send,recv,dispatch}.rs` の `RequestTable` 分岐と、次の型・イベントである。
  - `src/session/types.rs` の `RequestKind` / `NamespacePublication` / `NamespaceSubscription` / `TrackSubscription`
  - `src/session/types.rs` の `SessionEvent` の `NamespaceReceived` / `NamespaceDoneReceived` / `PublishSkippedReceived` / `SubscribeTracksReceived`
  - `src/session/types.rs` の `TerminationReason::NamespaceImplicitDone`
- テスト: 次のファイルに散っている。
  - `tests/test_session/namespace/` と `pbt/tests/prop_session/namespace.rs` (合計 7,269 行)
  - `tests/test_session/{parameter_rules,request_stream,goaway,track_property_filter,include_properties,outgoing_range_filter}.rs`
  - `tests/test_session/subscription/{request_update,publish_params,subscription_limits,range_filter_scope_order}.rs` と `pbt/tests/prop_message.rs`

## 設計方針

- 4 種の request とそれに付随するメッセージ・状態・イベント・公開 API を codec 層と session 層の両方から削除する。
- 削除する公開 API は次のとおりである。
  - namespace 告知: `send_publish_namespace` / `namespace_publication` / `namespace_publications` / `forget_namespace_publication`
  - namespace 発見: `send_subscribe_namespace` / `namespace_subscription` / `namespace_subscriptions` / `forget_namespace_subscription` / `send_namespace` / `send_namespace_done`
  - track 発見: `send_subscribe_tracks` / `track_subscription` / `track_subscriptions` / `forget_track_subscription` / `send_publish_skipped`
- `RequestKind` は `Subscribe` / `Publish` / `Fetch` / `TrackStatus` の 4 種にし、`RequestTable` をそれに合わせて縮小する。`RequestKind::Subscribe` と `Publish` の dispatch 分岐も、削除対象だった `handle_peer_publish` 内の `TrackSubscription` 経由 PUBLISH の紐付け処理を除いて現状維持とする。
- `PUBLISH_STATE_NOTIFY` は subscription の機構であり本削除の対象外とする (`src/session/subscription/recv.rs` の `handle_peer_publish_state_notify` は残す)。
- `REQUEST_PREFIX_OVERLAP` など wire 定義のエラーコード定数は `src/error.rs` に残す。draft に定義されたコードをライブラリから消す理由がないためである。
- 未知の request は既存の `recv_request` の fallback 経路で扱う。削除後に TRACK_STATUS 以外の未対応 request が届いた場合は、既存どおり `SESSION_PROTOCOL_VIOLATION` でセッションを閉じる。
- `tests/test_session/namespace/` と `pbt/tests/prop_session/namespace.rs` は削除する。他のテストファイルに散る該当ケースは削除し、無関係なケースが巻き添えで消えないことをレビューで確認する。
- 他の削除 issue (TRACK_STATUS 受信側の削除、RENDEZVOUS_TIMEOUT の削除) と作業範囲が重なるため、本 issue の実装を先に完了させてからそれらを実施する。

## 完了条件

- `ControlMessage` から namespace 発見・告知系の 6 variant が削除されていること
- `src/session/namespace.rs` と `src/session/namespace/` が削除されていること
- `Session` から namespace 発見・告知系の状態と公開 API が削除され、残る `RequestKind` が `Subscribe` / `Publish` / `Fetch` / `TrackStatus` の 4 種であること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
- `docs/IMPLEMENTATION.md` のコントロールメッセージ表と `README.md`、`skills/shiguredo-moqt/SKILL.md` の記述が削除後の実装と一致していること
- `CHANGES.md` の `## develop` に `[CHANGE]` エントリが追加されていること

## 解決方法

codec 層と session 層の両方から namespace 発見・告知機構を削除した。

- `src/message.rs` から `ControlMessage` の 6 variant (`PublishNamespace` / `SubscribeNamespace` / `SubscribeTracks` / `Namespace` / `NamespaceDone` / `PublishSkipped`) と対応するメッセージ型、メッセージ型定数、許可パラメータ定数を削除した。
- `src/session/namespace.rs` と `src/session/namespace/` を削除した。TRACK_STATUS の受信側は本 issue の対象外のため `src/session/track_status.rs` へ移設し、`pub mod track_status` として登録した。
- `Session` から namespace 発見・告知系の状態と公開 API を削除した。
  - 状態: `NamespaceState` / `track_subscriptions` / `pending_prefix_updates` / `track_prefix_history`
  - 公開 API 15 メソッド、`RequestTable` の該当 3 variant、`RequestKind` の該当 3 variant
  - `SessionEvent` の該当 4 variant と `TerminationReason::NamespaceImplicitDone`
  - `RequestKind` は `Subscribe` / `Publish` / `Fetch` / `TrackStatus` の 4 種になった。
- 共有ヘルパは削除ではなく移設した。`terminationreason_from_end` は `src/session/types.rs` へ、`prefix_overlaps` は `src/session/subscription/validation.rs` へ移し、後者はライブラリ本体で使わないため `#[cfg(test)]` とした。
- `handle_peer_publish` は SUBSCRIBE_TRACKS 経由の PUBLISH 紐付けが不要になったため戻り値を `Result<bool, SessionError>` から `Result<(), SessionError>` に変えた。
- `tests/test_session/namespace/` と `pbt/tests/prop_session/namespace.rs` を削除した。他のテストファイルに散る該当ケースは関数単位で削除し、SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS の `.session` 拒否テストと track-scoped request の Redirect テストは残した。`tests/test_session/track_property_filter.rs` は全 10 テストが SUBSCRIBE_TRACKS 前提のため削除した。
- `docs/IMPLEMENTATION.md` / `README.md` / `skills/shiguredo-moqt/SKILL.md` を削除後の実装に合わせ、`CHANGES.md` の `## develop` に `[CHANGE]` エントリを追加した。

検証は `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` がすべて通ることを確認した。削除規模は 41 ファイル、12,316 行削除である。
