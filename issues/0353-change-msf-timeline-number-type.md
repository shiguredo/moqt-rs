# MSF Timeline の時刻フィールドの数値型と検証を見直す

- Created: 2026-10-19
- Completed: 2026-11-22
- Branch: feature/change-msf-timeline-number-type
- Polished: 2026-11-21

## 目的

draft-ietf-moq-msf-02 §7.1.1 (Explicit entry format) と §8.2 (Event Timeline data format)、§7.4.1 (Template Format) は時刻を JSON Number と規定する。
§7.1.1 はメディア PTS を「floor in integral milliseconds」と定め、値が整数ミリ秒であることを要求するが、負値を禁じる記述はない。
現行の `src/msf.rs` は `u64` と `parse_u64` で実装しており、整数値を持つ小数表記 (`2002.0`) と負のメディア PTS (`-1`) を拒否する。
時刻フィールドについて、仕様が許す表現と実装の受理範囲を一致させ、契約を公開 doc とテストで固定する。公開型のフィールド型を変更する破壊的変更を含む。

## 現状

- `parse_u64` は `nojson::RawJsonValue::as_integer_str()` で JSON の整数リテラルだけを受理し、`2002.0` や `2.002e3` のような整数値を持つ小数・指数表記を拒否する。負値 (`-1`) は `u64` への変換で拒否する。
- Media Timeline の `pts_ms`、Event Timeline の `MsfEventIndex::MediaPtsMs`、Template の `start_media_time` / `delta_media_time` は `u64` である。
- wallclock は `MsfMediaTimelineEntry::wallclock_ms`、`MsfEventIndex::WallclockMs`、`MsfTemplate::start_wallclock` / `delta_wallclock` のすべてが `u64` である。
- §7.1.1 はメディア PTS を「floor in integral milliseconds」と規定する。§7.4.1 の `startMediaTime` は §7.1.1 の定義を参照し、`deltaMediaTime` は「JSON Number in milliseconds」である。§8.2 の `M` も §7.1 の定義を参照する。これらの値は整数ミリ秒でなければならないが、負値を禁じる記述はない。
- wallclock は §11.6 (Wallclock time) で epoch からの経過ミリ秒と定義され、負値は意味を持たない。§7.1.1 は VOD や不明時に 0 を SHOULD とする。
- `MsfMediaTimelineEntry::pts_ms` / `wallclock_ms`、`MsfEventIndex`、`MsfTemplate` の公開 doc は型と単位を示すだけで、受理範囲と表現を明記していない。
- `src/msf.rs` の Timeline 関連 doc・コメントと `tests/test_msf/event_timeline.rs` のコメントは `draft-ietf-moq-msf-02 §8.1 (Event Timeline data format)` としているが、v02 の Event Timeline data format は §8.2 であり、この節番号も本 issue で修正する。
- `docs/msf.md` は `[[pts_ms, [group_id, object_id], wallclock_ms], ...]` 形式と pts_ms の非降順を示すだけで、受理する数値の範囲と表現を明記していない。
- `tests/test_msf` には decode の入力について `2002.0` / `-1` / `i64` / `u64` の範囲外の受理・拒否を固定するテストがない。`resolve_entry` の計算結果の overflow を確認するテストは `tests/test_msf/template.rs` にあるが、decode の数値範囲は未固定である。`pbt/tests/prop_msf.rs` と `fuzz/fuzz_targets/fuzz_msf_template.rs` はメディア時刻を `u64` として生成している。
- 影響: 仕様に適合する `2002.0` を送る実装を decode できず、負のメディア PTS を送る実装と相互運用できない。逆に、非整数値 (`2002.5`) を受理すると、§7.1.1 の「floor in integral milliseconds」に反する値を通してしまう。公開型のフィールド型が変わるため、`MsfMediaTimelineEntry` / `MsfEventIndex` / `MsfTemplate` を構築する利用側には破壊的変更になる。

## 設計方針

- メディア PTS を非負整数に限定する案は、仕様にない制限を残すため採用しない。メディア時刻は負値を許す `i64` とし、wallclock は §11.6 の epoch からの経過ミリ秒、Location の ID は MOQT varint のため `u64` とする。`deltaMediaTime` はメディア PTS の間隔として `i64` に従う。
- `deltaWallclock` は §7.4.1 が「constant interval」と定めるだけで負値を明示的に禁じていないが、wallclock は epoch からの経過ミリ秒で、メディア時刻の増加に伴い増加する系列を表すため `u64` とする。
- メディア時刻 (PTS) とその間隔を表すフィールドを `i64` にする:
  - `MsfMediaTimelineEntry::pts_ms`
  - `MsfEventIndex::MediaPtsMs`
  - `MsfTemplate::start_media_time` / `MsfTemplate::delta_media_time`
  - これに伴い `MsfTemplate::resolve_entry` が返す `MsfMediaTimelineEntry::pts_ms` も `i64` になる。
- wallclock と MOQT Location の ID は `u64` のままとする:
  - `MsfMediaTimelineEntry::wallclock_ms`
  - `MsfEventIndex::WallclockMs`
  - `MsfTemplate::start_wallclock` / `delta_wallclock`
  - `group_id` / `object_id` / `start_group_id` / `start_object_id` / `delta_group_id` / `delta_object_id`
- 時刻フィールドの decode は、JSON の字句形式ではなく数学的な値で判定する:
  - §7.1.1 の「floor in integral milliseconds」は、送信側が整数ミリ秒を送ることを要求する。受信側は非整数値を丸めずに拒否する。
  - 値が整数ミリ秒であること。`2002`, `2002.0`, `2.002e3`, `2002e0`, `100e-2` は受理し、`2002.5`, `2.0025e3`, `1e-1` は拒否する。`2002.0` は値が整数 2002 であるため受理し、`2002.5` は値が非整数であるため拒否する。字句形式（小数点・指数）は受理判定に使わない。
  - メディア PTS は `i64` の範囲の整数を受理する (`-1`, `-1.0` を含む)。範囲外は拒否する。
  - wallclock は `u64` の範囲の非負整数を受理する。負値は拒否する。
  - 判定は f64 を経由しない。JSON の数値文字列を符号・整数部・小数部・指数に分解し、10 進の整数として範囲内かを判定する。`i64::MAX` / `u64::MAX` の境界と、2^53 を超える整数でも精度落ちしない。
  - 時刻フィールド専用のパーサを追加し、既存の `parse_u64` は Location の ID とカタログの数値フィールドに残す。Location の ID とカタログの `generatedAt` など、Timeline の時刻フィールド以外の `parse_u64` 利用箇所は本 issue の対象外とし、現行の受理範囲を維持する。
- `MsfTemplate::resolve_entry` は `n` を `u64` のまま受け取り、計算は `i128` / `u128` で行う。各系列の計算結果がフィールドの範囲 (`i64` / `u64`) を超えた場合に `None` を返す。`n` が `u64::MAX` でも、全系列が範囲内なら `Some` を返す。
- encode は整数リテラルで出力する。decode した `2002.0` は encode で `2002` に正規化される。
- no_std を維持する。パーサは `core` と `alloc` の範囲で実装する。
- 公開 doc に契約を明記する:
  - `MsfMediaTimelineEntry::pts_ms` / `wallclock_ms`
  - `MsfEventIndex` の `MediaPtsMs` / `WallclockMs`
  - `MsfTemplate` の `start_media_time` / `delta_media_time` / `start_wallclock` / `delta_wallclock`
  - `MsfMediaTimeline::decode` / `MsfEventTimeline::decode` / `MsfCatalogDocument::decode` (template を含む経路)
  - `docs/msf.md` の Timeline 節

## 変更対象

- `src/msf.rs` (型定義、decode の数値パーサ、`MsfTemplate::resolve_entry`、公開 doc・コメント)
- `docs/msf.md`
- `tests/test_msf/media_timeline.rs` / `tests/test_msf/event_timeline.rs` / `tests/test_msf/template.rs` / `tests/test_msf/error_cases.rs` (境界値と受理・拒否のテスト。既存の型に関するコメントと `u64::MAX` を使う media 系列の overflow テストを新しい型に合わせる)
- `tests/test_msf/timeline_gzip.rs` (`pts_ms` の型に合わせる)
- `pbt/tests/prop_msf.rs` (生成器の型)
- `fuzz/fuzz_targets/fuzz_msf_template.rs` (入力型)

## 完了条件

- メディア時刻 (PTS) とその間隔の型が `i64`、wallclock と Location の型が `u64` になっていること。
- Media Timeline / Event Timeline / Template の decode で次を固定するテストがあること:
  - `2002.0`, `2.002e3`, `2002e0`, `100e-2` を整数ミリ秒として受理する。
  - `2002.5`, `2.0025e3`, `1e-1` を拒否する。
  - メディア PTS の `-1`, `-1.0`, `i64::MIN`, `i64::MAX` を受理し、`i64::MIN - 1`, `i64::MAX + 1` を拒否する。
  - `9223372036854775807.0` (i64::MAX の小数表記) と `18446744073709551615.0` (u64::MAX の小数表記) を受理する (f64 を経由しないことの確認)。
  - `deltaMediaTime` の `-2002` を受理し、`resolve_entry(1)` が `startMediaTime - 2002` を返す。
  - wallclock の `0`, `0.0`, `u64::MAX` を受理し、`-1`, `u64::MAX + 1` を拒否する。`deltaWallclock` の `-1` も拒否する。
- `MsfTemplate::resolve_entry` が `n = u64::MAX` で全系列が範囲内なら `Some`、いずれかが範囲外なら `None` を返すテストがあること。
- decode した `2002.0` を encode すると `2002` になるテストがあること。
- Location の ID とカタログの `generatedAt` など、本 issue の対象外の `parse_u64` 利用箇所の受理範囲が変わっていないこと。Location の ID に `0.0` を渡すと拒否され、`0` は受理されるテストを含む。
- `docs/msf.md` と公開 doc に受理範囲・表現・負値の扱いが明記され、`src/msf.rs` の Timeline 関連 doc・コメントと `tests/test_msf/event_timeline.rs` の §8.1 (Event Timeline data format) 参照が §8.2 に修正されていること。
- `pbt/tests/prop_msf.rs` と `fuzz/fuzz_targets/fuzz_msf_template.rs` が新しい型でコンパイルできること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
- `cargo build -p shiguredo_moqt --target thumbv7em-none-eabihf` と `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` が通ること (no_std と公開 doc の維持)。
- `docs/msf.md` が markdownlint を通ること (CI の prek ジョブと同じ)。
- `fuzz/fuzz_targets/fuzz_msf_template.rs` が `cargo +nightly fuzz build fuzz_msf_template` でビルドできること (CI の `cargo test --workspace` は `fuzz/` を対象外とするため、ローカルで確認する)。

## 解決方法

- メディア PTS を `i64` にした: `MsfMediaTimelineEntry::pts_ms`、`MsfEventIndex::MediaPtsMs`、
  `MsfTemplate::start_media_time` / `delta_media_time`。
- wallclock と Location の ID は `u64` のままにした。
- f64 を経由しない 10 進整数パーサを追加し、`2002.0` / `2.002e3` / `2002e0` / `100e-2` を
  整数として受理し、`2002.5` / `1e-1` を拒否する。i64 / u64 の範囲外は拒否する。
- `MsfTemplate::resolve_entry` は i128 / u128 で計算し、各系列が範囲外なら `None` を返す。
- decode した `2002.0` は encode で `2002` に正規化される。
- `tests/test_msf` に整数値表現・負値・i64/u64 境界・Location ID の非受理・u64::MAX index・
  wallclock 境界・template の負の deltaWallclock のテストを追加した。
- `pbt/tests/prop_msf.rs` と `fuzz/fuzz_targets/fuzz_msf_template.rs` を新しい型に更新した。
- `docs/msf.md` と公開 doc に受理範囲と正規化を明記した。

`cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、
`cargo fmt --all -- --check`、`cargo doc`、`cargo +nightly check --manifest-path fuzz/Cargo.toml`、
`cargo build -p shiguredo_moqt --target thumbv7em-none-eabihf` が成功した。
