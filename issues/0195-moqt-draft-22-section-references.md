# コード内の draft-21 表記と節参照を draft-22 に同期する

- Created: 2026-10-02
- Completed: {YYYY-MM-DD}
- Branch: feature/update-draft-22-section-references
- Polished: 2026-10-02

## 目的

draft-21 から draft-22 で節構成・節番号が大きく変更された (例: LOCATION_FILTER は §9.20.10 → §9.20.9、Object Header は §11.1.1 → §2.1.1 (Object Fields)、
Object Status は §11.1.2 → §11.1.1、Publisher Interactions は §7.5 → §7.6、Reserved Namespaces は §2.4.2 → §2.4.3。
Forward Handling (§7.2) は番号不変のまま Paused Subscription Handling (§7.2) に改名されている)、引用文面も更新された。
Rust コード内の `draft-ietf-moq-transport-21` 表記と節参照を一次資料 `refs/moq/draft-ietf-moq-transport-22.txt` と突合して同期し、仕様トレーサビリティを回復する。実装の挙動は変えない。

## 現状

- `draft-ietf-moq-transport-21` / `draft-21` の表記は多数ある (例: `src/message.rs` 119 行、`src/message_parameter.rs` 96 行、`src/track_properties.rs` 49 行、
  `src/object_properties.rs` 32 行、`src/name.rs` 23 行、`src/error.rs` 20 行、`src/subgroup_tracker.rs` 20 行)。付随する `§` の節参照も同じファイル群に多数あり、`tests/` / `pbt/` / `examples/` / `fuzz/` にも分布する。
- `src/lib.rs` の crate doc と各モジュール doc にも「draft-ietf-moq-transport-21 に基づく」等の表記がある。
- 節番号の移動 (draft-21 の節番号 → draft-22 の節番号):
  - §9.20.2 (Allowed Parameters By Control Message) は削除され、許可パラメータの規定は各コントロールメッセージの節 (例: §9.3 (REQUEST_OK)) へ移動した
  - §9.20.3 (AUTHORIZATION TOKEN) → §9.20.2、§9.20.4 (SUBGROUP_DELIVERY_TIMEOUT) → §9.20.3、§9.20.5 (OBJECT_DELIVERY_TIMEOUT) → §9.20.4、
    §9.20.6 (FILL TIMEOUT) → §9.20.5、§9.20.7 (RENDEZVOUS TIMEOUT) → §9.20.6、§9.20.8 (SUBSCRIBER PRIORITY) → §9.20.7、
    §9.20.9 (GROUP ORDER) → §9.20.8、§9.20.10 (LOCATION FILTER) → §9.20.9
  - §9.20.11 (SUBGROUP FILTER) → §9.20.10、§9.20.12 (OBJECTID FILTER) → §9.20.11、§9.20.13 (PRIORITY FILTER) → §9.20.12、
    §9.20.14 (OBJECT PROPERTY FILTER) → §9.20.13、§9.20.15 (TRACK PROPERTY FILTER) → §9.20.14、§9.20.16 (FILL PARAMETERS) → §9.20.15、
    §9.20.17 (EXPIRES) → §9.20.16、§9.20.18 (LARGEST OBJECT) → §9.20.17、§9.20.19 (FORWARD) → §9.20.18、
    §9.20.20 (NEW GROUP REQUEST) → §9.20.19、§9.20.21 (TRACK_NAMESPACE_PREFIX) → §9.20.20、§9.20.22 (INCLUDE_PROPERTIES) → §9.20.21
  - §11.1.1 (Object Header) → §2.1.1 (Object Fields)、§11.1.2 (Object Status) → §11.1.1、§11.1.3 (Object Properties) → §11.1.2
  - §7.2 (Forward Handling) は番号不変のまま §7.2 (Paused Subscription Handling) に改名。Publisher Interactions は §7.5 → §7.6、Relay Track Handling は §7.6 → §7.7、Relay Object Handling は §7.7 → §7.8 (relay 節のためコードからは参照されていない)
  - §2.4.2 (Reserved Namespaces) → §2.4.3 (§2.4.2 に Namespace Prefix Matching が新設されたため)
  - 番号が同一でも文面が変わった引用がある
- 過去に同じ同期作業を行っている (`issues/closed/0038-doc-draft-21-section-references.md`)。その際の対象は `src/` と `tests/` であり、今回は未完了の `pbt/` / `examples/` / `fuzz/` も対象に含める。
- 0191 / 0192 / 0193 / 0194 の実装で変更される箇所と重複する場合は、各 issue の修正を優先する。

## 設計方針

- 対象はリポジトリ内の Rust コード (`src/` / `tests/` / `pbt/` / `examples/` / `fuzz/`) の draft-21 表記と節参照とする。
- ripgrep で `draft-ietf-moq-transport-21` と `draft-21`、および `§` 参照を全件列挙し、節番号・節タイトル・引用文を `refs/moq/draft-ietf-moq-transport-22.txt` と 1 件ずつ突合する。列挙漏れを防ぐため、修正後に再度全件を洗い出して未修正が残っていないことを確認する。
- `moq-transport` 以外の規約への節参照 (MSF / LOC / C4M / CMSF / RFC / ISO など) は対象外とし、`§` の全件列挙に混在していても `moq-transport-22` と突合して書き換えないこと (例: `tests/test_msf/uri.rs` の §11.1.2 は MSF の節、`examples/tokio-moq/src/lib.rs` の §7.1.1 は C4M の節であり、`moq-transport-22` の同じ番号とは無関係)。
- 実装の挙動は変えない (コメント・doc コメントのみ)。ALPN の `moqt-21` は IETF draft の ALPN 規則 (`moqt-` + draft 番号) により draft-22 でも正しいため変更しない。
- draft-22 に存在しない節・文面を引用している場合は、同じ規範の draft-22 上の正しい節へ付け替える。付け替え先が不明な場合は削除せず、正確な根拠を確認してから修正する。
- `README.md` (例: `examples/README.md`) / `docs/` / `skills/` は利用者向けドキュメントとして 0196 が扱うため対象外とする。

## 完了条件

- `src/` / `tests/` / `pbt/` / `examples/` / `fuzz/` の Rust コードに draft-21 の表記 (`draft-ietf-moq-transport-21` / `draft-21`) と節参照が残っていないこと (0191〜0194 が扱う重複箇所は各 issue が draft-22 へ更新するため対象外)。歴史的経緯を説明するために残す場合は、その理由が明記されていること
- 引用した節番号・節タイトル・引用文が draft-22 の一次資料と一致すること
- 実装の挙動が変わらないこと (既存テストが通ること)
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `prek run --all-files` が通ること

## 解決方法

{未着手}
