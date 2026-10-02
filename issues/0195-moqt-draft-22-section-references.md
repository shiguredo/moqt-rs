# コード内の draft-21 表記と節参照を draft-22 に同期する

- Created: 2026-10-02
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-draft-22-section-references
- Polished: {YYYY-MM-DD}

## 目的

draft-21 から draft-22 で節構成・節番号が移動し (例: LOCATION_FILTER は §9.20.10 → §9.20.9、Object Status は §11.1.2 → §11.1.1、Relay の Forward Handling は §7.5 → §7.2、Reserved Namespaces は §2.4.2 → §2.4.3)、引用文面も更新された。Rust コード内の `draft-ietf-moq-transport-21` 表記と節参照を一次資料 `refs/moq/draft-ietf-moq-transport-22.txt` と突合して同期し、仕様トレーサビリティを回復する。実装の挙動は変えない。

## 現状

- 節参照は多数ある (例: `src/message.rs` 119 箇所、`src/message_parameter.rs` 96 箇所、`src/track_properties.rs` 49 箇所、`src/object_properties.rs` 32 箇所、`src/name.rs` 23 箇所、`src/error.rs` 20 箇所、`src/subgroup_tracker.rs` 20 箇所)。`tests/` / `pbt/` / `examples/` / `fuzz/` にも分布する。
- `src/lib.rs` の crate doc と各モジュール doc にも「draft-ietf-moq-transport-21 に基づく」等の表記がある。
- 節番号の移動例: §9.20.10 → §9.20.9 (LOCATION FILTER)、§9.20.15 → §9.20.14 (TRACK PROPERTY FILTER)、§9.20.16 → §9.20.15 (FILL PARAMETERS)、§9.20.17 → §9.20.16 (EXPIRES)
- 節番号の移動例 (続き): §9.20.18 → §9.20.17 (LARGEST OBJECT)、§9.20.19 → §9.20.18 (FORWARD)、§9.20.20 → §9.20.19 (NEW GROUP REQUEST)、§9.20.21 → §9.20.20 (TRACK NAMESPACE PREFIX)
- 節番号の移動例 (続き): §9.20.22 → §9.20.21 (INCLUDE PROPERTIES)、§11.1.2 → §11.1.1 (Object Status)、§7.5 → §7.2 (Paused Subscription Handling)、§2.4.2 → §2.4.3 (Reserved Namespaces)。番号が同一でも文面が変わった引用がある。
- 過去に同じ同期作業を行っている (`issues/closed/0038-doc-draft-21-section-references.md`)。その際の対象は `src/` と `tests/` であり、今回は未完了の `pbt/` / `examples/` / `fuzz/` も対象に含める。
- 0191 / 0192 / 0193 / 0194 の実装で変更される箇所と重複する場合は、各 issue の修正を優先する。

## 設計方針

- 対象はリポジトリ内の Rust コード (`src/` / `tests/` / `pbt/` / `examples/` / `fuzz/`) の draft-21 表記と節参照とする。
- ripgrep で `draft-ietf-moq-transport-21` と `draft-21`、および `§` 参照を全件列挙し、節番号・節タイトル・引用文を `refs/moq/draft-ietf-moq-transport-22.txt` と 1 件ずつ突合する。列挙漏れを防ぐため、修正後に再度全件を洗い出して未修正が残っていないことを確認する。
- 実装の挙動は変えない (コメント・doc コメントのみ)。
- draft-22 に存在しない節・文面を引用している場合は、同じ規範の draft-22 上の正しい節へ付け替える。付け替え先が不明な場合は削除せず、正確な根拠を確認してから修正する。
- `README.md` / `docs/` / `skills/` は利用者向けドキュメントとして 0196 が扱うため対象外とする。

## 完了条件

- `src/` / `tests/` / `pbt/` / `examples/` / `fuzz/` の Rust コードに draft-21 の節参照が残っていないこと。歴史的経緯を説明するために残す場合は、その理由が明記されていること
- 引用した節番号・節タイトル・引用文が draft-22 の一次資料と一致すること
- 実装の挙動が変わらないこと (既存テストが通ること)
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `prek run --all-files` が通ること

## 解決方法

{未着手}
