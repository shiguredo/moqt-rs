# FetchStreamEncoder の Datagram 起源 Object 対応を修正する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-fetch-encoder-datagram-origin

## 目的

`FetchStreamEncoder` が Datagram Forwarding Preference の Object を含む FETCH 応答をエンコードできるようにする。draft-ietf-moq-transport-21 §11.4.1.1 (Flags) の Datagram 起源 Object 規則を満たす。

## 現状

`src/stream/encoder.rs` の `FetchStreamEncoder::encode_object` は、先頭 Object (`prior_state == None`) のとき常に `FetchSubgroupIdMode::Explicit(input.subgroup_id)` を選ぶ。`src/stream/fetch.rs` の `FetchStreamObject::encode` は `is_datagram_origin && !matches!(subgroup_id, Zero)` を
`ProtocolViolation` で拒否するため、先頭が Datagram 起源のとき `encode_object` が必ず失敗する。`Some(prior)` 分岐は `Zero` を選ぶため、先頭だけが壊れている。

加えて `src/stream/encoder.rs` の `prior_state` 更新は、`input.is_datagram_origin` のとき wire 上 `Zero` を選びながら `prior_state.subgroup_id` に実値を保存する。続く Object が同じ subgroup_id を持つと `PreviousSame` が選ばれ、デコーダは Datagram 起源を `0` として解決するため往復が壊れる。

根拠 (draft-ietf-moq-transport-21 §11.4.1.1):

> "When encoding an Object with a Forwarding Preference of 'Datagram' ... The publisher MUST SET bit 0x40 to '1'."

先頭 Object が Datagram 起源であることは禁止されていない。

テストは `is_datagram_origin: false` 固定 (`pbt/tests/prop_stream/encoder.rs`)、または戻り値を捨てる (`fuzz/fuzz_targets/fuzz_fetch_stream_encoder.rs`) ため検出できない。

## 設計方針

先頭 Object の分岐でも `input.is_datagram_origin` のとき `FetchSubgroupIdMode::Zero` を選ぶ。`prior_state` に保存する subgroup_id も Datagram 起源では `0` にする。

## 完了条件

- 先頭 Object が Datagram 起源の FETCH 応答をエンコードできること
- Datagram 起源 Object の後に同一 subgroup_id の Object が続いても往復が壊れないこと
- `is_datagram_origin = true` を含む単体テスト / PBT / fuzz が追加されていること
