# moqt-js の映像 jitter 上限と共有解除の修正を移植する

- Created: 2026-10-20
- Completed: 2026-10-20
- Branch: develop
- Polished: {YYYY-MM-DD}
- Reporter: @voluntas

## 目的

moqt-rs の `src/playout/timeline.rs` は moqt-js の `src/playout/timeline.ts` を移植したものだが、moqt-js の 2026-10-14 以降の修正が 2 件入っていない。qmux のバーストで映像の jitter 目標が上限 500 ms へ張り付き、A/V 同期が音声へ最大 100 ms を即時加算して音声の目標が段差で動く。また、基準の共有解除が戻らず、または往復して音声が到着基準と TIMESTAMP 基準を行き来する。あわせて、映像の学習判定が現フレーム間隔ではなく中央値を使っている点を moqt-js に合わせる。

## 現状

- `src/playout/timeline.rs` の `video_target_us` は、遅れの揺らぎを分位点へ入れる前に 500 ms 超を除外していない。moqt-js は `MAX_PLAYOUT_DELAY_MS` 超を分位点の計算前に捨てる。600 ms 遅れが半分を占める入力を固定したテストがあり、500 ms を期待している。
- `src/playout/timeline.rs` の `refresh_unshared_state` / `can_release_unshared_hold` は、解除のきっかけが「差の動き」であったことを早期解除の必須条件にしている。moqt-js の `61e1d576` はこの条件を削除し、直近に解除と解除を往復していないこと（`isUnsharedRepeatedly`）で判断する。Rust には往復検出が無く、解除の時刻も消している。
- `src/playout/timeline.rs` の `learns` は直近のフレーム間隔の中央値で判定する。moqt-js は現フレームの TIMESTAMP 間隔で判定する。

## 設計方針

- `video_target_us` は、正の揺らぎを `TIMELINE_MAX_PRESENTATION_DELAY_MS` 以下に絞ってから分位点を取る。上限で切るだけでは 500 ms に張り付くため、除外が必要である。テストは 600 ms 遅れが半分なら目標 0、ちょうど 500 ms は残ることを固定する。
- 共有解除は moqt-js の `61e1d576` に合わせる。解除のきっかけが動きだったかを見ず、`previous_unshared_at` と `last_unshared_at` の差が保持時間未満なら早期解除しない。解除時に `last_unshared_at` は消さず、往復検出に使う。テストは「閾値だけが動いた解除でも差が戻れば 2 秒で共有へ戻る」「30 秒以内の再解除は保持する」を固定する。
- `learns` は現フレームの TIMESTAMP 間隔で判定する。テストは「TIMESTAMP が飛んでも到着がフレーム間隔未満なら学習しない」を固定する。

## 完了条件

- 上記 3 点を単体テストで固定し、既存のテストの期待値を新しい挙動へ更新する。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 参照

- `src/playout/timeline.rs` の `video_target_us` / `refresh_unshared_state` / `can_release_unshared_hold` / `update_unshared_release` / `observe`
- moqt-js の `src/playout/timeline.ts` の `observe` / `holdUnsharedStream` / `canReleaseUnsharedHold` / `isUnsharedRepeatedly`

## 解決方法

- 映像の jitter 目標を求める `video_target_us` で、表示の遅れの上限を超える揺らぎを分位点へ入れる前に除外した。
- 基準の共有解除を moqt-js の修正に合わせ、解除のきっかけが動きだったかを見ず、直近の解除との間隔で往復を検出するようにした。解除の時刻は往復検出のために残す。
- 映像の学習判定 `learns` を、フレーム間隔の中央値ではなく現フレームの TIMESTAMP 間隔で行うようにした。
- 共有へ戻った直後にもう一度同期をそろえ直すようにした。プロパティテストで、保持が明けた最初の観測だけ表示時刻の差が目標より大きいまま残る不具合を見つけ、固定入力を単体テストへ追加した。
- 単体テストと PBT を更新・追加し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ることを確認した。
