# Gone 状態と End of Gone Range を FETCH に追加する

- Created: 2026-11-03
- Completed: 2026-11-04
- Branch: develop
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §2.1.2、§3.2.2、§11.4.1、§11.4.2](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/stream/fetch.rs` の `FetchStreamEntry` は Non-Existent / Unknown / Timed-Out の 3 種の範囲を扱う。
`src/stream/decoder.rs` の `DecodedFetchEntry`、`src/stream/encoder.rs` の公開メソッドと `crates/tokio-moq/src/moqt_client.rs` の writer に Gone の表現がない。

## 設計方針

Serialization Flags `0x40C` の End of Gone Range を codec、型付き decoder、writer に追加する。
Gone は元の publisher が再配送できないが存在自体を否定しない状態であり、Non-Existent と区別する。
`crates/moq-sub/src/pipeline.rs` の FETCH entry の分類・通知も追随させる。
元 publisher が state を失った過去を Non-Existent と断定しない使い方を明記する。
キャッシュやリレーを実装する課題には広げない。

## 完了条件

- Gone Range の encode / decode と分割受信、通常 Object との混在が動くこと。
- Gone の後に別経路から Object を受け取っても不存在との矛盾として拒否しないこと。
- Unknown / Timed-Out / Non-Existent とアプリが区別でき、範囲の delta 符号化と prior state が新規則に従うこと。

## 依存・関連

- 先行: [0396](0396-moqt-draft-23-fetch-range-delta.md)

## 解決方法

`src/stream/fetch.rs` の低水準 codec に `FetchStreamEntry::EndOfGoneRange` (0x40C) を追加した。
draft-ietf-moq-transport-23 §11.4.1 (Fetch Header) Table 8 の予約値であり、同 §11.4.2 (End of Range) に従って Group ID Delta と Object ID Delta を必ず運び、Subgroup ID / Priority / Properties は持たない。
encode は 1 個の vi64 (`0x84 0x0C`) と 2 個の delta を書き、decode は同じ 3 個の vi64 を読む。
4 種の End of Range はフィールド構造が同じで Serialization Flags だけが異なる。

Gone の意味は同 §2.1.2 (Object States) の "The original publisher cannot deliver the Object and will not be able to, but does not know whether it exists." であり、Non-Existent の "This state is permanent. All signals that an Object does not exist are authoritative." とは異なる。
元の publisher が再起動で state を失った場合や保持期間を過ぎた Object は Gone として通知し、Non-Existent と断定してはならない (同 §2.1.2 / §3.2.2 (Gaps in a Fetch Stream))。
この使い分けを `FetchStreamEntry` / `DecodedFetchEntry` / `FetchStreamEncoder::encode_end_of_gone_range` / `FetchResponseWriter::write_end_of_gone_range` の doc に明記した。

`src/stream/decoder.rs` の `FetchStreamDecoder` に `DecodedFetchEntry::EndOfGoneRange` を追加した。
絶対 Location への解決は既存の `resolve_end_of_range` (§11.4.2.1 (ID Decoding)) を共有するため、昇順の加算 / 降順の減算 / 同一 Group の +1 / u64 を外れる場合の DELTA_ENCODING_OVERFLOW がそのまま適用される。
prior state の更新も `update_prior_for_end_of_range` (§11.4.2.2 (Prior Object Fields)) を共有し、prior Subgroup ID / Priority は直前の actual Object の値を保持したまま Group ID / Object ID だけを Gone の終端に更新する。

デコーダは Gone の範囲を「存在しない範囲」として記録しない。
そのため Gone の後に同一 Group の続きや別 Group の Object が来ても不存在の矛盾として拒否せず、通常どおり解決する。
Non-Existent を恒久的な不存在として扱うかどうかはアプリの責務であり、Gone を Non-Existent と同じ集合に入れると、別経路から後で届く Object を §12.1 (Malformed Tracks) 条件 10 で誤って拒否してしまう。

`src/stream/encoder.rs` に `encode_end_of_gone_range` を追加した。
既存の `encode_end_of_range` を共有するため、Group Order と逆方向の Location、同一 Group で直前以前の Object ID、End of Track の後の書き出しは ProtocolViolation になる。
`crates/tokio-moq/src/moqt_client.rs` の `FetchResponseWriter` に `write_end_of_gone_range` を追加し、publisher が Gone の範囲を送出できるようにした。

`crates/moq-sub/src/pipeline.rs` の `log_fetch_boundary` は 4 種の End of Range を別の理由としてログに残すようにし、Gone を `gone`、Non-Existent を `non-existent` として区別した。
取得できなかった理由がアプリのログで区別できる。
キャッシュ / リレーの実装には広げていない (`crates/moq-pub` の `cache::FetchResponseEntry` は Non-Existent / Unknown の 2 種のままであり、Gone を返す取得計画は持たない)。

## 完了条件の検証

| 完了条件 | 検証 |
| --- | --- |
| Gone Range の encode / decode | `fetch_gone_range::gone_range_entry_roundtrip_has_flags_and_two_deltas` が 0x40C の vi64 と 2 個の delta からなる golden バイト列の往復を 3 つの prior 文脈で検証し、`fetch::fetch_stream_entry_end_of_range_roundtrip` が PBT で 4 種の flags の往復を固定する |
| 分割受信 | `fetch_gone_range::gone_range_split_reception_matches_whole_push` が 1 バイト / 2 バイトずつの push と一括 push の一致を検証し、`encoder::encoder_decoder_roundtrip_with_end_of_range` が Gone を含む操作列でも任意の 1 分割で結果が変わらないことを PBT で検証する |
| 通常 Object との混在 | `fetch_gone_range::gone_range_mixed_with_objects_roundtrips` が Object → Gone → Object → Gone → Object の往復と分割受信を、`decoder::test_fetch_end_of_range_entries_carry_no_properties` が 4 種の連続復号を検証する |
| Gone の後に届いた Object を不存在の矛盾として拒否しない | `fetch_gone_range::object_after_gone_range_is_not_rejected` が Gone の直後の同一 Group の Object と別 Group の Object を受理することを、`encoder::end_of_gone_range_then_object` がエンコーダ経由の往復で検証する |
| Unknown / Timed-Out / Non-Existent との区別 | `gone_range_is_distinct_from_non_existent_range` が同じ delta でも別 variant に復号されることを、`mixed_end_of_range_kinds_decode_in_order` が 4 種の順次復号を検証する。`log_fetch_boundary` は Gone を `gone` として別メッセージで通知する |
| delta 符号化と prior state が新規則に従う | `first_gone_range_is_absolute_and_after_object_is_delta` / `descending_gone_range_resolves_with_subtraction` が絶対 ID と加減算を、`gone_range_preserves_prior_actual_object_fields` が Subgroup ID / Priority の保持を、`encoder_writes_gone_range_golden_bytes` がエンコーダの golden を検証する |
| publisher が Gone を送出できる | `peer_harness::fetch_response_writer_writes_end_of_gone_range` が実 uni stream に 0x40C が載り、受信側が `EndOfGoneRange` として解決することを検証する |

上記に加え、次のテストと確認を行った。

- `fetch_stream_object::encode_end_of_range_with_properties_rejected` と `encode_end_of_range_without_properties_succeeds` が 4 種すべてで Properties の有無の扱いを検証する。
- `fetch_gone_range::encoder_rejects_gone_range_after_end_of_track` が End of Track 後の Gone の拒否と、拒否時にバイト列を書かないことを検証する。
- `fuzz_fetch_stream_decoder` が Gone を含む任意入力のクラッシュ耐性を、`fuzz_fetch_stream_encoder` が Gone を書く入力列のパニック安全性を担う。

`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`RUSTDOCFLAGS="-D warnings" cargo doc -p shiguredo_moqt --no-deps` を実行し、いずれも成功した。
