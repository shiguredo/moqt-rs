# peer 起点のセッション終了をコードと理由付きで通知できるようにする

- Created: 2026-10-09
- Completed: {YYYY-MM-DD}
- Branch: feature/add-peer-session-close
- Polished: {YYYY-MM-DD}

## 目的

peer が transport 層でセッションを終了したとき、その終了コードと理由をアプリが観測できるようにする。

draft-ietf-moq-transport-22 §6.6 (Termination) は、セッションの終了を transport 層で行い、native QUIC では
CONNECTION_CLOSE、WebTransport では CLOSE_WEBTRANSPORT_SESSION カプセルで終了すると定める。
終了コードは §12.2 (Session Termination Codes) の値である (例: CONTROL_MESSAGE_TIMEOUT (0x11))。
現状の Session にはこの終了コードと理由を渡す口が無く、peer が理由付きで終了しても
アプリには常に PROTOCOL_VIOLATION (0x3) + 固定文言 ("peer control stream reset" /
"peer control stream closed with FIN") として届く。peer が送ったコードと理由は失われる。

区別したい状況は次の 2 つである。

- transport の終了に伴う control stream の閉鎖 (peer 起点のセッション終了であり、§6.3 違反ではない)
- session の lifetime 中に control stream が不正に閉じられた場合 (§6.3 (Session initialization) 違反)

## 現状

- `src/session/core.rs` の `Session::recv_control_stream_closed(RequestStreamEnd)` は終了コードと理由を
  受け取らず、`RequestStreamEnd::Fin` で `SESSION_PROTOCOL_VIOLATION` + "peer control stream closed with FIN"、
  `RequestStreamEnd::Reset` で `SESSION_PROTOCOL_VIOLATION` + "peer control stream reset" を生成する。
- Session のセッション制御系の入力 API は `recv_control` / `recv_control_stream_type` /
  `recv_control_stream_closed` / `recv_request` / `recv_request_stream_closed` / `recv_stream_message` のみで、
  peer 起点のセッション終了をコードと理由付きで渡す口が無い (data plane の `recv_*` は対象外)。
- `SessionEvent::CloseSession` は `SessionError` を運ぶが、peer 起点 (`recv_control_stream_closed` 経由) と
  ローカル起点 (`Session::close` / `fail`) を区別する情報を持たない。
- `SessionError::reason` は `&'static str` である。peer から渡される理由は transport 由来の動的な文字列で
  あり、そのままでは保持できない。
- I/O 層の例: `examples/tokio-moq/src/moqt_client.rs` は control stream の終端を
  `StreamRead::Closed(end)` で受け取り `recv_control_stream_closed(end)` を呼ぶだけで、
  transport が運んだ終了コードと理由を Session へ渡していない。
- moqt-py 経由の実測では、WT-H3 で server が CONTROL_MESSAGE_TIMEOUT (0x11) と理由付きで終了しても、
  client の close イベントは (3, "peer control stream reset") になる。

## 設計方針

- peer 起点のセッション終了をコードと理由付きで通知する API (`Session::recv_session_closed` 相当) を追加する。
- `SessionEvent::CloseSession` で peer 起点とローカル起点を区別できるようにする。公開 API の変更になるため、
  起点を表すフィールドを追加する案と専用 variant を追加する案を比較して決める。
- `SessionError::reason` を peer の動的な理由を保持できる型 (例: `Cow<'static, str>`) に変える。
  peer の理由の長さと UTF-8 の扱い (transport 依存の値として上限を設けないか、§8.5 (Reason Phrase Structure)
  の 1024 バイト上限を流用するか) も決める。
- 新 API の呼び出し後は `Closing` → (`poll_event` で `CloseSession` を取り出した時点で) `Closed` へ遷移し、
  後続の `recv_control_stream_closed` を既存の Closing / Closed ガードと同じく no-op にする。
- `recv_control_stream_closed` の既存の挙動 (コード・理由・セッション状態) は変えない。transport の終了に
  伴う閉鎖と session 中の不正な閉鎖の区別は I/O 層が行い、transport の終了コードと理由を取得できた場合に
  新 API を呼ぶ。
- `examples/tokio-moq` の I/O 層は transport の終了コードと理由を取得して新 API を呼ぶ。取得経路は
  s2n-quic / shiguredo_http3 / shiguredo_http2 の API に依存するため、example 側の対応は取得可否を
  確認してから決める。

## 完了条件

- peer 起点のセッション終了コードと理由が `SessionEvent::CloseSession` で観測できること。
- ローカル起点 (`close` / `fail`) と peer 起点が区別できること。
- 既存の `recv_control_stream_closed` の挙動 (固定コード・固定理由・セッション状態) が変わらないこと。
- 新 API の呼び出し後に届く control stream の終了通知が no-op であること。
