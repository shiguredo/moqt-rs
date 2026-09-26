# GOAWAY の request stream deadline 満了後に遅延状態が残る

- Created: 2026-09-26
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-goaway-deadline-deferred-state
- Polished: {YYYY-MM-DD}

## 目的

[issues/closed/0141](../issues/closed/0141-bug-publish-sender-peer-fin-termination.md) で、PUBLISH の送信側は peer FIN を受信しても PUBLISH_DONE を送るまで終端を遅延するようになった。GOAWAY 受信後の request stream には reset deadline があり、deadline が満了してストリームを閉じた場合に
遅延状態 (`peer_fin_received` と Established のままの subscription、保留中の `pending_publish_done`) が
整理されるかを確認し、必要なら整理する。

## 現状

- GOAWAY 受信時に request stream へ reset deadline を設定し、満了時に `SessionEvent::ResetRequestStream` を発行する (`src/session/goaway.rs` / `Session::tick`)
- 0141 の変更で、`peer_fin_received` に登録された subscription は PUBLISH_DONE を送るまで `Established` を維持する
- deadline 満了で request stream を reset した場合に、この遅延状態と `request_id` ごとの `RequestTerminated` がどうなるかは未検証である (要確認)

## 設計方針

- deadline 満了でストリームを閉じた時点を「PUBLISH_DONE を送れない終端」とみなし、遅延中の subscription を終端して `RequestTerminated { reason: ... }` を 1 回だけ発行する
  - 終端理由は §6.4.2.3 (Request Cancellation and Rejection) の扱いに合わせて決める (GOAWAY による終端であることが分かる理由にする)
- `pending_publish_done` がある場合は破棄する (§9.9 の MUST NOT により stream を閉じた後に PUBLISH_DONE を送れないため)
- `peer_fin_received` / `local_fin_sent` / `pending_publish_done` の後始末を 1 箇所にまとめ、`forget_subscription` との重複を避ける
- 状態の整理漏れを PBT または例示テストで固定する

## 完了条件

- GOAWAY の deadline 満了で request stream を閉じたときに、遅延中の subscription が終端し `RequestTerminated` が 1 回だけ発行されることを固定するテストが追加されていること
- 保留中の PUBLISH_DONE が破棄され、その後に送信されないことを固定するテストが追加されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
