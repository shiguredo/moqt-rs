# playout::stretch の fuzz ターゲットを追加する

- Created: 2026-10-01
- Completed: {YYYY-MM-DD}
- Branch: feature/add-playout-stretch-fuzz
- Polished: 2026-10-01
- Updated: 2026-10-10

## 目的

`playout::stretch` の時間圧縮・伸長は任意のサンプル列を受け取る公開 API だが、任意入力に対するクラッシュ耐性を fuzzing で確認していない。PBT は境界長と構造の不変条件を検証しているが、サンプル値の極端な組み合わせを網羅するのは fuzzing の役割である。

## 現状

- `fuzz/fuzz_targets/` にデコーダ / パーサと C4M の暗号検証が中心のターゲットが 40 本あるが、`playout` を対象にしたものは無い
- `pbt/tests/prop_playout/stretch.rs` が境界長 (0 / 1 / 123 / 124 / 729 / 730 / 1440) と構造不変条件を 256 ケースで検証している
- `playout::stretch::compress` / `expand` はおおむね `[-1.0, 1.0]` に正規化された有限値を前提にしており (doc に明記)、任意の f32 ビット列に対する挙動は未検証である
- `playout::stretch` には 0206 (2026-10-06) で `conceal` / `concealment_end_gain` が追加されており、同じく `[-1.0, 1.0]` の有限値の前提を持つ。本 issue の対象を `compress` / `expand` に限るかは実装時に決める

## 設計方針

- `fuzz/fuzz_targets/` に `playout::stretch` のターゲットを 1 本追加する (ファイル名は既存の `fuzz_<機能>.rs` に合わせる)
- 入力からサンプルレート (対応 4 レートと対応外)、チャンネル数、入力長、サンプル値、出力長を組み立て、`compress` と `expand` を呼ぶ
- f32 は有限値に丸めず、任意ビット列から作る。panic しないことを第一の性質とし、有限値のときは PBT と同じ長さの契約も assert する
- `fuzz/Cargo.toml` に `[[bin]]` を追加し、`make fuzzing` の対象に含まれるようにする

## 完了条件

- `cargo fuzz list` に追加したターゲットが表示されること
- `cargo +nightly fuzz run <target> -- -max_total_time=30` が panic せずに終わること
- 既存ターゲットと同じファイル名・`[[bin]]` の構成になっていること

## 解決方法

{未着手}
