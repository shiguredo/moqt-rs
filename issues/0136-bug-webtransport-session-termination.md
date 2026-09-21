# WebTransport セッション終了の検知と WT_SESSION_GONE での中断

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-webtransport-session-termination
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-webtrans-http3-16 §6 (Session Termination) は、セッション終了を検知したら関連する全 uni / bidi ストリームを `WT_SESSION_GONE` で中断する MUST を定める。

```text
A WebTransport session over HTTP/3 is terminated when either of the following conditions is met:

*  the CONNECT stream is closed, either cleanly or abruptly, on either side; or

*  a WT_CLOSE_SESSION capsule is either sent or received.
```

> Upon learning that the session has been terminated, the endpoint MUST reset the send side and abort reading on the receive side of all unidirectional and bidirectional streams associated with the session (see Section 2.4 of [RFC9000])
> using the WT_SESSION_GONE error code; it MUST NOT send any new datagrams or open any new streams.

§4.7 は HTTP/3 GOAWAY と `WT_DRAIN_SESSION` を終了の合図とする。

> An HTTP/3 GOAWAY frame is also a signal to applications to initiate shutdown for all WebTransport sessions.  To shut down a single WebTransport session, either endpoint can send a WT_DRAIN_SESSION (0x78ae) capsule.

現状は自発 close で capsule と FIN だけを送ってストリームを中断せず、peer からの終了通知も検知しない。セッションが終了しても MOQT 層は受信を待ち続け、購読が静かに止まる。

## 現状

- `examples/moqt-transport/src/webtransport.rs` の `WtSession::close` は `WT_CLOSE_SESSION` capsule を送り、CONNECT stream に FIN を送るだけである。自セッションの uni / bidi ストリームは中断しない。`WtSession` は自セッションのストリームを把握していない。
- `WtClient::connect` はセッション確立後に CONNECT の受信半 (`recv_stream`) を drop する。以降の CONNECT stream のデータ (peer からの `WT_CLOSE_SESSION` / FIN / RESET) を観測できない。
- datagram 受信タスクと `WtSession::take_buffered_datagrams` は `Event::WebTransport(WebTransportEvent::Datagram { .. })` 以外のイベントを捨てる。
  依存 `shiguredo_http3` は `WebTransportEvent::SessionClosed` (`reset_streams` / `error_code` / `close_error_code` / `close_message` を持つ) と `WebTransportEvent::SessionDraining` を発火するが、example には届かない。
- `WtSession::close` は MOQT の close code を `u32` へ切り捨てる。`examples/moqt-transport/src/transport.rs` の `StreamHandle::close(code: u64, reason: &str)` から `code as u32` で呼ばれ、`shiguredo_moqt::session::types::SessionError::code` は `u64` である。`WT_CLOSE_SESSION` capsule の Application Error Code は 32 ビットなので、範囲外の値を渡すと別のコードに化ける。

## 設計方針

- CONNECT stream の受信半を `WtSession` が保持し、セッション確立後も受信を続ける。CONNECT stream の FIN / RESET、`WT_CLOSE_SESSION`、`WT_DRAIN_SESSION`、GOAWAY を終了 (drain は終了の合図) として扱う。h3 層が発火する `SessionClosed` / `SessionDraining` / `Event::GoawayReceived` を example のセッション状態へ反映し、datagram 以外のイベントを捨てる現状の経路をやめる。
- 終了を検知したとき、および自発 `close` のときに、自セッションに属する自側の全 uni / bidi ストリームの送信側を `WT_SESSION_GONE` (0x170d7b68) で reset し、受信側を stop_sending する。以後は新しい datagram を送らず、新しいストリームも開かない (§6 の MUST NOT)。
- ストリームの把握方法を決める。現状 example は WebTransport ストリームを h3 層へ register / feed していないため、h3 層の `SessionClosed.reset_streams` には example が直接扱っているストリームが含まれない。次のどちらかにする。
  - `ClientConnection::register_local_wt_stream` でローカル開始ストリームを h3 層に登録し、`SessionClosed.reset_streams` を中断対象として使う
  - `WtSession` が自セッションのストリーム台帳 (open / accept した stream ID) を持ち、終了時に一括で中断する

  実装時にどちらかを選び、選んだ理由をコメントに残す。`reset_stream_at` が無い現状の QUIC 実装では `RESET_STREAM` にフォールバックする (RESET_STREAM_AT 非対応そのものは [issues/pending/0094](../issues/pending/0094-bug-webtransport-reset-stream-at-unsupported.md) で扱う)。
- セッション終了を MOQT 層へ伝える。`accept_uni_stream` / `accept_bi_stream` / `recv_chunk` / `take_buffered_datagrams` が `TransportError::ConnectionClosed` を返し、publisher / subscriber の pipeline がセッション終了として処理する。受信ループが終了を待ち続ける現状をやめる。
- close code の切り捨てを明示する。`u32::try_from` で変換し、32 ビットに収まらない場合はエラーにする (または別のコードへ落とす判断をしてログに残す)。`as u32` は使わない。

## 完了条件

- セッション終了時にデータストリームが `WT_SESSION_GONE` で中断されることを確認できること。確認方法は次のとおり。
  - 単体テスト: セッション終了の種別 (CONNECT stream の FIN / RESET、`WT_CLOSE_SESSION`、`WT_DRAIN_SESSION`、GOAWAY、自発 close) から「全ストリームを `WT_SESSION_GONE` で中断し、新規送信を拒否する」動作を決める判定を純関数に切り出して固定する
  - 実機: relay 側から `WT_CLOSE_SESSION` を送る、または CONNECT stream を閉じ、example 側の受信ストリームが `WT_SESSION_GONE` で中断され、MOQT セッションが終了することを `RUST_LOG=debug` のログで確認する。WebTransport セッションの確立自体は 0094 の解消待ちであるため、実機確認は draft-15 相当の peer または 0094 の解消後に行う
- セッション終了の検知後に新しい stream を open せず、datagram も送らないこと
- 32 ビットに収まらない close code が切り捨てられないこと (単体テスト)
