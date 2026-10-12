# example に in-process の MOQT peer 試験基盤を追加する

- Created: 2026-10-12
- Completed: {YYYY-MM-DD}
- Branch: feature/add-moqt-peer-test-harness
- Polished: 2026-10-12

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

- `Session::new_server` を使う in-process peer ドライバを `examples/tokio-moq` の
  テスト専用モジュールとして追加する。SETUP 交換、peer 起点 request stream への
  メッセージ送信、client からの request 受信ができる最小限の機能を持つ。
- 置き場所と公開範囲: `examples/tokio-moq/src/test_peer.rs` (名前は実装時に決める) を追加し、
  `lib.rs` に `#[cfg(test)] mod test_peer;` を置く。テスト専用であることを module doc に明記し、
  通常ビルドには含めない。基盤を使うテストは crate 内の `#[cfg(test)] mod tests` に置き、
  `examples/tokio-moq` の `[dev-dependencies]` (rcgen など) をそのまま使えるようにする
  (この crate は `tests/` を持たず、既存のテストも `src/*.rs` 内の `#[cfg(test)] mod tests`
  である。`tests/` の結合テストからは `#[cfg(test)]` の module を参照できない)。
  moq-pub / moq-sub から使えるようにする変更は本 issue では行わない。0236 / 0240 の検証は
  tokio-moq のレベルで行い、pipeline の終了は pipeline 側の既存テストと配管の確認で担保する。
- 対象 transport は QUIC 直接接続 (`MoqtClient::establish_quic`) とする。wt-h2 / wt-h3 の
  ハーネスは本 issue の対象外とし、必要になった時点で別 issue にする。
- ハーネスは s2n-quic サーバーを 127.0.0.1 で立て、サーバー証明書は既存テストと同じく
  `rcgen` で生成する (`examples/tokio-moq` の `[dev-dependencies]` にある)。client の接続には
  `tokio_moq::quic::connect` を使い、その戻り値の `StopSendingReceiver` を
  `MoqtClient::establish_quic` へ渡す。
- SETUP: ハーネスは自分の control stream (uni) を開いて SETUP を送る。`Session::new_server` の
  SETUP は `SessionEvent::SendControl` として積まれるので、`poll_event` で取り出し
  `stream::encode_control_stream_setup` で書き出す。stream type prefix を前置する API は
  これだけであり、`ControlMessage::encode` 単体では prefix が無いため client は SETUP として
  解釈できず、ハンドシェイクが完了しない。
- client の SETUP の取り込み: ハーネスは client の control stream (uni) を accept し、stream
  type を検証してから client の SETUP を decode して `Session::recv_control` に渡す。これを
  しないと `Session` は Established にならず、`Session::recv_request` が
  `BeforeSessionEstablished` を返す。
- peer 起点 request stream: ハーネスは bidi stream を開き、先頭に任意の `ControlMessage` を
  送る。Request ID は peer 役割の parity (奇数) に合わせる。送れるのは本ライブラリが decode
  できる型に限り、未知の type ID は decode 失敗になり `BidiMessage::PeerRequestError` の経路に
  落ちるため、先頭メッセージの種別判定を確かめる入力には使えない。本 issue の完了条件のテスト
  入力には example が受理する request 種別 (SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS) を
  使う。非 request を送る能力は 0236 用に基盤が備えるが、0236 の検証は 0236 で行う。
- client からの request: Established の後に `Session::recv_request` で受理し、テストが要求の
  内容を確認できるようにする。
- 基盤を使うテストは `tokio::time::timeout` を掛け、ハングを失敗として観測できるようにする
  (既存の `webtransport_h2.rs` の結合テストと同じ方針)。
- 実トランスポート (s2n-quic) を使い、モック / スタブは使わない。
- テストから使うのは example の公開 API (`MoqtClient::establish_quic` など) とし、
  private な内部状態に依存しない。

## 完了条件

- 基盤を使ったテストが `examples/tokio-moq/src/` 内の `#[cfg(test)] mod tests` に 1 本以上
  追加され通ること。内容は「peer が SETUP を送って client のハンドシェイクが完了し、peer 起点
  request stream の先頭に送った request メッセージを example が受理するところまでを実行で
  確認する」とする。0236 / 0240 が扱う「終了コード付きで閉じる」ことの検証はそれらの issue で
  行う (本 issue の完了条件には含めない)。
- 実 relay を必要とせず `cargo test --workspace` で実行されること (`#[ignore]` を付けない)。
- 追加した基盤がモック / スタブを使っていないこと (実 s2n-quic サーバーと `Session::new_server`
  で駆動する)。
