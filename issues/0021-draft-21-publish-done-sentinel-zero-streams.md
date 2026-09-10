# PUBLISH_DONE の Stream Count で 0 stream 時の sentinel を禁止する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-publish-done-sentinel-zero-streams

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

`published_stream_count == 0` のときは `stream_count == 0` のみ許可する。`published_stream_count > 0` のときは sentinel を許可する。PBT の `should_accept` とコメントをコードに合わせて修正する。

## 完了条件

- `published_stream_count == 0` で sentinel を渡すと拒否されること
- `published_stream_count == 0` で 0 は許可されること
- PBT の `should_accept` がコードと一致すること
