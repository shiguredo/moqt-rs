# 受信した STOP_SENDING のエラーコードを MOQT のコードへ戻す

- Created: 2026-09-24
- Completed: 2026-10-09
- Branch: feature/fix-received-stop-sending-code-remap
- Polished: 2026-10-08
- Updated: 2026-10-08

## 目的

draft-ietf-webtrans-http3-16 §4.4 は RESET_STREAM と STOP_SENDING の両方で受信したエラーコードを対象とする。

> If a RESET_STREAM or STOP_SENDING frame is received with an error code outside the range reserved for WT_APPLICATION_ERROR, the stream is still considered reset, but the error code is not mapped to a WebTransport application error code.
> The WebTransport implementation SHOULD deliver this to the application as a stream reset with no application error code.

現状は受信した RESET_STREAM (受信ストリームの終端) のコードを MOQT のコードへ戻す経路だけが実装されており (0162 がその扱いを整備済み)、受信した STOP_SENDING (送信ストリームのエラーとして届く経路) のコードを MOQT のコードへ戻す経路が無い。送信ストリームのエラーは文字列化されるだけであり、peer が cancel した理由 (MOQT §12.5 のコード) を session 層へ伝えられない。

## 現状

- s2n-quic は peer の STOP_SENDING を受信すると、送信ストリーム側のエラーを `StreamError::stream_reset(frame.application_error_code.into())` として組み立てる
  (`s2n-quic-transport` の `send_stream.rs`、`init_reset(ResetSource::StopSendingFrame, ...)`)。
- `examples/tokio-moq/src/webtransport_h3.rs` の `WtSendStream::send` / `finish` は返った `StreamError` を
  `TransportError::transport` に畳むだけで、受信したコードを MOQT のコードへ戻す
  (`ApplicationErrorCode::from_http3_code`) 経路が無い。
  `from_http3_code` を呼ぶのは `wt_to_moqt_code` (受信ストリームの `RESET_STREAM` 経路) だけである。
  `WtSendStream::reset` は 0134 で送信方向の remap (`moqt_to_wt_code`) を実装済みのため対象外である。
- `TransportError::transport` は `TransportError::Transport(Box<dyn Error + Send + Sync>)` に保持するだけで
  downcast を契約にしておらず、`examples/tokio-moq/src/transport.rs` の `SendStream::send` / `finish` の
  QUIC 分岐は `TransportError::Quic(format!("{e}"))` と文字列化するため、`StreamError::StreamReset { error, .. }` が運ぶ
  wire のエラーコードは session 層へは渡らない。送信経路の呼び出し元
  (`examples/moq-pub/src/stream_writer.rs` の `SubgroupWriter` の `stream.send(...)`、`examples/tokio-moq/src/moqt_client.rs` の
  `drain_events` の `SessionEvent::SendRequest` / `SessionEvent::SendOnStream` 分岐、同ファイルの
  `MoqtClient::send_fetch_response` の `stream.send(...)` / `stream.finish()`) はエラーを `?` で伝播させるため、peer の STOP_SENDING で該当ストリームの送信タスクが停止する。
  停止時に対象ストリームの終端通知 (`recv_data_stream_stop_sending` / `fetch_stop_sending_received` /
  `recv_request_stream_closed`) も行われない。
- `src/session/types.rs` の `SessionEvent::StopSendingRequestStream` は「受信方向の cancel を送る」イベントで、
  peer から受信した STOP_SENDING を通知する経路ではない。
- 受信方向の通知 API はコードを運ばない形で存在する。`src/session/data.rs` の
  `Session::recv_data_stream_stop_sending(stream_id)` と `src/session/fetch.rs` の
  `Session::fetch_stop_sending_received(request_id)` は peer の STOP_SENDING を受けたときに呼ぶための API だが、
  **どちらもエラーコードを引数に取らず、example (I/O 層) からは一度も呼ばれていない**。
  `Session::recv_data_stream_stop_sending` は 0024 の再オープン禁止の記録に加えて、subscription 由来の
  fill fetch stream の STOP_SENDING を吸収して保留 PUBLISH_DONE を flush する分岐を持ち
  (draft-ietf-moq-transport-22 §3.4.1)、未知の stream id は `PROTOCOL_VIOLATION` で拒否する。
- peer の STOP_SENDING を観測できる API は依存に用意されている。s2n-quic は受信フレームを購読者へ
  publish するため `Subscriber::on_frame_received` で `Frame::StopSending { id, error_code }` を取得でき、
  QUIC 経路の example は `examples/tokio-moq/src/quic.rs` の `PacketDiag` を `.with_event` で既に差し込んでいる。
  一方 wt-h3 経路の s2n-quic クライアント (`examples/tokio-moq/src/webtransport_h3.rs` の `WtClient::connect`) は
  `.with_event` を付けておらず、`Connection::stop_sending` も `register_local_wt_stream` も呼んでいないため、
  `WebTransportEvent::StreamStopSending` と汎用 `Event::StopSending` は現状生成されない
  (`process_h3_events` の `other =>` は将来これらを捨てることになる)。
  また、送信 API の失敗からは STOP_SENDING を検知できない場合がある。s2n-quic は送信方向が FIN ack 済みの
  ストリームでは `init_reset(ResetSource::StopSendingFrame, ...)` が `ResetNotNecessary` を返すため、
  STOP_SENDING に由来するエラーが `send` / `finish` に現れない。
- WebTransport over HTTP/2 経路 (`wt-h2`) の driver は `WtEvent::StopSending { error_code }` でコードを受け取るが、
  送信は `TransportError::StreamClosed` で失敗するためコードは session 層へ渡らない。WT_STOP_SENDING は
  アプリケーションエラーコードを capsule に直接運ぶため (draft-ietf-webtrans-http2-15 §6.3)、HTTP/3 の
  code space からの remap は不要である。
- bidi request stream の送信方向へ STOP_SENDING が届いた場合も `WtSendStream::send` が同じ
  `StreamError::StreamReset` を返すが、送信方向の STOP_SENDING を Session へ通知する API は無い。
  draft-ietf-moq-transport-22 §6.4.2.3 は cancel で RESET_STREAM (自側が送る方向) と STOP_SENDING (受ける方向) の
  両方を送ると定めるが、canceller が既に送信方向を FIN 済みの場合は STOP_SENDING だけが届くため、
  この経路が request 終端の唯一の通知になる。

## 設計方針

- 前提だった [0162](closed/0162-bug-request-stream-end-no-error-code.md) は 2026-09-30 に closed 済みである。
  `RequestStreamEnd::Reset` の `error_code` と `TerminationReason::PeerStreamReset` の `error_code` は
  `Option<u64>` になり「アプリケーションエラーコード無し」を `None` で表し、`wt_reset_error_code` も
  `Option<u64>` を返す。本 issue はその表現を共用する。
- 対象は「自側が送信する方向への peer の STOP_SENDING」であり、経路は次の 3 つに分かれる。
  検知と Session への通知は QUIC 経路 (`moqt://`) と WebTransport over HTTP/3 経路 (`wt-h3`) で共通にし、
  HTTP/3 の code space からの remap だけを `wt-h3` に限定する。
  - outgoing subgroup data stream (自側が publisher の SUBSCRIBE / PUBLISH)
  - outgoing FETCH data stream (FETCH_OK 後の応答ストリーム)
  - bidi request stream の送信方向
- subscription 由来の fill fetch stream の STOP_SENDING (§3.4.1 の独立 cancel) は
  `Session::recv_data_stream_stop_sending` の既存の吸収経路で扱う (送信経路から同じ API へ `error_code` を渡す)。
  ただし example は fill fetch stream を開かない (`SessionEvent::OpenFillFetchStream` を無視しており、
  `send_fill_fetch_header` の呼び出しも無い) ため、example の配線対象には含めず Session API の単体テストで固定する。
  WebTransport over HTTP/2 経路 (`wt-h2`) は remap が不要であり、送信 API へコードを伝える仕組みも無いため
  本 issue の対象外とする。
- 検知: peer の STOP_SENDING は受信フレームの観測から行う。送信 API の失敗からの推測は行わない
  (FIN ack 済みのストリームでは STOP_SENDING に由来するエラーが現れず、検知できないため)。
  - QUIC 経路 (`moqt://`): `examples/tokio-moq/src/quic.rs` の `PacketDiag` に
    `Subscriber::on_frame_received` を実装し、`Frame::StopSending { id, error_code }` を観測する
  - wt-h3 経路: `WtClient::connect` の s2n-quic クライアントビルダーに `.with_event` を追加し、
    同じ購読者 (`examples/tokio-moq/src/quic.rs` の `PacketDiag`。クレート内で共有するため `pub(crate)` にする)
    で `Frame::StopSending { id, error_code }` を観測する。観測した STOP_SENDING は
    `Connection::stop_sending(stream_id, error_code)` へも渡し、h3 層へ STOP_SENDING の受信を通知する
    (h3 層は `WebTransportEvent::StreamStopSending` または汎用 `Event::StopSending` を発火するが、
    example はフレーム観測でコードを既に持つため、これらは h3 層の状態更新として扱う)
  - 観測した stream id は example の台帳 (QUIC stream id と送信ストリームの対応) で対象ストリームに引き当てる。
    観測は peer 由来に限られるため、自側が開始した reset と判別するための台帳照合は行わない
- remap: 新関数 `wt_stop_sending_error_code(http3_code: u64) -> Option<u64>` を追加し、`wt_to_moqt_code` で
  remap する。remap できた値は `Some(MOQT コード)`、できなかった値 (WT_APPLICATION_ERROR の範囲外と
  予約コードポイント) は `None` にして生値を warn ログに残す (0162 の「アプリケーションエラーコード無し」と同じ扱い)。
  warn 文言は STOP_SENDING 用に分ける (`wt_reset_error_code` の "RESET_STREAM received with ..." を流用しない。
  既存の文言と既存テスト 2 件は変更しない)。
  QUIC 経路 (`moqt://`) は QUIC のコード空間のため remap せず wire のコードをそのまま `Some` にする。
  変換は純関数に切り出し、spawn されたタスクの外から単体テストできるようにする。
- 通知 (session 層への配送):
  - outgoing subgroup data stream: `Session::recv_data_stream_stop_sending(stream_id, error_code: Option<u64>)` に
    拡張し、example の送信経路から呼ぶ。0024 が実装した再オープン禁止の記録と Forward State 0→1 での解除は維持し、
    受け取ったコードは停止記録のエントリに保持して参照 API
    (`Session::stopped_outgoing_subgroup_error_code(request_id, track_alias, group_id, subgroup_id) -> Option<Option<u64>>`。
    外側が停止の有無、内側がコードの有無を表す)
    から取得できるようにする (テストで観測できる形にする)。`shiguredo_moqt` は no_std でログ機構を持たないため、
    ログは example が呼び出し箇所で出す。
  - outgoing FETCH data stream (FETCH_OK 後の応答ストリーム): `Session::fetch_stop_sending_received(request_id, error_code: Option<u64>)` に
    拡張し、受け取ったコードを `Fetch` に保持して `Session::fetch` から参照できるようにする
    (draft-ietf-moq-transport-22 §3.2.4 の状態遷移と `SessionEvent::ResetRequestStream` の指示はそのまま)。
    subscription 由来の fill fetch stream (§3.4.1) は `Session::recv_data_stream_stop_sending` の既存の吸収経路で扱う。
  - bidi request stream: `Session::recv_request_stream_closed(request_id,
    RequestStreamEnd::Reset { error_code, reliable_size: None })` を送信方向の検知からも呼び、
    peer の cancel として request を終端させる (新規の Session API は不要。`reliable_size` は s2n-quic が
    `RESET_STREAM_AT` 非対応のため `None`。0141 の「Reset は即時終端」と同じ扱いで
    `RequestTerminated { reason: PeerStreamReset { .. } }` へ届く)。
    受信方向の RESET_STREAM と対で届いた場合は両者が同一 cancel に由来するため、MoqtClient が
    request_id 単位で「Session へ終端通知済み」を記録し、2 回目の通知を no-op にする。
    この記録は回収時に削除される `closed_request_streams` とは別に、終端後も保持する集合とする
    (回収後に遅れて届く送信方向の検知で再通知しないため)。
    終端済み request への `recv_request_stream_closed` は、subscription では unknown id として session を
    閉じ、fetch / track_status では `RequestTerminated` が重複するため、二重通知は必ず回避する。
    判定は純関数または小さな型に切り出し、`examples/tokio-moq/src/moqt_client.rs` の単体テストで固定する。
- example のパイプラインは peer の STOP_SENDING に由来する検知を fatal にしない。
  該当ストリームだけを終端として扱い、セッションと他のストリームの配信を継続する。

## 完了条件

- peer からの STOP_SENDING のエラーコードが MOQT のコードへ戻って `Some` になり、remap できない値は
  `None` になって 0162 の「アプリケーションエラーコード無し」の扱いと一致すること
  (wt-h3 経路の `wt_stop_sending_error_code` の単体テストで固定する。QUIC 経路は remap せず
  wire のコードを `Some` にする変換を純関数の単体テストで固定する)
- outgoing subgroup / outgoing FETCH / bidi request stream の 3 経路で、コードが上記の Session API へ届き、
  テストから観測できること (subgroup は `Session::stopped_outgoing_subgroup_error_code`、FETCH は
  `Session::fetch` が返す `Fetch` の保持値、bidi は `TerminationReason::PeerStreamReset { error_code }` で固定する。
  example の配線は I/O ハンドルが必要なため、変換関数への集約と呼び出し箇所の明示で担う)
- bidi request stream で STOP_SENDING と RESET_STREAM が対で届いたとき、request の終端が 1 回だけ
  Session へ通知されること (二重通知で session が閉じないこと) がテストで固定されていること
  (ガードは純関数または小さな型へ切り出し、`examples/tokio-moq/src/moqt_client.rs` の単体テストで固定する)
- example が peer の STOP_SENDING に由来する検知でパイプラインを停止しないこと
  (該当ストリームだけが終端し、セッションの継続が妨げられないこと)
- 自側が送る STOP_SENDING (`WtRecvStream::stop_sending` / `RecvStream::stop_sending`) の remap と
  QUIC 経路の挙動が変わらないこと (h3 側は既存の `moqt_to_wt_code` / `stream_application_error` のテストが
  変更なく通ることで確認する。QUIC 側の `examples/tokio-moq/src/transport.rs` は I/O ハンドルが必要で
  自動テストが無いため、コードを変更しないことをレビューで確認する)
- `skills/shiguredo-moqt/SKILL.md` の受信 API 一覧の `recv_data_stream_stop_sending` を新シグネチャへ更新し、
  記載の無い `fetch_stop_sending_received` を追記すること。`src/session.rs` のモジュール doc にある
  `recv_data_stream_stop_sending(stream_id)` の表記も新シグネチャへ更新すること。既存テスト
  (`tests/test_session/data_stream.rs`、`tests/test_session/fetch/fill.rs`、
  `tests/test_session/subscription/request_update.rs` の `recv_data_stream_stop_sending` 呼び出し、
  `tests/test_session/fetch/unified.rs` の `fetch_stop_sending_received` 呼び出し) が
  シグネチャ変更に追従し、`cargo test --workspace` が通ること
- 実接続での確認は [issues/pending/0094](../issues/pending/0094-bug-webtransport-reset-stream-at-unsupported.md) の解消後に行う

## 解決方法

peer の STOP_SENDING のエラーコードを MOQT のコードへ戻し、該当する送信ストリームの終端として
Session へ通知する経路を追加した。

- `Session::recv_data_stream_stop_sending` と `Session::fetch_stop_sending_received` に
  `error_code: Option<u64>` を追加した。`None` は「アプリケーションエラーコード無し」を表し、
  予約コードポイントも同じ扱いになる
- 参照 API `Session::stopped_outgoing_subgroup_error_code` と `Fetch::peer_stop_sending_error_code`
  を追加した。bidi request stream は既存の `Session::recv_request_stream_closed` の
  `RequestStreamEnd::Reset` として通知するため新規 API は不要である
- example は s2n-quic の `Subscriber::on_frame_received` で `Frame::StopSending` を観測する。
  QUIC 直接接続は wire のコードをそのまま、WebTransport over HTTP/3 は
  `wt_stop_sending_error_code` で MOQT のコードへ戻して Session へ渡す。wt-h3 では h3 層へも
  `ClientConnection::stop_sending` で通知する
- bidi request stream は RESET_STREAM と STOP_SENDING が対で届くため、example に終端通知済みの
  記録 (`TerminatedRequestStreams`) を追加した。通知に成功したときだけ記録し、二重通知で
  Session が未知 id としてセッションを閉じることを防ぐ
- 未知の stream id への通知は Session がセッションを閉じずにエラーを返すため、example は
  該当ストリームの終端として吸収する
- peer の STOP_SENDING に由来する送信エラーを `TransportError::StreamReset` として切り分け、
  moq-pub / moq-sub は該当ストリームだけを終端して配信 / 受信を継続する。moq-pub は
  再オープン禁止 (§11.3.2) を避けるため、cancel された Subgroup の group id を飛ばす
- `skills/shiguredo-moqt/SKILL.md` と `src/session.rs` の表記を新シグネチャへ更新した

テストは `tests/test_session/data_stream.rs` と `tests/test_session/fetch/unified.rs` に
受信コードの保持・未知 stream id と重複通知でセッションを閉じないことを追加し、
`examples/tokio-moq/src/{transport.rs,webtransport_h3.rs,moqt_client.rs}` と
`examples/moq-{pub,sub}/src/{error.rs,pipeline.rs}` に変換則・二重通知ガード・
ストリーム終端の切り分けの単体テストを追加した。

実接続での確認は pending/0094 の解消後に行う。なお WebTransport over HTTP/2 は STOP_SENDING の
観測経路を持たず、peer の cancel は `TransportError::StreamClosed` として現れるため、
その経路で publisher が終了する既知の制限は残っている。
