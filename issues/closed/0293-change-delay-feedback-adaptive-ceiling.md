# 閉ループの内部目標を適用上限に収め、圧縮が続くときだけ上限を広げる

- Created: 2026-10-16
- Completed: 2026-10-16
- Branch: feature/change-delay-feedback-adaptive-ceiling
- Polished: {YYYY-MM-DD}

## 目的

moqt-js のコミット 39d3fb7a (#496「playout のノイズの原因を修正し、目標遅延を経路に適応
させる」) のうち、`src/playout/audioDelayFeedback.ts` と
`src/playout/audioPlayoutTimingStats.ts` の変更を moqt-rs の `src/playout/feedback.rs` と
`src/playout/timing.rs` へ移植する。

移植する内容は 2 つある。

1. 閉ループの内部の目標を、実際に適用する上限でクランプする (moqt-js の欠陥 5)。
   moqt-js の実測 (2026-10-16、実リレー) では、内部の目標 (`feedbackTargetMs`) が最大
   239.9 ms まで巻き上がった一方、適用は 100 ms で切られていた。超えた分は決して適用され
   ないまま残り、遅れが収まってから減らし切る (毎秒 10 ms) までの 10 秒以上、適用が
   100 ms のままになる。内部の目標を 100 ms で切ると、実測で最大 100 ms になった
2. 圧縮 (時間圧縮 = 粗さ) が続くときだけ、目標遅延の上限を 160 ms へ広げる適応制御。
   moqt-js の実測 (実リレー、60 秒) では揺らぎだけの目標が 100〜340 ms に育ち、上限
   100 ms が常時拘束されて時間圧縮が 60 秒で 550〜1000 ms 続いていた。上限を広げると
   圧縮は減るが、遅延の代償を常に払うことになる。圧縮は「経路が 100 ms では足りていない」
   証拠であるため、そのときだけ払う。A/B の実測 (3 腕 × 3 回、各 60 秒) では、適応は
   静的 100 ms より圧縮が 43 % 少なく (350 [324〜535] ms 対 619 [593〜674] ms)、600 秒
   1 セッションでは圧縮 3791 ms (0.63 %)、適用が 100 ms を超えたのは 599 秒中 118 秒
   (20 %)、内部の目標の最大は 141.2 ms だった

## 現状

- `src/playout/feedback.rs` の `AUDIO_DELAY_FEEDBACK_MAX_TARGET_US` (100 ms) は
  `target_delay_us` の戻り値にだけ掛かる。内部の目標 (`feedback_target_us`) は
  `AUDIO_DELAY_FEEDBACK_MAX_US` (300 ms) または明示設定 (`set_ceiling_us`) まで上がり得る
- 圧縮を追跡する状態が無い。`AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US` (160 ms) に相当する
  定数も、上げる条件 (圧縮 5 ms/秒 が 2 秒) も戻す条件 (圧縮 0 と揺らぎの目標 100 ms 以下が
  15 秒) も無い
- `src/playout/timing.rs` の `AudioDelayFeedbackObservation` は、遅れ・鳴るまでの時間・余裕・
  累積の捨てだけを持ち、圧縮の増分と揺らぎだけから求めた目標遅延を持たない。moqt-js は
  前者を購読側 (`AudioPlayoutSession`) が、後者を時間軸 (`PlayoutTimeline`) が足している
- 計器 (`AudioDelayFeedbackSnapshot`) に、適用する上限と圧縮の速さが出ていない

## 設計方針

- `AudioDelayFeedbackObservation` に 2 つ足す。どちらも計器 (`AudioPlayoutTimingStats`) では
  分からない値であり、既定は 0 とする
  - `compression_us`: 前の観測から増えた圧縮の長さ (増分、マイクロ秒)。購読側
    (`examples/moq-sub`) が `AudioPlayoutScheduler::compressed_us` の差を渡す
  - `jitter_target_us`: 揺らぎだけから求めた目標遅延 (マイクロ秒)。`PlayoutTimeline` が
    `state.delay.target_delay_ms()` から埋める
- `feedback.rs` に追加する定数 (moqt-js と同名・同値)
  - `AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US` = 160_000
  - `AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND` = 5_000
  - `AUDIO_DELAY_FEEDBACK_COMPRESSION_CONFIRM_US` = 2_000_000
  - `AUDIO_DELAY_FEEDBACK_ARTIFACT_RELEASE_US` = 15_000_000
- 適用する上限 (`applied_ceiling_us`) は、広げていないとき 100 ms、広げているとき
  `max(100 ms, min(160 ms, 明示設定 ?? 160 ms))` とする。利用者が宣言した予算を超えず、
  既定の 100 ms を下回らない (揺らぎだけの目標に明示設定を掛けない既存の規則を変えない)
- 内部の目標 (`upper_us`) は `min(300 ms, 明示設定, 適用する上限)` にする。
  `target_delay_us` も同じ適用する上限で切る
- 圧縮の速さは、制御 (毎秒 1 回) までの増分を足し合わせ、制御からの時間で割って求める。
  最初の制御 (前回が無い) と時間が 0 のときは 0 とする
- 目標を動かす前に上限の判断を行う (同じ観測で新しい上限まで上げてよい)。上限を戻しても
  内部の目標は下げない (既存の減らす規則に任せる。戻した上限で切られるため、実際に使う値は
  100 ms 以下になる)
- 計器 (`AudioDelayFeedbackSnapshot`) に `applied_ceiling_us` と
  `compression_us_per_second` を足し、example のログにも出す

## 完了条件

- 内部の目標が適用する上限でクランプされること (`target_delay_us` の戻り値と snapshot の
  整合) がテストで固定されていること
- 適応の上限 (100 ms ⇄ 160 ms) が実装され、上げる条件 (圧縮 5 ms/秒 が 2 秒) と戻す条件
  (jitter 目標 100 ms 以下かつ圧縮 0 が 15 秒) と明示設定の優先が
  `tests/test_playout/feedback.rs` で固定されていること
- 計器が新しい値を出し、`tests/test_playout/timing.rs` と
  `pbt/tests/prop_playout/feedback.rs` が追随していること
- 既存の境界 (下限 50 ms、適用の上限 100 ms、jitter 目標は明示設定の影響を受けない、観測が
  無いときは閉ループを使わない) を壊していないこと
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` / `cargo test -p pbt` が通ること
- ソースコードに issue 番号を書かないこと。コメントは日本語、ログは英語、テストのログは
  日本語であること

## 解決方法

- `src/playout/feedback.rs`
  - `AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US` (160 ms)、
    `AUDIO_DELAY_FEEDBACK_COMPRESSION_MIN_US_PER_SECOND` (5 ms/秒)、
    `AUDIO_DELAY_FEEDBACK_COMPRESSION_CONFIRM_US` (2 秒)、
    `AUDIO_DELAY_FEEDBACK_ARTIFACT_RELEASE_US` (15 秒) を追加した。値と根拠
    (moqt-js の実測) は doc コメントに書いた
  - `AudioDelayFeedback::applied_ceiling_us` (公開) を追加した。広げていないときは
    100 ms、広げているときは `max(100 ms, min(160 ms, 明示設定 ?? 160 ms))` である
  - `upper_us` を `min(300 ms, 明示設定, 適用する上限)` にし、`target_delay_us` の最後の
    `min` を `applied_ceiling_us` に置き換えた。内部の目標が適用されない値へ育たなくなり、
    遅れが収まってからの復帰が減らす速さ (毎秒 10 ms) に縛られなくなる
  - `widened_ceiling`、`compression_streak_us`、`quiet_streak_us`、
    `compression_since_update_us`、`compression_us_per_second` を持ち、
    `update_artifact_ceiling` で上げる / 戻すを判断する。判断は目標を動かす前に行う
  - `compression_us` は制御までの間ぶんを足し合わせ、負の値は 0 とみなす。速さは
    `compression_us * 1_000_000 / span_us` (先に掛けてから割る) とした
  - `AudioDelayFeedbackSnapshot` に `applied_ceiling_us` と `compression_us_per_second` を
    足した。`reset` は広げた状態と連続時間も初期状態へ戻す
- `src/playout/timing.rs`: `AudioDelayFeedbackObservation` に `compression_us` (増分) と
  `jitter_target_us` を足した。`AudioPlayoutTimingStats::audio_delay_feedback` はどちらも 0 で
  返し、購読側と時間軸が埋めることを doc コメントに書いた
- `src/playout/timeline.rs`: `observe_audio_playout` が
  `state.delay.target_delay_ms() * 1_000` を `jitter_target_us` として観測へ足すようにした
  (moqt-js の timeline が `jitterTargetMs` を足す役割と同じ)
- `examples/moq-sub/src/main.rs`: `AudioPlayoutAssembly` に `last_compressed_us` を持ち、
  `feed_feedback` が `scheduler.compressed_us()` の増分 (0 で止める) を
  `compression_us` として渡すようにした。`log_fields` に `feedback_ceiling` と
  `feedback_compression` を足した
- テスト: `tests/test_playout/feedback.rs` を新しい契約へ追随させた。適用する上限の定数、
  閾値ちょうどでは広げないこと、2 秒続いたら広げること、制御までの増分を足し合わせること、
  15 秒続いたときだけ戻すこと (揺らぎの目標が 100 ms を超えている間は戻さない)、明示設定が
  広げる分に掛かることを固定した。内部の目標を増やす観測は、収束で下げるか上限を広げるか
  しないと余地が無いため、`widen_ceiling` ヘルパーで広げてから観測する形へ直した
- テスト: `tests/test_playout/timeline.rs` の観測のリテラル 2 箇所へフィールドを足した。
  `delay_breakdown_reports_the_closed_loop_state` は「既に適用する上限にあるため増分は
  出ない」形へ直し、`reset_track_keeps_the_closed_loop_target` は収束で下げた目標 (90 ms) が
  残ることを見る形へ直した (適用する上限が初期値と同じ 100 ms であるため、遅れでは増やせ
  ない)
- テスト: `examples/moq-sub/src/main.rs` の `playout_assembly_records_the_backlog_miss` は、
  並べすぎの捨てが閉ループへ届くこと (理由が `Backlog`) を残しつつ、内部の目標が適用する
  上限 (100 ms) に収まり、この観測では値が動かないことを見る形へ直した
- PBT: `pbt/tests/prop_playout/feedback.rs` に、適用する上限を閉ループと同じ規則で写し取る
  影の状態 (`advance_ceiling`) を足し、圧縮の増分と揺らぎの目標を任意に与えて、適用する
  上限が規則と一致すること、内部の目標と実際に使う値がそれを超えないことを検証した。
  圧縮が続く区間と静かな区間を作る専用のプロパティテストも足し、広げる / 戻すの両方の
  遷移に到達することを確かめた
- PBT: 専用のプロパティテストの区間の長さは、定数から導いて必ず条件を満たすようにした
  (広げるには 3 回以上の制御、戻すには乱れる回の後ろに 15 秒ぶんの制御)。最初の実装は
  区間の長さを運任せにしていたため、シードによって「戻す」遷移に到達せずに到達ゲートが
  落ちていた。到達ゲートは緩めず、サンプラー側で到達を保証する形に直した。
  `PBT_SEED` を 1 / 3 / 5 / 7 / 11 / 12345 / 0x18df0f3f45754f78 / 0xdeadbeef /
  0xffffffffffffffff / 0 で確認した
- 検証: `cargo test --workspace` (全テスト成功)、
  `cargo clippy --workspace --all-targets -- -D warnings`、
  `cargo fmt --all -- --check`、`cargo test -p pbt` が成功した
- moqt-js と意図的に変えた点: 圧縮の増分は観測の必須フィールド (0 が既定) とし、揺らぎだけの
  目標は時間軸が埋める形にした (TypeScript の `Omit` が使えないため)。計器の値は
  `AudioDelayFeedbackSnapshot` に足した (moqt-js と同じ)
- 実リレーでの確認 (合成入力、音声のみ、300 秒): 静かな経路では閉ループの目標が下限
  (50 ms) に落ち着き (`feedback_reason=Settled`)、`feedback_ceiling` は 100 ms のまま、
  `feedback_compression` は 0 ms/秒のままだった。上限を広げる経路 (圧縮が 2 秒続く場合) は
  この経路では起きないため、そちらは依然として実測で確かめていない
