# 音声の TIMESTAMP を配信側の壁時計へ合わせる AudioTimestampClock を追加する

- Created: 2026-10-10
- Completed: 2026-10-10
- Branch: feature/add-audio-timestamp-clock

## 目的

LOC の TIMESTAMP は Timescale を載せないとき、取得した時刻の壁時計 (Unix epoch マイクロ秒) として解釈される (draft-ietf-moq-loc-04 §2.3.1.1)。ところがデバイスが返す音声のメディア時刻は壁時計と同じクロックとは限らない。マイクから届く音声のタイムスタンプはデバイスの時計であり、そのまま壁時計として送ると、受信側は「音声は数百 ms 遅れて届いている」と解釈し、リップシンクのために映像をその分だけ遅らせる。

moqt-js はこの問題に対して、映像用の `WallClockMapper` (`src/mediaClock.ts`) とは別に、音声用の `AudioTimestampClock` (`src/audioTimestampClock.ts`) を持つ。音声の時計はサンプルレートのずれでゆっくり進み、デバイスの切り替えで段差で飛ぶため、映像と同じ追従方法では足りないからである。

moqt-rs には映像用の `WallClockMapper` だけがあり、音声にもそのまま使っている。音声の時計に起きる 2 つの動きに追従できておらず、配信側の TIMESTAMP が実際より古いままになる。

## 現状

- `src/media_clock.rs` の `WallClockMapper` は、観測した「読んだ壁時計 − メディア時刻」の最小値を対応にし、対応を小さくする向きにだけ、1 回の換算でメディア時刻の差の半分未満ずつ動かす。換算した時刻が前のフレームより戻らないことを保証するためである
- `examples/moq-pub/src/pipeline.rs` の `map_capture_timestamp_us` は、映像と音声の両方でこの `WallClockMapper` を使う
- このため音声では次の 3 つが起きる
  - 音声の時計が壁時計よりゆっくり進む (ドリフトする) とき、対応は小さくなる向きにしか動かないため追従できず、送る TIMESTAMP が実際より古いままになる。受信側の再生の目標が過去へずれ、音が遅れて届いていると解釈され続ける
  - 音声デバイスが切り替わって時計が段差で飛んだとき、全期間の最小値は古い観測に引きずられて動かない。段差の分だけ TIMESTAMP がずれ続ける
  - 補正の根拠 (生の観測の現在値・最小・最大・傾き) を確かめる手段が無く、実機でずれの原因を切り分けられない

## 設計方針

moqt-js の `src/audioTimestampClock.ts` を移植し、音声専用のクロックをライブラリに追加する。映像は `WallClockMapper` のままにする。ブラウザ API に依存せず、時刻は呼び出し側が引数で渡す (Sans-I/O)。

- `src/audio_clock.rs` に `AudioTimestampClock` を置く。`src/lib.rs` に `pub mod audio_clock;` を追加する
- 観測は「読んだ壁時計 − 音声の TIMESTAMP」であり、「音声の時計と壁時計のずれ」と「読むまでの遅れ (0 以上)」の和である。遅れの最小値を窓で取り直すことで、ずれに最小の遅れを足した値を推定する
- 窓の最小値は 2 つの動きに追従する
  - ゆっくりしたドリフト: 窓が滑るにつれて最小値が動く。補正は常に窓の最小値へ合わせる
  - 段差: 直近の窓の最小値が適用中の値より `AUDIO_TIMESTAMP_OFFSET_STEP_US` 以上大きくなったら、古い観測を捨ててその値へ取り直す。段差は音声の時計そのものが飛んだのであり、遅れが増えたのではない。取り直しを入れないと、窓が埋まるまで TIMESTAMP が実際より古いままになる
- 補正は記録のたびに更新し、`apply` はそのまま足すだけにする。同じ補正を当てた chunk どうしの間隔は音声の TIMESTAMP の間隔そのままになる。LOC の Timestamp は vi64 で負を表せないため、Unix epoch より前にはしない
- 観測の統計 (現在値・最小・最大・10 秒と 60 秒の傾き・適用中の補正・サンプル数) を返す。補正の取り直しでは最小・最大・サンプル数を消さない (生の観測の証拠を残す)
- 定数 (マイクロ秒): 観測の窓 2 秒 / 段差とみなす上振れ 200 ms / 段差を見る直近の窓 500 ms / 段差とみなすのに必要な観測の数 5 / 傾きの窓 10 秒・60 秒
- `examples/moq-pub/src/pipeline.rs` の音声経路を `AudioTimestampClock` へ置き換え、統計をログに出す

## 完了条件

- `AudioTimestampClock` がライブラリから使えること
- 音声の経路が `AudioTimestampClock` を使い、映像は `WallClockMapper` のままであること
- ドリフトと段差に追従すること、補正を当てた chunk の間隔が音声の TIMESTAMP の間隔のままであること、Unix epoch より前を返さないことがテストで固定されていること
- 補正の根拠 (現在値・最小・最大・傾き・適用中の補正・サンプル数) がログで確認できること
- `make pbt` / `make test` / `make clippy` / `make fmt` が通ること
- ソースコードに issue 番号や issue への言及を書かないこと

## 解決方法

- `src/audio_clock.rs` を追加し、moqt-js の `src/audioTimestampClock.ts` を移植した (時刻は引数で受ける Sans-I/O)
- 公開した: `AUDIO_TIMESTAMP_OFFSET_WINDOW_US` (2 秒) / `AUDIO_TIMESTAMP_OFFSET_STEP_US` (200 ms) / `AUDIO_TIMESTAMP_OFFSET_STEP_WINDOW_US` (500 ms) / `AUDIO_TIMESTAMP_OFFSET_STEP_MIN_SAMPLES` (5) / `AUDIO_TIMESTAMP_SLOPE_WINDOW_US` (10 秒) / `AUDIO_TIMESTAMP_SLOPE_LONG_WINDOW_US` (60 秒)、
- `AudioTimestampClock` (`record` / `apply` / `applied_us` / `snapshot` / `reset`)、`AudioTimestampOffsetStats`
- 補正は「読んだ壁時計 − 音声の TIMESTAMP」の 2 秒の窓の最小値へ合わせる。窓が滑るにつれて最小値が動くため、ゆっくりしたドリフトに追従する
- 適用中の補正があるとき、直近 500 ms の窓の観測が 5 個以上あり、その最小値が適用中の値より 200 ms 以上大きければ段差とみなし、その値へ取り直して古い観測を捨てる。窓が埋まるのを待つと、その間だけ TIMESTAMP が実際より古くなり、受信側の再生の目標が過去へずれて音が捨てられる
- `apply` は補正を足すだけであり、同じ補正を当てた chunk どうしの間隔は音声の TIMESTAMP の間隔そのままになる。LOC の Timestamp は vi64 で負を表せないため、Unix epoch より前にはしない
- 観測は `VecDeque` に時刻の昇順で持ち、60 秒より古い観測を先頭から捨てる。統計の最小・最大・サンプル数は補正の取り直しでは消さない (生の観測の証拠を残す)
- 傾きは窓を前半と後半に分け、それぞれの最小の観測の差を経過時間で割って求める。両端とも床を使うため読み出しの遅れの揺らぎを受けにくい。桁あふれは i128 で避ける
- `examples/moq-pub/src/pipeline.rs` の音声の換算を `AudioTimestampClock` に置き換えた (映像は `WallClockMapper` のまま)。映像と音声で追従の規則が違うため、換算の関数は音声専用の `map_audio_capture_timestamp_us` に分けた。残差の要約 (5 秒間隔) と同じ行に補正の統計 (現在値・最小・最大・10 秒と 60 秒の傾き・適用中の補正・サンプル数) をミリ秒で出す
- テスト: `tests/test_audio_clock.rs` に 14 件 (窓の最小値への追随 / ドリフト / 段差の取り直しと必要な観測数 / 閾値未満では取り直さない / `apply` の間隔保持 / 負にしない / 統計 / 傾き / `reset`)、`pbt/tests/prop_audio_clock.rs` に 3 件 (規則のモデルとの差分と分岐のカバレッジゲート)、example に 3 件を追加した
- 検証: `cargo fmt --check` / `make pbt` / `make test` / `cargo clippy --workspace --all-targets -- -D warnings` / `RUSTDOCFLAGS="-D warnings" cargo doc` / no_std ビルド (`thumbv7em-none-eabihf`) が成功した。`PBT_SEED` を変えても property が成功する
- 検証: 9 種類の欠陥 (段差の取り直しの無効化 / 必要な観測数のゲートの除去 / 窓の最小値を最大にする / epoch の丸めの除去 / 取り直しで統計を消す / 傾きの窓の分割の破壊 / 段差の窓の取り違え / 取り直しで古い観測を残す / 傾きの床を最大にする) を入れて、追加したテストと property がそれぞれ検出することを確認した
- 備考: `apply` は状態を変えないため `&self` にした。`src/lib.rs` の `pub mod audio_clock;` には `///` を付けない (`///` を置くと rustdoc がモジュール内の intra-doc link をクレート直下の名前空間で解決してしまうため)。理由は `//` コメントに書いた
