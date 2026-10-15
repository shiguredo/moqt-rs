# 未対応 request の予約名前空間を DOES_NOT_EXIST で拒否する

- Created: 2026-10-15
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-unsupported-request-reserved-namespace
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §2.4.3 (Reserved Namespaces) は "A Track Namespace whose first field
is exactly . (a single period, 0x2e) is reserved and MUST NOT be used for any purpose; endpoints
MUST NOT publish tracks or namespaces under it and MUST reject requests referencing it with
DOES_NOT_EXIST." と MUST を課す。未対応 request (PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE /
SUBSCRIBE_TRACKS) の経路でもこの MUST を満たす。

## 現状

- `src/session/core.rs` の `Session::handle_unsupported_request` は `decode_unsupported_request_body` で本体を解析するが、Track Namespace Prefix は `TrackNamespace::decode_from` で読み進めるだけで保持も検証もしていない。
- 受理する request の経路は `TrackNamespace::is_single_period` で拒否している
  (`src/session/subscription/recv.rs` の `handle_peer_subscribe` / `handle_peer_publish`、
  `src/session/fetch.rs` の `handle_peer_fetch`、`src/session/track_status.rs` の
  `handle_peer_track_status`)。送信側も `src/session/subscription/send.rs` の
  `send_subscribe` / `send_publish` で検証している。
- そのため先頭フィールドが `.` 単独の Track Namespace Prefix を持つ未対応 request は、`REQUEST_ERROR` の `NOT_SUPPORTED` で拒否されるだけで `DOES_NOT_EXIST` にならない。

## 設計方針

- `decode_unsupported_request_body` が Track Namespace Prefix も返すようにし、`handle_unsupported_request` で `is_single_period` を検証する。違反時は `REQUEST_ERROR` の `DOES_NOT_EXIST` で拒否し、セッションは維持する (受理経路の拒否と同じ扱い)。
- 3 種は共通構造 (Request ID → Track Namespace Prefix → Number of Parameters → Parameters) のため、検証は 1 箇所で足りる。
- メッセージ単位の reject であるため、draft §8.9 (Authorization Token Compression) の REGISTER 適用 MUST を先に適用する (`handle_unsupported_request` の既存の順序に合わせる)。
- 検証順は「セッション終了を招く検証 → メッセージ単位の reject」を維持する。予約名前空間の拒否はメッセージ単位なので、本体解析・スコープ検証との前後関係を実装時に決めて doc に残す。
- §2.4.3 の 1 文目 (先頭フィールドが `.` で始まる未認識の予約名前空間は Application へ渡す) は、未対応 request を Application へ渡さない本ライブラリでは該当しない。対象は `.` 単独だけとし、`.foo` は対象外とする (`is_single_period` と同じ判定)。

## 完了条件

- 先頭フィールドが `.` 単独の Track Namespace Prefix を持つ 3 種の未対応 request が、`REQUEST_ERROR` の `DOES_NOT_EXIST` で拒否され、セッションが維持されること。
- `.` 以外のプレフィックスを持つ未対応 request の挙動 (NOT_SUPPORTED) が変わらないこと。
- 上記が `tests/test_session/request_stream.rs` で固定されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
