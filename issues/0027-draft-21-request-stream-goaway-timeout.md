# request stream GOAWAY の timeout を実装する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-request-stream-goaway-timeout
- Polished: 2026-09-10

## 目的

draft-ietf-moq-transport-21 §9.2 (GOAWAY) の SHOULD に従い、request stream 上の GOAWAY で Timeout 経過後に当該 request stream を `GOING_AWAY` で reset する。セッションは閉じず、対象 request のみをマイグレーションする。

## 現状

- `src/session/goaway.rs` の `send_goaway_on_request_stream` は `SendOnStream` を push して `request_stream_sent` に記録するだけで、timeout を記録しない。
- control stream の GOAWAY は `GoawayState::local_deadline_ms` (単一スロット) で期限到達時に `SESSION_GOAWAY_TIMEOUT` のセッションクローズを扱う。request stream の GOAWAY はセッションを閉じず、当該 request stream のみを reset する必要がある。
- `DeadlineTimer::new` は `duration_ms == 0` を即期限切れとして扱う。request stream 版で timeout を無条件に渡すと、`timeout == 0` の GOAWAY が次の `tick` で即 reset される。
- `tick` 未経験 (`last_tick_ms == None`) のときの基準時刻が未定義。control stream 版は `local_pending_timeout_ms` に保留し、最初の `tick` を基準にする。
- `tests/test_session/goaway.rs` の既存テストは `send_goaway_on_request_stream(..., 0)` を複数呼ぶが、いずれも送信後に `tick` を呼ばないため、0 の誤扱いを検出できない。

根拠 (draft-ietf-moq-transport-21 §9.2):

> "When sent on a request stream, the sender SHOULD reset the stream with GOING_AWAY after the indicated timeout. A value of 0 indicates the sender has no specific timeout, but the recipient SHOULD migrate as quickly as possible."

reset のエラーコードは §12.5 (Stream Reset Error Codes) の `GOING_AWAY` (0x4) であり、`src/error.rs` の `STREAM_GOING_AWAY` が対応する。§12.3 の `REQUEST_GOING_AWAY` (0x6) は REQUEST_ERROR 用で本件には使わない。

## 設計方針

- `GoawayState` に request_id ごとの deadline を保持する独立マップを追加する。control stream の単一スロット `local_deadline_ms` とは共有しない (request ごとに独立して GOAWAY を送るため)。
- `tick` では既存の timeout 評価に加えて request 単位の期限到達を評価し、`ResetRequestStream { request_id, error_code: STREAM_GOING_AWAY }` を積む。セッションは閉じない。
- `timeout == 0` は deadline を設定しない (期限なし)。`DeadlineTimer` を使う場合は `None` に正規化する。
- `tick` 未経験で送信した場合は control stream 版と同じく保留し、最初の `tick` を基準時刻として確定する。
- 発火時にエントリを削除し、1 request につき `ResetRequestStream` を 1 回だけ発行する。
- request stream が期限前に終端した場合は deadline を破棄し、reset を発行しない。解除は既存の per-request 後始末に載せる。
  - `recv_request_stream_closed` (peer の FIN / RESET_STREAM 受信)
  - 各 `forget_*` (control message deadline の `clear_control_message_deadline` と同じ経路)
  - ローカル送信方向を PUBLISH_DONE / REQUEST_ERROR の FIN で閉じた後
- `send_goaway_on_request_stream` の doc にある「session 全体の deadline は設定しない」を、request 単位の deadline を設定する (セッションは閉じない) に更新する。

## 完了条件

- `send_goaway_on_request_stream(request_id, ..., timeout > 0)` の後、timeout 経過前の `tick` では `ResetRequestStream` が出ず、経過後の `tick` で `STREAM_GOING_AWAY` (0x4) の `ResetRequestStream` が 1 回だけ出ること
- `timeout == 0` では `tick` を進めても reset されないこと
- `tick` 前に送信した GOAWAY が、最初の `tick` を基準に timeout を計測すること
- 期限前に request stream が終端した場合 (peer FIN / RESET、`forget_*`、ローカル FIN) は、期限到達後も reset が発行されないこと
- 同一 request への 2 回目の `send_goaway_on_request_stream` が従来どおり拒否されること
- 回帰テストが `tests/test_session/goaway.rs` に追加され、`cargo test --workspace` が通ること
- `CHANGES.md` の `## develop` に `[FIX]` として記載されていること
