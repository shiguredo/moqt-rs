# A/V 同期の解除を、動きの持続と水準の復帰で決める

- Created: 2026-10-14
- Completed: {YYYY-MM-DD}
- Branch: feature/change-base-difference-drift-confirm
- Polished: {YYYY-MM-DD}

## 目的

moqt-js の `src/playbackTimeline.ts` は 2026-10-10〜10-13 に、A/V の基準の共有を解除する
判定を 2 回直した。

- 一過性の基準の差の動きを時計のずれと誤判定するのを直した (612b78b7)。読み出しが一瞬
  遅れただけでも差は閾値を超えて動き、元へ戻る。そのまま解除すると 30 秒の保持に入り、
  戻った後も解除が残る
- 解除のきっかけが去って差が戻ったら、保持を待たずに共有を戻すようにした (c3e00bfa)。
  実測 (CI の 4 vCPU の runner) では、動きの速さが 239.6 → 183.3 → 0.2 ms/秒と動く一方で
  基準の差は 3 ms 以内で安定しており、25 秒間ずっと解除されたままになっていた

moqt-rs の `PlayoutTimeline::base_difference_drifted` は「履歴の最も古い記録との差が
[`TIMELINE_BASE_DRIFT_US`] (50 ms) を超えているか」だけを見る。持続の確認も、水準への
復帰も、参照 (表示時刻が使う基準の差) の向きも見ないため、同じ誤判定が起きる。

## 現状

- `base_difference_drifted` は `base_difference_history` の「最も古い記録との差」の絶対値が
  `TIMELINE_BASE_DRIFT_US` を超えるかだけを返す。1 回の記録で大きく動いても、ゆっくり
  動いても同じ扱いになる
- 解除した側は `TIMELINE_BASE_UNSHARED_HOLD_US` (30 秒) の間は戻さない。解除のきっかけが
  去ったかどうかを見る仕組みが無い
- 解除の理由は `UnsharedReason::Drift` と `UnsharedReason::Difference` の 2 つであり、
  どちらがきっかけで解除したかを保持していない
- 購読の直後は、relay の cache から届いた分と購読を始めるまでにたまった分をまとめて復号する
  ため、基準の遅れが数百 ms から数秒動く。この動きを時計のずれとみなすと、落ち着いた後の
  観測がすべて解除されたままになる

## 設計方針

moqt-js の最新 (2026-10-13) の判定を移植する。

- 動きを見る値は、直近 [`TIMELINE_BASE_DIFFERENCE_RECENT_WINDOW_US`] の窓の最小値の差の
  ままにする (経路と復号の乱れの位相で動くため、これだけで判定しない)
- 水準 (`settled_base_difference_us`) は「動きが戻ったとき」と「落ち着いた水準を観測して
  から」だけ取り直す。記録のたびに取り直すと、ゆっくりしたドリフトでも水準が一緒に動き、
  離れた幅が閾値を超えない
  - `TIMELINE_BASE_DIFFERENCE_QUIET_US` (5 ms) 以下の変化が
    `TIMELINE_BASE_DIFFERENCE_SETTLE_US` (3 秒) 続いたら、または最初の記録から
    `TIMELINE_BASE_DIFFERENCE_START_US` (10 秒) たったら、参照 (表示時刻が使う基準の差)
    を水準にする
- 離れ幅が [`TIMELINE_BASE_DRIFT_US`] 以下へ戻ったら判定を消し、水準を取り直す
- 離れ幅が [`TIMELINE_BASE_DRIFT_STEP_US`] (200 ms) を超え、かつ次のどちらかなら待たずに
  解除する
  - 1 回の記録で [`TIMELINE_BASE_DIFFERENCE_JUMP_US`] (150 ms) を超えて離れた
  - 参照が同じ向きに [`TIMELINE_BASE_DRIFT_US`] を超えて動いた
- そうでなければ、離れたまま [`TIMELINE_BASE_DRIFT_CONFIRM_US`] (6 秒) 続いたら解除する
- 解除のきっかけが動き (Drift) であり、判定が解け、差が閾値の中へ戻り、水準へ復帰した状態が
  [`TIMELINE_BASE_UNSHARED_RELEASE_US`] (2 秒) 続いたら、保持を待たずに解除をやめる
- 状態を追加する (`settled_base_difference_us` / `base_difference_quiet_since_us` /
  `base_difference_first_at_us` / `base_difference_deviation_since_us` /
  `base_difference_deviation_reference_us` / `unshared_trigger_moved_value` /
  `unshared_returned_since_us`)
- 後方互換は不要 (破壊的変更を恐れない)

## 完了条件

- 一過性の動き (離れて戻る) では解除しないことがテストで固定されていること
- 戻らない動きは持続の確認の後に解除することがテストで固定されていること
- 1 回で大きく離れた段差は、参照が同じ向きに動いていれば待たずに解除することがテストで
  固定されていること
- 閾値だけが動いた解除 (差が同じ水準のまま) は、保持を待たずに戻さないことがテストで
  固定されていること
- きっかけが去って差が戻ったら、保持の 30 秒を待たずに戻ることがテストで固定されていること
- `pbt/tests/prop_playout/timeline.rs` が新しい契約に追随していること
- `make pbt` / `make test` / `make clippy` / `make fmt` が通ること
- ソースコードに issue 番号や issue への言及を書かないこと

## 解決方法

(対応時に記入)
