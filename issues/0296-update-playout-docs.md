# 計器と閉ループの doc を実装に合わせる

- Created: 2026-10-17
- Completed: 2026-10-17
- Branch: feature/update-playout-docs

## 目的

moqt-js は 2026-10-17 の変更 (`5fed7505`「playout のレビュー指摘を反映する」) で、doc と実装が
食い違っている箇所を直した。moqt-rs には同じ食い違いが残っている。計器と閉ループの doc は、
遅延と音の欠落の原因を数値から切り分けるときの読み方を決める。食い違ったままだと、

- 正常な状態を「時間軸が目標を決められていない」と誤診し、
- 鳴り終わっていない音の扱いを取り違え、
- 閉ループが動き出す条件を「実際に鳴った音があること」と誤解する

といった誤った判断につながる。実装どおりの記述へ揃える。

## 現状

- `src/playout/timing.rs` の `AudioPlayoutTimingSnapshot.arrival_planned_frames` の doc は
  「時間軸が目標を決められていないことを、この数と `unplanned_frames` が 0 であることの
  組み合わせで読む」としている。しかし到着基準 (`AudioPlayoutBasis::Arrival`) は、
  `AudioPlayoutScheduler::schedule` で `enforce_target` が false のとき (予定を決められても
  使わなかった音) と、`AudioPlayoutScheduler::rebase_by_arrival` で目標から離れすぎた音を
  到着基準へ並べ直したとき (時間軸は予定を決めている) にも選ばれる。
- `src/playout/timing.rs` の `PendingSound` と `AudioPlayoutTimingStats.pending` の doc は
  「予約したが、まだ鳴り始めていない音の区間」としている。しかし `pending` は
  `AudioPlayoutTimingStats::prune_pending` が「まだ鳴り終わっていない (`end_us > now_us`)」
  区間として保持し、`AudioPlayoutTimingStats::record_stopped` が鳴り始めていても終わって
  いない音の残りの長さを数える。
- `src/playout/feedback.rs` の `AudioDelayFeedback.observed` の doc は「実際に鳴った結果を
  1 つでも観測したか」としている。しかし `PlayoutTimeline::observe_audio_playout` は常に
  `AudioDelayFeedback::update` を呼び、moq-sub は鳴らさなかった音
  (`AudioPlayoutAssembly::record_miss`) と鳴らさずに止めた分
  (`AudioPlayoutAssembly::record_stopped`) からも `feed_feedback` を呼ぶため、鳴っていない
  音の通知でも `true` になる。
- `src/playout/timeline.rs` の `TrackBreakdown.jitter_delay_us` の doc は「自分の揺らぎから
  求めた値」としている。しかし値は音声の表示の遅れ (`own_delay_us`) であり、
  `AudioDelayFeedback::target_delay_us` が返す「揺らぎから求めた値と、閉ループが決めた目標の
  大きい方を適用の上限で切った値」である。
- `src/playout/timeline.rs` の `DelayBreakdown.base_drift_limit_us` の doc は「基準の差の
  動きの幅」としている。値そのものは `TIMELINE_BASE_DRIFT_US` であり、判定
  (`update_base_difference_drift`) が見るのは水準からの隔たり (`deviation_us`) の幅である。
  同じ並びにある `base_drift_us_per_second` は動きの速さであり、次元が違う。
- moqt-js は `AudioReceiveCatchUp` の設定オブジェクトを削除し、閾値が実測で決めた値であり
  購読ごとに変えないことを doc に書いている。moqt-rs の `AudioReceiveCatchUp` は元から設定を
  受け取らないが、その理由は書かれていない。

## 設計方針

- 対象は doc のみとし、コードの挙動は一切変えない。
- doc は実装を読んで確かめた内容にし、読む側が「どう読めばよいか」まで書く (誤った読み方を
  残さない)。
- `arrival_planned_frames` は「時間軸が予定を決められているかは `slack_us` と `lateness_us`
  が None かで読む」に改める。`AudioPlayoutTimingStats::record_play` は予定のある音だけ余裕と
  遅れを記録するため、予定を決められた音が 1 つも無ければ両方 None になる。
- `PendingSound` は「まだ鳴り終わっていない音の区間」に改める。
- `observed` は doc を「閉ループへ観測が届いたか」に改め、moqt-js と同じく名前も
  `received_observation` に変える。private フィールドであり参照は `AudioDelayFeedback` の
  中だけであるため影響は小さい。「実際に鳴った結果を観測したか」と読める名前を残すと、doc を
  直しても同じ誤解を招くためである。
- `jitter_delay_us` は「揺らぎから求めた値と閉ループが決めた目標の大きい方を適用の上限で
  切った値」に改める。観測が届く前の扱いも書く。
- `base_drift_limit_us` は「水準からの隔たりの幅」であり `base_drift_us_per_second` とは
  次元が違うことを明確にする。
- `AudioReceiveCatchUp` には、閾値が実測で決めた値であり購読ごとに変えないため設定を
  受け取らないことを足す (既存の記述と重複しない範囲で)。

## 完了条件

- 上記の doc が実装と一致していること (該当コードを読んで確認し、根拠を報告すること)。
- コードの挙動を一切変えないこと (doc のみ。名前の変更を含める場合も振る舞いを変えないこと)。
- `make test` / `make clippy` / `make fmt` / `make pbt` が通ること。
- ソースコードに issue 番号や issue への言及を書かないこと。

## 解決方法

- `src/playout/timing.rs` の `arrival_planned_frames` の doc を、到着基準が選ばれる経路
  (予定が無い、`enforce_target` が false、目標から離れすぎて並べ直した) を挙げ、
  「時間軸が予定を決められているかは `slack_us` と `lateness_us` が None かで読む」に改めた
- `src/playout/timing.rs` の `PendingSound` と `pending` の doc を「まだ鳴り終わっていない
  音の区間」に改め、`record_stopped` が残りの長さを数えることを書いた
- `src/playout/feedback.rs` の `observed` を `received_observation` に改名し、doc を「閉ループ
  へ観測が 1 つでも届いたか」に改めた。捨てた音と、鳴り終わっていない音を止めた通知でも
  `true` になることを書いた。`target_delay_us` の doc も、観測が届く前は揺らぎから求めた値を
  上限 (100 ms) で切って返すことを明記した
- `src/playout/timeline.rs` の `TrackBreakdown.jitter_delay_us` の doc を、揺らぎから求めた値
  と閉ループが決めた目標の大きい方を適用の上限で切った値に改めた。観測が届く前は揺らぎから
  求めた値だけを同じ上限で切る
- `src/playout/timeline.rs` の `DelayBreakdown.base_drift_limit_us` の doc を、水準からの
  隔たりの幅であり `base_drift_us_per_second` とは次元が違うこと、判定が使うのは隔たりの幅で
  動きの速さは計器として出すだけであることを書いた
- `src/playout/catch_up.rs` の `AudioReceiveCatchUp` の doc に、閾値がこのモジュールの定数で
  あり実測で決めた値であるため設定を受け取らないことを足した
