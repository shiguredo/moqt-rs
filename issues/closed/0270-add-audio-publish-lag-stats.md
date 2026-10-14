# 配信側の音声の遅れを、段ごとに定期的にログへ出す

- Created: 2026-10-14
- Completed: 2026-10-14
- Branch: feature/add-audio-publish-lag-stats
- Polished: {YYYY-MM-DD}

## 目的

moqt-js の `a082dc55` の計器を取り込む。実 relay の E2E で配信側の音声の遅れが伸びたとき、
失敗メッセージに出ていたのは TIMESTAMP の補正の統計だけで、遅れがどの段 (読み出し /
符号化のキュー / 送信のキュー) で溜まっているのかを CI のログから読めなかった。

moqt-rs には `AudioPublishCatchUp::snapshot()` があり、読み出しの遅れ・符号化のキューの
長さとフレーム数・送信のキューの長さとフレーム数・読んでから送り終えるまでの長さを持って
いる。しかしログに出しているのは「チャンクを捨てたとき」だけで、遅れが伸びている途中の
内訳は読めない。

## 現状

- `examples/moq-pub/src/pipeline.rs` は、追いつきでチャンクを捨てたときだけ
  `Dropping audio chunks to catch up (...)` を出す
- 遅れが伸びているがまだ捨てていない状態では、何も出ない
- TIMESTAMP の補正の統計 (`log_capture_clock_summary`) は 10 秒ごとに出ている

## 設計方針

- 配信のループの tick (100 ms) で、10 秒ごとに音声の追いつきの統計を `info` で出す
- 出す値は読み出しの遅れ・符号化のキューの長さとフレーム数・遅れ・上限・床・捨てた数と
  長さ・追いつきの最中か・始めた回数・送信のキューの長さとフレーム数・読んでから送り
  終えるまでの長さとその最大とする。遅れがどの段で溜まっているかを 1 行で読めるようにする
- 音声を配信していないとき (`--no-audio` や `--input-mp4`) は出さない
- ログメッセージは英語にする

## 完了条件

- 10 秒ごとに音声の追いつきの統計が 1 行で出ることがテストで固定されていること
- 音声を配信していないときは出ないことがテストで固定されていること
- E2E Test が成功すること
- `make pbt` / `make test` / `make clippy` / `make fmt` が通ること
- ソースコードに issue 番号や issue への言及を書かないこと

## 解決方法

- `examples/moq-pub/src/pipeline.rs` の配信のループの tick (100 ms) で、10 秒ごとに
  `AudioPublishCatchUp::snapshot()` の内容を `info` で 1 行に出すようにした
- 整形は `format_audio_publish_stats` に切り出した。読み出しの遅れ・符号化のキューの長さと
  フレーム数・遅れ・上限・床・捨てた数と長さ・追いつきの最中か・始めた回数・送信の
  キューの長さとフレーム数・読んでから送り終えるまでの長さとその最大を出す
- 音声を配信していないとき (`--no-audio` / `--input-mp4`) は出さない
- テスト: `audio_publish_stats_reports_every_stage` を追加し、段ごとの値がすべて 1 行に
  含まれることを固定した
- 検証 (ローカル): relay に対して publisher を起動し、次の 1 行が出ることを確認した

```text
Audio publish stats: read_lag=0ms pending=20ms/1 lag=Some(20)ms limit=60ms floor=Some(0)ms dropped=0(0ms) catching_up=false starts=0 send_queue=0ms/0 send_lag=None max_send_lag=None
```

- 検証: E2E Test が 2 件とも成功する
- 検証: `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all --check` が成功した
