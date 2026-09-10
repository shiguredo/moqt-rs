# doc コメントの実装との不整合を修正する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/doc-comment-inconsistencies

## 目的

実装と食い違う doc コメントを修正し、読者とエージェントの誤解を防ぐ。

## 現状

- `src/session/core.rs` のモジュール doc は data stream / datagram の送受信 state を自身の責務として列挙し、分離先に `data` を挙げていない。実際は `send_subgroup_header` / `recv_subgroup_header` / `recv_object_datagram` 等が `src/session/data.rs` にある。
- `src/kvp.rs` は「トレイト・マクロ・クロージャ・汎用中間表現は導入しない (CODEBASE.md 規約)」と引用するが、`CODEBASE.md` に該当記述は無い。
- `src/session/subscription/validation.rs` の `validate_group_order` doc の 1 行目と 2 行目が重複している。
- `src/msf.rs` の `MsfCatalogDocument::decode` 付近のコメントは「deltaUpdate が配列以外なら Full」と書くが、実コードは `InvalidCatalog` で拒否する。
- `src/session/namespace/publish_namespace.rs` / `subscribe_namespace.rs` / `track_subscription.rs` のコメントは「Session が close 意図を通知する SessionEvent variant は未実装」と書くが、`ResetRequestStream` / `StopSendingRequestStream` は定義済みで `ResetRequestStream` は実際に発行される。
- `src/message.rs` の `REQUEST_OK_ALLOWED_PARAMS` のコメントは「全応答 context の許可パラメータの和集合」と説明するが、実際はそれより広い集合。codec 層がワイヤ妥当性のみを検証し context 別検証は session 層が担う旨を正確に書く。

## 設計方針

各コメントを実装に合わせる。`core.rs` の責務列挙には `data` を加える。誤った規約参照は正しい根拠 (`shiguredo-rust` スキル等) に直す。

## 完了条件

- 上記コメントが実装と一致していること
- doc が実装の唯一の説明として使えること
