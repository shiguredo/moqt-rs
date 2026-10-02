# Descending Group Order の Fetch ギャップ処理のテストを追加する

- Created: 2026-10-02
- Completed: {YYYY-MM-DD}
- Branch: feature/test-descending-group-gap
- Polished: {YYYY-MM-DD}

## 目的

draft-22 §3.2.2 は Fetch のギャップを明文化し、Descending Group Order で複数 group をスキップする場合の期待順序 (最初は `{End.Location.Group, 0}`、最後は `{Start.Location.Group, 2^64 - 1}`) を規定した (A.1 #1895)。`FetchStreamDecoder` の descending デルタ解決は仕様と整合しているが、テストは隣接 2 group の roundtrip のみで、複数 group のスキップや End of Range との組み合わせが固定されていない。仕様解釈が退行しないようテストで固定する。

## 現状

- `src/stream/decoder.rs` の `FetchStreamDecoder` は descending を group = prior - (delta + 1)、同 group の object_id = prior + delta として解決し、group 降順と同 group 内 object 昇順を検証する。
- `tests/test_stream/encoder.rs` / `tests/test_stream/decoder.rs` の descending テストは隣接 2 group のみで、複数 group のスキップ (例: 10 → 3) や End of Range (0x8C / 0x10C / 0x20C) を混在させた順序を検証していない。
- ギャップの意味解釈 (Range Filter の有無による「非存在」/「不明」の区別、末尾ギャップの FIN 検出) はアプリ責務であり、decoder は絶対 Location と End of Range を提供するのみである。この責務境界もテストで固定されていない。
- relay の Fill Timeout 予算管理 (§3.2.3) は relay 非対応のため対象外である。

## 設計方針

- `tests/test_stream/encoder.rs` / `tests/test_stream/decoder.rs` に Descending Group Order のテストを追加する。対象は複数 group のスキップを含む roundtrip、group 変化時の object_id 絶対値と同 group 内デルタの混在、End of Range 後の順序、`FetchStreamDecoder::finish` による FIN 時の末尾ギャップ検出 (header のみ / partial) とする。
- ギャップの「非存在 / 不明」分類はアプリ責務のためライブラリ API では検証しない。decoder が返す絶対位置と End of Range が仕様どおりであることだけを固定する。
- 実装の挙動は変えない。テストの追加で実装の不足が見つかった場合のみ、別 issue として切り出す。

## 完了条件

- Descending Group Order で複数 group をスキップする encode / decode roundtrip テストが追加されていること
- End of Range と FIN 判定を含む順序テストが追加されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `prek run --all-files` が通ること

## 解決方法

{未着手}
