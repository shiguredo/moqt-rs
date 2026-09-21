# FETCH 経路で Object Properties が application に渡らない

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-fetch-object-properties
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-loc-04 §2.2 (MOQ Object Mapping) は、LOC の Public Properties を MOQT の Object Properties に載せると規定する。

> Media objects encoded using the container format defined in this specification populate the MOQ Object Properties with the LOC Public Properties, and populate the MOQ Object Payload with LOC Private Properties followed by the LOC Payload, as shown below.

draft-ietf-moq-transport-21 §11.4.1 (Fetch Header) の Fetch Object にも Properties フィールドがある。

> Each Object sent on a FETCH stream after the FETCH_HEADER has the following format:
>
> ```text
> {
>   Serialization Flags (vi64),
>   [Group ID Delta (vi64),]
>   [Subgroup ID (vi64),]
>   [Object ID Delta (vi64),]
>   [Publisher Priority (8),]
>   [Properties (..),]
>   Object Payload Length (vi64),
>   [Object Payload (..),]
> }
> ```
>
> Figure 28: MOQT Fetch Object Fields

Properties の有無は同 §11.4.1.1 (Flags) Table 9 の bit `0x20` が示す。

> +=========+==========================+=========================+
> | Bitmask | Condition if set         | Condition if not set    |
> |         |                          | (0)                     |
> +=========+==========================+=========================+
> | 0x20    | Properties field is      | Properties field is not |
> |         | present                  | present                 |
> +---------+--------------------------+-------------------------+

構造は同 §11.4.1 の次の記述により §11.1.3 (Object Properties) と同じである。

> The Object Properties structure is defined in Section 11.1.3.

現状は Subgroup 経路では Object Properties を取得できるが、FETCH 経路では decoder が破棄する。そのため FETCH で LOC メディアを取得すると Video Config (H.264/H.265 の parameter set) を失ってデコードできず、Timestamp / Timescale も失って PTS を算出できない。

## 現状

- `src/stream/decoder.rs` の `DecodedSubgroupObject` は `properties_bytes: Option<Vec<u8>>` を持ち、`SubgroupStreamDecoder::try_decode_object` が `SubgroupObject::decode` の返す Properties 生バイト列 (Properties Length varint + Properties データ) を詰めている
- `src/stream/decoder.rs` の `DecodedFetchObject` は `group_id` / `subgroup_id` / `object_id` / `publisher_priority` / `is_datagram_origin` / `payload_length` だけを持ち、Properties を持たない
- `src/stream/decoder.rs` の `FetchStreamDecoder::try_decode_entry` は `FetchStreamEntry::decode_with_properties` が返した `properties_bytes` を `resolve_and_transition` に渡すが、
  `resolve_and_transition` は Object エントリの分岐で `validate_fetch_object(&resolved, properties_bytes.as_deref())` に渡すだけで、`DecodedFetchEntry::Object` には詰めずに捨てている
- `src/stream/fetch.rs` の `FetchStreamObject::decode_after_flags` は Properties Length を含む生バイト列を返している (コメントは「Properties をスキップする」)。End of Range の 3 エントリは `properties_bytes` に `None` を返す
- `examples/moqt-subscriber/src/pipeline.rs` の `handle_fetch_stream` は「FetchStreamDecoder は properties_bytes を保持しないため video_config は渡せない (AV1 は Sequence Header が payload 内に含まれるため問題なし。H.264/H.265 は Subgroup 経由でのみ動作する)」とコメントし、`decode_and_send(&payload, None, video_decoder, sink)` を呼んでいる。
  H.264 の `VideoDecoder::decode` は `video_config` を AVCDecoderConfigurationRecord として解釈するため、これが無いと parameter set を適用できない
- `examples/moqt-subscriber/src/pipeline.rs` の `extract_video_config` / `extract_timestamp_timescale` / `extract_audio_config` は `DecodedSubgroupObject.properties_bytes` を受け取る形で、FETCH 経路からは呼ばれていない
- `pbt/tests/prop_stream/encoder.rs` の FETCH roundtrip テストは「`DecodedFetchEntry::Object` は Properties 生バイトを公開しないため、Properties の内容そのものは復元検証しない」と書いて復元検証を諦めている
- `DecodedFetchObject` と `DecodedFetchEntry` は `Copy` を derive している
- library の `Session::recv_fetch_entry` は `stream_id` だけを受け取る activity 通知であり、Object の内容は example が decoder から直接読む。Session 側の API 変更は不要

## 設計方針

- `DecodedFetchObject` に `properties_bytes: Option<Vec<u8>>` を追加し、`resolve_and_transition` が `validate_fetch_object` に渡した値をそのまま `DecodedFetchEntry::Object` に詰める。表現は Subgroup 経路と同じ「Properties Length varint + Properties データ」にそろえ、`LocProperties::decode` にそのまま渡せる形にする
- `Vec<u8>` を持つため `DecodedFetchObject` と `DecodedFetchEntry` の `Copy` を外す (`Clone` は維持する)。既存の利用箇所は参照で受けているため影響は局所に留まる
- `FetchStreamEntry::decode_with_properties` の Properties は Properties Length 込みなので、decoder 側で組み立て直す処理は追加しない
- example の `handle_fetch_stream` は `extract_video_config` の結果を `decode_and_send` に渡す。Timestamp / Timescale も `extract_timestamp_timescale` で取り出せる状態にし、映像 PTS への反映は [issues/0104](../issues/0104-add-subscriber-av-sync.md) に任せる
- [issues/0104](../issues/0104-add-subscriber-av-sync.md) は「FETCH 経路 (`handle_fetch_stream` の `FetchStreamDecoder`) は Properties を保持しないため映像 PTS を付けられない。映像を FETCH でも扱う必要が出たら別 issue にする」として本件を先送りしており、本 issue がその受け皿になる

## 完了条件

- FETCH 応答で Properties 付き Object を受信したとき、`DecodedFetchEntry::Object` の `properties_bytes` が Properties Length 込みの生バイト列になり、`LocProperties::decode` で Video Config / Timestamp / Timescale を取り出せることを固定するテストが `tests/test_stream/decoder.rs` の `encode_fetch_stream` を使う FETCH 系テストに追加されていること
- Properties を持たない (bit `0x20` が 0 の) Object の `properties_bytes` が `None` になること、End of Range の 3 エントリが Properties を運ばないことを固定するテストが追加されていること
- `pbt/tests/prop_stream/encoder.rs` の「`DecodedFetchEntry::Object` は Properties 生バイトを公開しないため、Properties の内容そのものは復元検証しない」というコメントと検証対象外の扱いが、Properties の往復検証に置き換わっていること
- `examples/moqt-subscriber/src/pipeline.rs` の `handle_fetch_stream` が FETCH 経路でも `extract_video_config` の結果を `decode_and_send` に渡し、「H.264/H.265 は Subgroup 経由でのみ動作する」という制約コメントが解消されていること
- `make test` (`cargo test --workspace`) と `make clippy` と `make fmt` が通ること
