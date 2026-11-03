# moqt claim を map 化し未知のキーを fail-closed で扱う

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-c4m-draft-02-moqt-claim-map
- Polished: 2026-11-03

## 目的

[draft-ietf-moq-c4m-02 §2.1、§2.1.1](../refs/moq/draft-ietf-moq-c4m-02.txt) の変更に追従する。

## 現状

`src/c4m.rs` の `MoqtClaim::decode` / `encode` は scopes の配列を claim value そのものとして扱う。
`src/c4m/cat.rs` の `CatClaims::decode` もこの形へ直接委譲するため、draft 02 の map を読み書きできない。

## 設計方針

moqt claim を CBOR map とし、キー 0 に scopes 配列を格納する。
未知の map key を見つけた場合、制限を黙って無視せず、その claim を含む claim set を受理不可とする。
最上位 claim set なら token を拒否する。
Composite の分岐評価を実装していない現在の API で、未知キーを取り除いて認可する処理は追加しない。
複数 scopes 自体の OR と、Composite claim set の OR を混同しない。

## 完了条件

- 新しい map の既知バイト列で encode / decode / authorize が一致すること。
- キー 0 の欠落・型不一致・空 scopes と、未知キー付き claim を検証すること。
- 未知の制限が加わった token を認可せず、旧 array-only の値を新形式として誤受理しないこと。

## 依存・関連

- 関連: [0413](0413-c4m-draft-02-provisional-claim-labels.md)
- 関連: [0419](0419-c4m-draft-02-test-vectors.md)
