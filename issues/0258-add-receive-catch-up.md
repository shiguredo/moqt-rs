# 受信側の音声が遅れたら復号器を作り直して追いつく

- Created: 2026-10-14
- Completed: 2026-10-14
- Branch: feature/add-receive-catch-up
- Polished: {YYYY-MM-DD}

## 目的

moqt-js の `src/audioReceiveCatchUp.ts` は 2026-10-10 に、受信側の音声が遅れて固定されたら
復号器を作り直して live へ戻す仕組みを入れた。

受信側の音声は、復号器の中に未処理の音が溜まると、溜まった分だけ復号の出力が送られた
TIMESTAMP から遅れたまま固定される。溜まった分は実時間と同じ速さでしか出てこないため、
待っても戻らない。鳴らす音を捨てても、捨てた分は復号器から出てきた後の音であり、中の分は
減らない。実測 (実リレーの E2E の失敗) では、音声の基準の遅れが 116.4 ms から 256.2 ms へ
段差で上がり、そのまま戻らなかった (映像は 111.5〜112.2 ms で安定、配信側の計器は健全)。

moqt-rs の `examples/moq-sub` には、この遅れを見て復号器を作り直す仕組みが無い。

## 現状

- 音声の復号は `examples/moq-sub/src/decoder.rs` の `AudioDecoder` が行い、
  `examples/moq-sub/src/pipeline.rs` が `Arc<Mutex<AudioDecoder>>` を共有して
  ストリーム経路と datagram 経路の両方から使う
- `AudioDecoder` にも `OpusDecoder` ([`examples/moq-sub/src/decoder/opus.rs`]) にも、
  内部の状態を捨てて作り直す手段が無い
- 復号の出力が送られた TIMESTAMP からどれだけ遅れているかは、どこにも記録されていない。
  遅れが固定されても、鳴らす音を捨てる (`AudioMissReason::CatchUp`) だけで、復号器の中の
  溜まりは減らない
- `src/playout/timing.rs` の `AudioMissReason::CatchUp` は「relay の cache から追いつく
  途中で鳴らさなかった」という別の理由であり、復号器の作り直しとは関係が無い

## 設計方針

- 新規モジュール `src/playout/catch_up.rs` に、判定だけを行う純粋な状態機械
  (`AudioReceiveCatchUp`) を置く。時刻と値は呼び出し側が引数で渡し、復号器には触れない
  - 入力: `now_ms` (呼び出し側の単調な時計)、`lag_ms` (復号の出力を受け取った壁時計 −
    送られた TIMESTAMP)、`duration_ms`、`timestamp_us`
  - 出力: `AudioReceiveCatchUpDecision { rebuild_decoder: bool }`
- 床は直近 [`AUDIO_RECEIVE_CATCH_UP_FLOOR_WINDOW_MS`] (5 秒) の最小の遅れにする。経路の
  遅延はここへ吸収され、その上での増加だけを見る
- 上限は `max(60 ms, 床 + 40 ms)`。遅れが上限以下なら追いつきを終える
  - 60 ms は 3 パケット分 (Opus の 20 ms)、40 ms は 2 パケット分である
- 始める条件は 2 つ。どちらもクールダウン (
  [`AUDIO_RECEIVE_CATCH_UP_COOLDOWN_MS`] の 1 秒) の間は始めない
  - 床 + [`AUDIO_RECEIVE_CATCH_UP_JUMP_MS`] (100 ms) を超えたら、持続を待たずに始める。
    A/V の同期では合わせられないほどのずれであり、放置すると遅れたままになる
  - 超えない場合は [`AUDIO_RECEIVE_CATCH_UP_CONFIRM_MS`] (100 ms) 続いて初めて始める。
    一過性の遅れで作り直すと、その分の音が飛ぶ
- 観測が [`AUDIO_RECEIVE_CATCH_UP_FLOOR_WINDOW_MS`] より長く途切れたら、途切れる前の床を
  [`AUDIO_RECEIVE_CATCH_UP_FLOOR_WINDOW_MS`] の間だけ保つ。途切れた後で現れた遅れが床に
  なると、追いつきが始まらなくなる
- 追いつきの間だけ、timestamp の跳び ([`AUDIO_RECEIVE_CATCH_UP_SKIP_MIN_MS`] より大きい
  差) から飛んだ音の数と長さを数える。作り直しで飛んだ分を計器に残す
- `examples/moq-sub` の復号の経路へ配線する。`AudioDecoder` に作り直す手段
  (`reset`) を足し、判定が真を返したら呼ぶ。遅れは復号の出力の TIMESTAMP と、そのときに
  読んだ受信側の壁時計から求める
- 後方互換は不要 (破壊的変更を恐れない)

## 完了条件

- 健全な状態 (遅れが上限の中) では作り直しを求めないことがテストで固定されていること
- 床 + 100 ms を超えたら持続を待たずに求めることがテストで固定されていること
- 床 + 100 ms 以下でも 100 ms 続いたら求めることがテストで固定されていること
- 上限へ戻ったら求めなくなることがテストで固定されていること
- クールダウン (1 秒) の間は遅れだけを根拠に始めないことがテストで固定されていること
- 観測が途切れた後も途切れる前の床を保つことがテストで固定されていること
- `examples/moq-sub` が判定を使い、復号器を作り直すこと
- `pbt/tests/prop_playout/catch_up.rs` が「遅れが戻れば必ず解除される」ことを検証して
  いること
- `make pbt` / `make test` / `make clippy` / `make fmt` が通ること
- ソースコードに issue 番号や issue への言及を書かないこと

## 解決方法

- 新規モジュール `src/playout/catch_up.rs` に `AudioReceiveCatchUp` を追加した。時刻と値は
  引数で受ける純粋な状態機械であり、復号器には触れない
  - 入力 `AudioReceiveCatchUpObservation` (now_ms / lag_ms / duration_ms / timestamp_us)、
    出力 `AudioReceiveCatchUpDecision` (`rebuild_decoder`)
  - 定数は床の窓 5 秒、下限 60 ms、増加 40 ms、跳び 100 ms、持続 100 ms、クールダウン
    1 秒、数える跳びの下限 1 ms。跳びの 100 ms は `TIMELINE_MAX_COMPENSATED_DIFFERENCE_US`
    と同じ値であり、A/V の同期では合わせられない遅れであることを doc に書いた
  - 床は直近 5 秒の最小値。観測が 5 秒より長く途切れたら、途切れる前の床を 5 秒保つ
  - 上限は `max(60 ms, 床 + 40 ms)`。上限の中へ戻ったら追いつきを終える
  - 始める条件は 2 つ。床 + 100 ms を超えたら持続を待たずに始め、超えないときは 100 ms
    続いて初めて始める。どちらもクールダウン (1 秒) の中では始めない
  - 追いつきの間だけ、TIMESTAMP の跳びから飛んだ音の数と長さを数える
- `src/playout.rs` にモジュールを登録した
- `examples/moq-sub/src/decoder.rs` の `AudioDecoder` に `reset` を追加し、
  `decoder/opus.rs` の `OpusDecoder::reset` (Opus のデコーダを作り直す) と
  `decoder/aac.rs` の `AacDecoder::reset` (専用スレッドごと作り直す) を実装した
- `examples/moq-sub/src/pipeline.rs` の `AudioDelivery` に `catch_up` を足し、
  `deliver` で復号の出力の TIMESTAMP と読んだ壁時計から遅れを求めて観測するようにした。
  判定が真なら `AudioDecoder::reset` を呼ぶ。壁時計の TIMESTAMP を持たない音
  (Timescale 付き) では観測しない。状態はストリーム経路と datagram 経路で共有する
- `examples/moq-sub/src/lib.rs` に `wall_clock_us` を移し、`main.rs` と `pipeline.rs` の
  両方から使えるようにした
- テスト: `tests/test_playout/catch_up.rs` に 9 件 (健全時・跳び・持続・解除・
  クールダウン・途切れた後の床・床が大きい環境・跳びの数え方・リセット)追加した
- PBT: `pbt/tests/prop_playout/catch_up.rs` に 2 件追加した。遅れが上限の中へ戻れば必ず
  追いつきが終わること、始めた間隔がクールダウン以上であること、跳びが無ければ飛んだ音を
  数えないこと、跳びの合計が観測と一致することを検証する
- 検証: `cargo fmt --all --check` / `cargo test --workspace` / `cargo test -p pbt` /
  `cargo clippy --workspace --all-targets -- -D warnings` が成功した
- 検証: `PBT_SEED` を 1 / 3 / 5 / 7 に変えても property が成功する
- 備考: 実リレーを介した遅れの再現 (メインスレッドを止める負荷) は E2E テストの領分で
  あり、この issue では判定と配線までを行った
