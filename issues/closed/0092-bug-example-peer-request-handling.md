# example が relay から転送される request に応答できない

- Created: 2026-09-16
- Completed: 2026-09-16
- Branch: feature/fix-example-peer-request-handling
- Polished: {YYYY-MM-DD}

## 目的

MOQT relay は subscriber の SUBSCRIBE / FETCH を publisher 側 session の新しい bidi request
stream として転送する (draft-ietf-moq-transport-21 §6.3 (Session initialization))。publisher は
この stream を受理して応答しなければ、subscriber の要求は relay で止まったまま無応答になり、
subscriber は応答を待ち続ける。

本リポジトリの example (moqt-publisher / moqt-subscriber / moqt-transport) は peer 起動の bidi
stream を受理しないため、relay を介した相互運用が成立しない。実測では relay に対して
SETUP 交換と PUBLISH までは成功するが、catalog の FETCH が publisher へ転送されても応答が返らず、
subscriber は `Catalog FETCH_OK received` に到達しないまま 30 秒の QUIC idle timeout で切断される。

## 現状

- `examples/moqt-transport/src/moqt_client.rs` の `MoqtClient::establish_quic` は
  `StreamAcceptor::split()` の bidi 側を破棄し、uni stream (data stream) しか受理しない。
  `MoqtClient::establish_wt` も同じ。
- `examples/` 配下に `Session::recv_request` の呼び出しが無く、peer 起動 request を Session へ
  通知する経路が存在しない (`recv_stream_message` は自側が開始した request stream の応答用)。
- `MoqtClient` の公開 API は client 側 (`publish_track` / `subscribe_track` / `fetch` /
  `send_request_update` / `send_goaway` / `close`) のみで、応答を返す `send_subscribe_ok` /
  `send_fetch_ok` / `send_request_error` と FETCH 応答ストリームを書く手段が無い。
- `MoqtClient::next_event` は session の注目イベントしか返さないため、アプリは peer 要求を
  受け取る手段を持たない。

再現手順:

1. relay を dev release で起動する (`moqt://127.0.0.1:4433`)
2. `moqt-publisher --url moqt://127.0.0.1:4433 --fake-capture-device` を起動する
3. `moqt-subscriber --url moqt://127.0.0.1:4433` を起動する
4. relay のログは `MOQT relay forwarded FETCH` まで進むが publisher は無応答のままで、
   subscriber のログに catalog の取得完了が出ない

## 設計方針

- peer 起動 bidi stream の受理タスクを `MoqtClient` が持つ。受理した stream の先頭メッセージを
  `Session::recv_request` へ渡して受理し、応答用の送信半を受信タスクと同じ台帳 (`bidi_sends`) に
  登録する。受信方向の STOP_SENDING 依頼も既存の台帳 (`bidi_stop_txs`) に載せ、自側が開始した
  request stream と同じ扱いにする。
- 先頭メッセージと後続メッセージの処理順が入れ替わらないよう、受理通知と後続メッセージを同じ
  mpsc チャネルに載せる。別チャネルに分けると、登録より先に後続メッセージを処理して未知の
  Request ID として扱われる。
- アプリへは `ClientEvent::Request` として要求を渡す。応答 API (`send_subscribe_ok` /
  `send_fetch_ok` / `send_request_error`) と、FETCH 応答ストリームを開いて Object を 1 件書く
  `send_fetch_response` を `MoqtClient` に追加する。
- publisher は video / audio の SUBSCRIBE に SUBSCRIBE_OK を返し、catalog の FETCH に
  FETCH_OK と FETCH 応答ストリームを返す。未知 track は REQUEST_ERROR (DOES_NOT_EXIST) で拒否する。
- subscriber は relay から要求を受け取らないため、届いた場合は REQUEST_ERROR (NOT_SUPPORTED) で
  拒否し、無応答ハングを避ける。
- 変更は example (`moqt-transport` / `moqt-publisher` / `moqt-subscriber`) に限定し、
  `shiguredo_moqt` 本体の公開 API は変更しない。

## 完了条件

- relay 経由で publisher の catalog FETCH が完了し、subscriber が catalog を取得できること
- relay が転送した SUBSCRIBE に publisher が SUBSCRIBE_OK を返し、subscriber が object を受信
  できること
- 未知 track への SUBSCRIBE / FETCH が REQUEST_ERROR で拒否されること
- 生 QUIC 経路と WebTransport over HTTP/3 経路の両方で peer 起動 bidi stream を受理すること
- `cargo build --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること
- `cargo test --workspace` が通ること

## 解決方法

- `moqt-transport` に `BidiStreamAcceptor` を追加し、peer 起動の双方向ストリームを受理するようにした。
  生 QUIC は `BidirectionalStreamAcceptor` を、WebTransport は WT の双方向ストリームヘッダーを
  読み捨てる `route_bi_stream` を追加して両経路で受理する。
- 受理した先頭メッセージを `Session::recv_request` に渡し、要求と応答用の送信半を
  `BidiMessage::PeerRequest` としてメインループへ渡す。自側が開始した request stream の
  メッセージと同じ mpsc チャネルに載せることで、先頭メッセージの登録より先に後続メッセージを
  処理しないことを保証する。
- `MoqtClient::next_event` は `ClientEvent` を返すようにし、`ClientEvent::Request` で peer からの
  要求をアプリへ渡す。応答用に `send_subscribe_ok` / `send_fetch_ok` / `send_request_error` /
  `send_fetch_response` を追加した。
- `moqt-publisher` は `serve_peer_request` で video / audio の SUBSCRIBE に SUBSCRIBE_OK、
  catalog の FETCH に FETCH_OK と FETCH 応答ストリームを返す。未知 track は REQUEST_ERROR
  (DOES_NOT_EXIST)、未対応種別は REQUEST_ERROR (NOT_SUPPORTED) で拒否する。
- `moqt-subscriber` は relay から要求を受け取らないため、届いた要求を REQUEST_ERROR
  (NOT_SUPPORTED) で拒否して無応答ハングを避ける。
- catalog 取得の読み出しループは fetch 応答ストリームの終端を `receive_registered_stream_data` で
  検知して抜けるため、抜けた後の drain が同じ stream の終端を二重に通知していた。drain を削除し、
  終端の通知は Session の受信 fetch stream 経路に一本化した。

検証:

- relay の dev relay (`moqt://127.0.0.1:4433`) に対して実測し、SETUP 交換 / PUBLISH /
  catalog FETCH / SUBSCRIBE / object 受信が成立し、subscriber が映像を 400 フレーム描画することを
  確認した。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ることを確認した。
