# Gone 状態と End of Gone Range を FETCH に追加する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/add-moqt-draft-23-fetch-gone-range
- Polished: {YYYY-MM-DD}

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
