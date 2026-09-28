# forget の順序契約を SKILL.md と Session::forget_subscription の doc に追記する

- Created: 2026-09-26
- Completed: {YYYY-MM-DD}
- Branch: feature/update-skill-forget-order-contract
- Polished: 2026-09-28

## 目的

[issues/closed/0141](../issues/closed/0141-bug-publish-sender-peer-fin-termination.md) の検証で、`Session::forget_subscription` を保留中の PUBLISH_DONE の flush より先に呼ぶと、送るはずの PUBLISH_DONE が失われることが分かった。利用者向けの `skills/shiguredo-moqt/SKILL.md` には forget の順序契約が書かれておらず、同じ誤用が起きうるため契約を明記する。

## 現状

- `Session::forget_subscription` (`src/session/subscription.rs`) は request ごとの状態 (`subscriptions` / `peer_object_properties` / `peer_object_fields` など) を削除する。
  呼び出せるのは `Subscription::cleanup_ready` (`src/session/types.rs`) が true のときだけだが、REQUEST_UPDATE 失敗応答で `Terminated` になり
  `Subscription::pending_publish_done` を保留したままでも cleanup_ready は true であり、
  `forget_subscription` は `pending_publish_done` を `None` にしてから残りの状態を除去する
- `Subscription::pending_publish_done` の flush は全 outgoing stream が閉じ、subscription が `Terminated` であることが条件である
  (`Session::maybe_flush_pending_publish_done`、`src/session/data.rs`)。stream を閉じないまま `forget_subscription` を呼ぶと
  保留中の PUBLISH_DONE は flush されずに破棄される
  (`tests/test_session/subscription/request_update.rs` の `pending_publish_done_discarded_on_forget_subscription` が固定している)
- `forget_subscription` は受信側の追跡状態 (`Session::peer_object_fields` の `ObjectFieldTracker` /
  `Session::peer_object_properties` の `ObjectPropertyTracker`、`src/session/data.rs` の `remove_incoming_data_streams_for_request`) も削除する。
  そのため再購読時は記録が無い状態から始まる
- `skills/shiguredo-moqt/SKILL.md` には forget の記述がなく、順序契約にも触れていない

## 設計方針

- 順序契約の一次文書は `Session::forget_subscription` の doc (`src/session/subscription.rs`) とし、次を明記する
  - 保留中の PUBLISH_DONE (`Subscription::pending_publish_done`) がある間は forget を呼ばない。
    呼ぶと全 outgoing stream を閉じても `Session::maybe_flush_pending_publish_done` は送信せず、
    §9.5.1 (REQUEST_UPDATE 失敗時、publisher は PUBLISH_DONE を送る MUST) を果たせない
    (§9.9 の MUST NOT により stream を閉じるまで送れないため)
  - forget は全 outgoing stream が閉じ、保留中の PUBLISH_DONE が送信 (flush) された後に呼ぶ
  - forget は受信側の追跡状態 (`ObjectFieldTracker` / `ObjectPropertyTracker`) も削除するため、
    再購読時は記録が無い前提で扱う
- `skills/shiguredo-moqt/SKILL.md` には forget の節を新設し、要旨 (保留中の PUBLISH_DONE がある間は
  `Session::forget_subscription` を呼ばないこと) と `Session::forget_subscription` の doc への参照を書く。
  契約本文は doc に置き、SKILL.md へ複製しない (doc と SKILL.md の二重管理を避ける)

## 完了条件

- `Session::forget_subscription` の doc に順序契約 (保留中の PUBLISH_DONE がある間は forget を呼ばないこと、
  forget は保留中の PUBLISH_DONE の送信後に呼ぶこと、記録が無い前提で再購読を扱うこと) が書かれていること
- `skills/shiguredo-moqt/SKILL.md` の forget の節に、要旨 (保留中の PUBLISH_DONE がある間は
  `Session::forget_subscription` を呼ばないこと) と `Session::forget_subscription` の doc への参照が書かれていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
