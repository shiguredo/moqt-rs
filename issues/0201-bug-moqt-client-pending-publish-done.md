# moqt_client の cleanup が保留中の PUBLISH_DONE を見ずに subscription を破棄しうる

- Created: 2026-10-06
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-moqt-client-pending-publish-done
- Polished: 2026-10-06

## 目的

`Session::forget_subscription` は、保留中の PUBLISH_DONE (`Subscription::pending_publish_done`) が
ある間は呼んではならない (順序契約として `Session::forget_subscription` の doc と
`skills/shiguredo-moqt/SKILL.md` に明記されている)。呼ぶと PUBLISH_DONE が送信されず、
draft-ietf-moq-transport-22 §9.5.1 (Updating Subscriptions) の MUST
(REQUEST_UPDATE が失敗した場合、publisher は PUBLISH_DONE を送る) を果たせない。

example の `MoqtClient::cleanup_closed_requests` は `subscription_cleanup_ready` だけで破棄するため、
この契約に反する破棄が起こりうる。

## 現状

- `examples/tokio-moq/src/moqt_client.rs` の `cleanup_closed_requests` は
  `session.subscription_cleanup_ready(request_id) == Some(true)` のとき `forget_subscription` を呼ぶ
- `Subscription::cleanup_ready` (`src/session/types.rs`) は受信側の drain
  (`publish_done` / delivery timeout / `open_incoming_subgroup_count`) だけを見ており、
  `pending_publish_done` と open 中の送信 stream を見ない。そのため publisher 役の
  REQUEST_UPDATE 失敗応答で保留した PUBLISH_DONE を持つ subscription でも true になり、
  open 中の送信 stream が残っていても破棄されうる
- 現状の example の REQUEST_UPDATE 処理 (`examples/moq-pub/src/pipeline.rs` と
  `examples/moq-sub/src/pipeline.rs`) は常に REQUEST_OK を返すため、保留が発生する経路には到達しない。
  ただし `MoqtClient::send_request_error` は公開されており、失敗応答にする経路を追加すると到達する
- `Session::forget_subscription` は `pending_publish_done` を破棄してから索引を除去するため、
  破棄後に全 outgoing stream を閉じても `Session::maybe_flush_pending_publish_done` は送信しない

## 設計方針

- `cleanup_closed_requests` の subscription の破棄条件に「保留中の PUBLISH_DONE が無いこと」を加える。
  判定には `Session::subscription` が返す `Subscription::pending_publish_done` を使う
- 破棄しなかった request は `closed_request_streams` に残し、flush (すべての送信 stream が閉じた後の
  `Session::maybe_flush_pending_publish_done`) の後の cleanup で破棄する
- fetch / track_status には保留 PUBLISH_DONE に相当する状態が無いため、条件の追加は subscription だけにする
- ログと他の挙動は変えない

## 完了条件

- 保留中の PUBLISH_DONE を持つ subscription が `cleanup_closed_requests` で破棄されないこと
- 保留が flush された後に破棄されること
- 破棄条件の判定がテストで固定されていること (判定を純関数または `pub(crate)` の述語に切り出して
  単体テストを追加する。モックやスタブは使わない)
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること
