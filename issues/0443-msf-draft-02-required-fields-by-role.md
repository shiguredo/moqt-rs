# MSF カタログの (packaging, role) 別必須フィールドを v02 に合わせる

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §5.2.4.1 (Required fields per packaging and track type) は、(packaging, role) の組ごとに必須フィールドを Table 6 で定めた。§5.2.7 (Track role) は role に `data` を加え、指定 role と衝突しないカスタム role の使用を MAY で許す。

現行実装は role ごとの必須フィールドを一部しか検証しておらず、v02 が要求する `framerate` と `lang` を見ていない。一方で、role を省略して codec から種別を判定する経路の Conditional MUST は実装済みで、これを失ってはならない。

## 現状

- `src/msf.rs` の `validate_media_track_fields` は、role `audio` / `audiodescription` に `codec` / `bitrate` / `samplerate` / `channelConfig` を、role `video` / `signlanguage` に `codec` / `bitrate` を要求する。
- v02 §5.2.4.1 Table 6 では、`loc` / `video` と `loc` / `signlanguage` は `framerate` も必須、`loc` / `audiodescription` は `lang` も必須である。現行検証はこの 2 点を要求しない。
- 現行実装は role だけでなく codec からも audio / video を判定し、role 省略時にも個別節の Conditional MUST (`bitrate` §5.2.23、`samplerate` §5.2.29、`channelConfig` §5.2.30) を適用する。`tests/test_msf/error_cases.rs` の `codec_audio_missing_samplerate_rejected` などがこの挙動を固定している。
- `role` は `MsfTrack.role: Option<String>` で、未知の role も受理する。v02 §5.2.7 は指定 role と衝突しないカスタム role を許すため、この挙動は仕様と一致する。
- Table 6 に `data` の行はなく、`(loc, data)` に追加必須フィールドはない。現行実装でも `data` role の最小 track は受理される。
- `tests/test_msf/error_cases.rs` には、`framerate` / `lang` なしで受理や特定のエラーを assert しているテストがある。必須化すると失敗し得る。
- カスタム role を受理する現行挙動は変えないため、後方互換のない変更にはならない。

## 設計方針

- §5.2.4.1 Table 6 に一致する (packaging, role) の組について、必須フィールドの表を追加する。`loc` / `video` と `loc` / `signlanguage` に `framerate`、`loc` / `audiodescription` に `lang` を加える。
- 表による検証は、現行の codec 由来の Conditional MUST を置き換えない。role 省略時や codec から audio / video を判定できる場合は、個別節の制約を従来どおり適用する。
- 未知の role は受理する。表にない組には `name` / `packaging` / `isLive` 以外の必須フィールドを課さない。role の値の集合で拒否しない。
- `data` role は `codec` / `bitrate` / `framerate` / `lang` を要求しない。最小 track の受理を回帰テストとして固定する。
- `tests/test_msf/error_cases.rs` の fixture に `framerate` / `lang` を追加し、既存のエラー期待値を壊さないようにする。

## 完了条件

- `loc` / `video` で `framerate` 欠如、`loc` / `signlanguage` で `framerate` 欠如、`loc` / `audiodescription` で `lang` 欠如が、それぞれ `MessageError::InvalidCatalog` で拒否されること。
- `loc` / `audio` の `codec` / `bitrate` / `samplerate` / `channelConfig` の要求が退行しないこと。
- role を省略し codec が opus の track で `samplerate` が欠如した場合に、従来どおり拒否されること。
- `(loc, data)` の最小 track (`codec` / `bitrate` / `framerate` / `lang` なし) が受理されること。
- カスタム role (例: `"foo"`) の track が、追加必須フィールドなしで受理されること。
- 上記を `tests/test_msf/catalog_decode.rs`、`tests/test_msf/encode_decode_roundtrip.rs`、`tests/test_msf/error_cases.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
