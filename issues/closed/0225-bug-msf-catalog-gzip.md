# MSF_COMPRESSION の GZIP カタログを復号できるようにする

- Created: 2026-10-09
- Completed: 2026-10-10
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

## 解決方法

本 issue は closed とする (対応不要)。根拠は次のとおり。

- 設計方針の「カタログ Object の payload にも gzip magic による自動復号を適用する」は
  draft-ietf-moq-msf-01 §12.1.1 (Track Property) と §12.1.2 (Object Property) に反する。
  前者は「If the property is absent, and no per-object compression is signaled, the
  subscriber MUST treat the payload as uncompressed.」、後者は「If the property is
  absent on an object, the subscriber MUST treat that object's payload as
  uncompressed.」と定める。signaling 無しの gzip payload を magic 検出で展開する本 issue
  の設計は、この MUST を実装側が破ることになる。
- 現状の `MsfCatalogDocument::decode` が payload を UTF-8 / JSON として解釈し、signaling
  無しの gzip payload を拒否するのは -01 に準拠した正しい挙動であり、報告されている
  「バグ」は仕様上は存在しない。§12.1 の「All MSF implementations MUST support both
  uncompressed payloads ... and GZIP compressed payloads (value 1)」は property の
  value 1 (GZIP) を signal 経由でサポートする要求であり、magic byte による自動判定を
  要求しているわけではない。
- MSF_COMPRESSION property の ID は未確定 (§14.3 の Track Property は TBD、§14.4 は
  Object Property のレジストリ登録要求自体が無く値レジストリの作成要求のみ) のため、
  現時点で signal を受信・処理することはできない。value 1 のサポートに必要な作業は
  DRAFT の ID 確定後に実施する。
- カタログ圧縮の仕様準拠対応 (property signaling による §5.5 Catalog Compression と、
  magic byte 検出の signal ベースへの置換) は `issues/pending/0002-change-msf-compression-signaling.md`
  で計画済みであり、その完了条件にも「§5.5 Catalog Compression ... が動作する」と
  「magic byte 検出は案 B (strict) で signal ベースに置き換え済み」が明記されている。
  同 issue は「property 無しで gzip バイトを受信したとき magic byte 検出で展開する案 A
  (-00 互換 fallback) は「uncompressed として扱う」規定に違反するため採用しない」と
  結論しており、本 issue の設計はこの確定済み方針と直接矛盾する。
- したがって本 issue の設計を実装すると CODEBASE.md の「最新ドラフトに準拠すること」に
  反する -01 違反コードが入り、かつ `issues/pending/0002-change-msf-compression-signaling.md`
  の実装時に削除されることになる一時的な非準拠しかもたらさない。対応は不要と判断した。

なお「§14.4 (MSF_COMPRESSION Object Property) で TBD」とする現状の記述は不正確であり、
実際は §14.4 に Object Property のレジストリ登録要求自体が存在しない (値レジストリ
「MSF Compression Algorithms」の作成要求のみ)。
