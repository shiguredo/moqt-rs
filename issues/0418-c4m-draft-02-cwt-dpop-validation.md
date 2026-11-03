# CWT DPoP の token 束縛・文脈・replay を一括検証する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/add-c4m-draft-02-cwt-dpop-validation
- Polished: 2026-11-03

## 目的

[draft-ietf-moq-c4m-02 §4.1、§4.3、§4.4](../refs/moq/draft-ietf-moq-c4m-02.txt) の変更に追従する。

## 現状

現行の `src/c4m/dpop.rs` の `verify_against_cat_token` / `verify_against_token` / `DpopReplayCache` は JWT の文字列 jti・文字列 ath・JSON actx を前提にする。
`CatToken::verify_with` は署名を検証する API であり、これだけで cnf を含む token の action を認可できるわけではない。

## 設計方針

CWT proof の署名、ckt / jkt 束縛、iat の鮮度、catdpop が要求する cti replay 検証と、actx の type・action・namespace・track を一括検証する API を用意する。
公開の受理入口には `CatToken`、CAT の検証用の信頼済み鍵 / `VerifyOptions`、`ClaimValidationOptions`、要求 action と target、0416 の claim label 設定、CWT 用 replay cache を渡す。
CAT の署名・MAC 検証 → `CatClaims::validate` → CWT proof の構造・署名 / 0417 の鍵束縛 / ath / actx / 鮮度の検証 → `CatClaims::authorize` による scope 認可 → replay の確認・記録の順に実行する。署名・claims・scope を呼び出し側が別途済ませたと仮定する API にしない。
SETUP は namespace / track の欠落を受理する。`PublishNamespace` / `SubscribeNamespace` / `SubscribeTracks` は namespace を必須、track を不要とし、`Subscribe` / `RequestUpdate` / `Publish` / `Fetch` / `TrackStatus` は両方を必須にする。
欠落と空の bytes / namespace 配列は区別し、存在する target は MOQT 要求のフィールド列・バイト列と正確に比較する。SETUP の target、namespace action の track など要求に存在しない余分な target は拒否する。旧 JWT のシリアライズ文字列や resource 検証は流用しない。
ath は提示された CAT token の正確なバイト列の SHA-256 (bstr) と照合する。
cnf がある action を proof なしで認可しない。
raw token を再エンコード・Base64 正規化してから hash しない。
全検証に成功してから replay 状態を記録し、異なる window の後続検証で既存 cti を早期破棄しない。
CWT 用 cache は cti を bstr のまま保持し、同じ token / proof を受理する検証主体が全接続で共有する。cache ごとに有限かつ正の `max_window_seconds` を固定し、各 proof の window がこれを超える場合は拒否する。
window は有限かつ非負とし、catdpop に window がない場合の値も呼び出し側から明示する。鮮度検証は iat の前後に同じ window を適用し、境界と等しい値は受理する。記録は `iat + max_window_seconds` と等しい時刻まで保持し、それを超えた後にだけ解放する。
0415 の bearer 接続束縛 cache と proof の cti cache は相互に代用しない。

## 完了条件

- proof 欠落、署名改変、別 token の ath、別鍵、action / namespace / track の相違、鮮度外・同一 cti replay を実暗号で拒否すること。
- CAT 自体の署名・通常 claims・scope のいずれかが不正な場合も拒否し、CWT proof が正しくても replay 状態を記録しないこと。
- 同じ意味でも異なる token bytes に同じ ath を流用できないこと。
- 異なる window の混在、失敗後の再試行、SETUP の target 省略を検証すること。
- 0346 の旧 JWT 向けの不具合を重複実装せず、必要な replay 保持期間の修正を新 CWT 側にも反映すること。

## 依存・関連

- 先行: [0414](0414-c4m-draft-02-action-codes.md)
- 先行: [0416](0416-c4m-draft-02-cwt-dpop-proof-codec.md)
- 先行: [0417](0417-c4m-draft-02-cose-key-thumbprint-binding.md)
- 関連: [0346](0346-bug-c4m-dpop-validation.md)
- 関連: [0415](0415-c4m-draft-02-connection-scoped-bearer-replay.md)
