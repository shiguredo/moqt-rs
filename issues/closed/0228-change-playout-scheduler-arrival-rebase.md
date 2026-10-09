# 音声の再生スケジューラを最新の moqt-js に合わせる

- Created: 2026-10-10
- Completed: 2026-10-10
- Branch: feature/change-playout-scheduler-arrival-rebase
- Polished: {YYYY-MM-DD}

## 目的

moqt-js の `src/audioPlayout.ts` は 2026-10-09〜10-10 に、鳴り遅れで音を捨てない扱い、到着の基準点の分離、到着基準へ並べ直す条件を変更した。鳴り遅れを理由に音を捨てると語尾が切れ、到着の基準点を `now` と同一視すると到着基準の並べ方が実際に鳴る位置からずれる、という実測不具合への対応である。

moqt-rs の `src/playout/scheduler.rs` は 2026-10-08 時点の移植のままで、`AUDIO_PLAYOUT_MAX_LATENESS_US` (500 ms) を超えると `Drop` する。到着基準の遅れにも上限が無く、学習値 (実測 316〜500 ms) がそのまま使われる。

## 現状

- `AudioPlayoutInput` は `now_us` を 1 本だけ受け取り、到着した音がまだ鳴っていない位置 (到着の基準点) を別に受け取れない
- 鳴り遅れが `AUDIO_PLAYOUT_MAX_LATENESS_US` を超えると `AudioPlayoutDecision::Drop` を返す。最新は捨てず、直前の音がまだ鳴っている間は遅れたまま鳴らし、音が途切れているときだけ到着基準へ並べ直す
- 到着基準の遅れの上限 (100 ms) が無く、`AUDIO_PLAYOUT_DELAY_US` (80 ms) の下限しか無い
- 到着基準へ並べ直すときの最小の跳び (10 ms) が無く、常に基準を取り直した回数として数える
- `AudioPlayoutDecision::Play` に、目標の時刻で並べたか到着基準で並べたかの区別が無い。計器が到着基準の計画を数えられない
- `schedule` は先頭で常に `confirm_stretch(0)` を呼ぶ。最新は目標を使う分岐でだけ呼ぶ

## 設計方針

moqt-js の最新の判定を移植する。

- `AudioPlayoutInput` に到着の基準点 (`arrival_us`) を追加する。呼び出し側が「いま鳴っている位置」を渡す
- `AUDIO_PLAYOUT_ARRIVAL_DELAY_US` (100 ms) と `AUDIO_PLAYOUT_RESYNC_MIN_JUMP_US` (10 ms) を追加する。到着基準の遅れは前の issue で追加する `audioArrivalPlayoutDelayUs()` の規則 ([80, 100] ms) を使う
- 鳴り遅れの分岐を「直前の音がまだ鳴っている (`last_end_us > now_us`) なら遅れたまま鳴らす」「音が途切れているなら到着基準へ並べ直す」に置き換える。並べ直しの基準は `max(arrival_us + 到着基準の遅れ, now_us + 最小リード, last_end_us)` とする
- 並べ直しの跳びが `AUDIO_PLAYOUT_RESYNC_MIN_JUMP_US` 未満のときは基準を取り直した回数に数えない
- `AudioPlayoutDecision::Play` に `basis` (`Timestamp` / `Arrival`) を追加し、`Drop` には理由 (`Backlog`) を追加する
- `confirm_stretch(0)` は目標の分岐でだけ呼ぶ
- 要求より長く詰められた分の会計は、moqt-rs が先行して直している (実際に詰めた長さを記録する) ため維持する。moqt-js は要求値で切るが、実測に合わせた moqt-rs の扱いを正とする
- `examples/moq-sub` の呼び出しを新しい入力と判定へ追随させる
- 後方互換は不要 (破壊的変更を恐れない)

## 完了条件

- 鳴り遅れで音を捨てず、音が途切れたときだけ到着基準へ並べ直すことがテストで固定されていること
- 到着基準の遅れが [80, 100] ms に収まり、跳びが 10 ms 未満のときは基準を取り直した回数に数えないことが固定されていること
- `basis` が目標の時刻で並べたか到着基準で並べたかを返すことが固定されていること
- `examples/moq-sub` が新しい API でビルドできること
- `pbt/tests/prop_playout/scheduler.rs` が新しい入力と判定に追随し、カバレッジゲートがあること
- `make pbt` / `make test` / `make clippy` / `make fmt` が通ること
- ソースコードに issue 番号や issue への言及を書かないこと

## 解決方法

- `src/playout/scheduler.rs` を moqt-js の `src/audioPlayout.ts` の最新 (2026-10-09〜10-10) に合わせた
- `AudioPlayoutInput` に `arrival_us` (到着した音がまだ鳴っていない位置) と `arrival_delay_us` (到着基準の遅れ) を追加した。`now_us` は出力のバッファへ積んだ分だけ実際に鳴る位置より先に進むため、到着基準の遅れは `arrival_us` から数える
- 到着基準の遅れは時間軸の `audio_arrival_delay_us` が返す [80 ms, 100 ms] を使う。定数は時間軸側 (`TIMELINE_ARRIVAL_DELAY_US` / `TIMELINE_AUDIO_DELAY_FLOOR_US`) を正とし、スケジューラ側に同じ値の定数を作らない
- `AUDIO_PLAYOUT_MAX_LATENESS_US` を超えた音を捨てるのをやめた。直前の音がまだ鳴っている間は遅れたまま順序と連続性を保って鳴らし、音が途切れているときだけ `rebase_by_arrival` で到着基準へ並べ直す
- `AUDIO_PLAYOUT_RESYNC_MIN_JUMP_US` (10 ms) を追加し、ずらす幅がこれ未満のときは基準を取り直した回数に数えない
- `schedule_by_arrival` の基準を `arrival_us` にし、最初の音は `arrival + arrival_delay` から並べる。timestamp が大きく飛んだときは `max(arrival + arrival_delay, now + MIN_LEAD, 直前の音の終わり)` から並べ直し、`now + (arrival_delay + BACKLOG)` を超えるなら並べすぎとして捨てる
- `AudioPlayoutBasis` (`Timestamp` / `Arrival`) と `AudioPlayoutDropReason` (`Backlog`) を追加し、`Play` に `basis`、`Drop` に `reason` を足した
- `confirm_stretch(0)` の呼び位置と、要求より長く詰められた分を実測として記録する扱いは moqt-rs の現状を維持した
- `examples/moq-sub` は音声出力が実際に鳴っている位置から `arrival_us` を求め、`arrival_delay_us` を時間軸の規則で渡す。`basis` と `Drop` の理由をログに出す
- テスト: `tests/test_playout/scheduler.rs` に「鳴り遅れの継続」「途切れ時の並べ直し」「到着の基準点の分離」「到着基準の遅れ」「resync の下限」の 5 件を追加し、既存 33 件の期待値を新しい規則 (basis / Drop の理由) に更新した
- テスト: `pbt/tests/prop_playout/scheduler.rs` に `arrival_us` と `arrival_delay_us` の生成器、`basis` ごとのカバレッジゲート、`Drop` の理由が常に `Backlog` であること、目標を使えないときは必ず到着基準であること、`Timestamp` のときは遅れが目標と一致することを追加した
- 検証: `cargo fmt --check` / `make pbt` / `make test` / `cargo clippy --workspace --all-targets -- -D warnings` / `RUSTDOCFLAGS="-D warnings" cargo doc` が成功した。`PBT_SEED` を 1 / 3 / 5 / 7 に変えても property が成功する
- 検証: 6 種類の欠陥 (鳴り遅れでの即時並べ直し / resync の下限の除去 / 到着の基準点を `now_us` と同一視 / 途切れ時の Drop への回帰 / `basis` の偽装 2 種) を入れて、追加したテストと property がそれぞれ検出することを確認した
- 備考: `examples/moq-sub` の `AudioJitterBuffer::drop_late` はバッファ段の Drop を残している。example 全体の組み立ての追従は後続の issue で行う
