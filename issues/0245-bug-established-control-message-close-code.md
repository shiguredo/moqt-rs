# 確立後の control stream の失敗でもセッション終了コードを送る

- Created: 2026-10-13
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-established-control-message-close-code
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §9 (Control Messages) は不正な control message の受信を
PROTOCOL_VIOLATION などの終了コードで閉じることを MUST で要求する。0240 で SETUP 交換中
(確立前) の失敗は終了コード付きで閉じるようにしたが、確立後の main ループ (`pump_once`) には
同じ欠陥クラスが 2 経路残っている。session が積んだ `CloseSession` が drain されず、example は
終了コードを送らずに接続を切る。

## 現状

- `examples/tokio-moq/src/moqt_client.rs` の `pump_once` は、control stream から受信した
  メッセージを `session.recv_control(message).map_err(|e| TransportError::Internal(format!(
  "recv_control: {e}")))?` で処理する。`?` は直後の `self.drain_events().await?` より先に
  return するため、`Session::recv_control` が `fail` で積んだ `CloseSession` が drain されない。
  確立後に届いた 2 つ目の SETUP (duplicate SETUP) がこの経路に入る。
- 同じ形が control stream のメッセージ受信の decode 失敗にもある
  (`session.recv_stream_message(...)` の結果を `?` で返し、その後の `drain_events` に到達しない)。
- 終了コードを送る経路は `drain_events` の `CloseSession` 処理 (`self.handle.close(err.code,
  err.reason)`) だけである。
- `Session::recv_request` の失敗経路 (0240 直後の 0236 で追加) は `drain_events` を呼んでおり
  欠陥は無い。
- 実測 (0240 のレビュー時): 確立後に 2 つ目の SETUP を制御ストリームへ送ると、example は
  `TransportError::Internal("recv_control: session error 0x3: duplicate SETUP received")` で
  終了し、peer 側では CONNECTION_CLOSE が観測されない (PROTOCOL_VIOLATION が届かない)。

## 設計方針

- `pump_once` の `recv_control` と `recv_stream_message` の失敗で、`SessionError` を受け取った
  場合は `drain_events` を呼んで `CloseSession` を処理させてから error を返す (0240 が
  `close_handshake_session` で行った扱いと同じ)。`drain_events` が処理した `CloseSession` は
  `ClientEvent::Session(SessionEvent::CloseSession(err))` としてアプリへも届くため、example が
  終了コード付きで閉じたことをアプリが観測できる。
- decode 失敗 (`TransportError::Moqt`) も同じ扱いにする。終了コードは 0240 で
  `examples/tokio-moq/src/error.rs` へ移設した `session_error_code` を使う (確立後の
  `close_on_message_error` / `Err(Error::Moqt(e))` の経路と同じ写像)。
- transport 自体の失敗 (`TransportError::Moqt` 以外) は接続が死んでいるため対象外とする。
- 検証は `examples/tokio-moq/src/peer_harness.rs` を使う。確立後に 2 つ目の SETUP を送る経路と、
  確立後に decode できない control message を送る経路を追加し、peer 側で CONNECTION_CLOSE の
  終了コードを観測する (`CloseCodeRecorder`)。

## 完了条件

- 確立後に control message の受信で session 層が失敗したとき (`recv_control` の duplicate SETUP
  など)、example が対応する §12.2 (Session Termination Codes) のコードでセッション終了を送って
  から終了すること。
- 確立後に control stream の decode に失敗したときも、同じく写像で決まるコードでセッション終了を
  送ってから終了すること。
- 上記がテストで固定されていること (peer が CONNECTION_CLOSE のコードを観測する)。モック /
  スタブを使わないこと。
