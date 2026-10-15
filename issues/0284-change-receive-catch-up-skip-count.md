# 受信側の追いつきで飛んだ音を数える位置を moqt-js に合わせ、棄却した案を記録する

- Created: 2026-10-16
- Completed: {YYYY-MM-DD}
- Branch: feature/change-receive-catch-up-skip-count
- Polished: {YYYY-MM-DD}

## 目的

moqt-js は 2026-10-15 の変更 (`origin/develop` の #492) で、受信側の追いつき
(`src/audioReceiveCatchUp.ts` の `measureSkip`) の判定順を変え、「追いつきの最中だけ数える」
判定を「TIMESTAMP の跳びかどうか」の判定の後ろへ移した。跳びの判定と追いつき中の判定の間に
副作用が無いため、観測される値は変わらない。

同時に、飛んだ音の計測について実装して測った 2 つの案を棄却した理由が記録された。これは
moqt-rs の `src/playout/catch_up.rs` にも同じ形で残しておく価値がある。同じ案を再試行して
同じ実測をやり直すのを防ぐためである。moqt-rs の `measure_skip` は判定順が moqt-js と
異なっており、記録も無い。

## 現状

- `src/playout/catch_up.rs` の `measure_skip` は、`last_timestamp_us` を更新した直後に
  追いつき中かどうかを判定し、そのあとで TIMESTAMP の跳びを判定する。moqt-js は逆順で
  ある (跳びの判定が先、追いつき中の判定が後)。
- 実測で棄却した案 (追いつきを始め直す間隔を、効かないまま繰り返すときだけ 2 倍にする案と、
  穴 (TIMESTAMP の跳び) の直後は始めない案) の記録が無い。実測はパケロス 5 % の 40 秒で
  どちらも作り直しの回数が変わらず (30〜31 回)、穴の直後を 60 秒間始めないようにすると
  作り直しは 1 回になるが、遅れが 1458 ms まで育ち、鳴らした長さも 30 秒で 27835 ms へ
  落ちた、というものである (moqt-js、実リレー)。
- 「穴の直後は始めない案」が効かないことは、作り直しが損失下でも live を保つ役目を
  果たしていることの根拠にもなっている。

## 設計方針

- `measure_skip` の判定順を moqt-js と同じにする (跳びの判定の後ろで追いつき中かどうかを
  見る)。観測される値は変えない。
- 棄却した 2 案とその実測、それでも作り直しをやめない理由を `measure_skip` または
  `AudioReceiveCatchUp::observe` の rustdoc に書く。出典が moqt-js の実測であることを
  明記する (moqt-rs では実リレーの再測が必要になった時点で測り直す)。
- 判定順の変更で値が変わらないことをテストで固定する (追いつき中でない跳びは数えない、
  追いつき中の跳びは数える)。

## 完了条件

- `measure_skip` の判定順が moqt-js と同じになること。
- 追いつき中でない跳びが数えられないこと、追いつき中の跳びが数えられることが
  `tests/test_playout/catch_up.rs` で固定されていること。
- 棄却した 2 案とその実測が rustdoc に書かれていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
- ソースコードに issue 番号や issue への言及を書かないこと。
