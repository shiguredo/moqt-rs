# h3 層の connection error を伝播して CONNECTION_CLOSE を送る

- Created: 2026-09-21
- Completed: 2026-09-26
- Branch: feature/fix-h3-connection-error-propagation
- Polished: 2026-09-22

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
- 捨てているのはこの 3 経路だけである。`WtSession::accept_uni_stream` は接続確立時に 1 回呼ばれるだけで、`WtSession::accept_bi_stream` は呼び出し元が無い (確立後は `take_bi_receiver` で receiver を取り出す)。
  確立後の受信ループは `examples/moqt-transport/src/transport.rs` の `StreamAcceptor::accept_recv_stream` (`uni_rx.recv().await`) / `BidiStreamAcceptor::accept_bidi_stream` (`bi_rx.recv().await`) / `StreamHandle::recv_datagrams` で待つ。
  datagram タスクの `break` はチャネルを閉じないため、待っている側は気づかない。
- 依存 `shiguredo_http3` は `Error::ConnectionError(ErrorCode)` を返し、`ErrorCode` に `MissingSettings` (0x10a) / `StreamCreationError` (0x103) / `ClosedCriticalStream` (0x104) / `H3DatagramError` (0x33) などが定義されている (Display は `H3_MISSING_SETTINGS` などの名前を返す)。`ErrorCode::code()` で数値が取れる。

## 設計方針

- HTTP/3 層へ流す経路 (`route_uni_stream` / `route_bi_stream` の `feed_stream_only`、datagram タスクの `feed_datagram`) の戻り値を捨てない。`Error::ConnectionError(code)` かどうかを example の純関数 (`fn connection_error_code(e: &shiguredo_http3::Error) -> Option<u64>` など) で切り分ける。
- `Error::ConnectionError(code)` は接続エラーとして扱い、`code.code()` を application error code として `s2n_quic::connection::Handle::close` に `s2n_quic::application::Error::new(...)` で渡して CONNECTION_CLOSE を送る。エラーコードは example 側で数値を再定義せず、h3 層の `ErrorCode` を使う。
- `Error::StreamError` など接続エラーでないものは接続を閉じず、そのストリームの処理を止めるだけにしてログに残す (スロットには入れない)。`let _ =` を match に置き換えて 2 分岐を明示する。h3 は `StreamError` を「そのストリームを reset すべき」と定義するが、example は他セッションの bidi を h3 へ流すだけで自前の reset 対象を持たないため、ここでは処理の停止とログにとどめる。
- 接続エラーの発生を MOQT 層の受信ループへ伝える。タスクは接続を閉じたうえで、共有スロット (エラー保持 + `Notify`) にエラーを記録して `Notify` を起こす。待機中の `recv().await` を終わらせる手段は次の 2 つで、読み出し側はどちらでもエラーを返す。
  - 読み出し側が `tokio::select!` で `Notify` を待ち、通知されたらスロットのエラーを返す (datagram タスクは `mpsc::Sender` を持たないため、datagram の停止はこの経路でしか伝わらない)
  - uni / bi の受信ループが持つ送信側チャネルは accept ループが保持しているため、接続を閉じて accept ループが終了し、元の sender が drop されて `recv()` が `None` を返す。読み出し側は `None` のときもスロットにエラーがあればそれを返す
  スロットはルーティングと datagram のタスクを spawn する前に `Arc` で作り、タスクへ clone を渡し、`WtSession` にも同じ `Arc` を保持させる (タスクの spawn は `WtSession` の構築より前なので、`WtSession` を直接参照できない)。
  読み出し側は次の 4 箇所に置く。`StreamAcceptor::accept_recv_stream` / `BidiStreamAcceptor::accept_bidi_stream` / `StreamHandle::recv_datagrams` / `ControlStream::recv_message` (`RecvStream::receive_chunk`)。チャネルが閉じて `None` になった場合も、スロットにエラーがあればそれを返す。
- datagram 受信タスクの停止 (h3 エラーによる `break`、および受信 API のエラー) もセッション終了として扱い、上記の経路で MOQT 層へ伝える。
- セッション終了の状態管理は [issues/0136](../issues/0136-bug-webtransport-session-termination.md) が導入する `tokio::sync::watch` と統合する。0136 の watch (セッション状態) と本 issue のスロット (エラー保持) は同じ受信経路に入るため、先に実装された側の方式に寄せて 1 箇所にまとめる。
  [issues/0135](../issues/0135-bug-webtransport-session-id-validation.md) も同じ 2 タスクへ接続クローズを足す (route の 2 タスクが接続を閉じるための `Handle` の clone は 0135 が導入する。0138 を先に実装する場合はここで用意する)。[issues/0137](../issues/0137-bug-webtransport-protocol-header.md) は接続エラーの表現 (`ProtocolNegotiationFailed`) を追加する。
  実装順は 0135 → 0136 → 0137 → 0138 とし、接続エラーの型は 0137 の variant と重複しない名前にする。

## 完了条件

- h3 層の connection error が発生したときに接続が閉じることを確認できること。確認方法は次のとおり。
  - 単体テスト: `Error::ConnectionError(ErrorCode::MissingSettings)` などと `Error::StreamError` を判別関数に与え、前者だけが接続を閉じる判断 (クローズするエラーコード付き) になり、後者がクローズしない判断になること
  - 単体テスト: datagram の Quarter Stream ID が上限 (`2^60 - 1`) を超える入力を `feed_datagram` に与えると `Error::ConnectionError(H3DatagramError)` が返ること。ただし `feed_datagram` は WebTransport の交渉が完了するまで入力を `Ok(())` で捨てるため、peer SETTINGS などを流して交渉済みの状態を作ってから入力する (この前準備をテストに書く)
  - 単体テスト: SETTINGS 以外のフレームで始まる制御ストリームを `feed_stream_only` へ流すと `Error::ConnectionError(MissingSettings)` が返ること (こちらは交渉済み状態を必要としない)
- datagram 受信タスクがエラーで停止した場合に、MOQT 層の受信ループがセッション終了を検知すること (購読が静かに止まらないこと)。スロットの有無と `Notify` の状態から読み出し側が返す値を決める判定を純関数に切り出して単体テストで固定する (読み出し側の 4 箇所は s2n-quic のハンドルを持つため単体テストで構築できず、配線は実装のレビューと実機確認で確かめる)
- `Error::StreamError` などの非 connection error で接続を閉じないこと (単体テスト)
- 実機: SETTINGS 以外の最初のフレームを送る制御ストリームを送出するテストクライアントを用意し、example が `H3_MISSING_SETTINGS` で接続を閉じることを `RUST_LOG=debug` のログで確認する。WebTransport セッションの確立自体は [issues/pending/0094](../issues/pending/0094-bug-webtransport-reset-stream-at-unsupported.md) の解消待ちであるため、実機確認は draft-15 相当の peer または 0094 の解消後に行う

## 解決方法

- `examples/moqt-transport/src/webtransport.rs`
  - h3 層へ入力を流す経路 (route タスクの feed、datagram の `feed_datagram`、CONNECT stream の feed と
    RESET_STREAM、確立待ちループの drain) の戻り値を `H3FeedOutcome` に集約し、`connection_error_code` で
    `Error::ConnectionError(code)` とそれ以外を切り分ける。`Error::ConnectionError` は `code.code()` を載せた
    `s2n_quic::connection::Handle::close` で接続を閉じ、共有するセッション状態を `ConnectionFailed` にして
    新しいストリームの open と datagram の送信を拒否し、既存ストリームの中断と受信経路の待機終了を起こす
  - 接続エラーでない場合 (ストリームエラー等) は接続を閉じず、具体コードを保持できる
    `TransportError::Http3(error)` を返してそのストリームの処理だけを止める
  - エラーコードは example 側で数値を再定義せず、h3 層の `ErrorCode` を使う
  - drain が返す接続エラーも入力のエラーと同じ経路で扱う (`drain_after` / `drain_events_outcome`)
  - 設計上の補足: issue の設計方針が挙げる「共有スロット + `Notify`」ではなく、0136 が導入した
    セッション状態の `watch` を 1 本にまとめる形にした。同じ受信経路が 1 つの値で状態と終了理由を観測でき、
    待機機構が 2 系統にならないため
- `examples/moqt-transport/src/transport.rs`
  - 受信経路 (`accept_recv_stream` / `accept_bidi_stream`) が `ConnectionFailed` を観測して
    `TransportError::ConnectionClosed` を返す
- 追加・更新したテスト
  - `connection_error_code_only_matches_connection_errors` / `connection_error_codes_match_registered_values`
    (接続エラーとそれ以外の判別、`H3_MISSING_SETTINGS` / `H3_STREAM_CREATION_ERROR` / `H3_CLOSED_CRITICAL_STREAM` /
    `H3_DATAGRAM_ERROR` の登録値)
  - `feed_datagram_reports_connection_error_for_oversized_quarter_stream_id` /
    `feed_datagram_reports_connection_error_for_truncated_payload` (RFC 9297 §2.1 の 2 条件。peer SETTINGS を流し
    `set_webtransport_transport_verified` を注入して交渉済みの状態を作る)
  - `feed_stream_reports_connection_error_for_control_stream_without_settings` /
    `feed_stream_accepts_control_stream_starting_with_settings` (RFC 9114 §6.2.1 の 1 条件と対照)
  - `connection_error_close_code_uses_the_h3_registry` / `connection_error_marks_the_session_as_failed`
  - `wait_for_peer_settings_ends_when_the_session_is_gone` / `wait_for_peer_settings_succeeds_after_settings_arrive`
  - 既存テストを `ConnectionFailed` を含む形に更新する
- 既知の残課題 (本 issue では対応しない)
  - `WtSession::accept_uni_stream` は呼び出し元が先に `take_uni_receiver` で receiver を取り出すため
    (受信順序の欠陥) 現状は到達せず、セッション終了を観測しない。受信順序を直す側で、この関数にも
    セッション終了の観測を足す必要がある
  - `send_request` / `take_stream_data` は h3 層の Sans I/O なエンコード経路であり、接続エラーの伝播は
    本 issue が対象とする入力を流す経路の範囲外である
- 実機確認 (SETTINGS 以外の最初のフレームを送るテストクライアントで `H3_MISSING_SETTINGS` を確認する) は、
  WebTransport セッションの確立自体が `issues/pending/0094` (s2n-quic の RESET_STREAM_AT 非対応) で
  塞がれているため未実施である。代替として、公開エンコーダで組み立てた wire のバイト列を h3 層へ流し、
  `Error::ConnectionError` が返ることを単体テストで固定した
