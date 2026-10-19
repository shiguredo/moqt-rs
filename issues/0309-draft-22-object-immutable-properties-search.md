# Object Properties の LOC プロパティ取得で IMMUTABLE_PROPERTIES の内側も探索する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-object-immutable-properties-search
- Polished: 2026-10-19

## 目的

draft-ietf-moq-transport-22 §10.7 (Immutable Properties) は "When looking for the value of a property, processors MUST search both the mutable properties and the contents of Immutable Properties." と MUST を課す。

draft-ietf-moq-loc-04 §3.1.2 (Immutable Properties) も、relay に visible なまま immutable にすべき LOC Properties を Immutable Properties として符号化することを SHOULD としている。

現状の LOC プロパティ取得は最上位リストだけを見るため、準拠 publisher が Immutable Properties に入れた Timestamp / Timescale / Video Config / Audio Config / Video Frame Marking / Audio Level を取得できない。

## 現状

- `src/loc.rs` の `LocProperties::varint_by_id` / `bytes_by_id` は最上位のみを走査する。`timestamp` / `timescale` / `video_config` / `audio_config` / `video_frame_marking` / `audio_level` の各 accessor はこの 2 つを経由するため、IMMUTABLE_PROPERTIES (0x0B) の内側に置かれた値は取得できない。
- `examples/moq-sub/src/pipeline.rs` の `extract_video_config` / `extract_timestamp_timescale` / `extract_audio_config` / `extract_video_keyframe` は `LocProperties::decode` の後で `props.iter()` を走査しており、最上位リストしか見ない。
  Immutable Properties の内側だけに Video Config / Timestamp / Timescale / Audio Config / Video Frame Marking がある Object では、構成情報・表示時刻・キーフレーム判定を取得できない。
- `src/object_properties.rs` の `ObjectProperties` は `find_varint` で mutable リストと IMMUTABLE_PROPERTIES の内側を探索し、`immutable_properties` で内側の生バイト列を返す。`LocProperties` は最上位しか探索せず、IMMUTABLE_PROPERTIES の内側を解釈するヘルパが無い。
- `docs/missing-features.md` §4.1 に既知ギャップとして記録がある。
- 既存の open 0125 は `TrackProperties` の Track スコープ側を対象としており、`LocProperties` の Object スコープ側は射程外である。0125 で追加する予定の `TrackProperties::find_bytes` は別の型であり、`LocProperties` に流用できない。

## 設計方針

- `LocProperties` に IMMUTABLE_PROPERTIES (0x0B) の内側探索を実装する。`ObjectProperties` / `TrackProperties` の getter と同じ規則で、最上位リストを先に探し、無ければ IMMUTABLE_PROPERTIES の内側を探す。IMMUTABLE_PROPERTIES の Property Type は `crate::track_properties::PROP_IMMUTABLE_PROPERTIES` を参照する。
- IMMUTABLE_PROPERTIES は奇数型の `LocPropertyValue::Bytes` として届き、そのバイト列は外側の `Properties Length` を伴わない delta-key KVP 列である (draft-ietf-moq-transport-22 §10.7 / §8.3 (Key-Value-Pair Structure))。`LocProperties::decode` は 0x0B を `Bytes` として保持しているため、そのバイト列を KVP として走査する。
- public な `find_varint(&self, prop_id: u64) -> Option<u64>` と `find_bytes(&self, prop_id: u64) -> Option<&[u8]>` を追加する。`find_bytes` は `self` 内のバイト列への参照を返し、内側の値を `Vec` にコピーしない。既存の accessor の戻り値型は変えない。
- 既存の `timestamp` / `timescale` / `video_config` / `audio_config` / `video_frame_marking` / `audio_level` は `find_varint` / `find_bytes` を使う。
- `iter()` は最上位リストのイテレータのままとする (mutable リストの表現)。Immutable Properties の内側を見たい呼び出し側は `find_varint` / `find_bytes` または既知 accessor を使う。
- `examples/moq-sub/src/pipeline.rs` の 4 つの抽出関数は `props.iter()` の走査をやめ、`find_bytes` / `find_varint` または既知 accessor に置き換える。これにより、Immutable Properties の内側だけに値がある Object でも構成情報・表示時刻・キーフレーム判定が成立する。
- `LocProperties` を `ObjectProperties` から構築する経路や、`ObjectProperties` に LOC accessor を追加する案は採らない。LOC 固有の型・値域検証を `src/loc.rs` に閉じ込め、既存の `ObjectProperties` / `TrackProperties` と同じ「型ごとの accessor + 内側探索」に揃える。
- 内側の KVP も `LocProperties::decode` と同じ重複プロパティ ID の検出と既知プロパティの値域・長さ検証 (Audio Level は 0-255、Video Frame Marking は 1-4 バイト) を再利用する。
- 内側の KVP をデコードできない場合や入れ子の IMMUTABLE_PROPERTIES を検出した場合は、既存の `ObjectProperties` / `TrackProperties` の getter と同じく探索結果を `None` とする。`LocProperties::decode` の戻り値とエラー分類は変えず、内側の検証は `find_varint` / `find_bytes` の探索時に行う。エラー分類の変更は本 issue の対象外とする。
- 最上位と内側の両方に同じプロパティがある場合は、既存の `ObjectProperties` / `TrackProperties` の getter と同じく最上位を優先する。LOC の既知プロパティは単一値であり、両方に置くことは §10.7 の「複数の値を許すプロパティに限り両方に置ける」に反するが、malformed 判定は本 issue の対象外とする。
- immutable-only のテストを `tests/test_loc.rs` に追加する。example の抽出関数のテストは `examples/moq-sub/src/pipeline.rs` の `#[cfg(test)]` に追加する。

## 完了条件

- IMMUTABLE_PROPERTIES の内側だけに Timestamp / Timescale / Video Config / Audio Config / Video Frame Marking / Audio Level がある Object から、`LocProperties` の `find_varint` / `find_bytes` と既知 accessor で値を取得できること。テストは `tests/test_loc.rs` に追加する。
- 最上位と内側の両方に同じプロパティがある場合、最上位の値が返ること。テストは `tests/test_loc.rs` に追加する。
- `examples/moq-sub/src/pipeline.rs` の `extract_video_config` / `extract_timestamp_timescale` / `extract_audio_config` / `extract_video_keyframe` が、Immutable Properties の内側だけに値がある Object から構成情報・表示時刻・キーフレーム判定を取得できること。テストは同ファイルの `#[cfg(test)]` に追加する。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
