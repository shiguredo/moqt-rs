# FETCH の観測 Largest Object を全 publisher subscription から求める

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-fetch-largest-all-subscriptions
- Polished: 2026-09-10

## 目的

同一 Track に複数の publisher 役 subscription が存在する場合でも、FETCH の INVALID_RANGE 判定と開始位置検証を正しく行う。Largest Object は Track 単位の値である。

## 現状

`src/session/fetch.rs` の `handle_peer_fetch` は、`subscriptions_by_track` の先頭 1 件だけを見て `observed_largest` を求める。

```rust
let observed_largest = self.aliases.subscriptions_by_track.get(&key)
    .and_then(|ids| ids.first())
    .and_then(|&sub_request_id| self.subscriptions.get(&sub_request_id))
    .and_then(|sub| { ... effective_largest_object(sub) });
```

先頭が未観測なら `observed_largest` が `None` になり、`has_track_subscription && observed_largest.is_none()` で誤って INVALID_RANGE を返す。先頭が他より小さければ `start > largest` を誤判定する。

`src/session/subscription/fill.rs` の `publisher_track_largest` は全候補の `.max()` を取っており、非対称。

根拠 (draft-ietf-moq-transport-21 §9.11 / §3.1.3): Largest Object は Track 単位。

## 設計方針

`publisher_track_largest` と同じく、該当 Track の全 publisher 役 subscription の `effective_largest_object` の max を取る。共通ヘルパーへ集約する。

## 完了条件

- 複数 publisher 役 subscription がある Track で最大の Largest Object が使われること
- 先頭が未観測でも他が観測済みなら INVALID_RANGE にならないこと
- 複数 subscription を張るテストが `tests/test_session/fetch/` に追加されていること
