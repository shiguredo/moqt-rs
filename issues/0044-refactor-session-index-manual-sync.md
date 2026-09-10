# session の subscription / request 台帳の手動同期を安全にする

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-session-index-manual-sync
- Polished: 2026-09-10

## 目的

`Session` が持つ購読系索引と bidi request stream 種別台帳の同期漏れ・二重管理を型で防ぎ、alias リークや誤った stream dispatch を起こしにくくする。

## 現状

- subscription 本体は `subscriptions`、alias 系は `src/session/core.rs` の `struct AliasState`（`my_publisher_aliases` / `peer_publisher_aliases` / `subscriptions_by_track` / `peer_alias_tombstones`）、bidi request stream 種別は `request_streams` が別々に管理される。
- 1 つの subscription は `subscriptions` / `subscriptions_by_track` / `request_streams` と、`my_role` に応じて `my_publisher_aliases` か `peer_publisher_aliases` の一方に登録される（alias 索引は publisher 役 / subscriber 役で排他）。
- `forget_subscription`（`src/session/subscription.rs`）は購読系索引に加え、`peer_alias_tombstones`、`SubgroupTracker`（`my_subgroups` / `peer_subgroups`）、`data_streams`（incoming / outgoing）、`control_message_deadlines`、request update credit などの後始末を手で行う。同期漏れが alias リークや `SubgroupTracker` の誤クリーンアップに直結する。
- `request_streams` は購読専用ではなく、`RequestKind` の 7 種類すべての bidi request stream を dispatch するための索引である（`src/session/core.rs` の `request_streams` doc を参照）。
- `RequestKind`（`src/session/types.rs`、7 変種）と `RequestTable`（`src/session/core.rs`、6 変種）が似た種別集合を表す。`RequestKind` の `Subscribe` / `Publish` はどちらも `RequestTable::Subscription` に対応する多対一で、逆変換はできない。両者を変換する関数が無く、手書きの match が複数箇所にある。

対象シンボル: `Session` / `AliasState` / `subscriptions` / `subscriptions_by_track` / `my_publisher_aliases` / `peer_publisher_aliases` / `peer_alias_tombstones` / `request_streams` / `forget_subscription` / `locate_request` / `RequestKind` / `RequestTable`。

## 設計方針

- 購読系索引の追加・削除を単一経路に閉じる。`AliasState` が既に alias 系マップをまとめているため、これを起点に `forget_subscription` が触る後始末の範囲を明示する。
- `request_streams` は全 request 共通の索引であり、購読専用構造体に混ぜない。現状の別管理を維持する。
- 「削除時に触る対象」を、購読索引・alias・tombstone に閉じる範囲と、data stream / timing / `SubgroupTracker` にまたがる範囲に切り分けて明記する。
- `RequestKind` → `RequestTable` の一方向変換関数を 1 箇所に定義し、手書き match を減らす。`locate_request` の探索源は state テーブルのまま維持し、`request_streams` で置き換えない。`request_streams` は終端処理で先行削除されうるため、置き換えると `locate_request` が `None` を返して挙動が変わる。多対一のため逆変換は定義しない。
- 公開型 `RequestKind` に公開メソッドを追加する場合は `CHANGES.md` に追記する。内部に閉じる実装に留める場合は `CHANGES.md` 不要と判断した旨を PR 本文に書く。

## 依存

- `issues/0009-draft-21-subscribe-tracks-stream-end-request-streams.md` は `close_track_subscription_on_stream_end` が `request_streams` を先行削除する不具合を扱う。0009 の修正が `request_streams` の保持期間を変えるため、0009 の修正を前提として取り込み、本 issue の完了条件「挙動が変わらないこと」はその修正後を基準とする。

## 完了条件

- 購読系索引の追加・削除が単一経路に集約されていること
- `request_streams` は全 request 共通索引として現状の管理を維持し、購読専用構造体に混在していないこと
- `RequestKind` → `RequestTable` の変換が 1 箇所に集約され、逆変換を定義していないこと
- `locate_request` の探索源が state テーブルのまま変わっていないこと
- 挙動が変わらないこと、既存テスト / PBT がすべて通ること
