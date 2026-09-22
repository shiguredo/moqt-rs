# WebTransport セッション終了の検知と WT_SESSION_GONE での中断

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-webtransport-session-termination
- Polished: 2026-09-22

## 目的

draft-ietf-webtrans-http3-16 §6 (Session Termination) は、セッション終了を検知したら関連する全 uni / bidi ストリームを `WT_SESSION_GONE` で中断する MUST を定める。

```text
A WebTransport session over HTTP/3 is terminated when either of the following conditions is met:

*  the CONNECT stream is closed, either cleanly or abruptly, on either side; or

*  a WT_CLOSE_SESSION capsule is either sent or received.
```

> Upon learning that the session has been terminated, the endpoint MUST reset the send side and abort reading on the receive side of all unidirectional and bidirectional streams associated with the session (see Section 2.4 of [RFC9000])
> using the WT_SESSION_GONE error code; it MUST NOT send any new datagrams or open any new streams.

§4.7 は HTTP/3 GOAWAY と `WT_DRAIN_SESSION` を **終了の開始** の合図とし、受信後もセッションを使い続けてよい (MAY) と定める。§6 の終了条件には含まれない。

> An HTTP/3 GOAWAY frame is also a signal to applications to initiate shutdown for all WebTransport sessions.  To shut down a single WebTransport session, either endpoint can send a WT_DRAIN_SESSION (0x78ae) capsule.

> After sending or receiving either a WT_DRAIN_SESSION capsule or a HTTP/3 GOAWAY frame, an endpoint MAY continue using the session:
> it MAY open new WebTransport streams and MAY send new datagrams.  The signal is intended for the application using WebTransport,
> which is expected to attempt to gracefully terminate the session as soon as possible.

現状は自発 close で capsule と FIN だけを送ってストリームを中断せず、peer からの終了通知も検知しない。セッションが終了しても MOQT 層は受信を待ち続け、購読が静かに止まる。

## 現状

- `examples/moqt-transport/src/webtransport.rs` の `WtSession::close` は `WT_CLOSE_SESSION` capsule を送り、CONNECT stream に FIN を送るだけである。自セッションの uni / bidi ストリームは中断しない。`WtSession` は自セッションのストリームを把握していない。
- `WtClient::connect` はセッション確立後に CONNECT の受信半 (`recv_stream`) を drop する。以降の CONNECT stream のデータ (peer からの `WT_CLOSE_SESSION` / FIN / RESET) を観測できない。
- datagram 受信タスクと `WtSession::take_buffered_datagrams` は `Event::WebTransport(WebTransportEvent::Datagram { .. })` 以外のイベントを捨てる。
  依存 `shiguredo_http3` は `WebTransportEvent::SessionClosed` (`reset_streams` / `error_code` / `close_error_code` / `close_message` を持つ) と `WebTransportEvent::SessionDraining` を発火するが、example には届かない。
- `WtSession::close` は MOQT の close code を `u32` へ切り捨てる。`examples/moqt-transport/src/transport.rs` の `StreamHandle::close(code: u64, reason: &str)` から `code as u32` で呼ばれ、`shiguredo_moqt::session::types::SessionError::code` は `u64` である。`WT_CLOSE_SESSION` capsule の Application Error Code は 32 ビットなので、範囲外の値を渡すと別のコードに化ける。

## 設計方針

- CONNECT stream の受信半を `WtSession` が保持し、セッション確立後も h3 層へ feed し続ける。FIN / RESET / `WT_CLOSE_SESSION` を h3 層が処理して `WebTransportEvent::SessionClosed` を発火するため、example が受信半を drop すると `SessionClosed` は発火しない。datagram 以外のイベントを捨てる現状の経路もやめる。
- 終了の種別と動作を分ける。§6 の終了 (CONNECT stream の close / `WT_CLOSE_SESSION` の送受信) と §4.7 の drain (`WT_DRAIN_SESSION` / GOAWAY) を同一視しない。
  - 終了 (`SessionClosed` の受信、自発 `close`): 自セッションの全 uni / bidi ストリームを `WT_SESSION_GONE` (0x170d7b68) で中断し、以後は新しい datagram を送らず、新しいストリームも開かない (§6 の MUST / MUST NOT)
  - drain (`SessionDraining` の受信): 新しいストリームを開かず datagram も送らないが、既存ストリームは継続する (§4.7 の "MAY continue using the session" を潰さない)。h3 層は GOAWAY の `goaway_id` で影響するセッションにだけ `SessionDraining` を発火するため、`Event::GoawayReceived` は使わない
- 中断の実行主体を決める。`s2n_quic::connection::Handle` にストリーム ID を指定した reset / stop_sending の API は無く、`WtSendStream::reset` は `&mut self`、実ハンドルは各ストリームタスクが所有している。したがって `WtSession` が ID の台帳だけを持って一括中断することはできない。
  `WtSession` に `tokio::sync::watch` のセッション状態を持たせ、ストリームを所有するタスクが状態変化を観測したときに自分の `WtSendStream::reset(WT_SESSION_GONE)` と `WtRecvStream::stop_sending(WT_SESSION_GONE)` を実行する。
  受信待ち (`WtRecvStream::recv_chunk` の `receive().await`) と送信待ちの中でも観測できるよう、`tokio::select!` で状態変化と I/O を同時に待つ。新規ストリーム / datagram の送信は `WtSession` が状態を見て拒否する。
- `WT_SESSION_GONE` (0x170d7b68) は WebTransport / HTTP/3 のプロトコルコードであり、MOQT のアプリケーションエラーコードではない。[issues/0134](../issues/0134-bug-webtransport-error-code-remap.md) が導入する `WtSendStream::reset` / `WtRecvStream::stop_sending` の remap (`ApplicationErrorCode::to_http3_code` で `0x52e4a40fa8db` 以上へ写す) を通してはならない。
  0134 の変換は MOQT のコード用であるため、プロトコルコードをそのまま渡す経路を分ける (`WtSendStream::reset_protocol_code` / `WtRecvStream::stop_sending_protocol_code` を追加するか、変換関数の引数を「MOQT のアプリケーションコード / H3 のプロトコルコード」の enum にする)。この区別をコメントに残す。
  h3 層の `SessionClosed.reset_streams` は example が h3 へ feed していないストリームを含まないため、中断対象の台帳としては使わない (`register_local_wt_stream` による登録も行わない)。
- `reset_stream_at` が無い現状の QUIC 実装では `RESET_STREAM` にフォールバックする (RESET_STREAM_AT 非対応そのものは [issues/pending/0094](../issues/pending/0094-bug-webtransport-reset-stream-at-unsupported.md) で扱う)。
- セッション終了を MOQT 層へ伝える。受信ループが待ち続ける現状をやめ、次の受信経路が `TransportError::ConnectionClosed` を返し、publisher / subscriber の pipeline がセッション終了として処理する。
  `examples/moqt-transport/src/transport.rs` の `StreamAcceptor::accept_recv_stream` / `BidiStreamAcceptor::accept_bidi_stream` / `StreamHandle::recv_datagrams` / `RecvStream::receive_chunk`
  (`ControlStream::recv_message` は `examples/moqt-transport/src/moqt_client.rs` にあり、内部で `RecvStream::receive_chunk` を呼ぶ)
- イベント処理の書き換えは 1 箇所に集約する。[issues/0137](../issues/0137-bug-webtransport-protocol-header.md) が同じ h3 イベント経路へ `WT_ALPN_ERROR` の判定を足し、[issues/0138](../issues/0138-bug-h3-connection-error-propagation.md) が同じ 2 タスクへ接続エラーの共有スロットを足す。
  実装順は 0136 → 0137 → 0138 とし、先に実装された側の方式に寄せて共通化する。
- close code の切り捨てをなくす。`examples/moqt-transport/src/transport.rs` の `session.close(code as u32, reason)` をやめ、`u32::try_from` で変換して 32 ビットに収まらない場合はエラーにする ([issues/0134](../issues/0134-bug-webtransport-error-code-remap.md) と同じ方針)。
  単体テストできるよう、変換は `webtransport.rs` の純関数 (例: `fn moqt_close_code(code: u64) -> Result<u32>`) に切り出し、`StreamHandle::close` はその結果を `WtSession::close` へ渡す。`as u32` は使わない。

## 完了条件

- セッション終了時にデータストリームが `WT_SESSION_GONE` で中断されることを確認できること。確認方法は次のとおり。
  - 単体テスト: セッション状態 (終了 / drain / 自発 close) から動作 (全ストリーム中断 + 新規拒否 / 新規拒否のみ) を決める判定を純関数に切り出し、`examples/moqt-transport/src/webtransport.rs` の `#[cfg(test)] mod tests` で固定する
  - 単体テスト: `WtSession` の状態を終了へ遷移させると、ストリームを所有するタスクへ渡す watch の値が変わり、中断時に `WT_SESSION_GONE` を渡すことを固定する (I/O ハンドルを持たない範囲)
  - 実機: relay 側から `WT_CLOSE_SESSION` を送る、または CONNECT stream を閉じ、example 側の受信ストリームが `WT_SESSION_GONE` で中断され、MOQT セッションが終了することを `RUST_LOG=debug` のログで確認する。WebTransport セッションの確立自体は 0094 の解消待ちであるため、実機確認は draft-15 相当の peer または 0094 の解消後に行う
- `SessionDraining` の受信では既存ストリームを中断せず、新しいストリームの open と datagram の送信だけを拒否すること (純関数のテストで固定する)
- セッション終了・drain の検知後に新しい stream を open せず、datagram も送らないこと
- CONNECT stream の受信半を feed し続けることで `SessionClosed` が発火すること (h3 層へ feed しないと発火しない) を実装で確認し、コメントに残す
- 32 ビットに収まらない close code が切り捨てられないこと (`moqt_close_code` の単体テストで `u64::MAX` がエラーになること)
