# 初回 SETUP の decode 失敗時にセッション終了コードを送る

- Created: 2026-10-12
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-initial-setup-decode-close-code
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §8.9 (Authorization Token Compression) は "If the Token structure
cannot be decoded, the receiver MUST close the Session with KEY_VALUE_FORMATTING_ERROR." と定める。
peer の初回 SETUP の decode に失敗した場合、example は接続を切るだけで終了コードを送らない。

## 現状

- `examples/tokio-moq/src/moqt_client.rs` の `finish_handshake` は
  `control_recv.recv_message().await?` で decode 失敗を `TransportError::Moqt` として返し、
  呼び出し元 (`MoqtClient::establish_quic` / `establish_wt` / `establish_wt_h2` → `moq-pub` /
  `moq-sub` の pipeline) が `?` で終了する。session は CloseSession を発行しない。
- 確立後の decode 失敗 (control 受信 task / request 先頭メッセージ) は 0222 で
  `session_error_code` に写して終了コード付きで閉じるようにしたが、初回 SETUP だけが
  同じ扱いになっていない。
- 終了コードの写像 `session_error_code` は `examples/moq-pub/src/pipeline.rs` と
  `examples/moq-sub/src/pipeline.rs` に private で定義されている。`finish_handshake` は
  `examples/tokio-moq` 側にあり、失敗時点で `MoqtClient` が存在しないため pipeline から
  閉じることはできない。
- `Session::close` は Closing / Closed 以外では状態を問わず CloseSession を発行するため、
  SETUP 交換中でも終了コードを送れる。
- `examples/moq-pub` / `examples/moq-sub` の doc は「peer の初回 SETUP の decode 失敗は
  接続確立 (`MoqtClient::establish_*`) の時点で確定し、確立前に終了コードを送れないため
  本関数には届かない (既知の限界)」と書いており、本対応で実態が変わる。

## 設計方針

- `finish_handshake` の decode 失敗で `Session::close` を終了コード付きで呼び、CloseSession を
  I/O 層へ流してから error を返す。
- あわせて、SETUP 交換中に session 層が失敗した場合 (`session.recv_control(peer_setup)` の
  role / transport / URI / REGISTER alias の検証失敗など) も同じ扱いにする。現状は
  `SessionError` を `TransportError::Internal` に畳んで伝播するため、終了コード
  (INVALID_AUTHORITY / INVALID_PATH / DUPLICATE_AUTH_TOKEN_ALIAS など) が peer に届かない。
  この場合 session は既に Closing で `Session::close` は no-op になるため、`poll_event` で
  `CloseSession` を取り出して I/O 層へ流す。
- 終了コードの写像は 0222 で入れた写像と同じ規則にする (§8.3 の書式違反は
  KEY_VALUE_FORMATTING_ERROR、§8.9 のトークン不正は MALFORMED_AUTH_TOKEN、それ以外は
  PROTOCOL_VIOLATION)。現在の写像は pipeline 側に private で置かれているため、
  `examples/tokio-moq` へ移設して `moq-pub` / `moq-sub` と共有する。
- `examples/moq-pub` / `examples/moq-sub` の doc から「初回 SETUP では終了コードを送れない」
  という既知の限界の記述を除く。
- 検証には peer を模擬する in-process 試験基盤 (0242) を使う。

## 完了条件

- peer の初回 SETUP の decode に失敗したとき、example が終了コード付きのセッション終了を
  送ってから終了すること。
- SETUP 交換中に session 層が失敗したときも、同じく終了コード付きのセッション終了を送ってから
  終了すること。
- 終了コードの写像が確立後の decode 失敗と同じであること (写像が `examples/tokio-moq` に
  集約され、`moq-pub` / `moq-sub` が同じ関数を使っていること)。
- `moq-pub` / `moq-sub` の doc の既知の限界の記述が消えていること。
- 上記がテストで固定されていること。モック / スタブを使わないこと。
