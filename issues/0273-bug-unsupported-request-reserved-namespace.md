# 未対応 request の `.` と `.session` 名前空間を DOES_NOT_EXIST で拒否する

- Created: 2026-10-15
- Completed: 2026-10-23
- Branch: feature/fix-unsupported-request-reserved-namespace
- Polished: 2026-10-23

## 目的

draft-ietf-moq-transport-22 §2.4.3 (Reserved Namespaces) は "A Track Namespace whose first field
is exactly . (a single period, 0x2e) is reserved and MUST NOT be used for any purpose; endpoints
MUST NOT publish tracks or namespaces under it and MUST reject requests referencing it with
DOES_NOT_EXIST." と MUST を課す。また §6.5 (Session-Level Tracks and Namespaces) は "An endpoint
that receives a request for an unrecognized session-level track or namespace MUST reject it with
REQUEST_ERROR using error code DOES_NOT_EXIST rather than passing it to the Application." と MUST
を課す。未対応 request (PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS) の経路でも
この 2 つの MUST を満たす。

## 現状

- `src/session/core.rs` の `Session::handle_unsupported_request` は
  `decode_unsupported_request_body` で本体を解析するが、Track Namespace Prefix は
  `TrackNamespace::decode_from` で読み進めるだけで保持も検証もしていない。
  `decode_unsupported_request_body` の doc は「予約名前空間 (draft §2.4.3) の
  `DOES_NOT_EXIST` 検証は、未対応 request を NOT_SUPPORTED で拒否する本経路では行わない
  (SUBSCRIBE 系の受理経路が別途検証する)」と明記している。
- 受理する request の経路は `TrackNamespace::is_single_period` と
  `TrackNamespace::is_session_level` で拒否している (`src/session/subscription/recv.rs` の
  `accept_incoming_track_request` / `handle_peer_publish` / `handle_peer_subscribe_tracks`、
  `src/session/fetch.rs` の `handle_peer_fetch`、`src/session/track_status.rs`)。送信側も
  `src/session/subscription/send.rs` の `send_subscribe` / `send_publish`、
  `src/session/fetch.rs`、`src/session/track_status.rs` で検証している。
- そのため先頭フィールドが `.` 単独または `.session` の Track Namespace Prefix を持つ未対応
  request は、`REQUEST_ERROR` の `NOT_SUPPORTED` で拒否されるだけで `DOES_NOT_EXIST` に
  ならない。
- `Session::handle_unsupported_request` の現在の検証順は次のとおりである。
  1. Request ID の parity と重複
  2. 本体の解析 (失敗は原因に応じた終了コードでセッション終了)
  3. 本体の Request ID とメッセージの Request ID の一致 (不一致はセッション終了)
  4. パラメータのスコープ検証 (違反は `PROTOCOL_VIOLATION` でセッション終了)
  5. AUTHORIZATION_TOKEN の一意性 (違反は REGISTER 適用後に `MALFORMED_AUTH_TOKEN` で
     メッセージ単位に拒否)
  6. REGISTER の token cache への適用 (失敗はセッション終了)
  7. `GOING_AWAY` (control GOAWAY 送信済み) または `NOT_SUPPORTED` でメッセージ単位に拒否
- §2.4.3 第 1 段落は 3 文で構成される。1 文目は先頭フィールドが `.` で始まるすべての Track
  Namespace を予約し、2 文目は IANA 登録があるまで使用を禁じ、3 文目は未認識の予約名前空間への
  request を Application へ渡すことを MUST とする。本ライブラリには未対応 request を
  Application へ渡す経路 (該当する `SessionEvent` / `RecvRequestError` の variant) が無いため、
  3 文目の MUST は満たせない。`.foo` など `.` 単独でも `.session` でもないプレフィックスは
  本 issue の対象外であり、現行どおり `NOT_SUPPORTED` のままとする。
- `docs/moqt.md` の「未対応」節は拒否コードの優先順を `MALFORMED_AUTH_TOKEN` > `GOING_AWAY` >
  `NOT_SUPPORTED` とし、本体解析時の検証一覧に §2.4.3 / §6.5 を含めていない。
  `src/session/core.rs` の `Session::recv_request`、`src/session/types.rs` の `RequestKind`、
  `src/message.rs` の `ControlMessage::Unsupported` の doc も同様に古い。

## 設計方針

- `decode_unsupported_request_body` が Track Namespace Prefix も返すようにし、
  `handle_unsupported_request` で `is_single_period` と `is_session_level` を検証する。
  該当時は `REQUEST_ERROR` の `DOES_NOT_EXIST` (0x10) で拒否し、セッションは維持する
  (受理経路の拒否と同じ扱い)。3 種は共通構造 (Request ID → Track Namespace Prefix →
  Number of Parameters → Parameters) のため、検証は 1 箇所で足りる。
- 検証順を次の 1〜7 に固定する。
  1. Request ID の parity と重複 (`validate_peer_request_id`)
  2. 本体の解析 (`decode_unsupported_request_body`)
  3. 本体の Request ID とメッセージの Request ID の一致
  4. パラメータのスコープ検証 (§9.20.1 の MUST。違反はセッション終了)
  5. AUTHORIZATION_TOKEN の一意性 (§8.9 の MUST。違反は REGISTER 適用後に
     `MALFORMED_AUTH_TOKEN` でメッセージ単位に拒否)
  6. REGISTER の token cache への適用 (§8.9 の MUST。失敗はセッション終了)
  7. Range Filter の検証 (0275 が追加する。`INVALID_FILTER` でメッセージ単位に拒否)
  8. 予約・セッション名前空間の検証 (該当時は `DOES_NOT_EXIST` でメッセージ単位に拒否)
  9. `GOING_AWAY` (control GOAWAY 送信済み) または `NOT_SUPPORTED` でメッセージ単位に拒否

  予約名前空間の拒否を 6 の後ろに置くのは、§8.9 の REGISTER 適用 MUST ("even if the message
  fails for other reasons") を満たすためである。また 8 を 9 より前に置くのは、§2.4.3 / §6.5 の
  `DOES_NOT_EXIST` が MUST であり、§9.2 の `GOING_AWAY` (MAY) と §1.6 の `NOT_SUPPORTED`
  (SHOULD) より優先するためである。
- メッセージ単位の拒否の優先順は `MALFORMED_AUTH_TOKEN` > `DOES_NOT_EXIST` > `GOING_AWAY` >
  `NOT_SUPPORTED` とする。セッション終了を招く検証 (本体解析・Request ID 一致・スコープ違反・
  REGISTER 適用失敗) はこれらより常に先である。`MALFORMED_AUTH_TOKEN` を先にするのは
  0237 で固定した既存の順序を維持するためである。
- 0274 (未対応 request の未登録 alias の扱い) の (a)/(b) は先取りしない。未登録 alias の扱いは
  現行のまま (受理経路はセッション終了、拒否経路はメッセージ拒否) とし、本 issue では変えない。
- 0275 (未対応 request の Range Filter 検証) が同じ関数へ `INVALID_FILTER` を追加する。
  受理経路 (`src/session/subscription/recv.rs` の `accept_incoming_track_request` など) では
  Range Filter の検証が予約名前空間の検証より先であり、0056 も Range Filter の拒否の優先順位は
  0275 に委ねている。したがって本 issue は予約名前空間の検証を `INVALID_FILTER` の後ろ (0275 が
  挿入する検証の直後、`GOING_AWAY` / `NOT_SUPPORTED` の前) に置く。
- `.foo` など `.` 単独でも `.session` でもない `.` 始まりのプレフィックスは対象外とする。
  §2.4.3 第 1 段落 3 文目の Application 委譲 MUST は、未対応 request を Application へ渡す
  経路が無いため満たせない。現行どおり `NOT_SUPPORTED` のままであることをテストで固定し、
  この制約を doc に残す (Application へ渡す API の追加は別 issue とする)。
- 次の doc を実装に合わせて更新する。
  - `src/session/core.rs` の `Session::recv_request` / `Session::handle_unsupported_request` /
    `decode_unsupported_request_body` の doc (`decode_unsupported_request_body` は prefix を
    返すことと `# Errors` を維持し、「この経路では `DOES_NOT_EXIST` 検証を行わない」という
    記述を削除する)
  - `src/session/types.rs` の `RequestKind` の doc
  - `src/message.rs` の `ControlMessage::Unsupported` の doc
  - `docs/moqt.md` の「未対応」節 (拒否コードの優先順と検証一覧に §2.4.3 / §6.5 を追加する)

## 完了条件

- 先頭フィールドが `.` 単独の Track Namespace Prefix を持つ 3 種 (PUBLISH_NAMESPACE /
  SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS) の未対応 request が、`REQUEST_ERROR` の
  `DOES_NOT_EXIST` で拒否され、セッションが維持されること。
- 先頭フィールドが `.session` の 3 種の未対応 request も同じく `DOES_NOT_EXIST` で拒否され、
  セッションが維持されること。
- `.foo` など `.` 単独でも `.session` でもないプレフィックスを持つ未対応 request は、
  従来どおり `NOT_SUPPORTED` で拒否されること。
- 次の優先順がテストで固定されていること。セッション終了を招く検証 (本体解析・Request ID
  不一致・スコープ違反・REGISTER 適用失敗) はメッセージ単位の拒否より先である。
  - `.` + 重複トークン: `MALFORMED_AUTH_TOKEN`
  - `.` + 自側 control GOAWAY 送信済み: `DOES_NOT_EXIST` (`GOING_AWAY` より優先)
  - `.` + スコープ違反: `PROTOCOL_VIOLATION` でセッション終了
  - `.` + REGISTER の cache 容量超過: `AUTH_TOKEN_CACHE_OVERFLOW` でセッション終了
  - `.` の拒否時も REGISTER が token cache に登録されること
- `tests/test_session/request_stream.rs` で固定されていること。既存の
  `unsupported_request_body_with_raw_parameters` は Track Namespace Prefix が `live` 固定
  なので、プレフィックスを指定できるヘルパを追加すること。
- 未登録 alias の既存挙動 (受理経路のセッション終了、拒否経路のメッセージ拒否) が変わらないこと。
- doc と実装が一致していること。「設計方針」に列挙した doc を更新すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。

## 解決方法

未対応 request の経路で、`.` 単独の予約名前空間と `.session` のセッション名前空間を
`DOES_NOT_EXIST` で拒否するようにした。

- `src/session/core.rs` の `decode_unsupported_request_body` が Track Namespace Prefix も
  返すようにし、`handle_unsupported_request` で `is_single_period` と `is_session_level` を
  検証した。該当時は `REQUEST_ERROR` の `DOES_NOT_EXIST` (0x10) でメッセージ単位に拒否し、
  セッションは維持する。検証順は Request ID → 本体解析 → 本体 Request ID 一致 → スコープ検証
  → AUTHORIZATION_TOKEN 一意性 → REGISTER 適用 → 予約・セッション名前空間 →
  `GOING_AWAY` / `NOT_SUPPORTED` である。§8.9 の REGISTER 適用 MUST (失敗したメッセージでも
  登録する) を先に満たすため REGISTER 適用の後ろに置き、§2.4.3 / §6.5 の MUST を §9.2 の MAY /
  §1.6 の SHOULD より優先した。0275 が同じ関数に追加する Range Filter 検証は、受理経路と
  同じく `INVALID_FILTER` を予約名前空間の検証より前に置く。
- `.foo` などその他の `.` 始まりの予約名前空間は、§2.4.3 第 1 段落 3 文目の Application 委譲
  MUST を本ライブラリが満たせない (未対応 request を Application へ渡す経路が無い) ため、
  従来どおり `NOT_SUPPORTED` のままとした。
- doc を実装に合わせて更新した。対象は `Session::recv_request` /
  `Session::handle_unsupported_request` / `decode_unsupported_request_body`、`RequestKind`、
  `ControlMessage::Unsupported`、`docs/moqt.md` の「未対応」節、example の
  `BidiMessage::PeerRequestRejected` である。拒否コードの優先順は `MALFORMED_AUTH_TOKEN` >
  `DOES_NOT_EXIST` > `GOING_AWAY` > `NOT_SUPPORTED` と明記した。
- `tests/test_session/request_stream.rs` にテストを追加した。3 種 × `.` / `.session` の
  `DOES_NOT_EXIST`、`.foo` の `NOT_SUPPORTED`、`["." , "x"]` の `DOES_NOT_EXIST`、および
  優先順 (`MALFORMED_AUTH_TOKEN` / `GOING_AWAY` / スコープ違反 / REGISTER 適用失敗 /
  REGISTER 適用) を固定した。Track Namespace Prefix を指定できるヘルパ (`namespace_prefix` /
  `unsupported_request_with_namespace*`) を追加した。予約名前空間の検証を無効化する変異で
  新テストが失敗することも確認した。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通る。
