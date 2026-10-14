# datagram の送信キューが満杯なら空きができるまで待つ

- Created: 2026-10-14
- Completed: {YYYY-MM-DD}
- Branch: feature/wait-for-datagram-queue
- Polished: {YYYY-MM-DD}

## 目的

E2E Test の `publish_and_subscribe_audio_datagram_roundtrip` が、publisher の

```text
datagram send error: Queue does not have room for more datagrams.
```

で失敗する。moq-pub が音声を datagram で配信し、subscriber が購読している最中に、s2n-quic
の送信キューの空きが無くなるとエラーになり、配信のパイプラインごと終了する。

datagram は再送されないため、ここで捨てると音声が飛ぶ。キューが満杯になるのは回線が
詰まっているときであり、待てば送れる。待たずにエラーを返すと、購読側は音声を一切受け取れ
ないまま publisher が終了する。

## 現状

- `examples/tokio-moq/src/transport.rs` の `StreamHandle::send_datagram` は、QUIC では
  `s2n_quic::provider::datagram::default::Sender::send_datagram` を呼び、キューに空きが
  無いと `DatagramError` を返す。呼び出し側 (moq-pub の `DatagramWriter`) はこれを
  `?` で伝播し、パイプラインが終了する
- WebTransport (H3 / H2) の経路は待つ実装になっている (HTTP Datagram の送信が待つ)
- `--audio-datagram` は 2026-10-14 の 0253 で追加された E2E テストで初めて実時間の
  配信として検証された

## 設計方針

- `Sender::poll_send_datagram` を使い、`Poll::Pending` なら空きができるまで待つ。
  この API は待つ間の `Waker` を登録するため、ポーリングで待つ必要はない
- 待つのは呼び出し側の 1 フレーム分であり、実時間より遅く送ることになる。しかし
  キューが満杯なのは回線が詰まっているときであり、待たないと音声が 1 つも届かない
- 送信そのものの失敗 (`ExceedsPeerTransportLimits` / `ConnectionError`) は今までどおり
  エラーとして返す
- 待っている間に接続が閉じた場合は `ConnectionError` が返るため、無限に待たない

## 完了条件

- 送信のキューが満杯のとき、空きができるまで待ってから送ること
- 送信そのものの失敗は、待たずにエラーとして返ること
- E2E Test が成功すること

検証は E2E Test で行う。送信のキューが満杯になる状況は s2n-quic の接続の内部状態であり、
`StreamHandle::Quic` を作るには実際の接続が要るため、単体テストでは再現できない。

- `make pbt` / `make test` / `make clippy` / `make fmt` が通ること
- ソースコードに issue 番号や issue への言及を書かないこと

## 解決方法

(対応時に記入)
