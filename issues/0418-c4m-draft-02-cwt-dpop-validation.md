# CWT DPoP の token 束縛・文脈・replay を一括検証する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/add-c4m-draft-02-cwt-dpop-validation
- Polished: {YYYY-MM-DD}

## 目的

[draft-ietf-moq-c4m-02 §4.1、§4.3、§4.4](../refs/moq/draft-ietf-moq-c4m-02.txt) の変更に追従する。

## 現状

現行の `src/c4m/dpop.rs` の `verify_against_cat_token` / `verify_against_token` / `DpopReplayCache` は JWT の文字列 jti・文字列 ath・JSON actx を前提にする。
`CatToken::verify_with` は署名を検証する API であり、これだけで cnf を含む token の action を認可できるわけではない。

## 設計方針

CWT proof の署名、ckt / jkt 束縛、iat の鮮度、catdpop が要求する cti replay 検証と、actx の type・action・namespace・track を一括検証する API を用意する。
ath は提示された CAT token の正確なバイト列の SHA-256 (bstr) と照合する。
cnf がある action を proof なしで認可しない。
raw token を再エンコード・Base64 正規化してから hash しない。
全検証に成功してから replay 状態を記録し、異なる window の後続検証で既存 cti を早期破棄しない。

## 完了条件

- proof 欠落、署名改変、別 token の ath、別鍵、action / namespace / track の相違、鮮度外・同一 cti replay を実暗号で拒否すること。
- 同じ意味でも異なる token bytes に同じ ath を流用できないこと。
- 異なる window の混在、失敗後の再試行、SETUP の target 省略を検証すること。
- 0346 の旧 JWT 向けの不具合を重複実装せず、必要な replay 保持期間の修正を新 CWT 側にも反映すること。

## 依存・関連

- 先行: [0414](0414-c4m-draft-02-action-codes.md)
- 先行: [0416](0416-c4m-draft-02-cwt-dpop-proof-codec.md)
- 先行: [0417](0417-c4m-draft-02-cose-key-thumbprint-binding.md)
- 関連: [0346](0346-bug-c4m-dpop-validation.md)
- 関連: [0415](0415-c4m-draft-02-connection-scoped-bearer-replay.md)
