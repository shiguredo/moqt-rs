# 音声 playout が隙間を材料化せず、境界の段差と再生位置の記録不整合を生む

- Created: 2026-10-20
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: {YYYY-MM-DD}
- Reporter: @voluntas

## 目的

qmux 経由の音声がプチプチする、という報告がある。moqt-js 側の実リレー計測では、原因は時間圧縮の継ぎ目ではなく「補間しなかった隙間が無音のまま境界に段差を作る」ことと「予約できない隙間がアンダーランになる」ことであった。moqt-rs の example は `raw_player` の FIFO に PCM を積み、PTS を再生時刻に使わないため、埋めなかった隙間は無音としても残らず、その分だけ後続が早く鳴る。累積すると出力バッファが枯渇し、SDL が無音を挿入して段差がクリックになる。

さらに、補間チャンクの再生位置を enqueue 後の累積サンプル数で記録しているため、補間が鳴り始めると `AudioPlayoutPosition` の対応が壊れ、`sounding_position_us` が `now_us` へ落ちて A/V ずれの実績記録が止まる。隙間の材料化・境界フェード・位置記録・計器をまとめて直す。

## 現状

- `src/playout/scheduler.rs` の `concealment_of` は、隙間が 5 ms 以下のときと、前の音の終わりが今 + 余裕より過去のときに補間を返さない。100 ms を超える分も切り捨てる。呼び出し側は補間できなかった分をどこにも積まない。
- `examples/moq-sub/src/main.rs` の `play_decoded_audio` は、補間が空なら何も積まずに今回の音へ進む。`raw_player` の `AudioPlayer` は積んだ順に鳴らすだけであり、隙間は再生位置の早期化として現れる。
- 同じ関数で、補間チャンクの累積サンプル数を `enqueue_pcm` の後に読んでいる。今回の音は enqueue 前に読んでおり、補間だけが後読みになっている。`sounding_pts_us` は経過サンプルが負になると保留を全消去するため、最初の補間以降、実鳴位置の追跡が復帰しにくい。
- `src/playout/timing.rs` の計器は、鳴らさなかった音（backlog / catchUp / error / stopped）だけを数える。鳴ったが無音だった区間、アンダーラン、境界の段差を数えない。

## 設計方針

- `src/playout/scheduler.rs` の再生決定を「隙間の全長 `gap_us`」と「補間してよい長さ `conceal_us`」に分ける。`gap_us` は 5 ms 下限でも 100 ms 上限でも切り捨てない。`conceal_us` は既存の条件（5 ms 超、前の音の終わりが今 + 余裕以降、100 ms 以下）で決める。
- `src/playout/gap.rs` を追加し、隙間を必ず要求サンプル数だけ生成する。補間できた分は補間で埋め、残りは無音で埋める。隙間の末尾は `AUDIO_GAP_FADE_US` で 0 へ落とし、今回の音の先頭も同じ長さで 0 から立ち上げる。前の音の末尾と隙間の先頭が段差にならないようにする。
- `examples/moq-sub/src/main.rs` は、返された `gap_us` に対して必ず隙間の PCM を積む。積んだ長さを計器へ返す。補間チャンクの位置は enqueue 前に読み、PTS は直前の音の終端から求める。
- `src/playout/timing.rs` に隙間とアンダーランの計器（回数・合計・最大）を足し、5 秒ログへ出す。
- `src/playout/scheduler.rs` に `last_end_us` と `note_underrun` を足し、出力が枯れたことを検出できるようにする。`note_underrun` は並べる基準を今へ戻し、枯れた時間を二重に隙間として材料化しない。

## 完了条件

- 隙間が要求サンプル数ちょうど積まれること、補間失敗時も無音で埋まること、隙間の前後が連続であることを単体テストと PBT で固定する。
- 補間チャンクを含む列で `sounding_position_us` が `None` に落ちず、PTS が単調に進むことを単体テストで固定する。
- 5 秒ログに隙間とアンダーランの回数・合計・最大が出ること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 参照

- `src/playout/scheduler.rs` の `AudioPlayoutScheduler::schedule` / `concealment_of` / `AudioPlayoutDecision`
- `src/playout/stretch.rs` の `conceal`
- `examples/moq-sub/src/main.rs` の `play_decoded_audio` / `apply_audio_playout` / `conceal_gap` / `AudioPlayoutPosition`
- moqt-js の `docs/AUDIO_PLAYOUT_GAPS_AND_TAIL.md` と `issues/0868-bug-playout-silence-gap-click.md`
