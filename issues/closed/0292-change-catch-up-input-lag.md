# 受信の追いつきを「復号が足している遅れ」で判定する

- Created: 2026-10-16
- Completed: 2026-10-16
- Branch: feature/change-catch-up-input-lag
- Polished: {YYYY-MM-DD}

## 目的

moqt-js の変更 (受信の追いつきを復号が足した遅れで判定する) を moqt-rs へ移植する。受信側の
追いつき (`src/playout/catch_up.rs`) は、いま復号の出力の遅れ (`lag_ms`) だけを見て復号器の
作り直しを決めている。出力の遅れだけでは、次の 2 つを区別できない。

- 経路が恒久的に悪化した: Object の到着も復号の出力も同じだけ遅れる。復号器の中に溜まった
  分は増えていないため、作り直しても遅れは戻らない。ここで作り直すと音を飛ばすだけになる
- 受信側が溜めたまま戻らない: Object は到着しているのに、復号器の中に溜まった分だけ復号の
  出力が遅れている。溜まった音は実時間でしか出てこないため、作り直して捨てない限り戻らない

判定を「復号が足している遅れ」= 復号の出力の遅れ − 受信の入口で測った入力の遅れ (Object の
到着の壁時計 − LOC TIMESTAMP) に変える。moqt-js が実装を駆動して測った再現では、8〜20 秒を
250 ms に固定し、20 秒以降も復号器に溜まったままにすると、13 秒で床が 250 ms へ追いつき、
20〜60 秒の 40 秒間は作り直しが 0 回のまま 250 ms で固定された。入力の遅れを引くと、経路が
遅いだけの状態では床が上がらず、受信側が溜めた状態では床が上がる前に作り直しが要求される
(段差の直後に 1 回で 30 ms へ復帰した)。

## 現状

- `src/playout/catch_up.rs` の `AudioReceiveCatchUpObservation` は `now_ms` / `lag_ms` /
  `duration_ms` / `timestamp_us` のみである。`observe` は `lag_ms` をそのまま床の更新と
  上限の判定と開始の判定に使い、`AudioReceiveCatchUpStats` にも入力の遅れが無い。
- `examples/moq-sub/src/pipeline.rs` の `AudioDelivery::deliver` は、復号の出力で
  `lag_ms = (now_us - pts_us) / 1_000` を計算して `observe` している。受信の入口の入力の
  遅れは測っていない。
- moqt-js は `src/playout/decodeInputTimestamps.ts` の `takeDecodeInputEntry` と
  `LastTimestampKind` で、復号へ渡した timestamp から復号の出力を許容付きで引く。WebCodecs の
  `AudioData.timestamp` が入力と完全には一致しない (実測: 100 マイクロ秒) ためである。
  moqt-rs に相当するものは無い (リポジトリ全体で 0 件)。
- 既存の食い違い: `examples/moq-sub/src/pipeline.rs` の `loc_timestamp_us` は Timescale が
  あっても `Some` を返し、`if let Some(pts_us)` でそのまま遅れを測って観測している。一方、
  その直前のコメントは「壁時計の TIMESTAMP を持たない音 (Timescale 付き) では遅れを測れない
  ため観測しない」と書いており、コードと食い違っている。moqt-js は壁時計の TIMESTAMP を
  持つ音でだけ観測する (`audioPlayoutSession.ts`)。

## 設計方針

- `src/playout/catch_up.rs`
  - `AudioReceiveCatchUpObservation` に `input_lag_ms: Option<i64>` を足す。
  - 判定に使う値 `decode_lag_ms` を「出力の遅れ − 入力の遅れ」とする。入力の遅れが測れない
    とき (`None`) は出力の遅れをそのまま使う (入力の遅れを導入する前と同じ挙動)。測れた
    ときに負になった場合は、2 つの遅れを別の時点で測った丸めであるため 0 に寄せる。
  - 床の更新・上限の判定・開始の判定は `decode_lag_ms` で行う。`lag_ms` (生の出力の遅れ)、
    `max_lag_ms`、`measure_skip` (TIMESTAMP の跳びから飛んだ音を数える) は生のまま残す。
  - `AudioReceiveCatchUpStats` に `input_lag_ms` と `decode_lag_ms` を足し、`floor_ms` が
    「復号が足している遅れ」の最小値であること、`max_lag_ms` が生の出力の遅れの最大値で
    あることを rustdoc に書く。
- 呼び出し側 (`examples/moq-sub/src/pipeline.rs`)
  - 復号へ渡す直前の到着の壁時計を測り、`input_lag_ms = (到着 − LOC TIMESTAMP) / 1_000` を
    観測へ渡す。moqt-js が object のコールバック内で同期に測るのと同じ位置である。
  - 壁時計の TIMESTAMP (Timescale が無い、または 0) を持つ音でだけ測る。TIMESTAMP が無い音と
    Timescale がある (メディア時刻である) 音では測れないため `None` を渡す。測れないことを
    理由に観測を止めない (結果は導入前と同じ挙動になる)。
  - 復号の出力と入力の対応は、復号が入力 1 つにつき出力 1 つ (Opus は同期、AAC は専用
    スレッドの完了待ち) であるため、局所の値で運ぶ。moqt-js の `decodeInputTimestamps.ts` に
    相当する対応表は新設しない。将来、内部に複数パケットを溜めて古い PCM を返す復号器を
    足す場合にだけ、timestamp を鍵にした対応表が要る。
- 配置の判断
  - 測定は example に置く。受信の入口 (Object の到着) を知っているのは example だけであり、
    library は「組み立ては example 側」という方針で、時刻と値を引数で受け取る判定だけを持つ。
  - 判定と統計は library (`src/playout/catch_up.rs`) に置く。購読ごとの状態であり、example
    だけに置くと受信経路 (subgroup stream / datagram / FETCH 応答) ごとに重複するためである。
- 既存の食い違い (Timescale ありでも観測している) は、この移植では変えない。Timescale ありの
  観測は `input_lag_ms = None` となり、結果として導入前と同じ判定になる。観測ごと止める変更は
  別途扱う。

## 完了条件

- `src/playout/catch_up.rs` の判定が「復号が足している遅れ」で行われること。
- 入力の遅れが測れないときは、出力の遅れをそのまま使うこと (導入前と同じ挙動)。
- `AudioReceiveCatchUpStats` に `input_lag_ms` と `decode_lag_ms` が入り、`floor_ms` が
  「復号が足している遅れ」の最小値、`max_lag_ms` が生の出力の遅れの最大値であること。
- `examples/moq-sub` が受信の入口で入力の遅れを測り、復号の出力まで運んで観測へ渡すこと。
- `tests/test_playout/catch_up.rs` に、経路だけが遅い場合に作り直さないこと、受信側が
  溜めた場合に作り直すことの境界が固定されていること (`pbt` も追随すること)。
- 既存の追いつきの挙動 (床の窓、飛んだ音の計測、クールダウン) を壊していないこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` / `cargo test -p pbt` が通ること。
- ソースコードに issue 番号や issue への言及を書かないこと。コメントは日本語、ログは英語、
  テストのログは日本語で書くこと。

## 解決方法

- `src/playout/catch_up.rs`
  - `AudioReceiveCatchUpObservation` に `input_lag_ms: Option<i64>` を足した。
  - `observe` で `decode_lag_ms` を求め、床の更新 (`update_floor`)・上限の判定・開始の判定
    (`should_start`) をこの値で行うようにした。`lag_ms` は生のまま統計へ残し、`max_lag_ms`
    と `measure_skip` は生の値のままにした。入力の遅れが測れないときは `lag_ms` をそのまま
    使い、測れたときに負になった場合だけ 0 に寄せる。
  - `AudioReceiveCatchUpStats` に `input_lag_ms` と `decode_lag_ms` を足し、`floor_ms` の
    意味 (「復号が足している遅れ」の最小値) と `max_lag_ms` の意味 (生の出力の遅れの最大値)
    を rustdoc に書いた。`reset` で新しい観測も消えるようにした。
  - モジュールの先頭のコメントに、判定が「復号が足している遅れ」である理由 (区別できる 2 つの
    状態)、入力の遅れが測れないときの扱い、床も同じ値で求める理由を moqt-js の実測とともに
    書いた。`AUDIO_RECEIVE_CATCH_UP_JUMP_MS` の rustdoc にも、経路が遅いだけの状態ではこの
    条件へ来ない (来ないのが正しい) ことを書いた。
- `examples/moq-sub/src/pipeline.rs`
  - `receive_input_lag_ms(timestamp, timescale, arrival_us, pts_us)` を足した。壁時計の
    TIMESTAMP (Timescale が無いか 0。`loc_timestamp_us` と同じ規則) のときだけ
    `(到着 − TIMESTAMP) / 1_000` を返し、TIMESTAMP が無い音と Timescale がある音では `None`
    を返す。
  - `AudioDelivery::deliver` で、復号へ渡す直前の到着の壁時計 (`arrival_us`) を測り、
    `observe` へ `input_lag_ms` を渡すようにした。復号器を作り直すときのログに
    `input_lag` と `decode_lag` を足した (`optional_ms_log` で測れない場合は `none`)。
  - コードと食い違っていたコメント (「Timescale 付きでは観測しない」) を、実際の挙動
    (壁時計の TIMESTAMP を持つ音でだけ入力の遅れを測り、観測そのものは続ける) に直した。
    挙動は変えていない。library 側の `AudioReceiveCatchUpObservation::lag_ms` の rustdoc にも
    同じ食い違いが残っていたため、独立レビューの指摘により直した (観測は続き、測れない音では
    `input_lag_ms` に `None` を渡す)。
- テスト
  - `tests/test_playout/catch_up.rs`: 観測に `input_lag_ms` を渡す helper を足し、moqt-js の
    `audioReceiveCatchUp.test.ts` の 4 本と同じ意図のテストを移植した。受信側が溜めたまま
    戻らない状態 (60 秒を駆動し、入力の遅れは健全なまま出力の遅れだけが 250 ms) では直った
    時点で 1 回作り直すこと、経路が恒久的に悪化した状態 (入力の遅れも同じだけ上がる) では
    作り直さないこと、入力の遅れを渡さないと同じ入力でも床が出力の遅れへ追いついて作り直さ
    なくなること (導入前の再現)、入力の遅れが測れないときは出力の遅れをそのまま使うことを
    固定した。統計の意味 (生の遅れと引いた値の区別、負の値の扱い) も固定した。
  - `pbt/tests/prop_playout/catch_up.rs`: 観測に `input_lag_ms` を混ぜ (測れない場合・一部で
    ある場合・同じ場合)、統計が生の遅れと「復号が足している遅れ」を混同しないこと、上限と
    追いつきの終了の判定が引いた値で行われることを検証するようにした。測れた場合と測れない
    場合の両方を観測したことも数えて確かめる。
  - `examples/moq-sub/src/pipeline.rs`: `receive_input_lag_ms` の単体テスト (壁時計・
    Timescale 0・負の遅れ・TIMESTAMP 無し・Timescale あり) と、壁時計の TIMESTAMP を持つ音を
    配送すると `input_lag_ms` が測られて `lag_ms − input_lag_ms = decode_lag_ms` になること、
    Timescale がある音では測られず出力の遅れがそのまま使われることを、実 Opus の復号を通して
    固定した (モックやスタブは使っていない)。
- 復号の出力と入力の対応
  - 復号が入力 1 つにつき出力 1 つであるため (Opus は同期、AAC は専用スレッドの完了待ち)、
    moqt-js の `src/playout/decodeInputTimestamps.ts` (`takeDecodeInputEntry` と
    `LastTimestampKind`) に相当する「timestamp を鍵にした対応表と許容」は新設しなかった。
    復号へ渡す直前の到着を局所の値のまま復号の出力の観測へ渡す。moqt-js がこの照合を持つのは
    WebCodecs の `AudioData.timestamp` が入力と完全には一致しない (実測: 100 マイクロ秒)
    ためであり、moqt-rs の復号器には当てはまらない
  - 将来、内部に複数パケットを溜めて古い PCM を返す復号器を足す場合は、timestamp を鍵にした
    対応表と、隣のフレームと取り違えない許容 (音 1 つの長さの半分) が要る
- 検証
  - `cargo test -p shiguredo_moqt --test test_playout`: 自分の担当の catch_up 14 件を含めて成功
  - `cargo test -p pbt`: すべて成功 (自分の担当の catch_up 2 件を含む)
  - `cargo test -p shiguredo_moqt --lib` / `--doc`: 成功
  - `cargo test --workspace --no-fail-fast`: 失敗したのは `moq-sub` の bin の 1 件
    (`playout_assembly_records_the_backlog_miss`) だけで、これは他の担当が編集中の閉ループの
    目標と圧縮の速さの変更によるものである。自分は `main.rs` を変更していない
  - `cargo clippy -p shiguredo_moqt --lib -- -D warnings` / `cargo clippy -p moq-sub --lib
    --tests -- -D warnings`: 成功
  - `rustfmt --edition 2024 --check` (変更した 4 ファイル): 成功
  - `cargo clippy --workspace --all-targets -- -D warnings` は `tests/test_playout/feedback.rs`
    の指摘 (`assertions_on_constants`) で失敗し、`cargo fmt --all -- --check` は
    `src/playout/feedback.rs`、`tests/test_playout/feedback.rs`、`pbt/tests/prop_playout/feedback.rs`、
    `examples/moq-sub/src/main.rs` の整形の差分で失敗した。いずれも他の担当が編集中のファイルで
    あり、自分の変更したファイルへの指摘は無い。他の担当の変更が落ち着いた時点でワークスペース
    全体を再実行して確認する
- 見込みと残る課題
  - moqt-rs の復号は入力と 1 対 1 で、中に複数の入力を溜めない (Opus は同期、AAC は専用
    スレッドの完了待ち)。そのため「復号が足している遅れ」は復号の処理そのものの遅れに
    なり、復号器の作り直しは稀になる見込みである。作り直しが要るのは、復号の後段 (書き出しや
    再生) が詰まって復号の出力が遅れる場合であり、そのときも入力の到着は健全なので
    `decode_lag_ms` が上がって正しく要求される。moqt-js の再現 (250 ms の固定) は WebCodecs の
    未処理投入キューに依存するため、同じ形の再現は moqt-rs には無い
  - この見込みは実測で確かめていない。`observe` の rustdoc が既に留保しているとおり、追いつきの
    頻度を実リレーで測り直すのは別途の作業である。なお、実リレーを介した E2E (`e2e-tests` の
    3 件) はこの対応の後に実行して成功しており、datagram 配送と冗長化した音声の往復は壊れて
    いない。ただし E2E は publish / subscribe の往復と MP4 の検証であり、追いつきが起きた
    回数や目標遅延の値を測るものではない
  - 入力の遅れを測る位置は「復号へ渡す直前」である。これは moqt-js が object のコールバック
    内で測る位置と同じであり、受信の入口より後ろで待った分 (書き出しや再生の詰まり) は
    「復号が足している遅れ」に入る。受信そのものの入口 (transport からの読み出し) で測る形に
    すると、その待ちが入力の遅れに混ざって経路の遅れと区別できなくなる
  - 既存の食い違い (Timescale ありでも観測している) は変えていない。Timescale ありの音は
    `input_lag_ms = None` となり、導入前と同じ判定になる
