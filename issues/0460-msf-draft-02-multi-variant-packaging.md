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

現行実装は `keyframeSpacing` を持たず、multiple variant の Group 割り当てを揃える仕組みもない。

## 現状

- `MsfTrack` / `MsfCloneTrack` に `keyframe_spacing` に相当するフィールドがない。v02 カタログの値は未知フィールドとして無視される。
- `crates/moq-pub/src/catalog.rs` の `build_catalog` は `render_group` を設定するが `alt_group` を設定せず、映像 track を 1 本しか作らない。
- `crates/moq-pub/src/pipeline.rs` は `ef.is_keyframe` で Group を切り替えるため、単一 track では「Group 先頭がキーフレーム」「1 Group 1 キーフレーム」を満たす。
- `crates/moq-pub/src/group_id.rs` の `allocate_initial_group_id` は track ごとに壁時計ベースの値を払い出す。alternate group 内で同一の開始 Group ID を割り当てる仕組みはない。
- v02 §5.2.46 は `keyframeSpacing` を keyframe interval の Object 数として定義する。

## 設計方針

- `MsfTrack.keyframe_spacing: Option<u64>` を追加し、wire 上の `keyframeSpacing` を保持する。`MsfCloneTrack` にも追加する。
- `alternateGroup` が同じ track 群について、`keyframeSpacing` の有無と、開始 Group ID を共有して 1 ずつ増やす割り当てを publisher 側に実装する。Group 割り当ては `allocate_initial_group_id` を拡張し、alternate group 単位で開始 ID を決める。
- 「Group 先頭がキーフレーム」「1 Group 1 キーフレーム」を、複数 variant を送る場合にも検証できるテストを追加する。
- Object 境界の time-alignment は、variant ごとの encoder 出力が同じ Object 境界を持つことに依存する。`crates/moq-pub` が複数 variant を生成する経路自体は未実装のため、本 issue では複数 track を入力として受け取れる割り当て API と検証までを対象にする。
- subscriber 側の切り替え手順は別 issue で扱う。

## 完了条件

- `"keyframeSpacing": 30` を持つカタログが decode / encode で往復一致すること。
- alternate group 内の track 群が同一の開始 Group ID を持ち、以降 1 ずつ増える Group ID で送信されること。
- alternate group 内で `keyframeSpacing` を持たない track がある場合に、publisher の packaging 違反として検出できること。
- Group 先頭がキーフレームでない、または 1 Group に複数キーフレームが入る状態を検出するテストがあること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
