# example の WebTransport セッション確立を修正する

- Created: 2026-09-11
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-example-webtransport-session-establishment
- Polished: {YYYY-MM-DD}

## 目的

`examples/moqt-transport/src/webtransport.rs` の WebTransport クライアントが現在の依存クレートでセッションを確立できず、datagram 受信も機能しない問題を解消する。0010 (WebTransport 経由の datagram 受信を機能させる) の調査で判明した前提不備をまとめて対応する。

## 現状

`WtClient::connect` は次の理由で WebTransport セッションを確立できない。

- peer の制御ストリーム (SETTINGS) を `h3_conn` へ feed する前に `h3_conn.send_request` で CONNECT を送っている。依存 `shiguredo_http3` の `send_request_inner` は `validate_wt_connect_request` を呼び、`peer_settings` が未受信だと `WtSetupError::PeerSettingsNotReceived` を返す。
  draft-ietf-webtrans-http3-16 §4.6 はクライアントが CONNECT 前にサーバー SETTINGS を受信するまで待つ MUST を定める。
- `set_webtransport_transport_verified` を一度も呼んでいない。同クレートの `is_wt_fully_negotiated()` が `wt_transport_verified` を要求するため、`feed_datagram` は受信 datagram を黙って破棄する (`wt_stream.rs` の `if !self.is_wt_fully_negotiated() { return Ok(()); }`)。
  検証には QUIC の `max_datagram_frame_size > 0` と `reset_stream_at` 対応が必要だが、s2n-quic 1.88 の公開 API に peer の transport parameter を取得する手段がなく、`RESET_STREAM_AT` 自体も実装されていない。draft-15/16 は `reset_stream_at` を要求する。
- セッション確立応答の処理で `process_stream_data` が返すイベントのうち `:status` と `HeadersEnd` 以外を破棄するため、確立時に flush されるバッファ済み datagram が失われる (draft-ietf-webtrans-http3-16 §4.6 は確立までのバッファリングを SHOULD とする)。
- 0010 で `ClientConnectionState::feed_datagram` の drain を除去する修正 (`feed_datagram_only`) を検討したが、上記により end-to-end では機能しないため、本 issue に含めて対応する。

## 設計方針

- CONNECT 送信前に peer 制御ストリームを受け取って SETTINGS を feed し、`peer_settings` が得られてから CONNECT を送る。
- QUIC transport parameter を検証して `set_webtransport_transport_verified` を呼ぶ。s2n-quic が peer の transport parameter や `RESET_STREAM_AT` を提供しない場合は、依存クレート側の対応 (s2n-quic の更新・shiguredo_http3 の検証経路の見直し) を設計判断として切り出す。
- 確立前後の datagram イベントを失わないようにする (`take_buffered_datagrams` への一本化、または確立処理中の Datagram を保持する)。
- `ClientConnectionState::feed_datagram` は feed のみ (`feed_datagram_only`) とし、drain は取り出し側に一本化する (0010 の修正を含める)。
- example publisher が Properties Length を二重前置する問題 (0014) は Session での受理に影響するため、本 issue の完了確認は 0014 の修正後に行う。

## 完了条件

- WebTransport 接続 (`https://`) で `WtClient::connect` が成功すること
- WebTransport 接続の subscriber の `data_plane.recv_datagram` に Object Datagram の payload が届くこと (Session での受理は 0014 の修正後に確認)
- QUIC 直結 (`moqt://`) の既存挙動が壊れないこと
- 手動確認手順または再現テストが追加されていること
