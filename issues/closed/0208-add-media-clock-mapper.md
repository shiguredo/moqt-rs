# 映像フレームのメディア時刻を壁時計へ換算する

- Created: 2026-10-06
- Completed: 2026-10-06
- Branch: feature/complete-playout
- Polished: {YYYY-MM-DD}

## 目的

映像デバイスの `timestamp_us` は取得元ごとに基準が異なり、Unix epoch の壁時計ではない
(mach 絶対時刻、Media Foundation のサンプル時刻、driver が選ぶ `CLOCK_MONOTONIC` /
`CLOCK_REALTIME` など)。LOC の TIMESTAMP は Timescale を載せない場合 Unix epoch
マイクロ秒 (draft-ietf-moq-loc-04 §2.3.1.1) であるため、publisher 側で壁時計へ換算する
必要がある。1 つのフレームだけで対応を取ると、その遅れの分だけ以降の TIMESTAMP が
撮影時刻より未来へずれる。遅れが最も小さいフレームを対応にして換算する口を
ライブラリに置く。

## 現状

- `examples/moq-pub/src/encoder/av1.rs` の `Av1Encoder::encode` などは `frame_count * timescale / fps` で Timestamp を作る。capture の `timestamp_us` は使っていない
- `examples/moq-pub/src/pipeline.rs` の `build_video_loc_properties` は `PROP_TIMESCALE` を keyframe にだけ付ける。delta frame を単体で見ると LOC の既定 (epoch マイクロ秒) で解釈される
- ライブラリに、メディア時刻を壁時計へ換算する口が無い

## 設計方針

`src/media_clock.rs` に `WallClockMapper` を追加する。時計にもデバイスにも触らず、フレームの timestamp とそのとき読んだ壁時計を引数で受ける。

- `observe(media_us, wall_clock_us)` で「壁時計 - timestamp」の最小値を目標の対応として更新する。撮影から読むまでの遅れが最も小さいフレームを対応にするためである
- `to_wall_clock_us(media_us, fallback_wall_clock_us)` で換算する。対応を後から小さくすると換算した TIMESTAMP が前のフレームより戻るため、1 回の換算で動かす量を前回換算したフレームとの timestamp の差の半分未満に抑える。これにより換算した TIMESTAMP の差は timestamp の差の半分より大きく保たれ、単調に増える
- 30 fps では 1 回あたり 16.7 ms 未満であり、開始時の数百 ms の遅れは 1 秒ほどで埋まる
- LOC の Timestamp は vi64 で負を表せないため、Unix epoch より前にはしない
- まだ 1 つも観測していないときは `fallback_wall_clock_us` をそのフレームの対応として使う。渡されなければ `None` を返す

## 完了条件

- 単調に増えること。対応を小さくするときも換算した TIMESTAMP が戻らないこと
- 遅れの最小値が対応になること (開始時に遅れが大きくても、その後のフレームで収束すること)
- 負の値にならないこと。観測が無く fallback も無いときは `None` を返すこと
- 単体テストと PBT があること
- `make test` (`cargo test --workspace`) と `make pbt` と `make clippy` と `make fmt` が通ること

## 参照

- draft-ietf-moq-loc-04 §2.3.1.1 (Timestamp) / §2.3.1.2 (Timescale)
- draft-ietf-moq-msf-01 §5.2.8 (Target latency)

## 解決方法

- `src/media_clock.rs` に `WallClockMapper` を追加した。`observe(media_us, wall_clock_us)` が「壁時計 − メディア時刻」の最小値を対応の目標にし、`to_wall_clock_us(media_us, fallback_wall_clock_us)` がその目標へ向けて 1 回の換算につき前回換算したメディア時刻との差の半分未満だけ動かす。換算した TIMESTAMP が戻らないための制限である。
- 起源が Unix epoch でも monotonic でも同じ式で扱える。音声と映像で別々の mapper を持つ前提であり、共通の対応は仮定しない。
- `tests/test_media_clock.rs` に 6 件、`pbt/tests/prop_media_clock.rs` に 1 件のテストを追加した。
- `examples/moq-pub` の live capture の Timestamp がこの換算を使うようになった (0103)。
