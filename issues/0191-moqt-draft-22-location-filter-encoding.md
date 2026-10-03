# LOCATION_FILTER を明示的な Location Filter Type 符号化に追従する

- Created: 2026-10-02
- Completed: {YYYY-MM-DD}
- Branch: feature/change-location-filter-encoding
- Polished: 2026-10-02

## 目的

draft-ietf-moq-transport-22 §9.20.9 (LOCATION FILTER Parameter) は、draft-21 までの「Length で後続フィールドを推論する」符号化を廃止し、明示的な Location Filter Type (vi64) が後続フィールドを選択する符号化に変更した (A.1 #1913, #1953)。本ライブラリは draft-21 準拠の length 推定型のままなので、ワイヤ非互換の乖離を解消する。

## 現状

- `src/message_parameter.rs` の `LocationFilter` は `RelativeGroup` / `NextObject` / `AbsoluteStart` / `AbsoluteRange` / `AbsoluteRangeWithEnd` を持ち、`encode_to_bytes` / `decode` が payload のフィールド数から形式を推論する。no filter は `Option<LocationFilter>` の `None` と長さ 0 の値で表現する。
- `NextObject` は `[0x00, 0x00]` として符号化され、`AbsoluteStart { start_group: 0, start_object: 0 }` は `NextObject` に正規化される。
- `PARAM_LOCATION_FILTER` (0x21) は `ValueEncoding::LengthPrefixed` で、`MessageParameterValue::LengthPrefixed(Vec<u8>)` に生バイトを保持する。`location_filter()` / `location_filter_update()` / `validate_location_filter_bytes()` は生バイト前提である。
- draft-22 §9.20.9 は Type 0x00 (None) / 0x01 (Relative Start) / 0x02 (Absolute Start) / 0x03 (Absolute Start, Group End) / 0x04 (Absolute Range) / 0x05 (Next Object) を定義し、0x06 以上を PROTOCOL_VIOLATION とする。Length フィールドは存在しない。
- `src/session/subscription/fill.rs` の `should_open_fill_stream` は空バイト (`Some([])`) を「トラック全体」として特別扱いしている。
- `REQUEST_UPDATE` / `PUBLISH_STATE_NOTIFY` では空値がフィルタ削除 / フィルタなし報告として使われている (`location_filter_update()` の `Removed`)。

## 設計方針

- `LocationFilter` を draft-22 の 6 形式に対応させる。no filter は Type 0x00 を表す variant として扱い、`Option<LocationFilter>` の `None` は「パラメータ省略」を表すように対応を整理する (REQUEST_UPDATE では Type 0x00 が `Removed`、省略が `Unchanged` になる)。
- `PARAM_LOCATION_FILTER` を `LengthPrefixed` から専用の `ValueEncoding` と型付き値 (`MessageParameterValue::LocationFilter`) へ切り替え、
  生バイト API (`location_filter()` / `validate_location_filter_bytes()`) は `fill.rs` の空判定を 3 状態判定へ置き換えたうえで廃止または内部化する。
  `fill.rs` の空バイト (`Some([])`) が track 全体を指していた箇所は、`location_filter_update()` の
  `Unchanged` = subscription のフィルタ使用、`Removed` = track 全体、`Set` = そのフィルタ、に置き換える。
- `AbsoluteStart { start_group: 0, start_object: 0 }` の `NextObject` への正規化を廃止し、Type 0x02 と Type 0x05 を区別する。
- `NextObject` は Type 0x05 単独で符号化する。
- `decode` は先頭の数値を Location Filter Type として読み、Type 0x06 以上は `ProtocolViolation`、
  0x00〜0x05 は Type ごとの必須フィールド数 (0x00 / 0x05 は 0 個、0x01 は 1 個、0x02 は 2 個、0x03 は 3 個、0x04 は 4 個) だけ読み切る。
  新符号化には Length が無く値は自己境界のため、必須フィールド数を超える余剰バイトは次パラメータの Delta Type として解釈され、
  LOCATION_FILTER 単体では検出できない。検出できるのは FILL_PARAMETERS 内側のような Length 境界のあるスコープの余剰バイトであり、
  これは既存の内側スコープの扱いと同じく `KeyValueFormattingError` にする。必須フィールドの欠落 (バッファ終端) は `UnexpectedEof` で失敗する。
- `location_filter_update()` の `Unchanged` / `Removed` / `Set` の 3 状態 API は維持し、`Removed` の判定源を「空バイト」から「Type 0x00」に変更する。
- FILL_PARAMETERS 内の no filter は新符号化の Type 0x00 として実装し、コメントに draft-22 §9.20.15 / §3.4 を引用する (draft-22 §3.4 の「LOCATION_FILTER inside FILL_PARAMETERS is zero-length」は旧符号化の名残であり、新符号化では Type 0x00 が「fill range = track 全体」を指すと読み替える)。
- `tests/test_message_parameter.rs` で 0x21 を LengthPrefixed の代表にしているテスト (65536 バイト長、range_filters ガードなど) は別の LengthPrefixed 型へ移す。PBT (`pbt/tests/prop_message.rs` の `sample_location_filter_bytes` など) と `examples/moq-sub/src/pipeline.rs` の構築箇所も型付き値に追従する。
- ワイヤ非互換の変更のため `CHANGES.md` の `## develop` に `[CHANGE]` として記載する。

## 完了条件

- LOCATION_FILTER が draft-22 §9.20.9 の Type 0x00〜0x05 でエンコード / デコードでき、Type 0x06 以上が `ProtocolViolation` になること (Type ごとのフィールド数は Type が一意に定めるため、フィールド数不一致を表すワイヤは存在しない。欠落は `UnexpectedEof`、FILL_PARAMETERS 内側の余剰バイトは `KeyValueFormattingError` になること)
- `NextObject` が Type 0x05、no filter が Type 0x00 として往復し、`AbsoluteStart {0, 0}` が `NextObject` に正規化されないこと
- FILL_PARAMETERS / REQUEST_UPDATE / PUBLISH_STATE_NOTIFY / SUBSCRIBE / FETCH / PUBLISH の各文脈のテストと PBT が新符号化で通ること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `prek run --all-files` が通ること
- `CHANGES.md` の `## develop` に `[CHANGE]` エントリが追加されていること

## 解決方法

{未着手}
