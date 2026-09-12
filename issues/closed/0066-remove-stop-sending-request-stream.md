# 未使用の SessionEvent::StopSendingRequestStream を削除する

- Created: 2026-09-12
- Completed: 2026-09-13
- Branch: feature/remove-stop-sending-request-stream
- Polished: {YYYY-MM-DD}

## 目的

発火箇所のない公開イベント `SessionEvent::StopSendingRequestStream` を削除し、公開 API と実装の齟齬をなくす。

## 現状

`src/session/types.rs` の `SessionEvent::StopSendingRequestStream` はライブラリ内から一度も push されない。doc 自身が「Session の自動発火経路は現在なく、受信方向の cancel は I/O 層主導で行う」と明記している。
受信方向の cancel は `Session::stop_sending` が I/O 層の責務として扱いイベントを発行しない設計であり、example も `MoqtClient::stop_sending` から直接 bidi 受信タスクへ指示する。公開 enum variant として残ると、利用者が到達しないイベントの処理を強いられる。

## 設計方針

- `SessionEvent::StopSendingRequestStream` を削除する (破壊的変更のため `CHANGES.md` は `[CHANGE]`)。
- example の到達しない match アームとスキップコメントも削除する。
- 将来、受信方向の cancel を Session 起点で通知する必要が生じた場合は別 issue で設計する。

## 完了条件

- `StopSendingRequestStream` への参照がリポジトリから消えていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
- `CHANGES.md` の `[CHANGE]` にエントリが追加されていること

## 解決方法

本 issue は取り下げる (open のままで実装できない)。

直近の 0028 (Malformed Track 検出時に bidi request stream を cancel する) で `SessionEvent::StopSendingRequestStream` が malformed cancel の発火経路として使われるようになり、削除対象ではなくなった。受信方向 cancel は I/O 層主導という前提も、Session が `StopSendingRequestStream` を発行して I/O 層へ指示する形に変わっている。event の削除や `ResetRequestStream` への統合を前提とした死にコード整理は行わない。
