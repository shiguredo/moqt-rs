# keyframeSpacing と multi-variant publisher packaging に対応する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §4.3 (Packaging for multi-variant track switching) は、alternate group 内で keyframe 間隔の異なる複数 track を配信する場合の条件を定めた。

- 各 MOQT Group はキーフレームで始まらなければならない (MUST)。1 Group に複数のキーフレームを置いてはならない (MUST NOT)。
- alternate group 内の全 track は、Group 境界だけでなく Object 境界でも time-aligned でなければならない (MUST)。
- 各 track は `keyframeSpacing` (§5.2.46) を指定しなければならない (MUST)。
- alternate group 内の全 track の開始 Group ID は同一で、以降の Group ID は 1 ずつ増えなければならない (MUST)。

§5.2.46 の `keyframeSpacing` は Required: Optional で、上記の MUST は keyframe 間隔が異なる複数 track を配信する場合に適用される。現行実装は `keyframeSpacing` を持たず、multiple variant の Group 割り当てを揃える仕組みもない。

## 現状

- `MsfTrack` / `MsfCloneTrack` に `keyframe_spacing` に相当するフィールドがない。v02 カタログの値は未知フィールドとして無視される。
- `crates/moq-pub/src/catalog.rs` の `build_catalog` は `Option<&VideoTrackParams>` を 1 本しか受け取らず、`alt_group` / `keyframe_spacing` も持たない。映像 track を 1 本しか作らない。
- `crates/moq-pub/src/pipeline.rs` の映像 encoder は 1 本で、複数 variant を送信する経路がない。
- `crates/moq-pub/src/group_id.rs` の `allocate_initial_group_id` は catalog / video / audio で独立に呼ばれる。alternate group 内で同一の開始 Group ID を割り当てる仕組みはない。
- 単一 track の publisher は `ef.is_keyframe` で Group を切り替えるため、「Group 先頭がキーフレーム」「1 Group 1 キーフレーム」を満たす。
- §4.3 の Object 境界の time-alignment は、variant ごとの encoder 出力が同じ Object 境界を持つことに依存する。複数 variant の encoder は未実装である。

## 設計方針

- `MsfTrack.keyframe_spacing: Option<u64>` と `MsfCloneTrack.keyframe_spacing: Option<u64>` を追加し、wire 上の `keyframeSpacing` を保持する。clone は明示値があれば上書きし、なければ親から継承する。
- alternate group 内の複数 variant に Group ID を割り当てる API を追加する。入力は alternate group の variant の並び、出力は共有する開始 Group ID と、各 variant の開始 Group ID とする。開始 ID は同一で、以降の Group ID は track ごとに 1 ずつ増える。encoder を使わず単体テストできる形にする。
- 複数 variant を配信する publisher 専用の packaging 検証 API を追加する。`keyframeSpacing` が全 variant にあること、開始 Group ID が同一であること、Group 先頭がキーフレームで 1 Group 1 キーフレームであることを、入力された Group / Object のメタデータで検証する。
- これらの検証は catalog の decode / encode / delta 適用後の共通検証には入れない。§4.3 の MUST は keyframe 間隔が異なる場合に限られ、共通検証に入れると `altGroup` だけを持つ既存カタログを誤って拒否する。0449 の altGroup 検証とも分ける。
- Object 境界の time-alignment は複数 variant の encoder が未実装のため本 issue の対象外とし、その旨を doc に明記する。
- 複数 variant の catalog 生成と実送信は、encoder の複数 variant 対応後に行う。本 issue はフィールド、割り当て API、publisher 専用の検証 API までを対象とする。

## 完了条件

- `"keyframeSpacing": 30` を持つカタログが decode / encode で往復一致すること。
- `MsfCloneTrack` が `keyframeSpacing` を親から継承し、明示値で上書きできること。
- alternate group の variant 群に Group ID を割り当てる API が、同一の開始 Group ID と track ごとの +1 を返すこと (単体テスト)。
- publisher 専用の packaging 検証 API が、`keyframeSpacing` 欠如、開始 Group ID の不一致、Group 先頭がキーフレームでない状態、1 Group に複数キーフレームがある状態を拒否すること。
- 共通の catalog 検証 (decode / encode / delta 適用後) の挙動が変わらないこと。`altGroup` を持ち `keyframeSpacing` を持たない既存カタログが引き続き受理されること。
- Object 境界の time-alignment が本 issue の対象外であることが doc に明記されていること。
- 上記を `crates/moq-pub` のテストと `tests/test_msf/encode_decode_roundtrip.rs`、`tests/test_msf/delta_apply.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
