# CWT 形式の DPoP proof を生成・解析する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/add-c4m-draft-02-cwt-dpop-proof-codec
- Polished: 2026-11-03

## 目的

[draft-ietf-moq-c4m-02 §4.2](../refs/moq/draft-ietf-moq-c4m-02.txt) の変更に追従する。

## 現状

`src/c4m/dpop.rs` の `DpopProof` / `DpopProofBuilder` は compact JWT、文字列 jti と JSON actx を扱い、CWT は未対応と説明している。
`src/c4m/cose.rs` には CoseSign1 があるが、Header は kccs (label 14) を型付きに解釈しない。

## 設計方針

COSE_Sign1 を使う proof の型と発行・解析 API を追加する。
既存 JWT の `DpopProof` / `DpopProofBuilder` とは別の CWT 用型・API にし、本 issue は encode / decode と proof 単独の署名検証までを扱う。CAT token への束縛、scope、鮮度、replay を統合した受理判定は 0418 で実装する。
protected header に alg / typ=`dpop-proof+cwt` / kccs を置き、kccs の CWT Claims Set 内の cnf から公開 COSE_Key を取り出す。
kccs は bstr や署名付き CWT ではなく CCS の map とし、`8: cnf` → `1: COSE_Key` → 公開鍵 map の構造を扱う ([RFC 9528 §3.5.3.1、§10.6](https://www.rfc-editor.org/rfc/rfc9528.html#section-3.5.3.1)、RFC 8747 §3.2)。発行時に秘密鍵パラメーターを含めず、受信側も公開鍵として検証に使う。
`src/c4m/cose.rs` の Header に kccs の解析を追加し、理解した label 14 を `crit` に含む protected header も処理できるようにする。未理解の critical header を受理する変更にはしない。
payload は cti (7、bstr)、iat (6、int)、actx と ath を持つ CWT とする。
actx は type / 数値 action / namespace の bstr 配列 / track の bstr を扱い、正規化しない。
actx と ath の claim label は TBD のため、実験用番号を呼び出し側から明示させ、正式割当を捏造しない。
同じ label 設定を発行・解析・検証へ渡す。actx と ath が同じ値、またはいずれかが iat=6 / cti=7 と衝突する設定は API の設定時に拒否する。
通常の未知 CWT claim は RFC 8392 §3 に従って保持・無視し、必須の既知 claim の代わりにはしない。重複 claim は拒否し、未理解 critical header の拒否とは区別する。

## 完了条件

- ES256 の実署名で発行・解析でき、対称 alg、protected 必須項目の欠落・誤配置、誤った typ / kccs / COSE 形式を拒否すること。
- SETUP / namespace / track action ごとの任意フィールドを表現し、JSON 文字列へ変換せず bytes を保持すること。
- 未知の通常 claim を保持・無視し、重複 claim と未理解 critical header を拒否すること。cti / iat の型違反、label 設定の衝突、署名領域を検証すること。
- モック暗号を使わず、実装済み `CoseCrypto` と aws-lc-rs feature で確認すること。

## 依存・関連

- 先行: [0414](0414-c4m-draft-02-action-codes.md)
- 関連: [0418](0418-c4m-draft-02-cwt-dpop-validation.md)
