# CWT 形式の DPoP proof を生成・解析する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/add-c4m-draft-02-cwt-dpop-proof-codec
- Polished: {YYYY-MM-DD}

## 目的

[draft-ietf-moq-c4m-02 §4.2](../refs/moq/draft-ietf-moq-c4m-02.txt) の変更に追従する。

## 現状

`src/c4m/dpop.rs` の `DpopProof` / `DpopProofBuilder` は compact JWT、文字列 jti と JSON actx を扱い、CWT は未対応と説明している。
`src/c4m/cose.rs` には CoseSign1 があるが、Header は kccs (label 14) を型付きに解釈しない。

## 設計方針

COSE_Sign1 を使う proof の型と発行・解析 API を追加する。
protected header に alg / typ=`dpop-proof+cwt` / kccs を置き、kccs の CWT Claims Set 内の cnf から公開 COSE_Key を取り出す。
payload は cti (7、bstr)、iat (6、int)、actx と ath を持つ CWT とする。
actx は type / 数値 action / namespace の bstr 配列 / track の bstr を扱い、正規化しない。
actx と ath の claim label は TBD のため、実験用番号を呼び出し側から明示させ、正式割当を捏造しない。

## 完了条件

- ES256 の実署名で発行・解析でき、対称 alg、protected 必須項目の欠落・誤配置、誤った typ / kccs / COSE 形式を拒否すること。
- SETUP / namespace / track action ごとの任意フィールドを表現し、JSON 文字列へ変換せず bytes を保持すること。
- 未知・重複 claim、cti / iat の型違反、署名領域を検証すること。
- モック暗号を使わず、実装済み `CoseCrypto` と aws-lc-rs feature で確認すること。

## 依存・関連

- 先行: [0414](0414-c4m-draft-02-action-codes.md)
