# MSF_COMPRESSION の GZIP カタログを復号できるようにする

- Created: 2026-10-09
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-catalog-gzip
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-01 §12.1 (Compression Signaling) は「All MSF implementations MUST support both
uncompressed payloads (value 0 or property absent) and GZIP compressed payloads (value 1).」と
MUST を定める。§12.1.1 (MSF_COMPRESSION Track Property) と §12.1.2 (MSF_COMPRESSION Object
Property) は publisher の property 付与と subscriber の property 確認も MUST とする。
§5.5 はカタログ payload の圧縮を認め、独立カタログを圧縮して delta は非圧縮にする例を挙げる。
現状は gzip 圧縮されたカタログを復号できず、MUST を満たしていない。

## 現状

- `src/msf.rs` の `MsfCatalogDocument::decode` は payload をそのまま UTF-8 と JSON として解釈し、
  gzip を復号しない。gzip 圧縮されたカタログは「catalog is not valid UTF-8」で失敗する。
- `examples/moq-sub/src/catalog.rs` の `CatalogObject::decode` はその失敗を `Error::Other` に
  変換し、moq-sub は当該 Object を捨てる。起動経路では example が終了する。
- 同じ `src/msf.rs` には Media / Event Timeline payload 用の `is_gzip_compressed` /
  `maybe_decompress_gzip` (gzip magic `0x1F 0x8B` の検出と展開、展開サイズの上限) があり、
  timeline の decode はこれを経由する。catalog は経由していない。
- MSF_COMPRESSION の Property ID は §14.3 (MSF_COMPRESSION Track Property) と
  §14.4 (MSF_COMPRESSION Object Property) で TBD のため、property による signaling は
  現時点の draft では実装できない。

## 設計方針

- カタログ Object の payload にも gzip magic による自動復号を適用する。JSON は `{` で始まるため
  非圧縮 payload を誤検出しない。展開サイズの上限は timeline と同じ扱いを共有する。
- 圧縮と非圧縮が同じカタログ track の Group 内で混在してよい (§12.1.2 の例) ため、Object 単位の
  判定にする。track 単位の状態を持たない。
- property による signaling (Track Properties / Object Properties の確認と、未対応アルゴリズムで
  payload を処理しない扱い) は Property ID の確定後に実装する。本 issue ではその残作業を
  切り分けて doc に残す。
- `MsfCatalogDocument::decode` は Sans I/O のままにする (payload の復号だけを追加し、I/O を
  持ち込まない)。

## 完了条件

- gzip 圧縮したカタログ Object を decode できることのテスト。
- 非圧縮カタログの decode 結果が変わらないことのテスト。
- 途中で切れた gzip など不正な圧縮データがエラーになることのテスト。
- MSF_COMPRESSION property による signaling が未対応であることと、その理由 (Property ID が
  TBD) がコードコメントとドキュメントに書かれていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。
