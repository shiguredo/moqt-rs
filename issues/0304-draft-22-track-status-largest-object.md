# TRACK_STATUS_OK に LARGEST_OBJECT を自動補完する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-track-status-largest-object
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §9.20.17 (LARGEST OBJECT Parameter) は "If Objects have been published on this Track the Publisher MUST include this parameter." と MUST を課す。SUBSCRIBE_OK / PUBLISH / REQUEST_UPDATE_OK / PUBLISH_STATE_NOTIFY は publish 済み最大 Location
を自動補完しているが、TRACK_STATUS_OK だけが補完せず `largest_location: None` のまま応答する。

## 現状

- `src/session/track_status.rs` の `send_ok_for_track_status` は応答済み記録と `INCLUDE_PROPERTIES=0` の空化だけを行い、`publisher_track_largest` を使った LARGEST_OBJECT の注入を行わない。シグネチャも `parameters` を受け取らない。
- `src/session/core.rs` の `send_request_ok` の TRACK_STATUS 分岐も parameters への注入を行わない。
- closed 0015 / 0078 で同種の MUST に対応した履歴があるが、現行の `src/session/track_status.rs` には自動補完が無く、回帰している。
- `tests/test_session/track_status.rs` のコメントは LARGEST_OBJECT を「任意」としており、仕様と逆である。
- 影響: publish 済み Track への TRACK_STATUS 応答が MUST 違反になる。アプリは `publisher_track_largest` を公開 API から取得できないため自力で補完できない。

## 設計方針

- `send_ok_for_track_status` に `parameters: &mut MessageParameters` を渡し、`publisher_track_largest` が返す Location があれば LARGEST_OBJECT を載せる。
- `send_request_ok` の TRACK_STATUS 分岐で、アプリが明示した値より大きい場合だけ置き換える方針を他 context と揃える。
- Object 未 publish の Track では載せない。

## 完了条件

- publish 済み Track への TRACK_STATUS_OK に LARGEST_OBJECT が載ること。
- publish 前の Track では載らないこと。
- テストのコメントを仕様に合わせ、上記を単体テストで固定すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
