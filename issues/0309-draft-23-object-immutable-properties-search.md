# Object Properties の LOC プロパティ取得で IMMUTABLE_PROPERTIES の内側も探索する

- Created: 2026-10-19
- Completed: 2026-11-04
- Branch: develop
- Polished: 2026-10-19
- Refreshed: 2026-11-04

## 目的

draft-ietf-moq-transport-23 §10.7 (Immutable Properties) は "When looking for the value of a property, processors MUST search both the mutable properties and the contents of Immutable Properties." と MUST を課す。

draft-ietf-moq-loc-05 §3.1.2 (Immutable Properties) も、relay に visible なまま immutable にすべき LOC Properties を Immutable Properties として符号化することを SHOULD としている。

現状の LOC プロパティ取得は最上位リストだけを見るため、準拠 publisher が Immutable Properties に入れた Timestamp / Timescale / Video Config / Audio Config / Video Frame Marking / Audio Level を取得できない。

## 現状

- `src/loc.rs` の `LocProperties::varint_by_id` / `bytes_by_id` は最上位のみを走査する。`timestamp` / `timescale` / `video_config` / `audio_config` / `video_frame_marking` / `audio_level` の各 accessor はこの 2 つを経由するため、IMMUTABLE_PROPERTIES (0x0B) の内側に置かれた値は取得できない。
- `crates/moq-sub/src/pipeline.rs` の `extract_video_config` / `extract_timestamp_timescale` / `extract_audio_config` / `extract_video_keyframe` は `LocProperties::decode` の後で `props.iter()` を走査しており、最上位リストしか見ない。
  Immutable Properties の内側だけに Video Config / Timestamp / Timescale / Audio Config / Video Frame Marking がある Object では、構成情報・表示時刻・キーフレーム判定を取得できない。
- `src/object_properties.rs` の `ObjectProperties` は `find_varint` で mutable リストと IMMUTABLE_PROPERTIES の内側を探索し、`immutable_properties` で内側の生バイト列を返す。`LocProperties` は最上位しか探索せず、IMMUTABLE_PROPERTIES の内側を解釈するヘルパが無い。
- `docs/missing-features.md` §4.1 に既知ギャップとして記録がある。
- 既存の open 0125 は `TrackProperties` の Track スコープ側を対象としており、`LocProperties` の Object スコープ側は射程外である。0125 で追加する予定の `TrackProperties::find_bytes` は別の型であり、`LocProperties` に流用できない。

## 設計方針

- `LocProperties` に IMMUTABLE_PROPERTIES (0x0B) の内側探索を実装する。`ObjectProperties` / `TrackProperties` の getter と同じ規則で、最上位リストを先に探し、無ければ IMMUTABLE_PROPERTIES の内側を探す。IMMUTABLE_PROPERTIES の Property Type は `crate::track_properties::PROP_IMMUTABLE_PROPERTIES` を参照する。
- IMMUTABLE_PROPERTIES は奇数型の `LocPropertyValue::Bytes` として届き、そのバイト列は外側の `Properties Length` を伴わない delta-key KVP 列である (draft-ietf-moq-transport-23 §10.7 / §8.3 (Key-Value-Pair Structure))。`LocProperties::decode` は 0x0B を `Bytes` として保持しているため、そのバイト列を KVP として走査する。
- public な `find_varint(&self, prop_id: u64) -> Option<u64>` と `find_bytes(&self, prop_id: u64) -> Option<&[u8]>` を追加する。`find_bytes` は `self` 内のバイト列への参照を返し、内側の値を `Vec` にコピーしない。既存の accessor の戻り値型は変えない。
- 既存の `timestamp` / `timescale` / `video_config` / `audio_config` / `video_frame_marking` / `audio_level` は `find_varint` / `find_bytes` を使う。
- `iter()` は最上位リストのイテレータのままとする (mutable リストの表現)。Immutable Properties の内側を見たい呼び出し側は `find_varint` / `find_bytes` または既知 accessor を使う。
- `crates/moq-sub/src/pipeline.rs` の 4 つの抽出関数は `props.iter()` の走査をやめ、`find_bytes` / `find_varint` または既知 accessor に置き換える。これにより、Immutable Properties の内側だけに値がある Object でも構成情報・表示時刻・キーフレーム判定が成立する。
- `LocProperties` を `ObjectProperties` から構築する経路や、`ObjectProperties` に LOC accessor を追加する案は採らない。LOC 固有の型・値域検証を `src/loc.rs` に閉じ込め、既存の `ObjectProperties` / `TrackProperties` と同じ「型ごとの accessor + 内側探索」に揃える。
- 内側の KVP も `LocProperties::decode` と同じ重複プロパティ ID の検出と既知プロパティの値域・長さ検証 (Audio Level は 0-255、Video Frame Marking は 1-4 バイト) を再利用する。
- 内側の KVP をデコードできない場合や入れ子の IMMUTABLE_PROPERTIES を検出した場合は、既存の `ObjectProperties` / `TrackProperties` の getter と同じく探索結果を `None` とする。`LocProperties::decode` の戻り値とエラー分類は変えず、内側の検証は `find_varint` / `find_bytes` の探索時に行う。エラー分類の変更は本 issue の対象外とする。
- 最上位と内側の両方に同じプロパティがある場合は、既存の `ObjectProperties` / `TrackProperties` の getter と同じく最上位を優先する。LOC の既知プロパティは単一値であり、両方に置くことは §10.7 が複数の値を許すプロパティに限り両方への出現を許すことに反し、§8.4 / §12.1 条件 9 の malformed になるが、malformed 判定は本 issue の対象外とする。
- immutable-only のテストを `tests/test_loc.rs` に追加する。example の抽出関数のテストは `crates/moq-sub/src/pipeline.rs` の `#[cfg(test)]` に追加する。

## 完了条件

- IMMUTABLE_PROPERTIES の内側だけに Timestamp / Timescale / Video Config / Audio Config / Video Frame Marking / Audio Level がある Object から、`LocProperties` の `find_varint` / `find_bytes` と既知 accessor で値を取得できること。テストは `tests/test_loc.rs` に追加する。
- 最上位と内側の両方に同じプロパティがある場合、最上位の値が返ること。テストは `tests/test_loc.rs` に追加する。
- `crates/moq-sub/src/pipeline.rs` の `extract_video_config` / `extract_timestamp_timescale` / `extract_audio_config` / `extract_video_keyframe` が、Immutable Properties の内側だけに値がある Object から構成情報・表示時刻・キーフレーム判定を取得できること。テストは同ファイルの `#[cfg(test)]` に追加する。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

`LocProperties` に IMMUTABLE_PROPERTIES (0x0B) の内側を探索する公開 API を追加し、moq-sub の
LOC 抽出関数をそこへ接続した。

- `src/loc.rs` の `LocProperties` に `find_varint` / `find_bytes` を追加した。最上位 (mutable)
  リストを先に、次に IMMUTABLE_PROPERTIES (0x0B) の値を delta-key KVP 列として探索する。
  `find_bytes` は外側の `Bytes` を借用したまま返し、値のバイト列を `Vec` へコピーしない
- `LocProperties::decode` の KVP 走査を `walk_kv_pairs` に抽出し、decode と内側探索で共有した。
  内側の走査でも既存の decode と同じ検証 (Audio Level の値域、Video Frame Marking の長さ、
  65535 バイト上限、単一に限られる既知 ID の重複、入れ子の 0x0B) を行う。decode の戻り値と
  エラー分類は変えていない
- 既知 accessor 6 個 (`timestamp` / `timescale` / `video_frame_marking` / `audio_level` /
  `video_config` / `audio_config`) を `find_varint` / `find_bytes` 経由に変更した。
  `iter` / `len` / `is_empty` は最上位リストだけを対象にする
- `crates/moq-sub/src/pipeline.rs` の `extract_video_config` / `extract_timestamp_timescale` /
  `extract_audio_config` / `extract_video_keyframe` を `props.iter()` の走査から
  `LocProperties` の accessor へ置き換え、IMMUTABLE_PROPERTIES の内側だけに値がある Object でも
  構成情報・表示時刻・キーフレーム判定を取得できるようにした
- `tests/test_loc.rs` に内側だけの 6 プロパティ取得、最上位優先、破損・入れ子・65535 超・
  切り詰め・delta overflow・値形式不一致・先頭 0x0B が VarInt・後続エラーで有効値も返さない
  各ケースを追加した。`pbt/tests/prop_loc.rs` に内側探索の PBT を、
  `fuzz/fuzz_targets/fuzz_decode_loc_properties.rs` に decode 後の accessor / find_* 呼び出しを
  追加した。`crates/moq-sub/src/pipeline.rs` の `#[cfg(test)]` に内側だけの値・内側 malformed・
  キーフレーム代用の契約テストを追加した
- `docs/missing-features.md` の §4.1 を「内側の探索は実装済み、Session の受信経路で残るのは
  内側だけの LOC 固有値域・長さ違反の MALFORMED_KEY_VALUE 通知であり、ライブラリ単体では
  cross-list 重複の検出と decode / encode の内側構造検証も残る」に更新し、
  `skills/shiguredo-moqt/SKILL.md` の LOC API 一覧に `find_varint` / `find_bytes` と探索規則、
  `len` / `is_empty` / `iter` の範囲を追記した

既知の制約: IMMUTABLE_PROPERTIES の内側だけにある LOC 固有の値域・長さ違反は、既存の
`ObjectProperties` / `TrackProperties` の getter と同じく探索結果を `None` とし、
§8.3 (Key-Value-Pair Structure) が要求する `MALFORMED_KEY_VALUE` によるセッション終了を
行わない。エラー分類の変更は本 issue の対象外であり、`docs/missing-features.md` §4.1 に
既知ギャップとして記録した。

## 完了条件の検証

| 完了条件 | 検証 |
| --- | --- |
| IMMUTABLE_PROPERTIES の内側だけに 6 プロパティがある Object から accessor で取得できること | `tests/test_loc.rs` の `finds_known_properties_only_inside_immutable_properties`、`pbt/tests/prop_loc.rs` の `immutable_properties_are_searched_like_top_level` |
| 最上位と内側の両方に同じプロパティがある場合に最上位が返ること | `tests/test_loc.rs` の `top_level_properties_win_over_immutable_properties` |
| moq-sub の 4 抽出関数が内側だけに値がある Object から構成情報・表示時刻・キーフレーム判定を取得できること | `crates/moq-sub/src/pipeline.rs` の `extract_*_reads_immutable_properties` 4 件 |
| `cargo test --workspace` / clippy / fmt が通ること | `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` を実行し、すべて成功した (Linux では libpulse 依存のため libpulse を展開した代替環境で実行) |

検証コマンド `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` /
`cargo test --workspace` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` は
すべて成功した。
