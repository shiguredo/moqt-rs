# relay 経由の datagram 配送の音声が subscriber に届かない

- Created: 2026-10-14
- Completed: 2026-10-14
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

- 原因は `examples/tokio-moq/src/quic.rs` の QUIC DATAGRAM の設定にあった。s2n-quic の
  `provider::datagram::default::EndpointBuilder` は `#[derive(Default)]` であり、
  **送信と受信のキューの容量の既定値がどちらも 0** である。moqt-rs は `with_recv_capacity(64)`
  だけを呼んでおり、送信の容量が 0 のままだった
- `Sender::poll_send_datagram` は `self.queue.len() == self.capacity` のときに `Poll::Pending`
  を返す。容量が 0 なら `0 == 0` が常に成り立ち、**datagram を 1 つも送れない**。
  `send_datagram` が永久に待ち続け、音声の配信ループがそこで止まる
- `with_send_capacity(64)` を追加した。送信の容量が 0 でないことは `BuilderError::ZeroCapacity`
  で守られており、設定しない限り気づけない
- 検証 (ローカル): sora-moq (develop) の relay を 4433 番で起動し、moq-pub と moq-sub を
  繋いだ。修正前は `DIAG audio chunk` が 1 回だけで止まり、subscriber は
  `0 streams received`、relay の `upstream_objects_received` は catalog の 1 件だけだった。
  修正後は `Received the first datagram batch from the relay: count=1` が出て、
  `1553 audio chunks` を受信し、MP4 (269291 バイト) が作られた
- 検証: E2E テストの `MOQT_E2E_DATAGRAM` によるガードを外し、常に実行するようにした
- 検証: `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all --check` が成功した
- 備考: 2026-10-14 の E2E Test で publisher が `application::Error(3)`
  (PROTOCOL_VIOLATION) で切断されていた件は、relay が
  `max_datagram_frame_size` を広告しない接続を閉じる実装
  (`sora_moq_listener.erl`) によるものである。容量を設定すると datagram の交渉が
  成立し、あわせて解消した
