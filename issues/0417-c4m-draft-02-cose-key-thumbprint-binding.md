# cnf の ckt による COSE Key Thumbprint 束縛を追加する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/add-c4m-draft-02-cose-key-thumbprint-binding
- Polished: 2026-11-03

## 目的

[draft-ietf-moq-c4m-02 §4.1、§4.3](../refs/moq/draft-ietf-moq-c4m-02.txt) の変更に追従する。

## 現状

`src/c4m/cat.rs` の `Confirmation` は jkt (323) と draft 01 の key 3 を扱うが、ckt (5) は raw に残る。
`src/c4m/dpop.rs` の `verify_key_binding` は JWK の SHA-256 サムプリントだけを照合する。

## 設計方針

Confirmation に ckt を追加し、proof の kccs から得た公開 COSE_Key のサムプリントと照合する。
公開鍵は 0416 の CWT proof 解析 API が返す表現を再利用し、kccs の別パーサーは追加しない。
ckt (key 5) は SHA-256 の 32 バイトの bstr として decode / encode し、型・長さが不正なら拒否する。
[RFC 9679 §3・§4](https://www.rfc-editor.org/rfc/rfc9679.html#section-3) に従い、必須公開パラメータだけを決定的 CBOR に符号化して SHA-256 を計算する。
EC2 は非圧縮 x / y を用い、秘密鍵や kid / alg などの任意項目をハッシュ対象にしない。
jkt の互換検証では同じ鍵の等価な JWK を使う。
RFC 8747 §3.1 が cnf に単一の PoP 鍵を要求するため、ckt と jkt (key 323) が併存する場合は、同じ proof 公開鍵に対して両方のサムプリントが一致する場合だけ受理する。ckt の不一致や不正値を jkt への fallback で救済しない。
未知 confirmation method だけの cnf は拒否する。既知 method と併存する未知項目は raw に保持するが、認可成功の根拠にしない。
key 3 の旧解釈問題は 0346 に任せ、新方式で曖昧な fallback を増やさない。

## 完了条件

- EC2 と対応する OKP 鍵で独立した既知ベクタと一致すること。
- 任意 COSE key 属性の有無で値が変わらず、異なる鍵の proof を拒否すること。
- ckt と jkt をそれぞれの方式で検証し、併存時は同じ鍵で両方の一致を要求すること。片方だけの一致、不正な ckt、未知 confirmation method だけの cnf を認可成功にしないこと。
- 公開鍵を表すデータだけを使い、秘密情報をテスト出力や文書へ追加しないこと。

## 依存・関連

- 関連: [0346](0346-bug-c4m-dpop-validation.md)
- 先行: [0416](0416-c4m-draft-02-cwt-dpop-proof-codec.md)
