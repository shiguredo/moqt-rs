# PUBLISH 送信側の peer FIN 遅延ロジックを PBT で固定する

- Created: 2026-09-26
- Completed: {YYYY-MM-DD}
- Branch: feature/add-pbt-peer-fin-deferral
- Polished: {YYYY-MM-DD}

## 目的

[issues/closed/0141](../issues/closed/0141-bug-publish-sender-peer-fin-termination.md) で追加した「PUBLISH の送信側が subscriber の FIN を受けても購読を終端せず、PUBLISH_DONE を送れるようにする」判定 (`Session::defers_peer_fin`) は、例示テストでのみ固定されている。判定条件の組み合わせ (state / initiator / my_role / 自側 FIN 送信済み) が増えると例示テストでは漏れが出るため、PBT で不変条件として固定する。

## 現状

- `Session::defers_peer_fin` (`src/session/data.rs`) は `request_id` と `RequestKind` を取り、Publish の場合に
  `state != SubscriptionState::Pending && initiator == SubscriptionInitiator::Publisher && my_role == TrackRole::Publisher && !local_fin_sent.contains(&request_id)` を返す
- 例示テストは `tests/test_session/request_stream.rs` / `tests/test_session/subscription/publish_done.rs` / `tests/test_session/fetch/fill.rs` にある
- `pbt/tests/prop_session/request_stream.rs` は 0141 で doc のみ更新しており、遅延ロジックの不変条件は固定していない

## 設計方針

- `pbt/tests/prop_session/request_stream.rs` に、subscription の状態 (Pending(Publisher) / Established / Terminated)、`my_role`、自側 FIN の送信有無、`RequestKind` (Publish / Subscribe / Fetch) をサンプリングし、peer FIN 受信後の `SubscriptionState` と `RequestTerminated` の発行有無を不変条件として固定する
  - 遅延する条件: 購読が Pending(Publisher) でなく、PUBLISH 起点で publisher 役で、自側が FIN を送っていない
  - 遅延しない条件: 上記以外は従来どおり peer FIN の受信時点で終端する
- 既存のサンプラ (RequestKind ごとの経路) を再利用し、新しいサンプラを増やさない
- 例示テストは残す (境界の意図を読めるようにするため)

## 完了条件

- PBT が遅延する / しない条件の組み合わせを固定し、判定条件を 1 つ落とすと落ちること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
