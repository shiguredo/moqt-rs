# example に in-process の MOQT peer 試験基盤を追加する

- Created: 2026-10-12
- Completed: {YYYY-MM-DD}
- Branch: feature/add-moqt-peer-test-harness
- Polished: {YYYY-MM-DD}

## 目的

`MoqtClient` は実トランスポートでのみ構築でき、モック / スタブの利用は禁止されている。
example の I/O 経路 (ハンドシェイク、peer 開始 request stream の受理、制御メッセージの送信) を
実行で検証する手段が無いため、0223 では FETCH の付与を、0222 では `BidiMessage::PeerRequestError`
の経路を、それぞれ実行テストで固定できなかった。0236 / 0240 の修正にも peer を模擬する基盤が要る。

## 現状

- example にはサーバー側の MOQT ドライバが無い (`examples/tokio-moq` は client 側の I/O 層だけを
  持つ)。ライブラリは `Session::new_server` を持ち、SETUP 交換と request 受信を sans-I/O で
  扱える。
- 実トランスポートのサーバー起動コードは既存のテストにある。`examples/tokio-moq/src/lib.rs` の
  テストは s2n-quic のサーバーを 127.0.0.1 で立てて接続を確認し、
  `examples/tokio-moq/src/webtransport_h2.rs` のテストは TLS + HTTP/2 のサーバーを立てて
  ストリームと datagram を往復している。MOQT の制御メッセージを話す部分だけが無い。

## 設計方針

- `Session::new_server` を使う in-process peer ドライバを example のテスト用モジュールとして
  追加する。SETUP 交換、peer 起点 request stream への任意メッセージ送信、client からの
  request 受信ができる最小限の機能を持つ。
- 実トランスポート (s2n-quic) を使い、モック / スタブは使わない。
- テストから使うのは example の公開 API (`MoqtClient::establish_quic` など) とし、
  private な内部状態に依存しない。

## 完了条件

- 基盤を使ったテストが 1 本以上通ること (例: peer が request stream の先頭に request でない
  メッセージを送ったとき、example がセッションを終了コード付きで閉じること)。
- 実 relay を必要とせず `cargo test --workspace` で実行されること。
- 追加した基盤がモック / スタブを使っていないこと。
