# LOC の CODEC_STRING を Track / Object Property として扱う

- Created: 2026-11-03
- Completed: 2026-11-04
- Branch: develop
- Polished: 2026-11-03
- Refreshed: 2026-11-04

## 目的

[draft-ietf-moq-loc-05 §2.3.1.3、§6.1](../refs/moq/draft-ietf-moq-loc-05.txt) の変更に追従する。

## 現状

`src/loc.rs` の Property 定数と `LocProperties` のアクセサに CODEC_STRING がない。
`src/track_properties.rs` にも codec 文字列の取得 API はない。
0125 は既存の TIMESCALE / VIDEO_CONFIG / AUDIO_CONFIG を Track スコープで公開する課題であり、新しい Type `0x11` は対象に含まれていない。

## 設計方針

`src/loc.rs` に `PROP_CODEC_STRING = 0x11` を追加し、`LocPropertyValue::Bytes` / `TrackPropertyValue::Bytes` に UTF-8 バイト列を保持する。長さは共通 KVP の length フィールドに UTF-8 のバイト数を入れ、独自の内側 length は追加しない。上限は既存の奇数型と同じ 65535 バイトとする。
`LocProperties` で構築・取得できるようにし、Track スコープの公開は 0125 の設計に合わせて新しい型だけを追加する。
`LocProperties::codec_string` / `TrackProperties::codec_string` は `Result<Option<&str>, MessageError>` を返し、欠落は `Ok(None)`、不正 UTF-8 はエラーとして区別する。
CODEC_STRING の encode / decode では最上位と IMMUTABLE_PROPERTIES 内の不正 UTF-8 を `MessageError::MalformedKeyValue` として拒否する (設計時の `KeyValueFormattingError` は現行コードに存在しないため、§8.3 の `MALFORMED_KEY_VALUE` に対応する variant に読み替える)。wire のエラー分類は MOQT draft 23 §8.3 の `MALFORMED_KEY_VALUE` に合わせる。未知型のバイト列は従来どおり保持する。
IMMUTABLE_PROPERTIES 内の探索は、Object 側で 0309 の `LocProperties::find_bytes`、Track 側で 0125 の `TrackProperties::find_bytes` の共通探索を再利用する。
不正 UTF-8 のエラーを欠落に畳まず、専用の KVP パーサーは追加せず既存の KVP 走査 (`walk_kv_pairs` / `walk_track_kv_pairs`) を再利用する。
`LocProperties::decode` が内側を検証するのは内側 KVP が解析できる場合に限り、解析できない内側の構造検証は Session の `ObjectProperties` に委ねる。
文字列は WebCodecs Codec Registry の codec 値として扱い、映像専用と決めつけない。
catalog と値が異なる場合の decoder 自動切り替え規則はこの draft に追加されていないので作らない。

## 完了条件

- Track / Object、最上位 / immutable 内で `0x11` を構築・取得できること。
- UTF-8 の値とバイト長、最上位 / immutable 内の不正 UTF-8、欠落との区別、型と値の不一致、65535 バイト上限、未知 Property の保持を既知バイト列で検証すること。`LocProperties::decode` の内側検証は、内側 KVP が解析できる場合に限る。
- `tests/test_loc.rs` と Track Properties の検証、仕様表・LOC 版の説明を更新すること。
- 0125 の既存 3 型の実装を重複起票せず、CODEC_STRING の追加として完了できること。

## 依存・関連

- 先行: [0125](0125-add-track-scope-loc-properties.md)
- 先行: [0309](0309-draft-23-object-immutable-properties-search.md)
- 関連: [0400](0400-moqt-draft-23-property-scope.md)

## 差分の確認範囲

- CODEC_STRING (0x11) の定義、Track / Object の Properties Registry への追加、accessor
  (`LocProperties::codec_string` / `TrackProperties::codec_string`)
- `src/property.rs` の共有検証 (`CODEC_STRING_TYPE` / `validate_codec_string_utf8`) と、
  Loc / Track / Object の encode / decode での UTF-8 検証
- `src/session/data.rs` の FETCH 応答 Object Properties decode エラーの分類
  (`session_error_from_data_message` 経由の MALFORMED_KEY_VALUE)
- LOC draft-04 → draft-05 のリポジトリ全体の表記更新
- テスト / PBT / fuzz / docs / README / SKILL の更新

TIMESTAMP / TIMESCALE の意味、映像の並べ替え規則、catalog の codec 値と decoder の自動切り替え
規則は変更していない。

## 解決方法

LOC draft-05 の CODEC_STRING (0x11) を Track / Object の両スコープで扱えるようにした。

- `src/property.rs` を新設し、CODEC_STRING の Property Type と UTF-8 検証を Track / Object /
  LOC の共有として 1 箇所に置いた。`src/loc.rs` に `PROP_CODEC_STRING` と
  `LocProperties::codec_string` を、`src/track_properties.rs` に
  `TrackProperties::codec_string` を追加した。いずれも `Result<Option<&str>, MessageError>`
  で、欠落は `Ok(None)`、不正 UTF-8 は `MalformedKeyValue` (MALFORMED_KEY_VALUE) として区別する
- `LocProperties` / `TrackProperties` / `ObjectProperties` の encode / decode で
  CODEC_STRING の UTF-8 を検証する。Track / Object の IMMUTABLE_PROPERTIES 内側も走査し、
  Object scope の 0x11 も `ObjectProperties` が検証する。`LocProperties::encode` は
  IMMUTABLE 内側の構造・値域も検証し、解析できない内側は拒否する。`LocProperties::decode` は
  0309 の「内側の構造破損を検出しない」を維持し、解析できる内側の 0x11 だけ検証する
- `src/property_scope.rs` に 0x11 を Track / Object 両スコープの既知・単一インスタンス型として
  登録した。これにより Object / Track の重複検証が 0x11 にも効く
- `src/session/data.rs` の FETCH 応答の Object Properties decode エラーを
  `session_error_from_data_message` に通し、CODEC_STRING の不正 UTF-8 が
  MALFORMED_KEY_VALUE (0x6) で通知されるようにした
- テストは `tests/test_loc.rs` / `tests/test_track_properties.rs` /
  `tests/test_object_properties.rs` / `tests/test_session/peer_track_properties.rs` /
  `tests/test_session/data_stream.rs` / `tests/test_session/fetch/nonexistence.rs` に追加した。
  PBT (`pbt/tests/prop_loc.rs`、`pbt/src/common.rs` / `prop_object_properties.rs` の生成器) と
  fuzz (`fuzz_decode_{loc,track}_properties.rs`) も追随した
- LOC 版の説明を repo 全体で draft-05 に更新した (README / docs/loc.md /
  docs/missing-features.md / SKILL.md / src と crates のコメント / e2e-tests)
- 既知の残ギャップ: IMMUTABLE 内側だけにある Audio Level の値域違反・Video Frame Marking の
  長さ違反の MALFORMED_KEY_VALUE 通知 (docs/missing-features.md §4.1)、最上位と内側をまたぐ
  単一値プロパティの重複検出 (LocProperties 単体)、OBJECT_PROPERTY_FILTER 経由の decode
  エラーの FilteredOut 扱い。CODEC_STRING の UTF-8 は内側 KVP が解析できる場合だけ
  `LocProperties::decode` が検証する

## 完了条件の検証

| 完了条件 | 検証 |
| --- | --- |
| Track / Object、最上位 / immutable 内で `0x11` を構築・取得できること | `tests/test_loc.rs` / `tests/test_track_properties.rs` / `tests/test_object_properties.rs` の `codec_string_*` テスト |
| UTF-8 の値とバイト長、最上位 / immutable 内の不正 UTF-8、欠落との区別、型と値の不一致、65535 バイト上限、未知 Property の保持を既知バイト列で検証すること | `codec_string_*` / `*_invalid_utf8_*` / `codec_string_length_boundary` / `codec_string_varint_value_rejected` / `unknown_property_preserved_alongside_codec_string` |
| `tests/test_loc.rs` と Track Properties の検証、仕様表・LOC 版の説明を更新すること | `tests/test_loc.rs` / `tests/test_track_properties.rs` に加え、`docs/loc.md` / `docs/missing-features.md` / `README.md` / `skills/shiguredo-moqt/SKILL.md` を LOC draft-05 に更新 |
| 0125 の既存 3 型の実装を重複起票せず、CODEC_STRING の追加として完了できること | `TrackProperties` には `codec_string` のみを追加し、TIMESCALE / VIDEO_CONFIG / AUDIO_CONFIG の既存 API は変更していない (`tests/test_session/peer_track_properties.rs` で非回帰を確認) |

検証コマンド `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` /
`cargo test --workspace` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` は
すべて成功した (Linux では libpulse 依存のため libpulse を展開した代替環境で実行)。
