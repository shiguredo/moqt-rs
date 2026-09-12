# shiguredo-moqt SKILL.md の FetchStreamEncoder 記述を実装に合わせる

- Created: 2026-09-13
- Completed: {YYYY-MM-DD}
- Branch: feature/doc-fetch-stream-encoder-skill
- Polished: {YYYY-MM-DD}

## 目的

`skills/shiguredo-moqt/SKILL.md` を読んだ利用者やエージェントが、`FetchStreamEncoder` で Descending の Group Order を指定できないと誤解する状態をなくす。記述を実装の公開 API と一致させる。

## 現状

- `skills/shiguredo-moqt/SKILL.md` の「デコーダ / エンコーダ」節の API 一覧は、`FetchStreamDecoder` の `fn new_with_group_order(group_order: u8) -> Result<FetchStreamDecoder, MessageError>` を列挙する一方、`FetchStreamEncoder` は `fn new(request_id: u64) -> FetchStreamEncoder` のみを列挙する。
- 同節の説明は「`FetchStreamEncoder` は前回オブジェクトとの差分 (デルタ圧縮) を内部状態で判断する。公開コンストラクタは Ascending 固定」としている。
- 実装の `src/stream/encoder.rs` の `FetchStreamEncoder` には `pub fn new_with_group_order(request_id: u64, group_order: u8) -> Result<FetchStreamEncoder, MessageError>` があり、`validate_fetch_group_order` が 0x01 (Ascending) と 0x02 (Descending) を許可する。doc コメントにも
  `FetchStreamDecoder::new_with_group_order` と対称の公開 API であると記載されている。
- このため「公開コンストラクタは Ascending 固定」は実装と一致しない。
- SKILL / README の API 一覧一般の整合は open issue 0040 が扱うが、`FetchStreamEncoder` のグループオーダーの記述は 0040 の現状に列挙されていない。

## 設計方針

- SKILL.md の `FetchStreamEncoder` の API 一覧に `new_with_group_order` を追加する。
- 「公開コンストラクタは Ascending 固定」を、`new` は Ascending、`new_with_group_order` は 0x01 / 0x02 を検証して受け付ける旨の説明に修正する。
- `FetchStreamDecoder` 側の書きぶりと揃える。
- 実装は変更しない。

## 完了条件

- SKILL.md の `FetchStreamEncoder` の公開コンストラクタの記述が `src/stream/encoder.rs` と一致すること
- 「Ascending 固定」という実装と異なる記述が残っていないこと
- 実装変更を伴わないこと
