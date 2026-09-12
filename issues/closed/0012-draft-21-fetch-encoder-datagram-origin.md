# FetchStreamEncoder の Datagram 起源 Object 対応を修正する

- Created: 2026-09-10
- Completed: 2026-09-12
- Branch: feature/fix-fetch-encoder-datagram-origin
- Polished: 2026-09-10

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

Datagram 起源 Object は Subgroup ID を持たないため、デコード結果の `subgroup_id` は `0` に解決される。既存 PBT の `object.subgroup_id == input.subgroup_id` という期待値は Datagram 起源では成立しないため、期待値を通常起源と Datagram 起源で分ける。

## 完了条件

- 先頭 Object が Datagram 起源の FETCH 応答をエンコードできること
- Datagram 起源 Object の後に同一 subgroup_id の Object が続いても往復が壊れないこと
- 単体テストと PBT に `is_datagram_origin = true` のケースが追加されていること。fuzz は既に任意入力で `is_datagram_origin = true` を生成しており、クラッシュ耐性の検査に変更を要しない

## 解決方法

`FetchStreamEncoder::encode_object` の Datagram 起源対応を修正した。

- 先頭 Object の分岐でも `input.is_datagram_origin` のとき `FetchSubgroupIdMode::Zero` を選ぶようにした。従来は常に `Explicit` を選び、`FetchStreamObject::encode` の Datagram 起源検証で必ず失敗していた。
- Datagram 起源では入力 `subgroup_id` を実効値 0 として扱い、`prior_state` にも 0 を保存するようにした。あわせて Datagram 起源の prior Object の直後は prior 参照を避け、0 は `Zero`、それ以外は `Explicit` を選ぶようにした。
- `FetchObjectInput.subgroup_id` と `PriorEncodeState` の doc に Datagram 起源の扱いを明記し、`update_prior_for_end_of_range` に EOR 後も prior を維持する理由を追記した。
- `tests/test_stream/encoder.rs` に 3 テストを追加した。先頭 Datagram 起源の往復、Datagram 起源後の同一 subgroup_id の往復、Datagram 起源直後の subgroup_id 0 が PreviousSame (0x01) ではなく Zero (0x00) になる flags バイト検証。
- `pbt/tests/prop_stream/encoder.rs` で Datagram 起源をランダム生成し、subgroup_id 0 も 1/2 の確率で混ぜるようにした。期待値は Datagram 起源では 0 に解決される形に分けた。
- `CHANGES.md` の `## develop` に `[FIX]` エントリを追加した。
