# Object Properties の LOC プロパティ取得で IMMUTABLE_PROPERTIES の内側も探索する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-object-immutable-properties-search
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §10.7 (Immutable Properties) は "When looking for the value of a property, processors MUST search both the mutable properties and the contents of Immutable Properties." と MUST を課す。現状の LOC プロパティ取得は最上位リストだけを見るため、準拠 publisher が Immutable Properties に入れた
Timestamp / Timescale / Video Config / Audio Config / Video Frame Marking を取得できない。

## 現状

- `src/loc.rs` の `LocProperties::varint_by_id` / `bytes_by_id` は最上位のみを走査する。
- `examples/moq-sub/src/pipeline.rs` は `LocProperties::decode` の accessor で構成情報・表示時刻を取得しており、Immutable Properties 内側に入れられると取得できない。
- `src/object_properties.rs` の `ObjectProperties` は内側探索を実装しており、LOC 経路だけが非対称である。
- docs/missing-features.md §4.1 に既知ギャップとして記録がある。既存の open 0125 は Track スコープ側の話で、Object スコープ側は射程外である。

## 設計方針

- `ObjectProperties` の内側探索と同じ規則 (最上位を優先し、無ければ IMMUTABLE_PROPERTIES の内側) で LOC プロパティを取得するヘルパを公開する。
- `LocProperties` を `ObjectProperties` から構築する経路、または `ObjectProperties` に LOC accessor を追加するかを実装時に決め、doc に残す。
- immutable-only のテストを追加する。

## 完了条件

- Immutable Properties の内側だけに Timestamp / Timescale / Video Config / Audio Config / Video Frame Marking がある Object から値を取得できること。
- 最上位と内側の両方にある場合は最上位が優先されること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
