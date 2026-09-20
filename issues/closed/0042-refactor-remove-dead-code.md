# 到達しない公開コードと過剰公開を整理する

- Created: 2026-09-10
- Completed: 2026-09-17
- Branch: feature/refactor-remove-dead-code
- Polished: 2026-09-17

## 目的

死にコードと過剰公開を除去し、公開 API と内部状態の境界を明確にする。

## 現状

- `src/session/subscription/delivery.rs` の `refresh_effective_object_delivery_timeout` / `refresh_effective_subgroup_delivery_timeout` / `set_subscription_subscriber_object_delivery_timeout` / `set_subscription_subscriber_subgroup_delivery_timeout` は同ファイル内からのみ呼ばれる。
- `src/session/types.rs` の `TrackSubscription::forward_state` は受信側で書き込まれるがライブラリ内部で読まれない (`src/session/namespace/track_subscription.rs` のコメントが自己申告)。
- `examples/moqt-transport/src/moqt_client.rs` の `send_publish_done` 後の `bidi_sends.remove(&request_id)` は、直前の `drain_events` 内 `fin: true` 処理で既に remove 済みのため常に `None`。`finish()` は到達しない。

## 設計方針

- `delivery.rs` の 4 関数: private 化する。
- `TrackSubscription::forward_state`: `send_publish` の初期 FORWARD 決定に使うか、削除する。
- example の no-op `remove`: 削除する。

## 完了条件

- 到達しない公開コードが削除または統合されていること
- 内部利用のみの関数が private になっていること
- 削除による公開 API 変更が `CHANGES.md` に記載されていること
- `cargo clippy --workspace --all-targets -- -D warnings` が通ること

## 解決方法

`src/session/subscription/delivery.rs` の同ファイル内からのみ呼ばれる 4 関数を private にし、
`examples/moqt-transport/src/moqt_client.rs` の到達しない no-op `remove` を削除した。
`TrackSubscription::forward_state` は対象の型自体が既に存在しないため、現存する
`Subscription::forward_state` の利用箇所を確認して維持と判断した。挙動と公開 API は変えていない。

- `src/session/subscription/delivery.rs`: `refresh_effective_object_delivery_timeout` /
  `refresh_effective_subgroup_delivery_timeout` /
  `set_subscription_subscriber_object_delivery_timeout` /
  `set_subscription_subscriber_subgroup_delivery_timeout` から `pub` を外した。呼び出し元は
  同ファイル内の `set_subscription_publisher_object_delivery_timeout` /
  `set_subscription_publisher_subgroup_delivery_timeout` /
  `update_subscription_subscriber_delivery_timeouts_if_present` だけであり、`session` モジュール
  全体に開いていた公開範囲を同ファイル内へ縮めた。`delivery` は `pub(super)` モジュールなので
  公開 API の変更はない (外部クレートから参照するコードが `module delivery is private` の
  コンパイルエラーになることを実際に確認した)
- `TrackSubscription::forward_state`: この型は relay 専用の namespace 発見・告知機構
  (SUBSCRIBE_NAMESPACE / PUBLISH_NAMESPACE / SUBSCRIBE_TRACKS) を削除した変更で
  `src/session/types.rs` と `src/session/namespace/track_subscription.rs` ごと削除済みであり、
  現在のコードベースに存在しない。同名フィールドを持つ現行の `Subscription::forward_state` は、
  `send_subscribe` / `send_publish` が初期 Forward State を決めるために書き込み、
  REQUEST_UPDATE の送信経路が楽観的に更新し、`object_passes_filters` / `header_passes_filters`
  が draft-ietf-moq-transport-21 §3.1 (Subscriptions) の "The publisher does not send Objects if
  the Forward State is 0" の判定に読む。自側 publisher の転送可否を決める生きた状態であり、
  「送信側 (自側 publisher) の初期 FORWARD 決定に使う」に該当するため削除せず維持とした
- `examples/moqt-transport/src/moqt_client.rs`: `send_publish_done` の
  `bidi_sends.remove(&request_id)` と `finish()` を削除した。`Session::send_publish_done` は
  `SendOnStream { fin: true }` を積み、直前の `drain_events` がそのイベントを処理する時点で
  送信半を台帳から外して FIN するため、ここでの `remove` は常に `None` になり `finish()` には
  到達しない。`CloseSession` は session を `Closing` に遷移させてから積まれるため、それが
  キューの先頭にあれば `Session::send_publish_done` 自体が `Err` になり、この位置へは到達しない
  (到達しない後始末を残すと、FIN 済みの stream をもう一度閉じる意図があるように読めてしまう)
- `CHANGES.md`: `## develop` の `### misc` に `[UPDATE]` エントリを追加した。4 関数は
  `pub(super)` モジュール内にあり外部クレートから名前を解決できないため、公開 API の変更は
  なく `[CHANGE]` ではない

検証は `cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、
`cargo fmt --all -- --check`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` で行い、すべて通った。
