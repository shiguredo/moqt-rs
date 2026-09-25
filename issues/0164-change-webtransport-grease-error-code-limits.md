# WebTransport 経路で 32 ビット超の MOQT エラーコードを送れない制限の扱いを決める

- Created: 2026-09-24
- Completed: {YYYY-MM-DD}
- Branch: feature/change-webtransport-grease-error-code-limits
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-webtrans-http3-16 §4.4 は WebTransport のアプリケーションエラーコードを 32 ビットに限る一方、
draft-ietf-moq-transport-21 §13 (Grease) の greasing 値 (`0x7f * N + 0x9D`、`0x9D, 0x11C, ..., 0x3fffffffffffffde`) は 32 ビットを超える値を取り得る。
§16.11.4 (Stream Reset Error Codes) の registry も greasing 予約を持つため、MOQT の reset コードとして 32 ビット超の値が正当にあり得る。

現状は QUIC 経路 (`moqt://`) では任意の varint (`2^62 - 1` 以下) を送れるが、WebTransport 経路では 32 ビット超の値を `internal error` として拒否する。
この非対称と呼び出し元での扱いが決まっていない。

## 現状

- `examples/moqt-transport/src/webtransport.rs` の `moqt_to_wt_code` は `u32::try_from` で 32 ビットに収まらない値を
  `TransportError::Internal` にしている。
- `examples/moqt-transport/src/transport.rs` の QUIC 分岐は `s2n_quic::application::Error::new` にそのまま渡すため、
  `2^32` 以上 `2^62` 未満の値は QUIC 経路では送信できる。
- `src/session/` の公開 API (`Session::reset_outgoing_data_stream_with_code` など) は任意の `u64` を受け付け、
  呼び出し元 (`examples/moqt-publisher/src/stream_writer.rs` など) はコードの範囲を検証しない。
  そのため WebTransport 経路では送信時に初めて `internal error` になり、リセットを送れない。

## 設計方針

- 次のいずれかを選び、選んだ理由を記録する
  - (a) WebTransport 経路の制限を公開 API の doc に書き、呼び出し元が 32 ビットを超えるコードを渡さない契約にする
  - (b) `Session` の API で受け付ける値を 32 ビットに制限し、送信前にエラーにする (QUIC 経路の挙動も変わる)
  - (c) greasing 値を送る経路 (QUIC と WebTransport) で扱いを分け、WebTransport では送れないことをエラーメッセージで明示する
- 数値の判定は `u32::try_from` を 1 箇所に保ち、QUIC 経路の varint 上限 (`2^62 - 1`) の判定と混同しない
- greasing 値の生成側 (MOQT §13) が 32 ビットを超える値を選ぶ場合の扱いを決める

## 完了条件

- 32 ビット超のコードを渡したときの挙動が文書化され、テストで固定されていること
- QUIC 経路と WebTransport 経路の差が、選んだ方針と一致していること
- §4.4 の u32 制限に由来する制限であることがコメント・doc に書かれていること
