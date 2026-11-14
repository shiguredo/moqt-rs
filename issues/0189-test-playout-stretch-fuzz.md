# playout::stretch の fuzz ターゲットを追加する

- Created: 2026-10-01
- Completed: {YYYY-MM-DD}
- Branch: feature/add-playout-stretch-fuzz
- Polished: 2026-11-14
- Updated: 2026-11-14

## 目的

`playout::stretch` の時間圧縮・伸長は任意のサンプル列を受け取る公開 API だが、任意入力に対するクラッシュ耐性を fuzzing で確認していない。PBT は境界長と構造の不変条件を検証しているが、サンプル値の極端な組み合わせを網羅するのは fuzzing の役割である。

## 現状

- `fuzz/fuzz_targets/` にデコーダ / パーサと C4M の暗号検証が中心のターゲットが 41 本あるが、`playout` を対象にしたものは無い
- `pbt/tests/prop_playout/stretch.rs` がレートごとの受理境界 (0 / 1 / 123 / 124 / 126 / 160 / 441 / 729 / 730 / 742 / 800 / 882 / 960 / 1_323) と構造不変条件を `compress` と `conceal` で 256 ケースずつ検証している
- `playout::stretch::compress` はおおむね `[-1.0, 1.0]` に正規化された有限値を前提にしており (doc に明記)、任意の f32 ビット列に対する挙動は未検証である
- `playout::stretch` には 0206 (2026-10-06) で `conceal` / `concealment_end_gain` が追加されており、同じく `[-1.0, 1.0]` の有限値の前提を持つ
- `expand` は 0299 で削除されたが、2026-11-01 の moqt-js 移植で `preemptively_expand` (時間伸長) が追加された。`compress_with_history` と `preemptively_expand` は `crates/moq-sub/src/main.rs` から呼ばれている
- したがって対象は `compress` / `compress_with_history` / `preemptively_expand` / `conceal` であり、`concealment_end_gain` は `conceal` から呼ばれる

## 設計方針

- `fuzz/fuzz_targets/` に `playout::stretch` のターゲットを 1 本追加する (ファイル名は既存の `fuzz_<機能>.rs` に合わせる)
- 入力からサンプルレート (8 kHz / 16 kHz / 32 kHz / 44.1 kHz / 48 kHz の対応 5 レートと対応外)、入力と出力のチャンネル数、チャンネルごとの長さ、サンプル値、`max_length_change_samples`、`conceal_us`、履歴、出力長を組み立て、`compress` / `compress_with_history` / `preemptively_expand` / `conceal` を呼ぶ。チャンネル数・長さ・履歴は一致する場合としない場合の両方を作る
- `conceal` の出力は要求長以上を確保する場合と不足する場合の両方を入力から作り、不足時は 0 を返すことを assert する
- f32 は有限値に丸めず、任意ビット列から作る。panic しないことを第一の性質とし、有限値のときは
  各 API の長さの契約 (`compress` / `compress_with_history` は戻り値が 0 以下で削る長さが
  `max_length_change_samples` と入力長以下、`conceal` は戻り値が 0 または要求長、
  `preemptively_expand` は追加長が `max_length_change_samples` 以下で、追加長が 0 のときは
  `channels` が空、1 以上のときは出力長が入力長 + 追加長) も assert する
- 入力長と履歴長は 2_000 サンプル以下、チャンネル数は 2 以下、出力長は 20_000 サンプル以下、`conceal_us` は -200_000 以上 200_000 以下、サンプルレートは 0 以上 96_000 以下に切る (1 入力あたりの割り当てを抑え、30 秒の実行で回数を稼ぐため)
- `fuzz/Cargo.toml` に `[[bin]]` を追加し、`make fuzzing` の対象に含まれるようにする

## 完了条件

- `cargo fuzz list` に追加したターゲットが表示されること
- `cargo +nightly fuzz run <target> -- -max_total_time=30` が panic せずに終わること
- 追加したターゲットが `compress` / `compress_with_history` / `preemptively_expand` / `conceal` を呼び、サンプルレートは 8 kHz / 16 kHz / 32 kHz / 44.1 kHz / 48 kHz と対応外を含み、有限値のときに設計方針の長さの契約を assert していること
- 既存ターゲットと同じファイル名・`[[bin]]` の構成になっていること

## 解決方法

{未着手}
