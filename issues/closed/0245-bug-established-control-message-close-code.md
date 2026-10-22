# 確立後に session 層が失敗したとき example がセッション終了コードを送る

- Created: 2026-10-13
- Completed: 2026-10-23
- Branch: feature/fix-established-control-message-close-code
- Polished: 2026-10-23

## 目的

draft-ietf-moq-transport-22 §9 (Control Messages) は、未知のメッセージ型を受信した endpoint に
セッションを閉じることを MUST で要求し、メッセージ本体の長さが一致しない場合も
PROTOCOL_VIOLATION で閉じることを MUST とする。終了コードの選択は §12.2 (Session Termination
Codes) が "the application MAY use any error message and SHOULD use a relevant code" と定める。
control stream 上の 2 通目の SETUP は §6.3 (Session initialization) の「各 peer は SETUP で
始まる control stream を 1 本だけ開く」と §9.1 (SETUP) に反し、本ライブラリは
PROTOCOL_VIOLATION (0x3) で扱う。0240 で SETUP 交換中 (確立前) の失敗は終了コード付きで閉じる
ようにしたが、確立後の main ループ (`pump_once`) には Session の失敗を drain しない経路が
2 つ残っており、example は終了コードを送らずに接続を切る。

## 現状

- `examples/tokio-moq/src/moqt_client.rs` の `pump_once` は、control stream から受信した
  メッセージを `session.recv_control(msg).map_err(|e| TransportError::Internal(format!(
  "recv_control: {e}")))?` で処理する。`?` は直後の `self.drain_events().await?` より先に
  return するため、`Session::recv_control` が `fail` で積んだ `CloseSession` が drain されない。
  確立後に届いた 2 つ目の SETUP (duplicate SETUP) がこの経路に入る。SessionError は
  `TransportError::Internal` に写るため、`examples/moq-pub` の `close_on_message_error` と
  `examples/moq-sub` の `Err(Error::Moqt(e))` の終了コード送信経路にも該当しない。
- 同じ形が bidi request stream のメッセージ処理 (`session.recv_stream_message(rid, msg)`) にも
  ある。`recv_result?` が `drain_events` より先に return するため、Session が積んだ
  `CloseSession` が drain されない。こちらも `Internal` に写る。
- control stream / bidi request stream の decode 失敗も `pump_once` の `match res?` で
  `drain_events` より先に return するが、この場合 Session は decode 失敗を観測せず
  `CloseSession` を積まない。`TransportError::Moqt` は `Error::Moqt` に保たれ、`moq-pub` の
  `close_on_message_error` と `moq-sub` の `Err(Error::Moqt(e))` 経路が `session_error_code` で
  終了コードを決めて閉じる (0222)。MoqtClient を直接使う利用者へは `TransportError::Moqt` と
  して伝わり、閉じるかは利用者が決める既存の契約である。本 issue の対象外とする。
- 終了コードを送る経路は、確立後の通常経路では `drain_events` の `CloseSession` 処理
  (`self.handle.close(err.code, err.reason)`) が担う。SETUP 交換中は
  `close_handshake_session` が `handle.close` を直接呼び、`MoqtClient::close` も公開 API として
  `handle.close` を直接呼ぶ。
- 実測 (0240 のレビュー時): 確立後に 2 つ目の SETUP を制御ストリームへ送ると、example は
  `TransportError::Internal("recv_control: session error 0x3: duplicate SETUP received")` で
  終了し、peer 側では CONNECTION_CLOSE が観測されない (PROTOCOL_VIOLATION が届かない)。

## 設計方針

- `pump_once` の次の 2 経路で、`SessionError` を受け取った場合は接続を閉じてから error を返す。
  - control stream: `session.recv_control(msg)` の失敗
  - bidi request stream: `session.recv_stream_message(rid, msg)` の失敗

  どちらも `Session` が `fail` で `CloseSession` を積んでいるため、`drain_events` を呼ぶと
  `CloseSession` が `self.handle.close(err.code, err.reason)` を実行する。
  - `lock_session` のガードを保持したまま `drain_events().await` を呼ぶと同一 `Mutex` の
    再ロックで停止するため、ガードを落としてから呼ぶ。
  - `drain_events` は `CloseSession` を処理したあと `notable_events` に積むが、`pump_once` は
    その後 `Err(TransportError::Internal(...))` を返す。`next_event` は `drain_events` →
    `take_notable_event` → `pump_once` の順であるため、アプリは同じ呼び出しで
    `ClientEvent::Session(CloseSession)` を観測しない。終了コードは transport の close として
    peer が観測する。この挙動を `pump_once` のコメントに明記する。
  - 戻り値は従来どおり `TransportError::Internal` とする (診断情報を保つため)。0236 が
    `recv_request` の失敗経路で `drain_events` の後に `Ok(())` を返してアプリへイベントを
    配送するのとは意図的に挙動を変える (既存のエラー戻り値を維持する)。
- decode 失敗 (`TransportError::Moqt`) は本 issue の対象外とする。Session が
  `CloseSession` を積まず、`moq-pub` / `moq-sub` は 0222 の経路で `session_error_code` を使って
  閉じている。`pump_once` で `MoqtClient` 自身が閉じる設計変更は、pipeline の close と責務が
  重複するため行わない。
- 検証は `examples/tokio-moq/src/peer_harness.rs` を使い、実 s2n-quic 接続で観測する。次の
  テスト用 API を追加する。
  - 既存の control stream (`TestPeer::control_send`) へ stream type prefix を付けずに
    control message を書くメソッド (例: `send_control_message(&mut self, message:
    ControlMessage)`)。
  - peer が開いた request stream (`OpenedRequest::send`) へ 2 通目以降の control message を
    書くメソッド (例: `send_opened_request_message(&mut self, request_id: u64, message:
    ControlMessage)`)。
  - 既存の `send_setup` / `send_control_stream_head` は新しい uni stream を開いて既存 control
    stream を FIN する。確立後に使うと client は control stream の終端を観測して 0x3 で閉じる
    ため、duplicate SETUP の経路を試さずに 0x3 が観測される偽陽性になる。確立後のテストでは
    使わない。
  - 検証対象は QUIC 直接接続に限る (0240 と同じ)。QMux の `TestPeer` は `recv_close_code` を
    観測できない。
- テストは `peer_harness.rs` の `mod tests` に置き、`#[ignore]` を付けず
  `cargo test --workspace` で実行する。モック / スタブは使わない。
- `pump_once` のコメントと `peer_harness` の module doc (提供能力一覧) を実装に合わせて更新する。

## 完了条件

- 確立後に既存 control stream へ 2 通目の SETUP を送ると、client が
  `SESSION_PROTOCOL_VIOLATION` (0x3) で CONNECTION_CLOSE を送ってから終了すること。peer が
  `recv_close_code` で 0x3 を観測し、client の error が `recv_control` と duplicate SETUP を
  含む `TransportError::Internal` であることを固定する (control stream の FIN による 0x3 と
  区別するため)。
- 確立後に、peer が開いた request stream へ 2 通目として Table 5 で "First" を持つ
  SUBSCRIBE を送ると、Session が PROTOCOL_VIOLATION を返す。client が
  `SESSION_PROTOCOL_VIOLATION` (0x3) で CONNECTION_CLOSE を送ってから終了し、peer が
  `recv_close_code` で 0x3 を観測し、client の error が `recv_stream_message` を含む
  `TransportError::Internal` であることを固定する (bidi request stream の
  `recv_stream_message` 経路)。
- decode 失敗の扱いを変えないこと (`TransportError::Moqt` をアプリへ伝える既存契約と 0222 の
  pipeline 経路を維持する)。
- doc: `pump_once` のコメントと `peer_harness` の module doc が実装と一致していること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。

## 解決方法

確立後の `pump_once` で Session が失敗した 2 経路について、Session が積んだ
`CloseSession` を drain してから error を返すようにした。

- `examples/tokio-moq/src/moqt_client.rs` の `pump_once` で、control stream の
  `session.recv_control(msg)` と bidi request stream の `session.recv_stream_message(rid, msg)`
  が `Err(SessionError)` を返した場合に、`lock_session` のガードを落としてから
  `drain_events` を呼び、`CloseSession` の処理 (`handle.close(err.code, err.reason)`) で
  終了コードを送ってから `TransportError::Internal` を返すようにした。ガードを保持したまま
  `drain_events().await` を呼ぶと同一 `Mutex` の再ロックで停止するため、先に落としている。
  `next_event` は同じ呼び出しで `take_notable_event` に到達しないため、アプリは
  `CloseSession` イベントを観測しない。終了コードは transport の close として peer が
  観測する。この挙動を `pump_once` のコメントに明記した。
- decode 失敗 (`TransportError::Moqt`) の扱いは変えていない。Session は `CloseSession` を
  積まず、`moq-pub` / `moq-sub` が 0222 の経路で `session_error_code` により終了コードを
  決めて閉じる。本 issue の対象外である。
- `examples/tokio-moq/src/peer_harness.rs` にテスト用 API を追加した。既存 control stream へ
  stream type prefix なしで control message を書く `send_control_message` と、peer が開いた
  request stream へ 2 通目以降の control message を書く `send_opened_request_message` である。
  新しい uni stream を開く `send_setup` / `send_control_stream_head` は既存 control stream を
  FIN するため、確立後の経路には使わない。module doc の提供能力一覧と `control_send` の doc を
  実装に合わせて更新した。
- テストを追加した。確立後に既存 control stream へ 2 通目の SETUP を送ると `recv_control` と
  duplicate SETUP を含む `TransportError::Internal` で終了し、peer が PROTOCOL_VIOLATION (0x3)
  を観測することを固定した。確立後に peer が開いた request stream へ 2 通目の SUBSCRIBE
  (Table 5 の "First" 違反) を送ると、`recv_stream_message` を含む
  `TransportError::Internal` で終了し、peer が 0x3 を観測することを固定した。どちらも QUIC
  直接接続の実 s2n-quic 接続を使い、モック・スタブや `#[ignore]` は使っていない。
  `drain_events` の呼び出しを削除する変異で両テストが失敗することも確認した。
