# relay 経由の datagram 配送の音声が subscriber に届かない

- Created: 2026-10-14
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-datagram-audio-delivery
- Polished: 2026-10-14

## 目的

E2E Test の `publish_and_subscribe_audio_datagram_roundtrip` が失敗する。publisher は
catalog と音声を送り、subscriber は catalog を取得して audio track を購読しているが、
subscriber に datagram が 1 つも届かず、`Pipeline stopped: 0 streams received` と
`No MP4 samples were recorded` になる。

E2E Test のログ (2026-10-14、run 38535073206) では次のようになっている。

```text
moq_pub::catalog: Sending MSF catalog (251 bytes): ... "tracks":[{"name":"audio",...
moq_pub::fake_audio_capture: Fake audio capture started (440 Hz sine, ...)
moq_pub::pipeline: Starting publish loop (video=false, audio=true, video_delivery=subgroup, audio_delivery=datagram)
moq_pub::pipeline: Received REQUEST_UPDATE: request_id=2
moq_sub::pipeline: Subscribed to audio track: alias=392, request_id=2
moq_sub::pipeline: Pipeline stopped: 0 streams received
```

publisher は購読され、音声を datagram で送っている。subscriber は購読を確立している。
しかし datagram が届いていない。

## 現状

- `examples/moq-pub` の `DatagramWriter` は draft-ietf-moq-transport-22 §11.2.1
  (Object Datagram) に沿って組み立てている
- `examples/moq-sub` の datagram の受信ループは `StreamHandle::recv_datagrams` を呼ぶ
- subscriber 側に「datagram を受け取った」ことを示すログが無く、届いていないのか、届いて
  いるのに配送経路へ流れていないのかを CI のログから切り分けられない
- 映像ありのテスト (`publish_and_subscribe_roundtrip`) は成功している

## 設計方針

- まず切り分けのためのログを追加する。最初の datagram を受け取ったときに 1 回だけ件数を
  出す。届いていなければ relay が転送していないことになり、届いていれば配送経路の問題で
  ある
- 切り分けの結果に応じて、次のいずれかを行う
  - relay が転送していない場合: moqt-rs 側では直せないため、relay が対応するまでの扱いを
    決める (テストの実行条件など)
  - 届いている場合: subscriber の配送経路を直す

## 完了条件

- datagram が届いているかどうかが CI のログで分かること
- 届いていない場合は、その事実と relay 側の対応待ちであることを issue に記録すること
- 届いている場合は、配送経路を直して E2E Test が成功すること
- `make test` / `make clippy` / `make fmt` が通ること
- ソースコードに issue 番号や issue への言及を書かないこと

## 解決方法

- 調査の結果、`examples/moq-sub` の datagram の受信ループが、datagram が 1 つも無いときに
  即座に空の結果を返していた。呼び出し側はそれを待たずに回り続けるため、CPU を 1 コア
  使い切り、同じ runtime で動く QUIC エンドポイントの I/O を飢えさせていた。
  `StreamHandle::recv_datagrams` を、最初の 1 つは届くまで `poll_recv_datagram` で待ち、
  続けて届いている分はまとめて取る形に直した (WebTransport の経路は元から待つ)
- あわせて、最初の datagram を受け取ったときに 1 回だけ件数を出すログを追加した
- 切り分けの結果、subscriber は relay から datagram を 1 つも受け取っていない
  (`Received the first datagram batch` が出ない、`Pipeline stopped: 0 streams received`)。
  publisher の側では catalog と audio の PUBLISH が解決した直後に、relay が
  `application::Error(3)` (PROTOCOL_VIOLATION) で接続を切っている

```text
Sent PUBLISH (request_id=0, alias=2)
Received REQUEST_UPDATE: request_id=0
Failed to accept peer bidi stream: QUIC error: The connection was closed on the application
level with error application::Error(3) by the remote endpoint
```

- 映像ありの `publish_and_subscribe_roundtrip` は同じ経路で成功するため、差分は
  `--no-video` と `--audio-datagram` の 2 点である。OBJECT_DATAGRAM の組み立ては
  draft-ietf-moq-transport-22 §11.2.1 に沿っており (`src/stream/datagram.rs`)、
  moqt-rs 側の書式の不備は見当たらない
- 残る問題は実 relay の OBJECT_DATAGRAM 配送への対応であり、moqt-rs 側では直せない。
  テストは `MOQT_E2E_DATAGRAM=1` を設定したときだけ実行する形にした。relay が対応したら
  この条件を外す
- `issues/pending` へ移した
