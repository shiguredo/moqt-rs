# relay 経由の datagram 配送の音声が subscriber に届かない

- Created: 2026-10-14
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-datagram-audio-delivery
- Polished: {YYYY-MM-DD}

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

(対応時に記入)
