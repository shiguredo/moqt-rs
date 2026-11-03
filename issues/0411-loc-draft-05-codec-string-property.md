# LOC の CODEC_STRING を Track / Object Property として扱う

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/add-loc-draft-05-codec-string-property
- Polished: {YYYY-MM-DD}

## 目的

[draft-ietf-moq-loc-05 §2.3.1.3、§6.1](../refs/moq/draft-ietf-moq-loc-05.txt) の変更に追従する。

## 現状

`src/loc.rs` の Property 定数と `LocProperties` のアクセサに CODEC_STRING がない。
`src/track_properties.rs` にも codec 文字列の取得 API はない。
0125 は既存の TIMESCALE / VIDEO_CONFIG / AUDIO_CONFIG を Track スコープで公開する課題であり、新しい Type `0x11` は対象に含まれていない。

## 設計方針

Type `0x11` の長さ付き UTF-8 Codec String を追加する。
`LocProperties` で構築・取得できるようにし、Track スコープの公開は 0125 の設計に合わせて新しい型だけを追加する。
IMMUTABLE_PROPERTIES 内の探索にも対応する。
文字列は WebCodecs Codec Registry の codec 値として扱い、映像専用と決めつけない。
catalog と値が異なる場合の decoder 自動切り替え規則はこの draft に追加されていないので作らない。

## 完了条件

- Track / Object、最上位 / immutable 内で `0x11` を構築・取得できること。
- UTF-8 の値、型と値の不一致、長さ上限、未知 Property の保持を既知バイト列で検証すること。
- `tests/test_loc.rs` と Track Properties の検証、仕様表・LOC 版の説明を更新すること。
- 0125 の既存 3 型の実装を重複起票せず、CODEC_STRING の追加として完了できること。

## 依存・関連

- 先行: [0125](0125-add-track-scope-loc-properties.md)
- 関連: [0400](0400-moqt-draft-23-property-scope.md)

## 差分の確認範囲

実質的な変更は CODEC_STRING の定義と Properties Registry への追加である。
その他は MOQT の参照版・日付と改行の更新であり、TIMESTAMP / TIMESCALE の意味や映像の並べ替え規則は変更されていない。
