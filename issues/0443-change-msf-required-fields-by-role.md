# MSF カタログの (packaging, role) 別必須フィールドを v02 に合わせる

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/change-msf-required-fields-by-role
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §5.2.4.1 (Required fields per packaging and track type) は、(packaging, role) の組ごとに必須フィールドを表で定めた。§5.2.7 (Track role) は role に `data` を追加した。
現行実装は role ごとの必須フィールドを一部しか検証しておらず、v02 が要求する `framerate` と `lang` を見ていない。v02 のカタログを受理するには、この表に一致させる必要がある。

## 現状

- `src/msf.rs` の `validate_media_track_fields` は、role `audio` / `audiodescription` に `codec` / `bitrate` / `samplerate` / `channelConfig` を、role `video` / `signlanguage` に `codec` / `bitrate` を要求する。
- v02 §5.2.4.1 の表では、`loc` / `video` と `loc` / `signlanguage` は `framerate` も必須、`loc` / `audiodescription` は `lang` も必須である。現行検証はこの 2 点を要求しない。
- role は `MsfTrack.role: Option<String>` で自由文字列のため、新しい role `data` は値としては受理されるが、role の一覧に対する検証はない。
- `eventtimeline` / `eventtimeline` の `eventType` は `validate_full_track` と `decode_track` が要求済みである。
- v02 §5.2.4.1 は、個別フィールドのより厳しい制約が優先されると述べる。§5.2.7 は track の意味に対して無関係なフィールドを含めないことを SHOULD NOT とする。

## 設計方針

- (packaging, role) を key にした必須フィールドの表を `src/msf.rs` に定義し、`validate_media_track_fields` をその表で駆動する。表は仕様の表と同じ組だけを持ち、該当しない組は追加の必須フィールドを要求しない。
- `loc` / `video` と `loc` / `signlanguage` に `framerate`、`loc` / `audiodescription` に `lang` を追加する。個別フィールドの検証 (codec の登録表記、言語タグなど) は現状の関数を維持する。
- role の値は §5.2.7 の表に `data` を加えた集合で検証するか、未知 role を受理し続けるかを決める。仕様の表にない role を受理すると誤った組の必須フィールド検証をすり抜けるため、表にある role だけを受理する方針とする。この判断は破壊的変更になるため doc に明記する。
- 無関係なフィールドを出力しない SHOULD は、どのフィールドがどの組で意味を持つかの網羅表が仕様にないため、本 issue では実装しない。

## 完了条件

- `loc` / `video` で `framerate` 欠如、`loc` / `signlanguage` で `framerate` 欠如、`loc` / `audiodescription` で `lang` 欠如がそれぞれ `MessageError::InvalidCatalog` で拒否されること。
- role `data` の track が受理されること。
- 表にない role が拒否されること (受理する方針を採る場合は、その理由を issue と doc に残す)。
- 既存の `audio` / `video` / `audiodescription` / `signlanguage` の検証が退行していないこと。
- 上記を `tests/test_msf/catalog_decode.rs` と `tests/test_msf/encode_decode_roundtrip.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
