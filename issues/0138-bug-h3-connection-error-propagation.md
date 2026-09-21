# h3 層の connection error を伝播して CONNECTION_CLOSE を送る

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-h3-connection-error-propagation
- Polished: {YYYY-MM-DD}

## 目的

RFC 9114 §6.2.1 (Control Streams) は、制御ストリームの違反を connection error として扱う MUST を定める。

> Each side MUST initiate a single control stream at the beginning of the connection and send its SETTINGS frame as the first frame on this stream.
> If the first frame of the control stream is any other frame type, this MUST be treated as a connection error of type H3_MISSING_SETTINGS.
> Only one control stream per peer is permitted; receipt of a second stream claiming to be a control stream MUST be treated as a connection error of type H3_STREAM_CREATION_ERROR.
> The sender MUST NOT close the control stream, and the receiver MUST NOT request that the sender close the control stream.
> If either control stream is closed at any point, this MUST be treated as a connection error of type H3_CLOSED_CRITICAL_STREAM.

RFC 9297 §2.1 (HTTP/3 Datagrams) は HTTP/3 Datagram の Quarter Stream ID の不正を H3_DATAGRAM_ERROR とする。

> Receipt of an HTTP/3 Datagram that includes a larger value MUST be treated as an HTTP/3 connection error of type H3_DATAGRAM_ERROR (0x33).

> Receipt of a QUIC DATAGRAM frame whose payload is too short to allow parsing the Quarter Stream ID field MUST be treated as an HTTP/3 connection error of type H3_DATAGRAM_ERROR (0x33).

現状は h3 層が返すエラーを破棄するため、違反を検知しても接続を閉じない。datagram 経路では受信タスクが止まり、購読が静かに止まる。

## 現状

- `examples/moqt-transport/src/webtransport.rs` の `route_uni_stream` は HTTP/3 層へ渡すストリーム (制御 / QPACK) の `feed_stream_only` の戻り値を `let _ = ...` で捨てる。
- 同 `route_bi_stream` も他セッション向け双方向ストリームを h3 層へ渡す際に `feed_stream_only` の戻り値を捨てる。
- 同 datagram 受信タスクは `ClientConnectionState::feed_datagram` のエラーで `tracing::warn!` を出して `break` する。以後 datagram を読まないが接続は閉じない。publisher / subscriber の pipeline はそのまま動き続け、購読が静かに止まる。
- 一方、セッション確立中の `ClientConnectionState::process_stream_data` / `wait_for_peer_settings` は `Result` を `?` で伝播しており、確立後だけエラーが捨てられている。
- 依存 `shiguredo_http3` は `Error::ConnectionError(ErrorCode)` を返し、`ErrorCode` に `H3_MISSING_SETTINGS` (0x10a) / `H3_STREAM_CREATION_ERROR` (0x103) / `H3_CLOSED_CRITICAL_STREAM` (0x104) / `H3_DATAGRAM_ERROR` (0x33) などが定義されている。`ErrorCode::code()` で数値が取れる。

## 設計方針

- HTTP/3 層へ流す経路 (`route_uni_stream` / `route_bi_stream` の `feed_stream_only`、datagram タスクの `feed_datagram`) の戻り値を伝播させる。`Result` を捨てる `let _ =` を残さない。
- `shiguredo_http3::Error::ConnectionError(code)` は接続エラーとして扱い、`code.code()` を application error code として `s2n_quic::connection::Handle::close` で CONNECTION_CLOSE を送る。エラーコードは example 側で数値を再定義せず、h3 層の `ErrorCode` を使う。
- `Error::StreamError` など接続エラーでないものは接続を閉じず、従来どおりそのストリームの処理にとどめる。切り分けは `Error` の variant で行う。
- datagram 受信タスクの停止 (エラーによる `break`、および受信 API のエラー) もセッション終了として扱い、MOQT 層へ伝える。タスクは接続を閉じたうえで、`WtSession` 経由で `TransportError::ConnectionClosed` などを返す。
- ルーティングと datagram のタスクは `WtClient::connect` の中で spawn されるため、接続エラーを example のエラー経路へ渡す共有スロット (エラー保持 + `Notify`) を `WtSession` に持たせる。`accept_uni_stream` / `accept_bi_stream` / `recv_chunk` / `take_buffered_datagrams` はスロットにエラーがあればそれを返す。

## 完了条件

- h3 層の connection error が発生したときに接続が閉じることを確認できること。確認方法は次のとおり。
  - 単体テスト: `ClientConnectionState` に connection error になる入力を与え、example が HTTP/3 のエラーコードで接続を閉じる判断をすること。少なくとも Quarter Stream ID が上限 (`2^60 - 1`) を超える datagram、および SETTINGS 以外のフレームで始まる制御ストリームを入力にする
  - 実機: SETTINGS 以外の最初のフレームを送る制御ストリームを送出するテストクライアントを用意し、example が `H3_MISSING_SETTINGS` で接続を閉じることを `RUST_LOG=debug` のログで確認する。WebTransport セッションの確立自体は [issues/pending/0094](../issues/pending/0094-bug-webtransport-reset-stream-at-unsupported.md) の解消待ちであるため、実機確認は draft-15 相当の peer または 0094 の解消後に行う
- datagram 受信タスクがエラーで停止した場合に、MOQT 層の受信ループがセッション終了を検知すること (購読が静かに止まらないこと)
- `Error::StreamError` などの非 connection error で接続を閉じないこと (単体テストまたは再現手順で確認)
