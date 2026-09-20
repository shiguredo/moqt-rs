# REQUEST_OK codec の許可パラメータを draft-22 §9.3 に合わせる

- Created: 2026-10-02
- Completed: 2026-10-03
- Branch: feature/change-request-ok-param-scope
- Polished: 2026-10-02

## 目的

draft-22 §9.3 (REQUEST_OK) は応答 context ごとの許可パラメータを明示し、その和集合は EXPIRES と LARGEST_OBJECT のみである (A.1 #1916)。`src/message.rs` の `REQUEST_OK_ALLOWED_PARAMS` は draft-21 時点の設計判断で 12 型を広く受理しており、仕様が PROTOCOL_VIOLATION を求めるパラメータを codec 層が受理してしまう。

## 現状

- `REQUEST_OK_ALLOWED_PARAMS` は EXPIRES / LARGEST_OBJECT に加えて OBJECT_DELIVERY_TIMEOUT / SUBGROUP_DELIVERY_TIMEOUT / SUBSCRIBER_PRIORITY / LOCATION_FILTER / FORWARD / NEW_GROUP_REQUEST / SUBGROUP_FILTER / OBJECTID_FILTER / PRIORITY_FILTER / OBJECT_PROPERTY_FILTER の 10 型を受理する。コメントには「狭めるとデコード層エラーパスが変わる」ため意図的に広くしていると記されている。
- Session 層は `handle_peer_request_ok` と context 別定数 (`PUBLISH_OK_ALLOWED_PARAMS` / `REQUEST_UPDATE_OK_ALLOWED_PARAMS` / `TRACK_STATUS_OK_ALLOWED_PARAMS`) で正しく検証しており、Session 経由のエンドツーエンド挙動は適合している。
- codec (`ControlMessage::decode`) を単体で使う利用者には、仕様違反のパラメータが `ProtocolViolation` にならない。
- `pbt/tests/prop_message.rs` の `REQUEST_OK_PARAMS` は 12 型を前提に roundtrip を生成している。

## 設計方針

- `REQUEST_OK_ALLOWED_PARAMS` を `[PARAM_EXPIRES, PARAM_LARGEST_OBJECT]` に縮小し、draft-22 §9.3 / §9.20.1 の MUST に合わせる。
- Session 層の context 別検証は維持する (PUBLISH_OK / REQUEST_UPDATE_OK / TRACK_STATUS_OK の絞り込みは引き続き必要)。
- `pbt/tests/prop_message.rs` の `REQUEST_OK_PARAMS` を 2 型に変更する。
- `tests/test_message.rs` に codec 境界テストを追加する (EXPIRES / LARGEST_OBJECT は encode / decode 可、それ以外は `ProtocolViolation`)。
- `src/message.rs` と `tests/test_session/parameter_rules.rs` の「codec は和集合で検証する」旨のコメントを draft-22 の内容に更新する。
- codec の受理範囲が狭まるため `CHANGES.md` の `## develop` に `[CHANGE]` として記載する。

## 完了条件

- `REQUEST_OK_ALLOWED_PARAMS` が 2 型になり、codec でそれ以外を含む REQUEST_OK が `ProtocolViolation` になること
- Session 層の context 別検証が維持され、既存の session テストが通ること
- PBT と codec 境界テストが追加・更新されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `prek run --all-files` が通ること
- `CHANGES.md` の `## develop` に `[CHANGE]` エントリが追加されていること

## 解決方法

- `src/message.rs` の `REQUEST_OK_ALLOWED_PARAMS` を `[PARAM_EXPIRES, PARAM_LARGEST_OBJECT]` の 2 型に縮小し、draft-ietf-moq-transport-22 §9.3 (REQUEST_OK) の context 別列挙の和集合に合わせた。
  encode / decode の両経路で和集合外のパラメータを `PROTOCOL_VIOLATION` として拒否する (§9.20.1 (Parameter Scope) の MUST に対応)。
- `tests/test_message.rs` に `mod request_ok` を追加し、和集合外の 10 型すべてで encode / decode が `ProtocolViolation` になることと、和集合内の 2 型が encode できることを固定した。
- `pbt/tests/prop_message.rs` の `REQUEST_OK_PARAMS` を 2 型に追随させ、`request_ok_params_cover_all_types` で sampler が両型を生成すること (往復の網羅) を固定した。
- `tests/test_session/parameter_rules.rs` のコメントを draft-22 の内容に更新し、和集合内で context 外になる `PUBLISH_OK` + `LARGEST_OBJECT` を受信側で拒否するテストを追加した。
- codec の受理範囲が狭まることで、codec 層の decode 失敗が I/O 層へ直接届くようになったため、examples が decode 失敗を終了コード付きで閉じるようにした。
  `examples/tokio-moq` に `TransportError::Moqt(MessageError)` を追加し、`examples/moq-sub` / `examples/moq-pub` が `session_error_code` で `PROTOCOL_VIOLATION` (書式違反は `KEY_VALUE_FORMATTING_ERROR`) に写してからエラー終了する。
- `CHANGES.md` の `## develop` に `[CHANGE]` を追加した。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` / `prek run --all-files` が通ることを確認した。
