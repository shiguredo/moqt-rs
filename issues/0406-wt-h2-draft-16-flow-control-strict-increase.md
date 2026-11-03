# HTTP/2 の WT_MAX 系 capsule に厳密な上限増加を要求する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-wt-h2-draft-16-flow-control-strict-increase
- Polished: 2026-11-03

## 目的

[draft-ietf-webtrans-http2-16 §6.4 〜 §6.7](../refs/webtrans/draft-ietf-webtrans-http2-16.txt) の変更に追従する。

## 現状

固定中の `shiguredo_http2 2026.1.0-canary.16` の `src/webtransport/flow_control.rs` の `WtFlowControl::update_send_max` / `update_max_streams` と `src/webtransport/stream.rs` の `update_send_max` は減少 (`<`) だけを拒否する。
同値の WT_MAX_DATA / WT_MAX_STREAM_DATA / WT_MAX_STREAMS を許してしまう。
adapter は `WtSession` に capsule 処理を委譲する。

## 設計方針

前回受信値、未受信なら該当する session / stream の初期値に対して、厳密に大きい値だけを受理する依存実装へ追従する。
双方向・単方向の上限を独立して検証する。
`crates/tokio-moq/src/webtransport_h2.rs` の `DriverState::abort_session_with_wt_error` / `wt_http2_error_code` で WT_FLOW_CONTROL_ERROR を CONNECT stream の reset へ伝える既存経路を使う。

## 完了条件

- 初期値と同値・小さい値、更新済み値と同値・小さい値を各 capsule で拒否し、増加値は受理すること。
- 初期値 0 と、SETTINGS 由来の非 0 値、stream ごとの初期値で検証すること。
- 自己署名証明書を使う実際の HTTP/2 peer とのテストで CONNECT stream が WT_FLOW_CONTROL_ERROR となること。モックは使わないこと。

## 依存・関連

- 関連: [0405](0405-wt-h2-draft-16-settings-codepoint.md)
