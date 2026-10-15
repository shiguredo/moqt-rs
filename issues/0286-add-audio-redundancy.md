# 音声 Object を冗長化して送り、受信側で 2 回目を捨てる

- Created: 2026-10-16
- Completed: {YYYY-MM-DD}
- Branch: feature/add-audio-redundancy
- Polished: {YYYY-MM-DD}

## 目的

moqt-js は 2026-10-15 の変更 (`origin/develop` の #492) で、音声 Object の冗長化を入れた。
datagram は再送されない (RFC 9221) ため、同じ Object を n 回送ると全ての copy が落ちる
確率が損失率の n 乗になる。実測 (実リレー、経路の損失を注入、30〜40 秒) では、鳴った割合は
次のようになった。

| 損失 | 1 回 | 2 回 | 3 回 |
| --- | --- | --- | --- |
| 5 % | 94.5 % | 99.4 % | - |
| 10 % | 89.3 % | 96.9 % | - |
| 20 % | 77.6 % | 96.3 % | 99.4 % |

損失の無い経路では品質が変わらない (欠落 0、飛ばし 0)。代わりに音声の帯域と datagram の数が
n 倍になる。冗長度は 1〜3 とし、既定はライブラリが 1、moqt-js の devtools が 2 である。
受信側は同じ識別子 (Group ID と Object ID) の 2 回目を捨てる (捨てないと同じ音を 2 回鳴らし、
媒体時刻が実時間より先へ進む)。

moqt-rs の moq-pub は音声を datagram で 1 回だけ送り、moq-sub は同じ識別子の Object が 2 回
届くと 2 回録画し 2 回復号する。移植する。

## 現状

- `examples/moq-pub/src/cli.rs` の `Config` は音声の配送 (`audio_datagram`) を持つが、冗長度の
  指定が無い。`--audio-subgroup` で subgroup 配送へ切り替えられる。
- `examples/moq-pub/src/datagram_writer.rs` の `DatagramWriter::write_object` は 1 つの
  Object を 1 回送り、`next_object_id` を 1 回進める。同じ Object を 2 回送る手段が無い。
- `examples/moq-sub/src/pipeline.rs` の `AudioDelivery::deliver` は、購読境界 (`SUBSCRIBE_OK`
  の Largest Object) 以下の Object を落とし、境界より後は送出済みの TIMESTAMP より古い
  Object を落とす (`AudioDeliveryOrder`)。同じ識別子の Object が 2 回届くことは考慮して
  いないため、2 回録画し、2 回復号し、2 回送出する。
- moqt-js の受信側の判定は、再生経路の入口 (`src/createMediaSubscriber.ts` の
  `receiveAudioObject`) で行う。覚える識別子は直近 256 件 (`AUDIO_RECENT_OBJECT_IDS_MAX`) で
  あり、統計より前に捨てる。
- moqt-js の配信側は、datagram 配送のときだけ冗長度の数だけ送る (`src/createMediaPublisher.ts`)。
  subgroup 配送は Data Stream が再送されるため 2 回以上にする意味が無い。

## 設計方針

- `examples/moq-pub/src/cli.rs` に `--audio-redundancy <N>` (1〜3 の整数、既定 1) を足す。
  範囲外と非整数はエラーにする。`--audio-subgroup` のときは意味が無いため、指定されても
  送信は 1 回のままとし、無視する旨を警告する (moqt-js と同じ扱い)。
- `DatagramWriter` は冗長度を持つ。`write_object` はフィルタ評価とサイズ判定を 1 回だけ行い、
  同じ Object ID の datagram を冗長度の数だけワイヤへ出し、`next_object_id` は 1 回だけ
  進める。フィルタ不通過 (`Skip`) のときは 1 回も送らない。
- `examples/moq-sub/src/pipeline.rs` の音声の再生経路の入口で、識別子 (group_id, object_id) が
  直近に届いたものと同じなら捨てる。覚える数は 256 件とする。状態は購読ごとに 1 つ持つ
  `AudioDeliveryOrder` に置き、datagram / subgroup stream / FETCH 応答の経路で共有する。
  判定は録画・復号・統計より前に行う。
- 購読境界の判定と TIMESTAMP の順序の判定は変えない。重複の判定はそれらとは独立に足す。
- `moq-sub` の pending subgroup buffer は、保留した Object を解放時に 1 回だけ `deliver` へ
  渡すため、重複の判定に影響しない (同じ Object の 2 回目の配送として数えない)。
- 後方互換は不要である (破壊的変更を恐れない)。

## 完了条件

- `--audio-redundancy` の既定が 1 で、1 と 3 は受理し、0 と 4 と非整数はエラーになることが
  テストで固定されていること。
- 同じ Object ID の datagram が冗長度の数だけ送られ、Object ID が 1 回だけ進むこと。
- 同じ識別子の音声 Object を 2 回配送しても、録画・復号・送出が 1 回になることが
  `examples/moq-sub/src/pipeline.rs` のテストで固定されていること (モックやスタブは使わず、
  既存の `AudioDelivery` のテスト補助を使う)。
- 257 件目以降の古い識別子は覚えていないことをテストで固定すること (256 件の窓)。
- 境界以下や TIMESTAMP が古い Object の扱いが変わらないこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
- ログは英語、コメントは日本語で書くこと。
- ソースコードに issue 番号や issue への言及を書かないこと。
