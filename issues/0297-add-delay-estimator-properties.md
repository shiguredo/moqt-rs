# 到着の遅れ推定器の property を moqt-js に合わせて追加する

- Created: 2026-10-17
- Completed: 2026-10-17
- Branch: feature/add-delay-estimator-properties

## 目的

moqt-js は 2026-10-17 の変更 (`origin/develop` の 5fed7505「playout のレビュー指摘を反映
する」) で、到着の遅れから目標遅延を決める推定器 (`src/playout/audioDelayEstimator.ts`) に
property テストを 4 本追加した (`src/playout/audioDelayEstimator.prop.ts`、144 行)。

moqt-rs の `src/playout/delay.rs` の `JitterDelayManager` はこの推定器の移植であるが、
`pbt/tests/prop_playout/delay.rs` には「目標が範囲 (20〜2000 ms) に収まる」1 本しか無い。
分布のバケットの倍数であること、観測した遅れの最大を超えないこと、揺らぎが無ければ下限まで
下がること、同じ到着列なら同じ学習になることが固定されていない。学習の性質を入力の生成で
確かめられるようにする。

## 現状

- `pbt/tests/prop_playout/delay.rs` は `target_delay_stays_in_range` の 1 本のみである。
- `tests/test_playout/delay.rs` には、観測が無いときの初期値 (`AUDIO_DELAY_START_MS`) と
  `reset` で初期値へ戻ること (`reset_returns_to_the_start_delay`)、揺らぎの無い到着列で
  下限のバケット (20 ms) になること
  (`target_delay_starts_at_eighty_and_drops_to_the_minimum_without_jitter`) が既にある。
- moqt-js の 4 本のうち「観測が無いうちは初期値を返す」は上記の単体テストが担っている
  (決定的な性質であり、property で生成する意味が無い)。
- 残る 3 本 (バケットの倍数と上限 / 揺らぎが無ければ下限 / 決定性と `reset`) は入力の生成に
  よって確かめる性質であり、moqt-rs には無い。

## 設計方針

- `pbt/tests/prop_playout/delay.rs` に 3 本を追加する。音 1 つの長さは Opus の 1 フレームと
  同じ 20 ms とし、到着は TIMESTAMP + 遅れとして観測する (moqt-js の helper と同じ形)。
- バケットの倍数と上限: 音の数はヒストグラムへ入れる区間 (500 ms) を必ず 1 回以上またぐ
  50 個以上にする。それ未満では初期値のままで、学習した値の性質を確かめられない。
- 下限のバケット: 一定の遅れで届く音だけを観測し、相対の遅れが常に 0 になることを使う。
- 決定性: 同じ遅れの列を 2 つの推定器へ与えて同じ目標になること、`reset` の後も同じに
  なることを確かめる。
- 単体テストで担っている性質 (初期値と `reset`) は pbt へ重複して書かない
  (`pbt` に unittest を書かない方針に従う)。

## 完了条件

- 上記 3 本の property が `pbt/tests/prop_playout/delay.rs` にあり、複数のシードで成功する
  こと。
- 既存の `target_delay_stays_in_range` と `tests/test_playout/delay.rs` の検証を壊さない
  こと。
- `make test` / `make clippy` / `make fmt` / `make pbt` が成功すること。
- ソースコードに issue 番号や issue への言及を書かないこと。

## 解決方法

- `pbt/tests/prop_playout/delay.rs` に 3 本を追加した。`target_delay_is_a_bucket_multiple_
  within_the_observed_delays` (バケットの倍数、下限以上、観測した遅れの最大 + 1 バケット
  以下、バケットの範囲以下)、`target_delay_drops_to_the_floor_bucket_without_jitter`
  (一定の遅れなら下限のバケット)、
  `target_delay_is_deterministic_and_reset_restarts_the_learning` (同じ到着列なら同じ
  目標、`reset` の後も同じ)
- 音 1 つの長さを 20 ms とする helper (`observe_constant` / `observe_series`) を足し、
  moqt-js の property と同じ形で観測列を作るようにした
- 観測が無いときの初期値と `reset` は `tests/test_playout/delay.rs` の
  `target_delay_starts_at_eighty_and_drops_to_the_minimum_without_jitter` と
  `reset_returns_to_the_start_delay` が担っているため、pbt へは重複して書いていない
- 検証: `cargo test -p pbt --test prop_playout delay` が 4 本とも成功し、`PBT_SEED` を
  1 / 3 / 5 / 7 / 11 / 12345 / 0xdeadbeef に変えても成功した
