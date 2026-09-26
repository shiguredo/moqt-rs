# セッション終了時に期待される失敗が warn ログとして出る

- Created: 2026-09-26
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-expected-session-end-warn-logs
- Polished: {YYYY-MM-DD}

## 目的

WebTransport のセッション終了は異常ではないが、終了に伴う失敗が `warn!` として出るため、ログから実際の異常を区別できない。受信ループの accept 経路と datagram 経路は `ConnectionClosed` を `info!` として扱うようになっており、レベルが揃っていない。

## 現状

- `examples/moqt-subscriber/src/pipeline.rs` のストリーム処理は、`RecvStream::receive_chunk` が返す `TransportError::ConnectionClosed` を「failed to read subgroup header」「Failed to read object payload」などの `warn!` として出す。
- `examples/moqt-publisher/src/pipeline.rs` の終了時の後始末は、セッション終了後に PUBLISH_DONE / GOAWAY / `close(0, "")` が失敗すると `warn!` を出す。
- セッション終了は `examples/moqt-transport/src/webtransport.rs` のセッション状態 (`WtSessionState`) で表現され、ストリームの中断と新規 open / datagram 送信の拒否として現れる。

## 設計方針

- セッション終了由来の失敗 (transport の `ConnectionClosed` と、pipeline の `Error::ConnectionClosed`) は `info!` または `debug!` に落とし、実際の異常と区別する。
- 判別は既存の純関数 (`is_transport_session_end` と、transport 側の同等の判定) と同じ方針で行い、共通化できるものは共通化する。
- ログの文言とレベル以外の挙動は変えない。

## 完了条件

- セッション終了に伴うストリーム処理と後始末の失敗が warn で出ないこと
- 実際の異常 (他の transport エラー) は従来どおり warn / error で出ること
- 判別をテストで固定すること
- `make test` / `make clippy` / `make fmt` が通ること
