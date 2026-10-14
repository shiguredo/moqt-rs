# 音声の時間圧縮に不感帯と削る長さの上限を入れる

- Created: 2026-10-14
- Completed: 2026-10-14
- Branch: feature/change-audio-stretch-deadband
- Polished: {YYYY-MM-DD}

## 目的

moqt-js の `src/audioPlayout.ts` は 2026-10-14 に、時間圧縮を始める遅れの不感帯
(`AUDIO_PLAYOUT_STRETCH_MIN_SECONDS` の 20 ms) と、削ってよい長さの上限
(`src/audioTimeStretch.ts` の `compressSamples` の `maxLengthChangeSamples`) を入れた。

時間圧縮は波形のピッチ周期を 1 つ削って繋ぐ操作であり、削る長さは周期 (2.5〜15 ms) で
決まって要求では選べない。遅れが数 ms しかないときに動かすと、削った分だけ「早すぎる」側へ
振れ、次の音でまた遅れて詰める往復になる。往復のたびに周期が 1 つ消えるため、声では
粗さ・ざらつきとして聞こえる。

moqt-js の実測 (2026-10-14、実リレー経由の 60 秒、16 kHz モノラル) では、遅れが約
2.5 ms で安定しているのに 60 秒の再生で 2727 ms を時間圧縮していた。不感帯を 5 ms に
すると 1691 ms、10 ms で 1911 ms、20 ms で 722〜780 ms まで減り、鳴り始めの遅れは 98 ms
から 113〜114 ms へ僅かに伸びるだけであった。不感帯の 20 ms は、時計の対応付けが意図して
無視する幅 (`AUDIO_CLOCK_DEADBAND_MS` の 30 ms) の内側で吸収できる遅れである。

## 現状

- `src/playout/scheduler.rs` の `AudioPlayoutScheduler::schedule` は
  `compress_us` を `lateness_us.min(duration_us / 2)` で決める。遅れが 1 ms でも毎回
  周期 1 つ分を削る要求を出す
- `src/playout/stretch.rs` の `compress` は求めた周期 1 つ分を必ず削る。周期が遅れより
  長いときも削るため、削りすぎた分が「早すぎる」側への振れになる
- `src/playout/scheduler.rs` の `confirm_stretch` は、要求より長く詰められた分を
  「前の音が手前で終わった」として `last_end_us` から引く。削りすぎの往復はこの経路で
  次の音の開始を前へ動かし、隙間 (`gap_us`) を作る
- 不感帯を入れると、詰めずに残した遅れが閉ループ (`src/playout/feedback.rs`) へ遅れとして
  渡る。moqt-js の実測では、不感帯を入れたまま不感帯の分を引かずに学習へ渡すと、目標遅延が
  60 秒で 289 ms、実際の遅延が 296 ms まで上がった。不感帯の分を引くと目標 80 ms /
  実測 100 ms で安定した
- `examples/moq-sub` の `feed_feedback` は `AudioPlayoutTimingStats::audio_delay_feedback`
  が返した観測をそのまま `PlayoutTimeline::observe_audio_playout` へ渡す

## 設計方針

moqt-js の最新 (2026-10-14) の判定を移植する。

- `src/playout/scheduler.rs` に `AUDIO_PLAYOUT_STRETCH_MIN_US` (20 ms) を追加する。
  `lateness_us` がこれ未満のときは `compress_us` を 0 にする。このとき
  `confirm_stretch` は要求が無いものとして扱うため、`last_end_us` は詰めない長さになる
- `src/playout/stretch.rs` の `compress` に「削ってよい長さの上限 (サンプル数)」の引数を
  追加する。求めた周期が上限を超えるときは操作せず 0 を返す。moqt-js 側の呼び出しは
  `Math.round(compressSeconds * sampleRate)` を渡すため、丸めはサンプル数への
  四捨五入にする (既存の `samples_to_us` と同じ向き)
- 閉ループへ渡す遅れの `p50` から不感帯の分を引く。`p95` と `max` は揺らぎの裾であり、
  引くと「たまに大きく遅れた」事実まで消えるため引かない
- 引く処理は遅れの観測を作る側 (`examples/moq-sub`) ではなく、`playout` の中で完結させる。
  `AudioPlayoutScheduler` の不感帯と閉ループの観測は同じ規則を共有するためである
- 後方互換は不要 (破壊的変更を恐れない)

## 完了条件

- 遅れが `AUDIO_PLAYOUT_STRETCH_MIN_US` 未満のときは `compress_us` が 0 になり、
  ちょうどのときは詰めることがテストで固定されていること
- 周期が上限を超えるときは `compress` が波形も長さも変えないことがテストで固定されて
  いること
- 閉ループが不感帯の分を遅れとして見ない (不感帯の中の遅れでは目標を増やさない) ことが
  テストで固定されていること
- `examples/moq-sub` が新しい API でビルドでき、詰めてよい長さの上限を渡していること
- `pbt/tests/prop_playout/` の `scheduler` と `stretch` が新しい API に追随し、
  不変条件を検証していること
- `make pbt` / `make test` / `make clippy` / `make fmt` が通ること
- ソースコードに issue 番号や issue への言及を書かないこと

## 解決方法

- `src/playout/scheduler.rs` に `AUDIO_PLAYOUT_STRETCH_MIN_US` (20 ms) を追加し、
  `AudioPlayoutScheduler::schedule` の `compress_us` を、遅れが不感帯に届いたときだけ
  `min(lateness_us, duration_us / 2)` にするよう変えた
- `src/playout/stretch.rs` の `compress` に `max_length_change_samples` (削ってよい
  サンプル数の上限) を追加した。求めた周期が上限を超えるときは操作せず 0 を返す。上限を
  外すときは `usize::MAX` を渡すことを doc に書いた
- `src/playout/feedback.rs` の `AudioDelayFeedback::update` で、観測の遅れの p50 から
  `AUDIO_PLAYOUT_STRETCH_MIN_US` を引くようにした。p95 と max は引かない (引くと
  「たまに大きく遅れた」事実まで消えるため)。引いた後が
  `AudioDelayFeedbackSnapshot` の遅れになる (閉ループが判断に使った値を見せる)
- `examples/moq-sub` の `apply_audio_playout` が `us_to_samples` (四捨五入) で上限を
  求めて `stretch::compress` へ渡すようにした。`us_to_samples` と `us_to_samples_ceil`
  の使い分けを doc に書いた
- テスト: `tests/test_playout/scheduler.rs` に不感帯の境界 2 件 (1 µs 手前では詰めない、
  ちょうどでは詰める) を追加した。詰めの機構を確かめていた既存 5 件は、遅れが不感帯に
  届く入力へ直した (要求が 10 ms 以下になるところを 20 ms 以上にした)
- テスト: `tests/test_playout/feedback.rs` に「不感帯の中の遅れでは目標を動かさない」
  を追加し、`snapshot` の遅れが引いた後になることを固定した
- テスト: `tests/test_playout/stretch.rs` の既存 17 件の呼び出しに上限 (`usize::MAX`) を
  渡した。上限そのものの契約は `max_length_change_samples` の doc と PBT で守る
- テスト: `tests/test_playout/timeline.rs` と `examples/moq-sub` の計器の期待値を、
  引いた後の値に更新した
- PBT: `pbt/tests/prop_playout/scheduler.rs` に「詰めるのは遅れが不感帯に届いたときだけ」
  を追加した。`pbt/tests/prop_playout/stretch.rs` に上限の sampler と「上限を超えて
  削らない」を追加した。`pbt/tests/prop_playout/feedback.rs` は判断と計器を不感帯を
  引いた値で検証するようにし、不感帯と許容の境界を生成器へ含めた
- 検証: `cargo fmt --all --check` / `cargo test --workspace` / `cargo test -p pbt` /
  `cargo clippy --workspace --all-targets -- -D warnings` が成功した
- 検証: `PBT_SEED` を 1 / 3 / 5 / 7 に変えても property が成功する
- 備考: `examples/tokio-moq` の rustdoc リンク切れ (`resolve_socket_addrs` が非公開の
  `RESOLVE_TIMEOUT` へリンクする) は変更前からある別の問題であり、この issue では
  触っていない
