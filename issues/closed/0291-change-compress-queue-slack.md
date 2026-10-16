# 時間圧縮を遅れが減らせる分だけに制限する

- Created: 2026-10-16
- Completed: 2026-10-16
- Branch: feature/change-compress-queue-slack
- Polished: {YYYY-MM-DD}

## 目的

moqt-js は 2026-10-16 の変更 (39d3fb7a、「playout のノイズの原因を修正し、目標遅延を経路に適応させる」) のうち `src/playout/audioPlayout.ts` の `schedule` で、時間圧縮 (波形の周期を削って目標へ戻す操作) の量を「遅れ」「音の長さの半分」「キューの余裕」の最小値に制限した。キューの余裕 = 直前の音の終わり − (今 + 最小の余裕) であり、余裕が 0 以下なら詰めない。

目的は、出力のバッファが下限を決めているときに時間圧縮をしても遅れが減らず、削った分がそのまま無音の穴になるためである。moqt-js
の実測 (2026-10-16、実装を直接駆動。出力のリード 95.5 ms、表示の遅れ 66 ms、10 ms の音 100 フレーム) では、100
件すべてが圧縮を要求して 5 ms ずつ削れ、99 件・495 ms の穴 (5 ms 以下は `concealmentOf`
の規則で補間されないため無音) が空き、遅れは 39.5 ms のまま 1 ms も減らなかった。修正後は圧縮 0 件・穴 0
件で、遅れは 39.5 ms のまま変わらない。キューが遅れているときの追いつき
(音が途切れているときの到着基準への並べ直し) は変えていない。

moqt-rs も同じ欠陥を持つ。`src/playout/scheduler.rs` の `schedule` は遅れと音の長さの半分だけで詰める量を決めており、`concealment_of` が「5 ms 以下」と「開始が今 + 余裕より前」の隙間を補間しないため、余裕を超えて詰めた分は無音の穴として残る。遅れは減らないまま音だけが短くなる。

## 現状

- `src/playout/scheduler.rs` の `schedule` は `compress_us = if lateness_us >= AUDIO_PLAYOUT_STRETCH_MIN_US { lateness_us.min(input.duration_us.max(0) / 2) } else { 0 }` である。キューの余裕を見ていない。
- 直前の音の終わりは `last_end_us` (`Option<i64>`) であり、moqt-js の `lastEnd` (無いときは null) に対応する。鳴らす時刻の下限は `AUDIO_PLAYOUT_MIN_LEAD_US` (10 ms) である。
- 余裕が負でも詰める。直前の音が既に鳴り終わっていて (キューが空で) 遅れて届いた音では、鳴り始めが今 + 最小の余裕で決まるため遅れは減らず、削った分だけ音が短くなり、次の音との間に穴が空く。
- moqt-js は浮動小数点の許容 `AUDIO_PLAYOUT_STRETCH_MIN_EPSILON_SECONDS` (1e-9 秒) を置き、余裕がこの値以下のときは詰めない。Rust 側には相当する許容が無い。
- 到着基準の並べ方 (`schedule_by_arrival` と `rebase_by_arrival`) は元から圧縮しないため、この欠陥は無い。

## 設計方針

- キューの余裕を直前の音の終わり (`last_end_us`) と今 (`now_us`) から求める。直前の音が無いときは余裕も無いものとして扱う (moqt-js の「直前の音の終わり = -Infinity」に相当する)。`Option` と `saturating_sub` で表現し、i64 のオーバーフローを起こさない。
- 詰める量を「遅れ」「音の長さの半分」「キューの余裕」の最小値にする。余裕が 0 以下のときと、許容 `AUDIO_PLAYOUT_STRETCH_MIN_EPSILON_US` 以下のときは 0 にする。
- 許容は、マイクロ秒の整数で表現できる最小の正の値である 1 マイクロ秒にする。moqt-js の 1e-9 秒は 1 マイクロ秒未満であり、この許容以下で詰めても削れる長さは 48 kHz の 1 サンプル (約 21 マイクロ秒) にも満たない。
- 既存の追いつき (`rebase_by_arrival`、音が途切れているときの到着基準への並べ直し) の挙動と、`compress_us` を呼び出し側が `stretch::compress` の上限として渡す意味は変えない。
- 破壊的変更を恐れない。直前の音が無い最初の音では詰めなくなる。

## 完了条件

- キューの余裕を超えて詰めないこと、余裕が 0 以下 (直前の音が無い場合を含む) と許容以下のときは詰めないことが実装されていること。
- `tests/test_playout/scheduler.rs` が境界 (余裕ちょうど、余裕 0、余裕が負、許容ちょうど、直前の音が無い) を固定していること。
- 既存の追いつきの挙動が変わっていないこと。
- `pbt/tests/prop_playout/scheduler.rs` が新しい契約 (詰める量がキューの余裕以内であること) に追随していること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `cargo test -p pbt` が通ること。

## 解決方法

- `src/playout/scheduler.rs` の `schedule` で、詰める量を「遅れ」「音の長さの半分」「キューの余裕」の最小値にした。
  - キューの余裕 = 直前の音の終わり (`last_end_us`) − (今 (`now_us`) + `AUDIO_PLAYOUT_MIN_LEAD_US`)。`last_end_us` が `None` のとき (直前の音が無いとき) は余裕も無いものとして扱う (moqt-js の「直前の音の終わり = -Infinity」に相当する)。`saturating_sub` と `saturating_add` だけを使い、i64 のオーバーフローを起こさない。
  - 余裕が `AUDIO_PLAYOUT_STRETCH_MIN_EPSILON_US` (1 マイクロ秒) 以下、または遅れが `AUDIO_PLAYOUT_STRETCH_MIN_US` (20 ms) 未満のときは 0 にする。
- 定数 `AUDIO_PLAYOUT_STRETCH_MIN_EPSILON_US` (1 マイクロ秒) を追加した。moqt-js の `AUDIO_PLAYOUT_STRETCH_MIN_EPSILON_SECONDS` (1e-9 秒) に相当する。マイクロ秒の整数では 1e-9 秒を表現できないため、表現できる最小の正の値である 1 マイクロ秒を置いた。この許容以下で詰めても、削れる長さは 48 kHz の 1 サンプル (約 21 マイクロ秒) にも満たない。
- 既存の追いつき (`rebase_by_arrival` と到着基準の並べ方) は変えていない。到着基準では元から `compress_us` を 0 にしており、`compress_us` を呼び出し側が `stretch::compress` の上限として渡す意味も変えていない。
- moqt-js と一致させた点: 余裕の求め方、余裕が 0 以下なら詰めないこと、詰める量を 3 つの最小値にすること、直前の音が無いときに詰めないこと、キューが遅れているときの追いつきを変えないこと。
- moqt-js と意図的に変えた点: 許容の値。moqt-js は秒の浮動小数点数であり 1e-9 秒を置くが、Rust はマイクロ秒の整数であるため 1 マイクロ秒にした (1e-9 秒は表現できない)。余裕が正でも 1 サンプルに満たない場合 (2〜20 マイクロ秒) は詰める要求を出すが、実際に削れる長さは波形の周期 (2.5 ms 以上) で決まるため実害は無い。
- `tests/test_playout/scheduler.rs` は 40 件から 44 件になった。境界を固定する 4 件を追加し、既存 11 件の入力と期待値を作り直した。
  - 直前の音が無いときは詰めない (`late_sound_without_a_previous_sound_is_not_compressed`。旧 `late_sound_is_shifted_and_compressed` を作り直した)
  - 余裕ちょうどまでしか詰めない (`compression_is_capped_by_the_queue_slack`)
  - 余裕が 0 のときは詰めない (`zero_queue_slack_is_not_compressed`)
  - 余裕が負のときは詰めない (`negative_queue_slack_is_not_compressed`)
  - 余裕が許容ちょうどのときは詰めず、許容を 1 マイクロ秒超えるとその分だけ詰める (`queue_slack_at_the_epsilon_is_not_compressed`)
  - 既存のテストは「並べてある音」を 1 つ作ってから遅れた音を並べる形へ直した。詰めるにはキューの余裕が要るためである (`schedule_queued_sound` ヘルパーを追加した)
- `pbt/tests/prop_playout/scheduler.rs` に「詰める量はキューの余裕以内である」ことを追加した。`schedule` が記録する直前の音の終わりを呼び出し側でも同じ式 (要求した詰める量と、`confirm_stretch` が返した実際に詰めた長さの差) で写し取り、本当の余裕で検査する。詰める決定に到達した回数も数え、検証が空振りしていないことを確かめる。
  - 独立レビューの指摘により、最初の実装 (鳴り始めを余裕の上限の代わりに使う) を直した。鳴り始めは直前の音の終わり以上であるため実際より広い余裕を許してしまい、`previous_end_us` の代わりに鳴り始めを使う回帰を見逃す。issue の完了条件 (詰める量がキューの余裕以内であることの固定) を満たしていなかった。
- `examples/moq-sub/src/main.rs` のテスト `playout_applies_the_gap_and_the_compression` の 3
  つ目の入力だけを直した。キューが空のときは詰めない契約へ変わるため、直前の音がまだ鳴っている
  (キューの余裕がある) 形へ直した。`now_us` を 1_100_000 から 1_030_000 へ、`target_start_us` を
  `Some(1_090_000)` から `Some(1_020_000)` へ、`arrival_us` を 1_100_000 から 1_030_000 へ直した
  (到着の基準を今と同じにする)。キューの余裕は 1_050_000 − (1_030_000 + 10_000) = 10_000 であり、遅れ 30 ms
  と音の長さの半分 10 ms のうち小さい 10 ms を詰める。
- `examples/moq-sub/src/main.rs` のテスト `playout_assembly_records_the_play_in_the_stats` も、キューの余裕がある形へ直した。このテストは音を 1 つだけ予約する状況で詰め (10 ms) を期待しており、新しい契約では詰めなくなるためである。
  - 先に「目標どおりに鳴る音」を 1 つ `arrange` で並べる (計器へは記録しない)。この音は `target_us` から鳴り、`target_us + 20_000` に終わる。
  - 遅れた音の予約を `now_us = target_us + 10_000` から `now_us = target_us` へ直した。キューの余裕 = (`target_us + 20_000`) − (`target_us` + 10_000) = 10_000、遅れ = 20_000、音の長さの半分 = 10_000 となり、詰める要求は 10_000 のままである。
  - 期待値 (`played_us` = 10_000、`snapshot.played_us` = 15_000、`played_frames` = 1、`last_slack_us` = 0、`last_start_delay_us` = 20_000、`last_lateness_us` = 20_000) は変えていない。テストの意図 (実際に詰めた長さが計器へ記録されること) も変えていない。
  - 先に並べる音は `commit` しない。`commit` すると閉ループへ観測が渡り、次の音の目標が動いてしまうためである (`arrange` は時間軸と計器を書き換えない)。
- 検証: `cargo test --workspace` が成功した (EXIT=0)。`cargo test -p pbt` が成功した (EXIT=0)。
- 検証: この変更だけを HEAD の他ファイルと組み合わせたコピーで、`tests/test_playout/scheduler.rs` の 44
  件、`pbt/tests/prop_playout/scheduler.rs`、`cargo clippy` (lib と pbt の all-targets、`-D warnings`)、`cargo
  fmt --all -- --check`、`RUSTDOCFLAGS="-D warnings" cargo doc` がすべて成功した。PBT は `PBT_SEED` を 1 / 3 /
  5 / 7 / 11 / 12345 に変えても成功し、詰める決定は 1 実行あたり 194〜273 回踏むため到達の検証は空振りしない。
- 検証: 最終的に 4 つとも成功した。`cargo test --workspace` (EXIT=0)、`cargo clippy --workspace --all-targets -- -D warnings` (EXIT=0)、`cargo fmt --all -- --check` (EXIT=0)、`cargo test -p pbt` (EXIT=0)。
- 備考: 検証の途中では、同じ workspace で進めていた別の移植タスク (feedback) のファイルが作業中であり、`cargo clippy` と `cargo fmt` がそれらのファイルだけで一時的に失敗していた (`pbt/tests/prop_playout/feedback.rs` の `manual_clamp`、`tests/test_playout/feedback.rs` の `assertions_on_constants` 3 件、およびそれらの整形差分)。この issue の対象ファイルに指摘は無く、最終実行では 4 つとも成功した。
