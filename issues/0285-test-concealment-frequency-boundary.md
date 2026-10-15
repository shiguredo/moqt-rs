# 音声の補間の可否を周波数の境界でテストに固定する

- Created: 2026-10-16
- Completed: 2026-10-16
- Branch: feature/test-concealment-frequency-boundary
- Polished: {YYYY-MM-DD}

## 目的

moqt-js は 2026-10-15 の変更 (`origin/develop` の #492) で、欠落した区間の補間の可否が
「末尾の周期が 4 kHz へ間引いた格子に乗るか」で決まることをテストで固定した。

実測 (48 kHz、20 ms のフレーム、振幅 0.5、20 ms の穴) では、1000 Hz と 440 Hz は 960 サンプル
を生成し、300 / 220 / 150 / 100 Hz は生成しない。1000 Hz は 4 kHz で 4 サンプル (格子に乗る)
であり、440 Hz は 9.09 サンプル (乗らない) であるが、位数の 18 が探索範囲 (10〜60) に入り、
継ぎ目の段差が許容に収まるため通る。

この境界は「実装の欠陥」と誤解されやすい (実リレーの E2E では補間の量が小さく、正弦波の
E2E 音では周期が格子に乗りにくいためである)。moqt-rs の `src/playout/stretch.rs` の `conceal`
は同じ判定を持つが、周波数ごとの境界がテストで固定されていない。移植して固定する。

## 現状

- `src/playout/stretch.rs` の `conceal` は、末尾の 2 周期分の正規化相関 (`find_tail_lag`) と
  継ぎ目の段差 (`is_seam_smooth`) で補間の可否を決める。
- 事前確認 (48 kHz、20 ms、振幅 0.5、20 ms の穴) では、現実装は 1000 Hz → 960、
  440 Hz → 960、300 / 220 / 150 / 100 Hz → 0 を返す。moqt-js と一致している。
- この境界を周波数ごとに固定するテストが `tests/test_playout/stretch.rs` にも
  `src/playout/stretch.rs` の `#[cfg(test)]` にも無い。

## 設計方針

- 正弦波の周波数ごとに補間したサンプル数を返す補助関数をテスト内に置き、1000 Hz と 440 Hz が
  960、300 / 220 / 150 / 100 Hz が 0 であることを固定する。
- テストのコメントに「なぜこの境界なのか」(4 kHz の格子、位数が探索範囲に入る場合、
  継ぎ目の段差の許容) と、E2E の正弦波では補間が働きにくいことを書く。
- `conceal` は公開 API であるため、テストは `tests/test_playout/stretch.rs` に置く。

## 完了条件

- 上記の境界がテストで固定されていること。
- テストのコメントに境界の理由が書かれていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
- ソースコードに issue 番号や issue への言及を書かないこと。

## 解決方法

- `tests/test_playout/stretch.rs` に、正弦波の周波数ごとに補間したサンプル数を返す補助
  (`conceal_for_frequency`) を置き、48 kHz / 20 ms のフレーム / 振幅 0.5 / 20 ms の穴で、
  1000 Hz と 440 Hz は 960、300 / 220 / 150 / 100 Hz は 0 になることを固定した
- テストのコメントに境界の理由を書いた。4 kHz の格子と探索範囲 (10〜60、20 ms のフレームでは
  末尾の 2 周期分が取れる範囲に限られるため 10〜39)、格子に乗らない音では探索範囲に入る位数
  (周期の倍数) のうち相関が最も強いものが選ばれること、440 Hz は位数 2 の 18 で継ぎ目の段差が
  許容に収まること、300 / 220 / 150 Hz は周波数が低いほど小さくなる自然な段差の許容を超える
  こと、100 Hz は 1 周期ちょうどの 40 が選べないこと、実リレーの E2E の正弦波では補間が
  働きにくいことを書いた
- 実装 (`src/playout/stretch.rs` の `conceal`) は変更していない。事前確認のとおり
  1000 Hz / 440 Hz が 960、300 / 220 / 150 / 100 Hz が 0 を返すことをテストで確認した
- 検証: `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が成功した
