# 受信した STOP_SENDING のエラーコードを MOQT のコードへ戻す

- Created: 2026-09-24
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-received-stop-sending-code-remap
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-webtrans-http3-16 §4.4 は RESET_STREAM と STOP_SENDING の両方で受信したエラーコードを対象とする。

> If a RESET_STREAM or STOP_SENDING frame is received with an error code outside the range reserved for WT_APPLICATION_ERROR, the stream is still considered reset, but the error code is not mapped to a WebTransport application error code.

現状は受信した STOP_SENDING のコードを MOQT のコードへ戻す経路が無く、送信ストリームのエラーとして文字列化されるだけである。
peer が cancel した理由 (MOQT §12.5 のコード) を session 層へ伝えられない。

## 現状

- s2n-quic は peer の STOP_SENDING を受信すると、送信ストリーム側のエラーを `StreamError::stream_reset(frame.application_error_code.into())` として組み立てる
  (`s2n-quic-transport` の `send_stream.rs`、`init_reset(ResetSource::StopSendingFrame, ...)`)。
- `examples/moqt-transport/src/webtransport.rs` の `WtSendStream::send` / `finish` / `reset` は返った `StreamError` を
  `TransportError::transport` に畳むだけで、`ApplicationErrorCode::from_http3_code` を通す経路が無い。
  `from_http3_code` を呼ぶのは `wt_to_moqt_code` (受信ストリームの `RESET_STREAM` 経路) だけである。
- `src/session/types.rs` の `SessionEvent::StopSendingRequestStream` は「受信方向の cancel を送る」イベントで、
  peer から受信した STOP_SENDING のコードを session 層へ通知する経路は無い。

## 設計方針

- 送信ストリームのエラーが peer からの STOP_SENDING 由来かどうかを判別できる範囲で判別し、
  remap できたコードを session 層へ伝える経路を用意する (s2n-quic のエラー型が reset 理由の種別をどこまで公開しているかを実装前に確認する)
- remap できない値は [issues/0162](../issues/0162-bug-request-stream-end-no-error-code.md) の「アプリケーションエラーコード無し」と同じ扱いにそろえる
- 判別と remap は純関数に切り出し、spawn されたタスクの外からテストできるようにする
- QUIC 経路 (`moqt://`) は QUIC のコード空間のため remap しない

## 完了条件

- peer からの STOP_SENDING のエラーコードが MOQT のコードへ戻り、session 層へ伝わること (単体テストで固定する)
- remap できない値の扱いが [issues/0162](../issues/0162-bug-request-stream-end-no-error-code.md) の方針と一致すること
- 自側が送る STOP_SENDING (`WtRecvStream::stop_sending`) の remap と QUIC 経路の挙動が変わらないこと
- 実接続での確認は [issues/pending/0094](../issues/pending/0094-bug-webtransport-reset-stream-at-unsupported.md) の解消後に行う
