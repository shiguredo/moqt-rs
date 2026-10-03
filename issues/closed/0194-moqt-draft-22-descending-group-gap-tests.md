# Descending Group Order の Fetch ギャップ処理のテストを追加する

- Created: 2026-10-02
- Completed: 2026-10-04
- Branch: feature/test-descending-group-gap
- Polished: 2026-10-02

## 目的

draft-22 §3.2.2 は Fetch のギャップを明文化し、Descending Group Order で複数 group をスキップする場合の期待順序 (最初は `{End.Location.Group, 0}`、最後は `{Start.Location.Group, 2^64 - 1}`) を規定した (A.1 #1895)。`FetchStreamDecoder` の descending デルタ解決は仕様と整合しているが、テストは隣接 2 group の roundtrip のみで、複数 group のスキップや End of Range との組み合わせが固定されていない。仕様解釈が退行しないようテストで固定する。

## 現状

- `src/stream/decoder.rs` の `FetchStreamDecoder` は descending を group = prior - (delta + 1)、同 group の object_id = prior + delta として解決し、group 降順と同 group 内 object 昇順を検証する。`src/stream/encoder.rs` の `FetchStreamEncoder` も同式で descending のデルタを計算する。
- `tests/test_stream/encoder.rs` の `test_descending_group_order_roundtrip` は隣接 2 group (5 → 4) の roundtrip のみで、複数 group のスキップ (例: 10 → 3) や group 変化時の object_id 絶対値と同 group 内デルタの混在を検証していない。
- `tests/test_stream/decoder.rs` の descending に関するテストは、昇順 group を descending モードが拒否するテストのみで、正例の roundtrip が無い。また End of Range (0x8C / 0x10C / 0x20C) を descending に混在させた順序を検証するテストは無い。
- `pbt/tests/prop_stream/encoder.rs` の `encoder_decoder_roundtrip` は ascending 固定 (`FetchStreamEncoder::new`) で、descending の往復を性質として固定していない。
- ギャップの意味解釈 (Range Filter の有無による「非存在」/「不明」の区別、End Location に対する末尾ギャップの FIN 検出) はアプリ責務であり、decoder は絶対 Location と End of Range を提供するのみである (End Location は bidi request stream 上の FETCH_OK で運ばれ、`FetchStreamDecoder` はそれを知らない)。この責務境界もライブラリ側のテストで固定されていない。
- relay の Fill Timeout 予算管理 (§3.2.3) は relay 非対応のため対象外である。

## 設計方針

- roundtrip は性質テストの責務であるため、`pbt/tests/prop_stream/encoder.rs` の `encoder_decoder_roundtrip` を descending 対応にする。group を降順にも生成し、`FetchStreamEncoder::new_with_group_order` / `FetchStreamDecoder::new_with_group_order` の両順序で encode → decode の往復が絶対値を保存することを固定する。
- `tests/test_stream/encoder.rs` / `tests/test_stream/decoder.rs` に固定値の単体テストを追加する。対象は複数 group のスキップ (例: 10 → 3) の roundtrip、group 変化時の object_id 絶対値と同 group 内デルタの混在、End of Range (0x8C / 0x10C / 0x20C) を混在させた順序とする。`FetchStreamDecoder::finish` は group order に依存しないため、FIN 時の受容は End of Range を混在させたテストに含めて確認する。
- ギャップの「非存在 / 不明」分類と End Location に対する末尾ギャップの判定はアプリ責務のためライブラリ API では検証しない。decoder が返す絶対位置と End of Range が仕様どおりであることだけを固定する。
- 実装の挙動は変えない。テストの追加で実装の不足が見つかった場合のみ、別 issue として切り出す。

## 完了条件

- `pbt/tests/prop_stream/encoder.rs` の roundtrip PBT が ascending / descending の両方を検証していること
- `tests/test_stream/encoder.rs` / `tests/test_stream/decoder.rs` に、複数 group のスキップ (例: 10 → 3)、group 変化時の object_id 絶対値と同 group 内デルタの混在、End of Range (0x8C / 0x10C / 0x20C) を混在させた順序 (FIN 時の受容を含む) を固定するテストが追加されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `prek run --all-files` が通ること

## 解決方法

- `pbt/tests/prop_stream/encoder.rs` の `encoder_decoder_roundtrip` を昇順 (0x01) / 降順 (0x02) の両方に拡張した。
  Group は `group_order` の向きに狭義単調に生成し、降順は減算で underflow しない初期値から始める。デコーダは `drive_fetch_with_group_order` で同じ Group Order を使い、昇順 / 降順 / 降順で Group が変化するケースの観測をゲートする。
- `tests/test_stream/encoder.rs` に降順の固定値テストを追加した。複数 Group のスキップ (10 → 3 → 1)、Group 変化時の Object ID 絶対値 (5001 → 1) と同 Group 内デルタの混在、End of Range (0x8C / 0x10C / 0x20C) と Object の混在を固定する。
- `tests/test_stream/decoder.rs` に降順の正例を追加した。Group デルタの解決 (10 → 3)、End of Range が prior を更新すること (3 種すべて)、End of Range 直後の Object ID Delta 省略が prior + 1 になることを生デルタのフィクスチャで固定する。
- End of Range のテストは、3 種すべてで Group を直前 Object の Group と変えることで「End of Range が prior を更新しない退行」を検出できる。`update_prior_for_end_of_range` を no-op にすると encoder 側 / decoder 側のテストがそれぞれ落ちることを実測で確認した。
- `tests/test_stream.rs` に共有ヘルパー (`drain_fetch_payload` / `decoded_fetch_object`) を置き、`tests/test_stream/decoder.rs` の重複定義を削除した。
- ギャップの意味解釈 (非存在 / 不明の区別、End Location に対する末尾ギャップの FIN 判定) はアプリ責務のためライブラリ API では検証せず、decoder が返す絶対位置と End of Range が仕様どおりであることだけを固定した。実装の挙動は変えていない (src/ に差分なし)。
- `CHANGES.md` の `### misc` に `[UPDATE]` を追加した。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `prek run --all-files` が通ることを確認した。
