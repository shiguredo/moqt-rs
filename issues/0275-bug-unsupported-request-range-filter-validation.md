# 未対応 request の Range Filter を §3.3.2 の MUST で検証する

- Created: 2026-10-15
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-unsupported-request-range-filter-validation
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §3.3.2 (Range Filters) の 2 つの MUST を未対応 request の経路でも満たす。

> If the same combination of Parameter Type, SetID, and Property Type (only in the Track and Object Property Filters) repeat in any message, an endpoint MUST reject this with REQUEST_ERROR with error code INVALID_FILTER.

> Range Filters are only allowed if the setup option MAX_FILTER_RANGES is non-zero, which limits
> the total number of Ranges allowed in all Range Filter parameters for a given subscription or
> fetch. If this limit is exceeded, an endpoint MUST reject this with REQUEST_ERROR with error code
> INVALID_FILTER.

## 現状

- `src/session/core.rs` の `Session::check_incoming_range_filters` が自側の `MAX_FILTER_RANGES`
  (`Session::local_max_filter_ranges`) との比較と `MessageParameters::validate_range_filters`
  による重複・デルタ溢出の検証を行う。受理する request では `handle_peer_subscribe` /
  `handle_peer_publish` (src/session/subscription/recv.rs)、`handle_peer_fetch` /
  `handle_peer_request_update` (src/session/fetch.rs)、`handle_peer_request_update`
  (src/session/subscription/recv.rs) が呼ぶ。
- `src/session/core.rs` の `Session::handle_unsupported_request` は本体を解析してパラメータの
  スコープ (§9.20.1) を検証するが、Range Filter の内容は検証せず `NOT_SUPPORTED` を返す。
- SUBSCRIBE_TRACKS の許可集合は Range Filter 5 種 (0x25-0x29) を含むため、同一 (Parameter Type,
  SetID, Property Type) の重複や `MAX_FILTER_RANGES` を超える Range 総数を持つ未対応 request が、
  `INVALID_FILTER` ではなく `NOT_SUPPORTED` で拒否される。
- したがって、Range Filter の MUST を満たすために必要な検証が未対応 request の経路に無い。

## 設計方針

- `handle_unsupported_request` で `check_incoming_range_filters` を呼び、違反時は `REQUEST_ERROR` の `INVALID_FILTER` で拒否してセッションを維持する。
- 検証順は「セッション終了を招く検証 → メッセージ単位の reject」を維持する。スコープ検証 (§9.20.1 の MUST) と Range Filter 検証の順序、および §8.9 の一意性検証・REGISTER 適用との順序を決めて doc に残す。
- PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE は Range Filter を許可しないため、スコープ検証で先に `PROTOCOL_VIOLATION` になる。3 種共通で呼んでも実害はないが、順序をテストで固定する。
- 累積の Range 総数 (subscription 単位) は未対応 request では subscription を作らないため、1 メッセージ内の検証に限る。

## 完了条件

- 同一 (Parameter Type, SetID, Property Type) の重複を持つ SUBSCRIBE_TRACKS が `REQUEST_ERROR` の `INVALID_FILTER` で拒否され、セッションが維持されること。
- `MAX_FILTER_RANGES` を超える Range 総数を持つ SUBSCRIBE_TRACKS が同じく `INVALID_FILTER` で拒否されること。
- PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE に Range Filter が載った場合は、従来どおり `PROTOCOL_VIOLATION` のままであること。
- 上記が `tests/test_session/request_stream.rs` で固定されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
