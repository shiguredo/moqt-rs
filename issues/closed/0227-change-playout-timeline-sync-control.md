# 共有の時間軸の同期制御を最新の moqt-js に合わせる

- Created: 2026-10-10
- Completed: 2026-10-10
- Branch: feature/change-playout-timeline-sync-control
- Polished: {YYYY-MM-DD}

## 目的

moqt-js の `src/playbackTimeline.ts` は 2026-10-09 の一連の修正で A/V 同期の制御を入れ替えた。基準を共有しない理由の分類、基準の差の動き (ドリフト) による検出、相手側へ移す遅延の上限、共有をやめた後に判定を戻さない保持、共有できないときに映像だけを音声の到着基準へ合わせる処理が入っている。

moqt-rs の `src/playout/timeline.rs` は 2026-10-02 時点の移植のままで、実測で問題になった挙動が残っている。音声の TIMESTAMP が段差でずれ、そのずれを到着の揺らぎとして学習した遅延 (実測 600 ms 超) を映像へそのまま足し、映像の表示待ちが数十秒戻らない。基準を共有できないと判定すると同期制御ごと止まり、音声と映像が別々の基準で並んで約 100 ms のずれが残る。

## 現状

- `PlayoutTimeline::sharing_bases` は `drifted_track().is_none()` であり、どちらかのトラックが未観測のとき `true` を返す。最新は未観測を「共有していない」として `false` を返す
- ずれた側の判定は基準の差の大きさだけで、差の動き (ドリフト) を見ない
- 相手側へ足す遅延の上限 (`PLAYOUT_MAX_COMPENSATED_DIFFERENCE_MS` 相当) が無い
- 一度共有をやめた後に判定を戻さない保持 (`PLAYOUT_BASE_UNSHARED_HOLD_MS` 相当) が無い
- 基準を共有できないときは `present_us` がずれた側で `None` を返し、`update_sync_delays` も早期に戻る。最新は映像だけを音声の到着基準の遅れへ合わせる
- 音声を到着基準で並べるときの遅れの規則 (`AUDIO_PLAYOUT_ARRIVAL_DELAY_MS` と `audioArrivalPlayoutDelayMs` の [80, 100] ms) が無い
- 遅延の内訳 (基準の遅れ / 揺らぎ / 同期で足した分 / 表示の遅れと上限) と共有しない理由を返す API が無い
- `src/playout/scheduler.rs` は到着基準でも学習した遅れをそのまま使う

## 設計方針

moqt-js の最新の判定順 (unobserved → drift → difference → hold → none) と計算式をそのまま移植する。時刻はこれまでどおりマイクロ秒の `i64` で扱い、ミリ秒の定数はマイクロ秒へ換算して公開する。

- 定数を追加する: `TIMELINE_ARRIVAL_DELAY_US` (100 ms)、`TIMELINE_MAX_COMPENSATED_DIFFERENCE_US` (100 ms)、`TIMELINE_BASE_DRIFT_US` (50 ms)、`TIMELINE_BASE_DRIFT_WINDOW_US` (5 秒)、`TIMELINE_BASE_UNSHARED_HOLD_US` (30 秒)
- 公開型を追加する: `UnsharedReason` (`None` / `Unobserved` / `Drift` / `Difference` / `Hold`)、`TrackBreakdown`、`DelayBreakdown`
- `PlayoutTimeline` に `unshared_reason` / `audio_arrival_delay_us` / `delay_breakdown` を追加し、`sharing_bases` と `drifted_track` を新しい判定へ置き換える
- 基準の差の履歴を保持する (`record_base_difference` 相当)。動きは直近 2 秒と 5 秒の窓から毎秒あたりに換算して求め、差の大きさより先に判定する
- 共有できないときは映像の同期の足し込みを音声の到着基準の遅れ (`audioArrivalPlayoutDelayUs`) へ合わせ、足した分は毎秒 20 ms で戻す
- 同期の足し込みは `TIMELINE_MAX_COMPENSATED_DIFFERENCE_US` を超えない。目標の差は `max(TIMELINE_SYNC_MIN_DELTA_US, |素の差| - 上限)` とする
- `TimedWindow` に `oldest` と `min_after` を追加する (`src/timedValues.ts` の `TimedValues` の対応)
- 音声の表示の遅れを決める閉ループは後続の issue で入れるため、この issue では学習値のままとする。内訳の `audioDelayFeedback` に相当する欄は後続で足す
- 後方互換は不要 (破壊的変更を恐れない)

## 完了条件

- 未観測 / ドリフト / 差の大きさ / 保持 / 上限 / 共有できないときの映像の合わせ込みが、それぞれテストで固定されていること
- ドリフトは差が動き続けた時点で検出され、検出後は `TIMELINE_BASE_UNSHARED_HOLD_US` の間は共有に戻らないことが固定されていること
- `delay_breakdown` の表示の遅れが「基準の遅れ + max(揺らぎ, targetLatency) + 同期で足した分」と一致することが固定されていること
- `tests/test_playout/timeline.rs` と `pbt/tests/prop_playout/timeline.rs` が新しい意味に合わせて更新され、PBT にカバレッジゲートがあること
- `make pbt` / `make test` / `make clippy` / `make fmt` が通ること
- ソースコードに issue 番号や issue への言及を書かないこと

## 解決方法

- `src/playout/timeline.rs` の同期制御を moqt-js の `src/playbackTimeline.ts` の最新 (2026-10-09) に合わせた
- 定数を追加した: `TIMELINE_AUDIO_DELAY_FLOOR_US` (80 ms)、`TIMELINE_ARRIVAL_DELAY_US` (100 ms)、`TIMELINE_MAX_COMPENSATED_DIFFERENCE_US` (100 ms)、`TIMELINE_BASE_DRIFT_US` (50 ms)、`TIMELINE_BASE_DRIFT_WINDOW_US` (5 秒)、`TIMELINE_BASE_UNSHARED_HOLD_US` (30 秒)。到着基準の遅れは `audio_arrival_delay_us` で [80, 100] ms に切る
- 公開型 `UnsharedReason` (`None` / `Unobserved` / `Difference` / `Drift` / `Hold`)、`TrackBreakdown`、`DelayBreakdown` を追加し、`unshared_reason` / `delay_breakdown` / `playout_delay_us` / `audio_arrival_delay_us` を追加した
- `sharing_bases` は未観測を「共有しない」扱いに変えた
- ずれた側の判定に差の動き (ドリフト) を加えた。直近 2 秒の最小値の差を 250 ms ごとに記録し、5 秒の窓で 50 ms を超えたら時計のずれとみなす。判定順は未観測 → ドリフト → 差の大きさ → 保持である
- 共有をやめた側は `TIMELINE_BASE_UNSHARED_HOLD_US` (30 秒) の間は保持する
- 同期で相手側へ移す分は `TIMELINE_MAX_COMPENSATED_DIFFERENCE_US` を超えず、目標の差を `max(TIMELINE_SYNC_MIN_DELTA_US, |自然な差| - 上限)` とした (`desired_difference_us`)
- 基準を共有できない側が音声のときは、映像だけを音声の到着基準の遅れへ合わせる (`update_sync_delays_to_arrival_audio`)
- 共有できないときは、足した分を毎秒 `TIMELINE_DELAY_DECAY_US_PER_SECOND` までで戻す (`decay_sync_extras`)
- `TimedWindow` に `oldest` と `min_after` を追加し、基準の差の履歴に使う
- テスト: `tests/test_playout/timeline.rs` に未観測 / 遅延の内訳 / ドリフト検出 / 保持 / 映像の到着基準への合わせ込み / 到着基準の遅れの 6 件を追加し、既存 2 件の期待値を新しい規則 (移す分は上限まで) に更新した
- テスト: `pbt/tests/prop_playout/timeline.rs` に、共有の判定と理由・内訳の整合、ずれている側の表示時刻が返らないこと、表示時刻が内訳から計算できること、移す分が目標の差を超えないことを確かめる property を追加した
- 検証: `make pbt` / `make test` / `make clippy` / `make fmt` が通る。`PBT_SEED` を 1 から 8 まで変えても追加した property が成功する
- 検証: 4 種類の欠陥 (移す分の上限の除去 / 共有の判定の固定 / 保持の無効化 / 基準の差の履歴の不記録) を入れて、追加したテストと property がそれぞれ検出することを確認した
