# PUBLISH_DONE の Stream Count で 0 stream 時の sentinel を禁止する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-publish-done-sentinel-zero-streams
- Polished: 2026-09-10

## 目的

draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE) の MUST を満たす。stream を 1 本も開いていない購読では Stream Count に 2^64-1 の sentinel を送らせない。

## 現状

`src/session/subscription/send.rs` の `send_publish_done` は次の条件で検証する。

```rust
if stream_count != PUBLISH_DONE_STREAM_COUNT_UNKNOWN
    && stream_count != published_stream_count
```

`PUBLISH_DONE_STREAM_COUNT_UNKNOWN` (2^64-1) は常に許可されるため、`published_stream_count == 0` でも sentinel を送れる。doc コメントは「published_stream_count == 0 なら stream_count == 0 のみ許可」と書いており、コードと自己矛盾している。

`pbt/tests/prop_session/subscription.rs` の `should_accept` も UNKNOWN を許可しており、コメントの意図と一致しない。

根拠 (draft-ietf-moq-transport-21 §9.9):

> "If the publisher did not open any streams for this subscription, the publisher MUST set Stream Count to 0. If the publisher is unable to set Stream Count to the exact number of streams opened for the subscription, it MUST set Stream Count to 2^64 - 1."

## 設計方針

`send_publish_done` の受理判定を次のとおりにする。

- `published_stream_count == 0` のときは `stream_count == 0` のみ許可する (draft §9.9 の「stream を開かなかった場合は MUST 0」)。
- `published_stream_count > 0` のときは `stream_count == published_stream_count` または `PUBLISH_DONE_STREAM_COUNT_UNKNOWN` を許可する (draft §9.9 の「正確な数を表明できない場合は MUST 2^64-1」の escape を残す。本ライブラリは tracking 値を持つため常に exact を表明できるが、既存の escape 経路は維持する)。
- `src/session/subscription/send.rs` の該当 doc コメント (「sentinel は tracking 値との一致比較から常に許可する」と「0 なら 0 のみ許可」の内部矛盾) を新条件に合わせて修正する。
- `pbt/tests/prop_session/subscription.rs` の `send_publish_done_stream_count_invariant` の `should_accept` と冒頭コメントを新条件に合わせて修正する。
- `published_stream_count == 0` のまま sentinel を送って Ok を期待している既存テストを新条件に追従させる:
  - `tests/test_session/subscription/publish_done.rs` の `peer_publish_done_with_sentinel_does_not_set_overrun` /
    `peer_publish_done_with_u64_max_does_not_set_overrun` は PUBLISH_DONE の受信側挙動の検証が主目的のため、
    事前に 1 本 stream を open→close して `published_count > 0` を作るか、`ControlMessage::PublishDone` を直接組み立てて
    `recv_stream_message` に渡す形へ変更する。
  - `tests/test_session/goaway.rs` の `peer_goaway_does_not_suppress_send_publish_done` は `send_publish_done` の送信自体が検証対象
    (GOAWAY 受信が PUBLISH_DONE 送信を抑制しないこと) のため、受信側への置き換えはせず、事前に 1 本 stream を open→close して `published_count > 0` を作る形に限定する。
  - `send_publish_done` の受理判定は新テストと PBT で担保する。

## 完了条件

- `published_stream_count == 0` で sentinel を渡すと拒否されること
- `published_stream_count == 0` で 0 は許可されること
- `published_stream_count > 0` で sentinel が許可されること
- PBT の `should_accept` と `src/session/subscription/send.rs` の doc コメントが実装と一致すること
- 既存の PUBLISH_DONE 関連テストが新条件に追従し、`cargo test --workspace` と PBT が通ること
