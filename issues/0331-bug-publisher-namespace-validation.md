# moq-pub が SUBSCRIBE / FETCH の Track Namespace を検証する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-publisher-namespace-validation
- Polished: {YYYY-MM-DD}

## 目的

Track は Track Namespace + Track Name で識別される (draft-ietf-moq-transport-22 §9.6 / §9.11)。moq-pub は受信 SUBSCRIBE / FETCH / REQUEST_UPDATE の track name だけを見て受理するため、別 namespace の要求に自分の namespace のコンテンツを返し得る。誤った配信を防ぐ。

## 現状

- `examples/moq-pub/src/pipeline.rs` の `serve_peer_request` は `subscribe.track_name` だけで track を判定し、`subscribe.track_namespace` を比較しない。
- FETCH も `fetch.track_name` だけで catalog / メディアを判定し、`fetch.track_namespace` を見ない。`serve_media_fetch` は常に `config.namespace` の cache を引く。
- REQUEST_UPDATE も `subscription_track_name` の名前だけで映像購読と判定する。
- 影響: 別 namespace の SUBSCRIBE / FETCH に自分のコンテンツを返す。relay 経由でも起こり得る。

## 設計方針

- `subscribe.track_namespace` / `fetch.track_namespace` を `config.namespace` と比較し、不一致は `REQUEST_DOES_NOT_EXIST` で拒否する。
- 判定を `is_published_track(namespace, name)` のようなヘルパに切り出し、単体テストを追加する。
- catalog FETCH も同じ検証を通す。

## 完了条件

- 別 namespace の SUBSCRIBE / FETCH / REQUEST_UPDATE が `REQUEST_DOES_NOT_EXIST` で拒否されること。
- 自分の namespace の要求は従来どおり受理されること。
- 上記を example の単体テストで固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
