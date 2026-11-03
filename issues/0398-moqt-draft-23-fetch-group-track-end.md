# FETCH の End of Group と End of Track 指示を実装する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/add-moqt-draft-23-fetch-group-track-end
- Polished: {YYYY-MM-DD}

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
