# MSF カタログに accessibleWindowTime を追加する

- Created: 2026-11-16
- Completed: 2026-11-17
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §5.2.45 (Accessible Window Time) は、DVR ウィンドウを wallclock ミリ秒で表す `accessibleWindowTime` フィールドを新設した。同じ alternate group の track は同じ `accessibleWindowTime` を持たなければならない (MUST)。

§7 は publisher に timeline を accessible window の外まで書かないことを SHOULD、subscriber に timeline に基づく取得の前に accessible window の制約を適用することを MUST とする。timeline の配信・購読経路は未実装のため、本 issue はフィールドの追加とカタログ検証までを対象とする。

## 現状

- `MsfTrack` / `MsfCloneTrack` / `TrackJsonParts` / `CommonTrackFields` に `accessibleWindowTime` に相当するフィールドがない。
- `decode_common_track_fields` は未知フィールドを無視するため、v02 カタログの値は捨てられる。`write_track_json` も出力しない。
- `validate_group_target_latency` と `validate_group_buffers` は altGroup 内の一致を検証するが、いずれも `!is_live` と `None` を比較対象外にする。§5.2.45 には isLive=false の無視規定も、値の省略を許す規定もない。
- `MsfCloneTrack::into_track` は属性ごとに `self.x.or(parent.x)` で継承する。`accessibleWindowTime` を追加する場合、継承の実装とテストが必要になる。
- 既存のカタログ検証は `decode_full_catalog`、`validate_full_catalog_for_encode`、`validate_after_delta` の 3 入口から呼ばれ、`tracks` と `publishTracks` を分けて検証する。

## 設計方針

- `MsfTrack.accessible_window_time: Option<u64>` と `MsfCloneTrack.accessible_window_time: Option<u64>` を追加し、wire 上の `accessibleWindowTime` を wallclock ミリ秒として保持する。
- `MsfCloneTrack::into_track` で、明示値があれば上書きし、なければ親から継承する。
- 同じ `altGroup` を持つ track について、すべてが `accessibleWindowTime` を持たないか、すべてが同じ値を持つことのどちらかを要求する。一部だけが持つ場合と、値が異なる場合は `MessageError::InvalidCatalog` にする。isLive の値で例外にしない。
- 検証は `decode_full_catalog`、`validate_full_catalog_for_encode`、`validate_after_delta` の 3 入口から呼ぶ。`tracks` と `publishTracks` はそれぞれの集合内で altGroup ごとに検証する。
- `altGroup` を持たない track は単独で扱い、他の track との一致を要求しない。
- §7 の timeline の trim と subscriber の取得制約は、timeline の配信・購読経路の実装に依存するため本 issue では実装しない。その旨を issue と doc に残す。

## 完了条件

- `"accessibleWindowTime": 60000` を持つカタログが decode / encode で往復一致すること。
- 同じ `altGroup` の全 track が同じ値を持つカタログが受理されること。
- 同じ `altGroup` で値が異なるカタログが、decode と encode の両方で `MessageError::InvalidCatalog` になること。
- 同じ `altGroup` で一部の track だけが値を持つカタログが拒否されること。
- 同じ `altGroup` の全 track が値を持たないカタログが受理されること。
- `isLive` が false の track でも、同じ `altGroup` の一致規則が適用されること。
- `MsfCloneTrack` が `accessibleWindowTime` を親から継承し、明示値で上書きできること (`tests/test_msf/delta_apply.rs`)。
- delta 適用後の検証でも、同じ `altGroup` の不一致が拒否されること。
- `publishTracks` の `altGroup` も同じ規則で検証されること。
- 上記を `tests/test_msf/catalog_decode.rs`、`tests/test_msf/encode_decode_roundtrip.rs`、`tests/test_msf/delta_apply.rs`、`tests/test_msf/error_cases.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- `MsfTrack` / `MsfCloneTrack` に `accessible_window_time: Option<u64>` を追加し、wire の `accessibleWindowTime` を decode / encode するようにした。
- `MsfCloneTrack::into_track` で明示値があれば上書きし、なければ親から継承する。
- `validate_alt_group_accessible_window_time` を追加し、同じ altGroup の track がすべて値を持たないか、すべて同じ値を持つことを検証する。一部だけの保持、値の不一致、isLive=false の例外を拒否する。
- full decode / full encode / delta 適用後の 3 入口から、tracks と publishTracks をそれぞれ検証するようにした。
- `tests/test_msf/catalog_decode.rs` / `encode_decode_roundtrip.rs` / `delta_apply.rs` に、往復、altGroup の一致・不一致、publishTracks、clone の継承・上書き、delta 適用後の拒否を追加した。
- §7 の timeline trim と subscriber の取得制約は timeline 経路の実装に依存するため本 issue の範囲外であることを doc に明記した。

確認コマンド: `cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` が通ることを確認した。
