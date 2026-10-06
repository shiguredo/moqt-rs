# 音声の欠落した隙間を直前の音の時間伸長で補間する

- Created: 2026-10-06
- Completed: 2026-10-06
- Branch: feature/complete-playout
- Polished: {YYYY-MM-DD}

## 目的

経路の欠落や再生の遅れで音声に隙間が空くと、そのまま無音になって途切れとして聞こえる。直前の音を波形の周期で伸長して隙間を埋め、途切れを目立たなくする。

## 現状

- `src/playout/scheduler.rs` の `AudioPlayoutScheduler::schedule` は `AudioPlayoutDecision::Play { start_at_us, compress_us }` か `Drop` を返すだけで、前の音の終わりと今回の開始の間に空いた分を扱わない
- `src/playout/stretch.rs` には `compress` (波形の周期で詰める) と `expand` (伸ばす) があるが、隙間を埋める処理が無い
- `src/playout/timeline.rs` の `presentation_delay_us` は音声の表示の遅れを返すが、隙間の補間には使われていない

## 設計方針

### 隙間を埋める (`src/playout/stretch.rs`)

- `conceal(channels, output, sample_rate, conceal_us) -> isize` を追加する。直前の波形の周期を使って隙間を埋め、埋めた長さを返す
- 無音に近い入力 (`TIME_STRETCH_SILENCE_PEAK`) では周期が求まらないため埋めない
- 継ぎ目で振幅が跳ばないよう、埋めた部分の終端の利得を下げる (`concealment_end_gain`)
- 継ぎ目の 1 サンプルあたりの変化量に上限 (`TIME_STRETCH_MAX_SEAM_STEP_RATIO`) を置く

### 隙間を知らせる (`src/playout/scheduler.rs`)

- 隙間は `schedule` の中で「前の音の終わり」と今回の開始の差から求める。判定に必要な値は既にあるため `AudioPlayoutInput` は変えない
- `AudioPlayoutDecision::Play` に `gap_start_us` と `gap_us` を足す。`gap_us` は上限 (`AUDIO_PLAYOUT_MAX_CONCEAL_US`) で切った値であり、0 のときは補間しない
- `AUDIO_PLAYOUT_MIN_CONCEAL_US` (5 ms) 以下と、開始が今 + 余裕より前の隙間は補間しない
- 実際に埋めた長さを `confirm_concealment` で返す。`confirm_stretch` と同じく、返さなかった分は埋まらなかったものとして扱う
- 隙間の長さは「前の音の終わり」と「今回の開始」の差であり、`start_at_us` を決める処理の中で求める

## 完了条件

- 隙間の開始と長さが `AudioPlayoutDecision::Play` に載ること。上限を超える隙間は上限で切られること
- 最小の隙間 (5 ms 以下) と、開始が余裕より前の音では補間しないこと
- `conceal` が無音・周期のある波形・短い隙間・上限を超える隙間・長さ 0 で panic しないこと
- `stretch` と `scheduler` の単体テストと PBT があること
- `make test` (`cargo test --workspace`) と `make pbt` と `make clippy` と `make fmt` が通ること

## 参照

- `src/playout/stretch.rs` の `compress` / `expand`
- `src/playout/scheduler.rs` の `AudioPlayoutScheduler` / `AudioPlayoutInput` / `AudioPlayoutDecision`
- draft-ietf-moq-loc-04 §2.3.1.1 (Timestamp)

## 解決方法

- `src/playout/stretch.rs` に `conceal` / `concealment_end_gain` と関連する定数を追加した。直前の音の末尾 2 周期の相関から周期を求め、末尾 1 周期を繰り返して `conceal_us` 分を埋める。周期が取れないとき、相関が足りないとき、継ぎ目の段差が大きいとき (`TIME_STRETCH_MAX_SEAM_STEP_RATIO`)、無音のときは 0 を返して何も書かない。
- `src/playout/scheduler.rs` の `AudioPlayoutDecision::Play` に `gap_start_us` / `gap_us` を追加し、`confirm_concealment` / `concealed_us` / `concealments` を追加した。隙間は 5 ms 以下と、開始が今 + 余裕より前のものを除き、100 ms で切る。
- 隙間は `AudioPlayoutInput` を増やさず、`schedule` の中の「前の音の終わり」と既存の `now_us` / `start_at_us` / `AUDIO_PLAYOUT_MIN_LEAD_US` から求める。判定に必要な値は既にあるためである。
- `tests/test_playout/{stretch,scheduler}.rs` に 24 件、`pbt/tests/prop_playout/{stretch,scheduler}.rs` に 3 件のテストを追加した。
- 無音では補間しない点だけ意図的に非対称である (埋めても無音であり、周期も求まらないため)。
