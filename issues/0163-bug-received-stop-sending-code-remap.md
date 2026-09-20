# 受信した STOP_SENDING のエラーコードを MOQT のコードへ戻す

- Created: 2026-09-24
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-received-stop-sending-code-remap
- Polished: 2026-09-27

## 目的

draft-ietf-webtrans-http3-16 §4.4 は RESET_STREAM と STOP_SENDING の両方で受信したエラーコードを対象とする。

> If a RESET_STREAM or STOP_SENDING frame is received with an error code outside the range reserved for WT_APPLICATION_ERROR, the stream is still considered reset, but the error code is not mapped to a WebTransport application error code.
> The WebTransport implementation SHOULD deliver this to the application as a stream reset with no application error code.

現状は受信した RESET_STREAM (受信ストリームの終端) のコードを MOQT のコードへ戻す経路だけが実装されており (issues/0162 がその扱いを整備する)、受信した STOP_SENDING (送信ストリームのエラーとして届く経路) のコードを MOQT のコードへ戻す経路が無い。送信ストリームのエラーは文字列化されるだけであり、peer が cancel した理由 (MOQT §12.5 のコード) を session 層へ伝えられない。

## 現状

- s2n-quic は peer の STOP_SENDING を受信すると、送信ストリーム側のエラーを `StreamError::stream_reset(frame.application_error_code.into())` として組み立てる
  (`s2n-quic-transport` の `send_stream.rs`、`init_reset(ResetSource::StopSendingFrame, ...)`)。
- `examples/moqt-transport/src/webtransport.rs` の `WtSendStream::send` / `finish` / `reset` は返った `StreamError` を
  `TransportError::transport` に畳むだけで、`ApplicationErrorCode::from_http3_code` を通す経路が無い。
  `from_http3_code` を呼ぶのは `wt_to_moqt_code` (受信ストリームの `RESET_STREAM` 経路) だけである。
- `TransportError::transport` は `StreamError` を Display で文字列化するだけであり、`StreamError::StreamReset { error, .. }` が運ぶ
  wire のエラーコードは session 層へは渡らない。送信経路の呼び出し元
  (`examples/moqt-publisher/src/stream_writer.rs` の `SubgroupWriter` の `stream.send(...)`、`examples/moqt-transport/src/moqt_client.rs` の
  `drain_events` の `SessionEvent::SendRequest` / `SessionEvent::SendOnStream` 分岐) はエラーを `?` で伝播させるため、peer の STOP_SENDING で該当ストリームの送信タスクが停止する。
  停止時に対象ストリームの終端通知 (`recv_data_stream_stop_sending` / `fetch_stop_sending_received` /
  `recv_request_stream_closed`) も行われない。
- `src/session/types.rs` の `SessionEvent::StopSendingRequestStream` は「受信方向の cancel を送る」イベントで、
  peer から受信した STOP_SENDING を通知する経路ではない。
- 受信方向の通知 API はコードを運ばない形で存在する。`src/session/data.rs` の
  `Session::recv_data_stream_stop_sending(stream_id)` と `src/session/fetch.rs` の
  `Session::fetch_stop_sending_received(request_id)` は peer の STOP_SENDING を受けたときに呼ぶための API だが、
  **どちらもエラーコードを引数に取らず、example (I/O 層) からは一度も呼ばれていない**。
  `Session::recv_data_stream_stop_sending` は 0024 で導入された再オープン禁止の記録だけを行う。
- bidi request stream の送信方向へ STOP_SENDING が届いた場合も `WtSendStream::send` が同じ
  `StreamError::StreamReset` を返すが、送信方向の STOP_SENDING を Session へ通知する API は無い。
  draft-ietf-moq-transport-21 §6.4.2.3 は cancel で RESET_STREAM (自側が送る方向) と STOP_SENDING (受ける方向) の
  両方を送ると定めるが、canceller が既に送信方向を FIN 済みの場合は STOP_SENDING だけが届くため、
  この経路が request 終端の唯一の通知になる。

## 設計方針

- 本 issue の実施は [issues/0162](../issues/0162-bug-request-stream-end-no-error-code.md) の完了を前提とする。
  0162 が `RequestStreamEnd::Reset` の `error_code` を `Option<u64>` にして「アプリケーションエラーコード無し」を
  `None` で表し、`wt_reset_error_code` の戻り値も `Option<u64>` に変えるため、本 issue はその表現を共用する。
  0162 の実装が入るまでは着手しないこと。
- 対象は「自側が送信する方向への peer の STOP_SENDING」であり、経路は次の 3 つに分かれる。
  検知と Session への通知は QUIC 経路 (`moqt://`) と WebTransport 経路で共通にし、remap だけを
  WebTransport 経路に限定する。
  - outgoing subgroup data stream (自側が publisher の SUBSCRIBE / PUBLISH)
  - outgoing fill fetch data stream (FETCH_OK 後の応答ストリーム)
  - bidi request stream の送信方向
- 検知: `StreamError::StreamReset { error, .. }` から `u64::from(error)` で wire のコードを取り出して
  peer 由来の STOP_SENDING として扱う。s2n-quic の `StreamError` は reset の由来
  (自側 / STOP_SENDING / 内部) を公開しないため、自側が開始した reset (`WtSendStream::reset_with` /
  `abort_session_gone`、`SendStream::reset`) の後はストリームを台帳から除去して以後 send しない
  既存の使い方を前提に、台帳に残るストリームで観測した `StreamReset` を peer 由来と判別する。
- remap: WebTransport 経路は 0162 で整備される `wt_to_moqt_code` (`ApplicationErrorCode::from_http3_code`) を
  送信方向でも使い、remap できた値は `Some(MOQT コード)`、できなかった値 (WT_APPLICATION_ERROR の範囲外と
  予約コードポイント) は `None` にして生値を warn ログに残す (0162 の「アプリケーションエラーコード無し」と同じ扱い)。
  QUIC 経路 (`moqt://`) は QUIC のコード空間のため remap しない (wire のコードが MOQT §12.5 のコードそのもの)。
  判別・remap は純関数に切り出し、spawn されたタスクの外から単体テストできるようにする。
- 通知 (session 層への配送):
  - outgoing subgroup data stream: `Session::recv_data_stream_stop_sending(stream_id)` に
    `error_code: Option<u64>` を追加し、example の送信経路から呼ぶ。0024 が実装した再オープン禁止の記録と
    Forward State 0→1 での解除は維持し、コードは記録・ログ・`skills/shiguredo-moqt/SKILL.md` へ反映する。
  - outgoing fill fetch data stream: `Session::fetch_stop_sending_received(request_id)` に
    同じ `error_code: Option<u64>` を追加し、example から呼ぶ (§3.2.1 の状態遷移と
    `SessionEvent::ResetRequestStream` の指示はそのまま)。
  - bidi request stream: `Session::recv_request_stream_closed(request_id,
    RequestStreamEnd::Reset { error_code, reliable_size: None })` を送信方向の検知からも呼び、
    peer の cancel として request を終端させる (新規の Session API は不要。`reliable_size` は s2n-quic が
    `RESET_STREAM_AT` 非対応のため `None`。0141 の「Reset は即時終端」と同じ扱いで
    `RequestTerminated { reason: PeerStreamReset { .. } }` へ届く)。
    受信方向の RESET_STREAM と対で届いた場合は両者が同一 cancel に由来するため、MoqtClient が
    ストリーム閉塞を 1 回だけ記録し、2 回目の通知は no-op にする (終端済み request への
    `recv_request_stream_closed` は unknown id で session を閉じるため、二重通知は必ず回避する)。
- example のパイプラインは peer の STOP_SENDING に由来する `StreamError` を fatal にしない。
  該当ストリームだけを終端として扱い、セッションと他のストリームの配信を継続する。

## 完了条件

- peer からの STOP_SENDING のエラーコードが MOQT のコードへ戻って `Some` になり、remap できない値は
  `None` になって 0162 の「アプリケーションエラーコード無し」の扱いと一致すること
  (WebTransport 経路の純関数の単体テストで固定する)
- outgoing subgroup / outgoing fill fetch / bidi request stream の 3 経路で、コードが上記の Session API へ
  届くこと (各 Session API の単体テストで固定する。example の配線は I/O ハンドルが必要なため、
  変換関数への集約と呼び出し箇所の明示で担う)
- bidi request stream で STOP_SENDING と RESET_STREAM が対で届いたとき、request の終端が 1 回だけ
  Session へ通知されること (二重通知で session が閉じないこと) がテストで固定されていること
- example が peer の STOP_SENDING に由来する `StreamError` でパイプラインを停止しないこと
  (該当ストリームだけが終端し、セッションの継続が妨げられないこと)
- 自側が送る STOP_SENDING (`WtRecvStream::stop_sending` / `RecvStream::stop_sending`) の remap と
  QUIC 経路の挙動が変わらないこと (QUIC 経路のコードは remap しない)
- `skills/shiguredo-moqt/SKILL.md` と既存テスト (`tests/test_session/data_stream.rs` の
  `recv_data_stream_stop_sending` 呼び出し、`tests/test_session/fetch/fill.rs` の
  `fetch_stop_sending_received` 呼び出し) がシグネチャ変更に追従し、`cargo test --workspace` が通ること
- 実接続での確認は [issues/pending/0094](../issues/pending/0094-bug-webtransport-reset-stream-at-unsupported.md) の解消後に行う
