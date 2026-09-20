# recv_object_datagram 直呼びでも Object Datagram の status と properties を検証する

- Created: 2026-09-13
- Completed: 2026-09-17
- Branch: feature/fix-recv-object-datagram-blob-validation
- Polished: {YYYY-MM-DD}

## 目的

`Session::recv_object_datagram` に手組みの `ObjectDatagram` を渡した場合でも、wire 経路 (`recv_datagram` → `ObjectDatagram::decode`) と同じ protocol 検証を行い、不正な status と properties を受理しないようにする。公開 API の入口で draft の MUST / SHOULD を一貫して適用する。

## 現状

- wire 経路の `src/session/data.rs` の `recv_datagram` は `ObjectDatagram::decode` を呼び、次の検証を行い、違反時は `SESSION_PROTOCOL_VIOLATION` でセッションを閉じる。
  - 未定義 Type ビットと STATUS + END_OF_GROUP の同時指定 (`validate_object_datagram_type`)
  - Properties blob の Length 整合と Length = 0 の拒否 (`validate_datagram_properties_blob`)
  - Object Status の値域 (`validate_object_status`)
  - 非 Normal status への Properties 付与
- `recv_object_datagram` は decode 済みまたは手組みの `ObjectDatagram` を受け取るが、上記の検証を行わない。手組み構造体で確認したところ、次の入力が `Ok(Accepted)` になる。
  - `status = Some(0x5)` (未知 status): `largest_received_location` が更新される
  - `status = Some(0x3)` かつ `end_of_group = true`
  - `status = Some(0x3)` かつ `properties_data = Some(...)` (非 Normal status + Properties)
- Properties Length と実データ長が一致しない blob を `recv_object_datagram` に渡すと、`ObjectPropertyTracker::observe_object` の `"malformed track: invalid object properties"` として subscription だけが Terminated になる。wire 経路では decode の時点でセッションが閉じるため扱いが異なる。
- draft-ietf-moq-transport-21 §11.1.2 (Object Status) は次のように規定する。

  > Any other value SHOULD be treated as a protocol error and the session SHOULD be closed with a PROTOCOL_VIOLATION (Section 12.2).

- draft-ietf-moq-transport-21 §11.1.3 (Object Properties) / §11.2.1 (Object Datagram) は次のように規定する。

  > If an endpoint receives properties on an Object with status that is not Normal, it MUST close the session with a PROTOCOL_VIOLATION.

- `ObjectDatagram` は Object Payload を保持しないため、status の有無に応じた payload 長の規則 (status ありなら payload なし、status なしの zero-length の禁止) は Session では検証できない。この点は wire decode 層の責務である。

## 設計方針

- `recv_object_datagram` の入口 (Track Alias 解決と状態更新より前) で wire 経路と同じ検証を行う。
  - status の値域
  - STATUS と END_OF_GROUP の同時指定
  - properties blob の Length 整合と Length = 0 の拒否
  - 非 Normal status への Properties 付与
- 違反時は wire 経路と同じ `SESSION_PROTOCOL_VIOLATION` でセッションを閉じる。
- payload 長の規則が Session の責務外であることを `recv_object_datagram` の doc に明記する。
- 正常な `ObjectDatagram` の受信挙動と `TrackDataAcceptance` の返り値は変えない。

## 完了条件

- 手組みの `ObjectDatagram` で未知 status / STATUS + END_OF_GROUP / 非 Normal status + Properties / Length 不整合 blob / Length = 0 blob が、wire 経路と同じくセッションクローズ (`SESSION_PROTOCOL_VIOLATION`) で拒否されること
- 拒否時に `largest_received_location` などの subscription 状態が更新されないこと
- `ObjectDatagram::decode` 経由と手組みの両方で、正常な datagram の受信の既存挙動が維持されること
- 上記を検証するテストが追加されていること

## 解決方法

`src/session/data.rs` の `recv_object_datagram` の入口 (Track Alias 解決と状態更新より前) に、
wire 経路 (`recv_datagram` → `ObjectDatagram::decode`) と同じ protocol 検証を追加した。

- `ObjectDatagram` の struct から wire の Type Flags を復元し、
  `validate_object_datagram_type` で STATUS + END_OF_GROUP の同時指定を拒否する
- `validate_object_status` で status の値域 (0x0 / 0x3 / 0x4) を検証する
- `validate_datagram_properties_blob` で Properties Length の整合と Length = 0 を検証する
- 非 Normal status への Properties 付与を拒否する
- いずれも違反時は `session_error_from_data_message` を通して `SESSION_PROTOCOL_VIOLATION` に変換し、
  `self.fail(...)` でセッションを閉じてから `Err` を返す (wire 経路と同じ扱い)
- 検証は Track Alias 解決より前に行うため、拒否時に `largest_received_location` などの
  subscription 状態を汚染しない
- Object Payload 長の規則 (status がある場合は payload を保持しない、status がなく zero-length
  payload を禁止する) は `ObjectDatagram` が payload を保持しないため本 API では検証できない。
  `recv_object_datagram` の doc に wire decode 層の責務であることを明記した
- 正常な `ObjectDatagram` の受信挙動と `TrackDataAcceptance` の返り値は変えていない

テストは `tests/test_session/data_stream.rs` に 5 本追加した。

- `recv_object_datagram_rejects_unknown_status`: status 0x5 が `SESSION_PROTOCOL_VIOLATION` で
  拒否され、`largest_received_location` が `None` のままであること
- `recv_object_datagram_rejects_status_with_end_of_group`: STATUS + END_OF_GROUP が拒否されること
- `recv_object_datagram_rejects_properties_on_non_normal_status`: 非 Normal status + Properties が
  拒否されること
- `recv_object_datagram_rejects_malformed_properties_blob`: Properties Length と実データ長の
  不一致が拒否されること
- `recv_object_datagram_rejects_zero_length_properties_blob`: Properties Length = 0 が拒否されること

検証は `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
`cargo fmt --all -- --check` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` の通過で確認した。
