# SKILL.md に forget の順序契約を追記する

- Created: 2026-09-26
- Completed: {YYYY-MM-DD}
- Branch: feature/change-skill-forget-order-contract
- Polished: {YYYY-MM-DD}

## 目的

[issues/closed/0141](../issues/closed/0141-bug-publish-sender-peer-fin-termination.md) の検証で、`Session::forget_subscription` を保留中の PUBLISH_DONE の flush より先に呼ぶと、送るはずの PUBLISH_DONE が失われることが分かった。利用者向けの `skills/shiguredo-moqt/SKILL.md` には forget の順序契約が書かれておらず、同じ誤用が起きうるため契約を明記する。

## 現状

- `Session::forget_subscription` (`src/session/core.rs`) は request ごとの状態 (`subscriptions` / `peer_object_properties` / `peer_object_fields` など) を削除する
- `Subscription::pending_publish_done` の flush は subscription が `Terminated` であり全 outgoing stream が閉じていることが条件である (`Session::maybe_flush_pending_publish_done`)
- `skills/shiguredo-moqt/SKILL.md` の forget の説明は状態の破棄のみで、順序契約に触れていない

## 設計方針

- `skills/shiguredo-moqt/SKILL.md` の forget の節に、次を追記する
  - 保留中の PUBLISH_DONE がある間は forget を呼ばない (呼ぶと PUBLISH_DONE が送られないまま状態が消える)
  - forget は「送信側の終端処理が完了した後」または「subscription が不要になった後」に呼ぶ
  - 受信側の重複検出 (`ObjectFieldTracker`) の記録も forget で消えるため、再購読時は記録が無い前提で扱う
- 順序契約は `Session::forget_subscription` の doc にも書き、SKILL.md から参照する (doc と SKILL.md の二重管理を避ける)

## 完了条件

- `skills/shiguredo-moqt/SKILL.md` に forget の順序契約 (保留中の PUBLISH_DONE を先に送る) が書かれていること
- `Session::forget_subscription` の doc に同じ契約が書かれ、SKILL.md と参照関係になっていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
