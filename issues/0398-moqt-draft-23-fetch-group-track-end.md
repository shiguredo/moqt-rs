# FETCH の End of Group と End of Track 指示を実装する

- Created: 2026-11-03
- Completed: 2026-11-03
- Branch: develop
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §3.2.2、§11.4.1、§11.4.2.2、§12.1](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/stream/fetch.rs` / `decoder.rs` / `encoder.rs` にフィールドを持たない `0x80` / `0x81` の entry がない。
Track 終端は `FetchOk::end_of_track` に依存しており、`crates/moq-sub/src/pipeline.rs` の FETCH entry 処理も Object または範囲指示を前提にしている。

## 設計方針

直前 Object の Group 終端を `0x80`、Track 終端を `0x81` で表現する。
End of Track は End of Group も意味し、prior Object ID を `2^64 - 1` にする。
publisher は既知の Track 終端 Object の後に End of Track、応答末尾 Object が Group 終端と分かる場合は End of Group を送る。
decoder の prior state、終端後の不正 Object 判定、利用側の通知・破棄に反映する。

## 完了条件

- marker 自体に後続 ID フィールドがなく、分割受信で正常に読めること。
- marker の前後の prior state と次 Group の復元が仕様に合い、存在しない prior Object を参照する形を検証すること。
- FETCH_OK の旧フラグを使わず Track / Group 終端を把握し、既知の終端を送出できること。

## 依存・関連

- 先行: [0396](0396-moqt-draft-23-fetch-range-delta.md)
- 先行: [0397](0397-moqt-draft-23-fetch-object-id-delta.md)

## 解決方法

`src/stream/fetch.rs` の低水準 codec に `FetchStreamEntry::EndOfGroup` (0x80) と `FetchStreamEntry::EndOfTrack` (0x81) を追加した。
draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields) の "When Serialization Flags is 0x80 (End of Group) or 0x81 (End of Track), no other fields are present." に従い、encode は marker の vi64 だけを書き、decode もその vi64 だけを消費する。
0x80 / 0x81 は 128 以上の特殊値なので、Table 8 に無い 128 以上を MALFORMED_OBJECT にする判定より先に値で分岐する。
vi64 は 2 バイト (`0x80 0x80` / `0x80 0x81`) であり、1 バイト目だけではデータ不足になるため、分割受信では 2 バイト目が届いた時点で entry が確定し、続くバイトは次の entry として読まれる。

`src/stream/decoder.rs` の `FetchStreamDecoder` は、marker を受けると `prior_state` の `group_id` と `subgroup_id` / `publisher_priority` を保持したまま `object_id` だけを `2^64 - 1` にする。
同節の "The prior Object is the last Object in its Group, or in the Track, respectively. End of Track also indicates End of Group. The prior Object ID becomes 2^64-1." をそのまま写した。
sentinel は実在する Object を指さないため、終端した Group に後続 Object を置くと (Group ID Delta が absent のままでは) prior Object ID + Object ID Delta + 2 または prior + 1 が u64 を超え、DELTA_ENCODING_OVERFLOW (0x400) になる。
同 §12.2 (Session Termination Codes) の "When more than one code applies, an endpoint uses DELTA_ENCODING_OVERFLOW or FIELD_LENGTH_EXCEEDED in preference to the other codes." により、この場合は malformed track ではなく DELTA_ENCODING_OVERFLOW を優先する。
次の Group の Object は Group ID Delta が present になり Object ID が絶対値になるため、そのまま復元できる。

marker より前に Object も End of Range も無い場合は、存在しない prior Object を参照する形になるため MALFORMED_OBJECT (0x201) にする (§12.2 の "a flag references a prior Object that does not exist")。
End of Track の後の Object と End of Range は §12.1 (Malformed Tracks) 条件 5 ("An Object is received whose Group and Object ID are larger than the final Object in the Track.") に該当するため MalformedTrack にする。
marker の重複は End of Track が End of Group も意味するため受理する。

`src/stream/encoder.rs` の `FetchStreamEncoder` に `encode_end_of_group` / `encode_end_of_track` を追加し、直前の状態から marker を書ける条件を検証する。
Object も End of Range も無い状態では ProtocolViolation、End of Track の後の Object と End of Range も ProtocolViolation にする。
End of Group の後に同じ Group の Object を書くことも、prior Object ID が `2^64 - 1` になるため ProtocolViolation にする。

`crates/tokio-moq/src/moqt_client.rs` の `FetchResponseWriter` に `write_end_of_group` / `write_end_of_track` を追加し、publisher が既知の終端を送出できるようにした。
`crates/moq-sub/src/pipeline.rs` の FETCH entry 処理は `log_end_of_range` を `log_fetch_boundary` に広げ、End of Group / End of Track を応答の終端としてログに残して配送しない。
FETCH_OK の End of Track フラグは読まず、データストリームの marker で終端を把握する。

## 完了条件の検証

| 完了条件 | 検証 |
| --- | --- |
| marker に後続 ID フィールドがない | `fetch_stream_object::end_group_and_track_markers_have_no_following_fields` が marker の golden バイト列と消費バイト数 2 を 3 つの prior 文脈で検証し、`fetch::fetch_stream_marker_roundtrip` が PBT で固定する |
| marker の分割受信 | `fetch_stream_object::end_group_marker_split_across_the_varint_is_completed_by_the_second_byte` が 1 バイト目で UnexpectedEof、2 バイト目で確定、直後から次の entry を読めることを検証する |
| marker 前後の prior state と次 Group の復元 | `end_of_group_ends_the_group_and_restores_the_next_group` と `end_of_group_restores_the_next_group_in_descending_order` が昇順 5 → 6 と降順 10 → 9 の Group と絶対 Object ID の復元を検証する |
| 存在しない prior Object を参照する形 | `marker_before_any_object_is_malformed_object` が MALFORMED_OBJECT を、`object_in_the_ended_group_is_delta_encoding_overflow` が sentinel を参照する Object の DELTA_ENCODING_OVERFLOW を検証する |
| FETCH_OK の旧フラグを使わない | `crates/moq-sub/src/pipeline.rs` の `FetchOkReceived` 分岐が `end_of_track` を読まず、`log_fetch_boundary` が End of Group / End of Track を応答の終端として通知する |
| 既知の終端を送出できる | `encoder_writes_end_of_group_and_end_of_track` が 2 バイトの marker を低水準 codec で確認し、`peer_harness::fetch_response_writer_writes_end_of_group_and_track` が実 stream への送出と受信側の解決を検証する |

上記に加え、次のテストでも同じ契約を検証している。

- `end_of_group_after_end_of_range_keeps_the_decoded_group` が End of Range の Location を marker が引き継ぐことを検証する。
- `entry_after_end_of_track_is_malformed_track` が End of Track 後の Object と End of Range の MalformedTrack、marker の重複の受理を検証する。
- `fetch_group_track_end` の 2 テストが 1 バイトずつの push と一括 push の一致を検証する。
- `decoder::fetch_decoder_split_invariance_with_markers` と `encoder::encoder_decoder_roundtrip_with_group_and_track_end` が任意の 1 分割での一致を PBT で検証する。
- `rg end_of_track crates/moq-sub/src crates/moq-pub/src` が 0 件であり、FETCH_OK の旧フラグへの依存が残っていないことを確認した。

`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` を実行した。
`cargo test --workspace` のうち、FETCH の変更が影響する `-p shiguredo_moqt --test test_stream`、`-p pbt --test prop_stream`、`-p tokio-moq`、`-p moq-sub`、`-p moq-pub` はすべて成功している。
