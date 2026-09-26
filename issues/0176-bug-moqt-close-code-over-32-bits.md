# 32 ビットに収まらない close code で WebTransport セッションを閉じられない

- Created: 2026-09-26
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-moqt-close-code-over-32-bits
- Polished: {YYYY-MM-DD}

## 目的

`StreamHandle::close` に 32 ビットを超える MOQT の close code が渡された場合でも、接続を閉じる手段を確保する。MOQT §13 (Grease) の greasing 値は `0x7f * N + 0x9D` で 32 ビットを超える値を取り得る。

## 現状

- `examples/moqt-transport/src/webtransport.rs` の `moqt_close_code` は `u32::try_from` で変換し、収まらない場合は `TransportError::Internal` を返す (切り捨てない)。
- `examples/moqt-transport/src/transport.rs` の `StreamHandle::close` はこの変換結果を `WtSession::close` へ渡すため、変換に失敗すると閉じる操作自体が失敗する。
- 呼び出し元 (`examples/moqt-publisher/src/pipeline.rs` / `examples/moqt-subscriber/src/pipeline.rs` / `examples/moqt-transport/src/moqt_client.rs`) は `warn!` を出して継続するため、セッションは閉じないまま残る。
- `WT_CLOSE_SESSION` capsule の Application Error Code は 32 ビットである (draft-ietf-webtrans-http3-16 §6)。QUIC の CONNECTION_CLOSE が運ぶアプリケーションエラーコードは varint であり 32 ビットに限らない。

## 設計方針

次のいずれかを実装時に決める。

- 32 ビットに収まらない場合は `s2n_quic::connection::Handle::close` で CONNECTION_CLOSE を送る。`WT_CLOSE_SESSION` の詳細メッセージは送れないが、接続は閉じる。
- あるいは `SessionError::code` を 32 ビットに制限し、WebTransport 経路で表現できないコードを生成しない (公開 API の変更になる)。

第一候補は前者とする (コードを切り捨てずに閉じる操作を成立させる)。採用した方針と理由をコメントに残す。

## 完了条件

- 32 ビットを超える close code でも接続が閉じること (単体テストで固定する)
- 32 ビットに収まる close code の挙動 (`WT_CLOSE_SESSION` + FIN) が変わらないこと
- 採用した方針と理由がコメントに残っていること
- `make test` / `make clippy` / `make fmt` が通ること
