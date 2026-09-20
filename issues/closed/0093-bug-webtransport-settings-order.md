# example の WebTransport クライアントが peer SETTINGS を待たずに CONNECT を送る

- Created: 2026-09-16
- Completed: 2026-09-16
- Branch: feature/fix-webtransport-settings-order
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-webtrans-http3-16 Section 3.1 / 4.6 は、WebTransport CONNECT の送信前に peer の
SETTINGS (`SETTINGS_WT_ENABLED` / `SETTINGS_ENABLE_CONNECT_PROTOCOL` / `SETTINGS_H3_DATAGRAM`) と
QUIC transport parameter (`max_datagram_frame_size` / `reset_stream_at`) を検証していることを
要求する。満たさない状態で CONNECT を送ると `WtSetupError` で失敗する。

`examples/moqt-transport` の WebTransport クライアントは、自分の SETTINGS を送った直後に
サーバーの SETTINGS を受信しないまま CONNECT を送っており、`--url https://...` の経路が
常に `peer SETTINGS not received` で失敗する。

## 現状

- `examples/moqt-transport/src/webtransport.rs` の `WtClient::connect` は H3 ストリームを開いて
  自分の SETTINGS を送った後、単方向ストリームの受信タスクを起動する前に
  `shiguredo_http3::ClientConnection::send_request` で CONNECT を送っている。
- `send_request` は内部で `validate_wt_connect_request` を呼び、`peer_settings` が未設定だと
  `WtSetupError::PeerSettingsNotReceived` を返す。サーバーの SETTINGS はサーバーの制御
  ストリーム (単方向) で届くため、受信タスクを起動して HTTP/3 状態へ流し込むまで
  `peer_settings` は設定されない。
- `set_webtransport_transport_verified` を呼んでおらず、SETTINGS を待つようにしても
  `WtSetupError::TransportNotVerified` で失敗する。

再現手順:

1. MOQT relay (dev relay) を起動する
2. `moqt-subscriber --url https://127.0.0.1:4433/moqt` を起動する
3. `Fatal: WebTransport: http3 error: webtransport setup error: peer SETTINGS not received` で
   終了する (MOQT 層に到達しない)

## 設計方針

- 接続を分割して単方向ストリームの受信タスクを先に起動し、サーバーの制御ストリームを
  HTTP/3 状態へ流し込めるようにする。WT の双方向ストリーム受信タスクは session_id の検証に
  CONNECT stream の id が要るため、CONNECT を開いた後に起動する。
- 自分の SETTINGS を送った後、peer の SETTINGS を受信するまで待ってから CONNECT を送る。
  受信タスクは状態更新のたびに `Notify` で通知するため、通知と状態確認を組み合わせて待つ
  (取りこぼしを避けるため、状態確認より先に通知の受信登録を行う)。上限時間を設け、
  届かない場合は接続が壊れているものとして失敗させる。
- QUIC transport parameter の検証結果を `set_webtransport_transport_verified` で注入する。
  `max_datagram_frame_size` は s2n-quic の datagram provider を有効にして接続していることを
  根拠に真とし、`reset_stream_at` は s2n-quic が対応しないため偽とする (issue 0094 を参照)。

## 完了条件

- `--url https://...` の経路で `peer SETTINGS not received` が出なくなること
- `--url https://...` の経路で `TransportNotVerified` が出なくなること
- 生 QUIC 経路の挙動が変わらないこと
- `cargo build --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` / `cargo test --workspace` が通ること

## 解決方法

- `WtClient::connect` の順序を変更し、接続を分割して単方向ストリームの受信タスクを CONNECT より
  先に起動するようにした。サーバーの制御ストリーム (単方向) が HTTP/3 状態へ流し込まれ、
  peer SETTINGS が設定される。
- 自分の SETTINGS を送った後、`wait_for_peer_settings` で peer SETTINGS の受信を待ってから
  CONNECT を送る。通知の取りこぼしを避けるため状態確認より先に `Notify` の受信登録を行い、
  10 秒の上限を設けて届かない場合は接続が壊れているものとして失敗させる。
- WT の双方向ストリーム受信タスクは session_id の検証に CONNECT stream の id が要るため、
  CONNECT を開いた後に起動するよう移動した。
- `set_webtransport_transport_verified(true, false)` を呼び、QUIC transport parameter の検証結果を
  注入する。`max_datagram_frame_size` は s2n-quic の datagram provider を有効にして接続して
  いることを根拠に真、`reset_stream_at` は s2n-quic が対応しないため偽とした。

検証:

- `--url https://127.0.0.1:4433/moqt` で `peer SETTINGS not received` と `TransportNotVerified` が
  解消し、次の draft-16 要件 (`reset_stream_at`) で停止することを確認した。この残要件は
  issue 0094 で pending として管理する。
- 生 QUIC 経路 (`moqt://`) の相互運用が従来どおり成立することを確認した
  (catalog FETCH / SUBSCRIBE / 映像 300 フレーム描画)。
- `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo test --workspace` が通ることを確認した。
