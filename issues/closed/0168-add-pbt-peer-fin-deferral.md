# PUBLISH 送信側の peer FIN 遅延ロジックを PBT で固定する

- Created: 2026-09-26
- Completed: 2026-10-06
- Branch: feature/add-pbt-peer-fin-deferral
- Polished: 2026-09-28

## 目的

[issues/closed/0141](../issues/closed/0141-bug-publish-sender-peer-fin-termination.md) で追加した「PUBLISH の送信側が subscriber の FIN を受けても購読を終端せず、PUBLISH_DONE を送れるようにする」判定 (`Session::defers_peer_fin`) は、例示テストでのみ固定されている。判定条件の組み合わせ (state / initiator / my_role / 自側 FIN 送信済み) が増えると例示テストでは漏れが出るため、PBT で不変条件として固定する。

## 現状

- `Session::defers_peer_fin` (`src/session/core.rs`) は `request_id` と `RequestKind` を取り、Publish の場合に
  `state != SubscriptionState::Pending && initiator == SubscriptionInitiator::Publisher && my_role == TrackRole::Publisher && !local_fin_sent.contains(&request_id)` を返す
- 例示テストは `tests/test_session/request_stream.rs` / `tests/test_session/subscription/publish_done.rs` / `tests/test_session/fetch/fill.rs` にある
- `pbt/tests/prop_session/request_stream.rs` は 0141 で doc のみ更新しており、遅延ロジックの不変条件は固定していない

## 設計方針

- `pbt/tests/prop_session/request_stream.rs` に、subscription の状態 (Pending(Publisher) / Established / Terminated)、`my_role`、自側 FIN の送信有無、`RequestKind` (Publish / Subscribe / Fetch) をサンプリングし、peer FIN 受信後の `SubscriptionState` と `RequestTerminated` の発行有無を不変条件として固定する
  - 遅延する条件: 購読が Pending(Publisher) でなく、PUBLISH 起点で publisher 役で、自側が FIN を送っていない
  - 遅延しない条件 (PUBLISH 起点に限る): 上記以外は従来どおり peer FIN の受信時点で終端する
- 既存のサンプラ (`sample_end` / `sample_fin_order`) と RequestKind ごとのセットアップ経路 (既存テストと同じ `send_*` / `recv_*` の組み合わせ) を再利用し、新しいサンプラを増やさない
- 例示テストは残す (境界の意図を読めるようにするため)

## 解決方法

`pbt/tests/prop_session/request_stream.rs` に PBT
`publish_sender_defers_peer_fin_by_state_role_and_local_fin` を追加し、PUBLISH を送った側の
peer FIN 遅延 (`Session::defers_peer_fin`) の判定条件を不変条件として固定した。

- 既存のサンプラ `sample_end` (FIN / RESET_STREAM) と `noprop::sample_bool` を組み合わせ、
  「自側が PUBLISH を送った側か」「REQUEST_OK (PUBLISH_OK) 受信済みか」「最終メッセージ
  (PUBLISH_DONE) 送信済みか」をサンプリングする (新しいサンプラは増やしていない)
- 遅延するのは「自側が PUBLISH を送った側」「REQUEST_OK 受信済み」「最終メッセージ未送信」
  「peer の FIN」がすべて成立する場合だけであり、その場合は subscription が `Established` の
  まま残り `RequestTerminated` を発行せず、その後 PUBLISH_DONE を送れること
  (送信で `PeerStreamFin` が確定すること) を検証する
- それ以外は peer の終端で `Terminated` になり、理由が終端種別 (FIN → `PeerStreamFin` /
  RESET_STREAM → `PeerStreamReset`) に対応することを検証する
- 到達確認として、遅延する組み合わせ / `Pending(Publisher)` / 最終メッセージ送信後の
  `Terminated` / subscriber 役 がそれぞれ 1 度以上サンプルされることを seed 付きで検証する
- 判定条件のうち `state != Pending(Publisher)` と `my_role == Publisher` を落とすと本 PBT が
  落ちることを確認した。`initiator == Publisher` は RequestKind::Publish では常に成立し、
  最終メッセージ未送信の条件は送信後に両方向が閉じて同じ結果になるため、いずれも防御であり
  単独では観測できない (その旨をテストの doc に明記した)
- 例示テストは残した。REQUEST_UPDATE 失敗応答で PUBLISH_DONE を保留した `Terminated` は
  本 PBT では作らず、例示テスト (`tests/test_session/subscription/request_update.rs`) が固定する

## 完了条件

- PBT が遅延する / しない条件の組み合わせを固定し、判定条件を 1 つ落とすと落ちること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
