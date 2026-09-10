# session の subscription 索引の手動同期を安全にする

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-session-index-manual-sync

## 目的

`Session` が持つ subscription 関連索引の同期漏れを型で防ぎ、alias リークや tracker の誤クリーンアップを起こしにくくする。

## 現状

1 つの subscription が `subscriptions` / `subscriptions_by_track` / `my_publisher_aliases` / `peer_publisher_aliases` / `request_streams` に重複登録され、追加・削除のたびに全索引を同期する。`src/session/subscription.rs` の `forget_subscription` は 5 つのマップを手で掃除しており、同期漏れが alias リークや `SubgroupTracker` の誤クリーンアップに直結する。

あわせて、ほぼ同じ種別集合を表す `RequestTable` (`src/session/core.rs`) と `RequestKind` (`src/session/types.rs`) が 2 つ存在し、`locate_request` と `request_streams` の両方で管理している。役割差は doc にあるが、変換ミスを型で防げない。

## 設計方針

subscription 索引を 1 つの構造体にまとめ、insert / remove を 1 メソッドに閉じる。少なくとも「削除時に触る索引一覧」を型で強制する。`RequestTable` と `RequestKind` は変換関数を用意するか、`RequestKind` に `table()` を持たせて `locate_request` を一本化する。

## 完了条件

- 索引の追加・削除が単一経路に集約されていること
- `RequestTable` / `RequestKind` の変換が一箇所に集約されていること
- 既存テスト / PBT がすべて通ること
- 挙動が変わらないこと
