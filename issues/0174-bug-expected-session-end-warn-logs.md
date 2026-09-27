# セッション終了時に期待される失敗が warn ログとして出る

- Created: 2026-09-26
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-expected-session-end-warn-logs
- Polished: 2026-09-27

## 目的

WebTransport のセッション終了 (draft-ietf-webtrans-http3-16 §6) は異常ではないが、終了に伴う失敗が `warn!` として出るため、ログから実際の異常を区別できない。受信ループの accept 経路と datagram 経路は `TransportError::ConnectionClosed` を `info!` として扱うようになっており、ストリーム処理と後始末の失敗だけレベルが揃っていない。

## 現状

- `examples/moqt-subscriber/src/pipeline.rs` のストリーム処理 (`handle_stream_body` / `decode_video_stream` / `decode_audio_stream` / `handle_fetch_stream` が使う `receive_registered_stream_data` と `peek_stream_type`) は、セッション終了後に `RecvStream::receive_chunk` が返すエラー (変換後の `Error::ConnectionClosed`) を「failed to read subgroup header」「Failed to read object payload」などの `warn!` として出す。
- `examples/moqt-publisher/src/pipeline.rs` の終了時の後始末は、セッション終了後に PUBLISH_DONE / GOAWAY / `close(0, "")` が失敗すると `warn!` を出す。`examples/moqt-subscriber/src/pipeline.rs` の後始末でも、peer 起点のセッション終了後に GOAWAY / `close(0, "")` が失敗すると `warn!` を出す (STOP_SENDING は `should_stop_sending` が peer 終了時に送らない)。
- セッション終了は `examples/moqt-transport/src/webtransport.rs` のセッション状態 (`WtSessionState`) で表現され、ストリームの中断 (WT_SESSION_GONE) と新規 open / datagram 送信の拒否として現れる。`TransportError::ConnectionClosed` は WebTransport 経路のみが生成し、publisher / subscriber の `From<TransportError>` が `Error::ConnectionClosed` に変換する。QUIC 経路の接続クローズは accept が `Ok(None)` を返して情報ログになり、ストリーム受信の失敗は `Error::Quic` に畳まれるため、本 issue の対象外である (既知の限界)。

## 設計方針

- セッション終了由来の失敗 (publisher / subscriber の `Error::ConnectionClosed`) は `info!` または `debug!` に落とし、実際の異常と区別する。既存の accept / datagram 経路と同じ `info!` が基本で、ストリーム処理のように終了時に複数回出得る箇所は `debug!` でもよい。
- 判別は既存の純関数 `is_transport_session_end` (publisher / subscriber の pipeline にあり、`Error::ConnectionClosed` を variant で判定する) と同じ方針で行い、共通化できるものは共通化する。後始末の `close(0, "")` の失敗には CONNECT stream の送信失敗として `Error::WebTransport` に畳まれる経路が残るため、セッション終了後の後始末と分かる失敗も対象に含める。
- ログの文言とレベル以外の挙動は変えない。

## 完了条件

- WebTransport のセッション終了に伴うストリーム処理と後始末の失敗が warn で出ないこと (QUIC 経路の接続クローズは対象外)
- 実際の異常 (セッション終了以外の transport エラー) は従来どおり warn / error で出ること
- セッション終了由来かどうかの判別をテストで固定すること
- `make test` / `make clippy` / `make fmt` が通ること
