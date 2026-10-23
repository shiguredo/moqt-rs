# FETCH 応答 Object の Properties の扱いを明確にする

- Created: 2026-10-19
- Completed: 2026-10-21
- Branch: feature/change-fetch-stream-entry-properties
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §11.4.3 (Fetch Object) の Object は Properties を持ち得るが、`FetchStreamEntry` は decode 時に Properties を黙って捨てる。フィルタ評価にだけ使って利用者へ渡さない設計なら、その契約を doc に明記する。利用者へ渡す設計なら accessor を追加する。黙って捨てる現状は、利用者が Properties の存在を認識できない。

## 現状

- `src/stream/fetch.rs` の `FetchStreamEntry::decode` は Properties を消費して `properties_bytes` として保持するが、公開 accessor が無い。
- `src/session/fetch.rs` は応答の受信時に Properties を購読フィルタの評価に使うが、アプリへ渡す経路が無い。
- テストは Properties の有無を検証していない。
- 影響: アプリが Object 単位のメタデータ (Priority / Expires / Delivery Timeout など) を取得できない。

## 設計方針

- Properties をアプリへ渡す場合は、`FetchStreamEntry` に `properties()` を追加し、session の受信イベントにも載せる。
- 渡さない場合は、decode の doc とモジュール doc に「Properties はフィルタ評価にのみ使い、利用者へは渡さない」と明記する。
- どちらを採るかを決めてテストで固定する。

## 完了条件

- Properties の扱いが doc と実装で一致していること。
- 非空 Properties を持つ FETCH Object のテストがあること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

本 issue は `issues/closed/0124-bug-fetch-object-properties.md` と実質同一であり、現行実装では報告された問題は存在しないため closed にする。コード変更は行わない。

- 重複先: `issues/closed/0124-bug-fetch-object-properties.md` は「FETCH 経路で Object Properties が application に渡らない」を扱い、`Completed: 2026-09-23` で `DecodedFetchObject::properties_bytes` を追加する修正を完了している。本 issue は同じ問題を別の言い方で扱っている。
- 実装済み: `src/stream/decoder.rs` の `DecodedFetchObject` は `pub properties_bytes: Option<Vec<u8>>` を持ち、`FetchStreamDecoder::try_decode_entry` の doc は「Object エントリの Object Properties は `properties_bytes` に保持される。」と明記している。
  application は `FetchStreamDecoder` で FETCH 応答をデコードし、`DecodedFetchObject::properties_bytes` から Properties を取得できる。
- `FetchStreamEntry::decode` は `decode_with_properties` の戻り値から Properties を捨てるが、`decode_with_properties` は `pub(crate)` であり、application 向けの公開経路は `FetchStreamDecoder::try_decode_entry` である。`FetchStreamDecoder` は Properties を `DecodedFetchObject` に保持するため、`FetchStreamEntry` 単体に accessor を追加する必要はない。
- `src/session/data.rs` の `recv_fetch_entry` は「entry の内容自体は検証しない (codec 側で検証済みのため)」とし、FETCH stream の activity 記録だけを行う。
  `src/session/fetch.rs` にも Object Properties を購読フィルタの評価に使う処理は無く、`include_properties` は FETCH 要求のパラメータ検証と FETCH_OK の Track Properties にだけ使われる。したがって「`src/session/fetch.rs` は応答の受信時に Properties を購読フィルタの評価に使う」は現行実装と一致しない。
- `tests/test_stream/decoder.rs` の `test_fetch_object_properties_are_exposed` は、非空 Properties を持つ FETCH Object をデコードし、`properties_bytes` から `LocProperties::decode` で Timestamp / Timescale / Video Config を取り出せることを固定している。
  `test_fetch_object_without_properties_is_none`、`test_fetch_object_with_empty_properties_keeps_length_varint`、`test_fetch_non_minimal_properties_length_is_preserved` も存在する。したがって「テストは Properties の有無を検証していない」は現行実装と一致しない。
- `examples/moq-sub/src/pipeline.rs` の `read_video_fetch_stream` / `read_audio_fetch_stream` は `obj.properties_bytes.as_deref()` を配送処理へ渡しており、application が FETCH 経路で Properties を利用している実例になっている。したがって「アプリへ渡す経路が無い」「アプリが Object 単位のメタデータを取得できない」は現行実装と一致しない。
- 仕様参照の誤り: draft-ietf-moq-transport-22 に §11.4.3 は存在しない。FETCH Object のフィールドは §11.4.1 (Fetch Header) の Figure 29 に `[Properties (..),]` として規定され、Properties の有無は §11.4.1.1 (Flags) の bit 0x20 で規定される。
- 設計方針の前提誤り: `FetchStreamEntry` は `#[derive(Debug, Clone, Copy, PartialEq)]` の wire レベル enum であり、Properties を保持しない。application 向けの `DecodedFetchObject` が既に Properties を保持しているため、`FetchStreamEntry` に `properties()` を追加する必要はない。
  Session は FETCH 応答の Object 内容をイベントで運ぶ設計ではなく、application が `FetchStreamDecoder` で data stream を直接デコードする設計である (`src/session/data.rs` の `recv_fetch_entry` の doc、および `src/session/data.rs` の `report_mid_object_fin` の doc を参照)。
