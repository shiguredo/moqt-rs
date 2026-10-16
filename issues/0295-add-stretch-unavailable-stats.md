# 時間圧縮を使えないサンプルレートを計器に出す

- Created: 2026-10-17
- Completed: 2026-10-17
- Branch: feature/add-stretch-unavailable-stats

## 目的

moqt-js は 2026-10-17 の変更 (`dd77381a`「時間圧縮を使えないサンプルレートを計器に出す」) で、
4 kHz へ間引くフィルタの表に無いサンプルレートの音を計器へ数えるようにした。表に無いレート
(44.1 kHz の AAC など) ではピッチの周期を求められず、時間圧縮も補間も働かない。落ちずに
「操作しない」を返すため呼び出し側からは区別できず、遅れが目標へ戻らない、欠落した区間が
埋まらないといった症状の原因を数値から切り分けられない。moqt-rs の moq-sub は AAC を扱い
(`examples/moq-sub/src/decoder.rs` の `AudioDecoder::Aac`)、AAC は 44.1 kHz が一般的である
ため、この状態は実際に起こり得る。

## 現状

- `src/playout/stretch.rs` の `downsample_filter` は 8 / 16 / 32 / 48 kHz の表を持ち、表に無い
  サンプルレートでは `None` を返す。
- `compress` / `expand` / `conceal` は表に無いレートで 0 (操作なし) を返す。呼び出し側は
  「周期が求まらなかった」のか「レートが対象外」なのかを区別できない。
- `AudioPlayoutTimingStats` の観測値 (`snapshot`) に、この状態を読む値が無い。
- `examples/moq-sub/src/main.rs` の `AudioPlayoutAssembly::arrange` は、時間圧縮と補間を
  使えるサンプルレートかを確かめずにスケジューラへ渡している。
- moqt-js は同じ状態を `isTimeStretchAvailable` / `stretchUnavailableFrames` で計器に出し、
  devtools の Audio Playback で読めるようにしている。

## 設計方針

- `src/playout/stretch.rs` に `is_time_stretch_available(sample_rate)` を追加し、間引きの
  フィルタの表を引けるかどうかを返す。
- 判定は表の完全一致 (`downsample_filter(sample_rate).is_some()`) にする。moqt-js は
  `Math.round(sampleRate / 4_000) * 4_000` で 4 kHz の倍数へ丸めてから表を引くが、この丸めは
  写さない。moqt-js の時間圧縮は 4 kHz へ間引いた信号だけで周期を求めるため丸めた値でも
  動くのに対し、moqt-rs の `compress` / `expand` はサンプルレートそのもので
  `downsample_filter` を引く。丸めを写すと、44.1 kHz を「使える」と答えながら実際には
  `compress` も `expand` も動かない状態を作る (表に無いレートでは 0 を返す)。
- `AudioPlayoutTimingStats` に累積の `stretch_unavailable_frames` と
  `record_stretch_unavailable` を追加し、`snapshot` へ出して `reset` で 0 に戻す。到着の
  遅れと同じく購読の開始からの累積にする。
- 数える位置は復号した音を受け取る時点 (`AudioPlayoutAssembly::arrange` の先頭) にする。
  moqt-js の `handleDecodedAudio` と同じく、鳴らすかどうかの決定より前に数える。
- 数えるだけで鳴らし方は変えない。詰めも補間も予約も従来どおりにする。
- moq-sub のログ (`AudioPlayoutAssembly::log_fields`) に出す。moqt-js は devtools に出して
  いるが、moqt-rs には devtools が無いため example のログを受け皿にする。

## 完了条件

- `is_time_stretch_available` が 8 / 16 / 32 / 48 kHz で true、44.1 kHz と 0 と 22.05 kHz で
  false になること。
- `stretch_unavailable_frames` が `snapshot` に出て累積され、`reset` で 0 に戻ること。
- 44.1 kHz の音を受け取ったときに数えられ、48 kHz の音では数えないことがテストで固定されて
  いること。
- 時間圧縮が使えるレートの挙動 (既存の `compress` / `expand` / `conceal`) が変わらないこと。
- `make test` / `make clippy` / `make fmt` / `make pbt` が通ること。
- ソースコードに issue 番号や issue への言及を書かないこと。

## 解決方法

- `src/playout/stretch.rs` に `pub fn is_time_stretch_available(sample_rate: u32) -> bool` を
  追加した。`downsample_filter(sample_rate).is_some()` による表の完全一致であり、moqt-js の
  丸めは写していない。理由 (moqt-rs の `compress` / `expand` はサンプルレートそのもので
  間引きフィルタを引くため、丸めると使えないレートを使えると答えてしまう) は rustdoc に
  書いた
- `src/playout/timing.rs` に `AudioPlayoutTimingSnapshot.stretch_unavailable_frames: u64` と
  `AudioPlayoutTimingStats` の累積カウンタ、`record_stretch_unavailable` を追加し、
  `snapshot` へ出して `reset` で 0 に戻すようにした。到着の遅れと同じく購読の開始からの
  累積であり、分布の窓の外へ出ても残る
- `examples/moq-sub/src/main.rs` の `AudioPlayoutAssembly::arrange` の先頭で
  `stretch::is_time_stretch_available` を確かめ、使えないレートの音を計器へ数えるように
  した。鳴らすかどうかの決定より前に数えるため、捨てた音も数える。`log_fields` に
  `stretch_unavailable=` を追加した
- テスト: `is_time_stretch_available_accepts_only_the_downsample_table_rates` (表の 4 レートが
  true、44.1 kHz / 22.05 kHz / 0 / 8.2 kHz が false)、
  `record_stretch_unavailable_counts_cumulatively_and_resets` (数える / 窓の外でも累積 /
  `reset` で 0)、`playout_assembly_counts_only_the_stretch_unavailable_sample_rates`
  (44.1 kHz は数え、48 kHz は数えない) を追加した
