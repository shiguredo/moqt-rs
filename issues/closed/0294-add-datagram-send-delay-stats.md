# 音声の送信の遅れを配信の example で記録する

- Created: 2026-10-16
- Completed: 2026-10-17
- Branch: feature/add-datagram-send-delay-stats

## 目的

moqt-js は 2026-10-16 の変更 (`origin/develop` の #496「playout のノイズの原因を修正し、
目標遅延を経路に適応させる」) で、datagram で送った音声 Object の送信の遅れを記録するように
した。datagram の書き込みは送信のキューへの投入そのものであり完了が返らない
(fire-and-forget) ため、投入の直後を完了として記録する。

記録しないと、送信の遅れ (`sendLagMs` / `maxSendLagMs` / `sendQueueMs`) が datagram の経路
では常に 0 のままになり、受信側の到着の遅れを「配信側が足した分」と「経路」に分けられない。
moqt-js はこの穴のせいで到着の裾の切り分けをやり直すことになった。実測 (2026-10-16、600 秒)
では送信の Object の間隔が p50 20.00 / p95 20.90 / p99 21.60 ms で 40 ms を超えた回が 0 件で
あり、到着の裾 (p99 108.9 ms) は配信側では作れないと分かっている。この切り分けができないと、
遅れを直す先が配信側なのか経路なのかを判断できない。

moqt-rs では `AudioPublishCatchUp::record_send_start` / `record_send_complete` は移植済みで
あるが、examples から一度も呼ばれておらず、moq-pub の統計は送信の遅れを常に `None` / 0 で
出す。到着の裾が配信側と経路のどちらから来ているかを、リポジトリの実装だけで切り分けられる
ようにする。

## 現状

- `src/playout/publish_catch_up.rs` の `AudioPublishCatchUp` は
  `record_send_start` / `record_send_complete` を持ち、`send_queue_ms` /
  `send_queue_frames` / `send_lag_ms` / `max_send_lag_ms` を出す。
- `examples/moq-pub/src/pipeline.rs` の音声の送出は `record_encoded_chunk` だけを呼び、送信の
  開始と完了を記録していない。リポジトリ全体で `record_send_start` /
  `record_send_complete` の呼び出しが無い。
- 音声の送出は datagram と subgroup stream の 2 経路があり、どちらも `write_object` の戻りで
  フィルタを通過したかが分かる。
- `examples/moq-pub/src/pipeline.rs` の `format_audio_publish_stats` は送信の遅れを出す書式を
  既に持ち、値が入ればログに出る。

## 設計方針

- 音声を送出する直前に送信のキューへ入れた時刻を控え、送出が終わった後に読んだ壁時計を残す。
  datagram は書き込みの戻りを完了とみなす。
- 記録するのはフィルタを通過した Object だけにする。Skip された Object は送信のキューへ
  入っていない。
- TIMESTAMP が入力のタイムスケールである (再エンコード) 音は記録しない。捕捉の時刻との差が
  遅れにならないためである。
- 記録するのは通常の送出経路だけにする。音声入力が閉じたときの最終フレームの送出は、統計を
  読む前にセッションが終わるため対象外とする。
- 測るのは「捕捉から投入まで」である。datagram では送信のキューの長さは測れない (`write` は
  投入で解決し、ワイヤに出た時刻は分からない) ため `send_queue_ms` は 0 のままになる。
  moqt-js と同じ制限である。
- 時刻は他の記録 (`AudioPublishCatchUp::evaluate` の `now_ms` と、完了が返らない記録を捨てる
  判定) と同じ壁時計のミリ秒に揃える。

## 完了条件

- moq-pub の音声の送出 (datagram と subgroup stream の両方) で、送信の開始と完了が記録される
  こと。
- `format_audio_publish_stats` の `send_lag` / `max_send_lag` / `send_queue` が実際の値を出す
  こと。
- 記録の意味 (捕捉から完了までの遅れ、Skip は記録しない、再エンコードは記録しない、完了前は
  待ちに数える) がテストで固定されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
- ソースコードに issue 番号や issue への言及を書かないこと。

## 解決方法

- `examples/moq-pub/src/pipeline.rs` に `record_audio_send` を追加し、送信のキューへ入れた
  時刻 (`record_send_start`) と送出が終わった時刻 (`record_send_complete`) を記録するように
  した。捕捉の時刻は TIMESTAMP (LOC TIMESTAMP、Unix epoch マイクロ秒) から取り、完了は送出の
  直後の壁時計から取る
- datagram の経路は書き込みの戻りを完了とみなし、subgroup stream の経路は `finish` までを
  含めて完了とした。どちらもフィルタを通過した Object だけを記録し、再エンコードの音は
  記録しない
- 音声入力が閉じたときの最終フレームの送出は、統計を読む前にセッションが終わるため対象外と
  した
- テスト: `capture_to_completion` (捕捉から完了までの遅れ) / `keeps_the_max_delay` (最大の
  保持) / `shows_the_queue_until_completion` (完了前は待ちに数える) を
  `examples/moq-pub/src/pipeline.rs` に追加した
