# src/session.rs の module doc の開始メッセージ一覧に SUBSCRIBE_TRACKS を追加する

- Created: 2026-09-13
- Completed: {YYYY-MM-DD}
- Branch: feature/doc-session-start-message-list
- Polished: {YYYY-MM-DD}

## 目的

`src/session.rs` の module doc が示す bidi request stream の開始メッセージ一覧を実装と一致させ、利用者がモジュール doc から実装範囲を正しく読み取れるようにする。実装の挙動は変えない。

## 現状

`src/session.rs` の module doc の「Stream 抽象化」節は request stream を「bidi、SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS / PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE のいずれかで始まる」と説明しており、6 種類しか挙げていない。

一方、`src/session/core.rs` の `recv_request` の doc は開始メッセージを「SUBSCRIBE / PUBLISH / FETCH / PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / TRACK_STATUS / SUBSCRIBE_TRACKS の 7 種類」としており、`recv_request` の実装もこれら 7 種類を受理する。module doc には SUBSCRIBE_TRACKS が含まれておらず、`src/session.rs` の module doc 内に
SUBSCRIBE_TRACKS の記載自体が存在しない。

## 設計方針

- `src/session.rs` の module doc の開始メッセージ一覧に SUBSCRIBE_TRACKS を追加し、`recv_request` の doc と同じ 7 種類にする。列挙の順序は `recv_request` の doc と揃える。
- doc の記述のみを変更し、コードは変えない。

## 完了条件

- `src/session.rs` の module doc の開始メッセージ一覧が実装 (`recv_request`) と同じ 7 種類になっていること
- module doc の記述が実装と矛盾しないこと
- `cargo test --workspace` と `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` が通ること
- `CHANGES.md` の `## develop` の `### misc` に `[UPDATE]` エントリが追加されていること
