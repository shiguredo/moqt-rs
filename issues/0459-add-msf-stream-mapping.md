# MSF の LOC stream mapping (group / object) に対応する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/add-msf-stream-mapping
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §4.1.1 (LOC to MOQT Stream mapping) は、LOC packaging の Object から MOQT Stream への割り当てモードを新設した。

- publisher は `group` または `object` のいずれかを MUST で選択し、`packagingMetadata` の `streamMapping` キーで per-track に通知する (§5.2.5)。
- subscriber は両モードを消費できなければならない (MUST)。
- `packagingMetadata` がない、またはキーがない場合は `group` と仮定する (MUST)。
- v01 の「各 Object を新しい MOQT Stream に MUST マップする」は削除された。

現行実装は `group` モード固定で、`object` モードの送信も受信もできず、モードを通知する経路もない。

## 現状

- `crates/moq-pub` の `SubgroupWriter::build_header` は `SubgroupIdMode::Zero` を使い、1 Group 内の全 Object を 1 Subgroup に載せる `group` モード固定である。
- `crates/moq-sub` の `decode_video_stream` は 1 Subgroup 内の複数 Object を処理できるが、`object` モードのように 1 Object ごとに Subgroup が分かれる受信経路の検証がない。
- `VideoDecodeOrder::admit` は同じ Group で `object_id` が飛んだ delta を `prior_object_id_gap` の範囲外なら捨てる。`object` モードでは Subgroup 間の到着順が保証されないため、後続 Object の取りこぼしが起きうる。
- カタログに `packagingMetadata` がなく、`streamMapping` を通知できない。フィールド自体は別 issue で追加する。
- v02 §4.1.1 は、モードにかかわらず sample から Object、GOP から Group への対応は変えないと定める。

## 設計方針

- `streamMapping` の enum (`Group` / `Object`) を追加し、既定値を `Group` とする。値の解釈は `packagingMetadata` の accessor に載せる。
- publisher は `object` モードを選べるようにし、1 Object ごとに Subgroup を作る送信経路を追加する。既定は `group` のままにして既存挙動を変えない。
- subscriber は `streamMapping` を読み、`object` モードでは Subgroup をまたいだ Object の並びを Object ID 順に処理できるようにする。`VideoDecodeOrder` の gap 判定は、`object` モードで到着順の入れ替わりを正しく扱えるようにする。
- `group` モードの既存の送受信挙動は変えない。

## 完了条件

- `"packagingMetadata": "streamMapping=object"` の track を `crates/moq-pub` が `object` モードで送信し、各 Object がそれぞれの Subgroup に載ること。
- `"streamMapping=group"` と `packagingMetadata` なしの track が従来どおり 1 Subgroup に載ること。
- `crates/moq-sub` が両モードを受信でき、Object ID 順に復号できること。
- `object` モードで Subgroup の到着順が入れ替わっても、Object の取りこぼしがないこと。
- 上記を `crates/moq-pub` と `crates/moq-sub` のテストで固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
