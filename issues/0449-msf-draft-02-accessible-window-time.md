# MSF カタログに accessibleWindowTime を追加する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §5.2.45 (Accessible Window Time) は、DVR ウィンドウを wallclock ミリ秒で表す `accessibleWindowTime` フィールドを新設した。同じ alternate group の track は同じ `accessibleWindowTime` を持たなければならない (MUST)。
§7 は publisher に timeline を accessible window の外まで書かないことを SHOULD、subscriber に timeline に基づく取得の前に accessible window の制約を適用することを MUST とする。
現行実装はこのフィールドを持たず、altGroup 間の一致検証もない。

## 現状

- `MsfTrack` / `MsfCloneTrack` / `TrackJsonParts` / `CommonTrackFields` に `accessibleWindowTime` に相当するフィールドがない。
- `decode_common_track_fields` は未知フィールドを無視するため、v02 カタログの値は捨てられる。`write_track_json` も出力しない。
- `altGroup` の一致検証として `validate_group_target_latency` と `validate_group_buffers` があるが、accessible window の一致検証はない。
- `MsfMediaTimeline` / `MsfEventTimeline` の取得制約にウィンドウを適用する処理はない。timeline の配信・購読経路自体が未実装である。

## 設計方針

- `MsfTrack.accessible_window_time: Option<u64>` と `MsfCloneTrack.accessible_window_time: Option<u64>` を追加し、wire 上の `accessibleWindowTime` を wallclock ミリ秒として保持する。
- `validate_group_target_latency` / `validate_group_buffers` と同じ形の検証を追加し、同じ `altGroup` を持つ track の `accessibleWindowTime` が一致しない場合に `MessageError::InvalidCatalog` にする。
- `altGroup` がない track は単独で検証し、値の一致は要求しない。
- §7 の「timeline の trim」と「subscriber の取得制約」は、timeline の配信・購読経路の実装に依存するため本 issue では実装しない。フィールドの追加とグループ検証までを対象範囲とし、その旨を issue に残す。

## 完了条件

- `"accessibleWindowTime": 60000` を持つカタログが decode / encode で往復一致すること。
- 同じ `altGroup` の track で `accessibleWindowTime` が異なるカタログが `MessageError::InvalidCatalog` で拒否されること。
- `altGroup` が異なる track 間で値が異なっても受理されること。
- `accessibleWindowTime` を持たない track の挙動が変わらないこと。
- 上記を `tests/test_msf/catalog_decode.rs` と `tests/test_msf/encode_decode_roundtrip.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
