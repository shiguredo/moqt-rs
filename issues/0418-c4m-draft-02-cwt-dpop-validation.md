# CWT DPoP の token 束縛・文脈・replay を一括検証する

- Created: 2026-11-03
- Completed: 2026-11-04
- Branch: develop
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

- 先行: [0414](closed/0414-c4m-draft-02-action-codes.md)
- 先行: [0416](closed/0416-c4m-draft-02-cwt-dpop-proof-codec.md)
- 先行: [0417](closed/0417-c4m-draft-02-cose-key-thumbprint-binding.md)
- 関連: [0346](0346-bug-c4m-dpop-validation.md)
- 関連: [0415](closed/0415-c4m-draft-02-connection-scoped-bearer-replay.md)

## 差分の確認範囲

- `CwtDpopVerification` と `CwtDpopProof::verify_against_cat_token` (CAT 署名から replay まで
  を 1 つの入口で検証)
- `CwtAuthorizationContext::verify_request` / `CwtDpopProof::verify_authorization_context` /
  `verify_freshness`
- `CwtDpopReplayCache` (window 上限固定、`iat` + 上限までの保持、失敗時の非記録)
- actx の action ごとの target 規則、`ath` の照合、`DpopError` の追加 variant
- テスト / PBT の追加

proof のデコード (COSE_Sign1 / `typ` / 非対称 `alg` の検査) は 0416 の codec の責務であり、
この入口はデコード済みの proof を受け取る。旧 JWT 形式の `DpopReplayCache` の保持期間の
不具合は 0346 の担当であり、CWT 側には正しい保持期間だけを実装した (旧 API は変更しない)。
0415 の `BearerReplayCache` は bearer トークンの接続束縛用であり、proof の `cti` 用に
代用しない。docs / README / skills の更新は 0420 で行う。CHANGES.md は CODEBASE の方針に
より更新しない。

## 解決方法

draft-02 §4.3 の受理判定を 1 つの入口で行う API を追加した。

- `CwtDpopVerification` は、提示された `CatToken` (raw bytes を含む)、CAT の信頼済み鍵、
  `VerifyOptions`、`ClaimValidationOptions`、要求 action、`Option` の namespace / track、
  0416 の claim label 設定 (`CwtDpopProofCodec`)、`catdpop` に window が無い場合の既定
  window を保持する
- `CwtDpopProof::verify_against_cat_token` は、CAT の署名 / MAC 検証
  (`CatToken::verify_with`) → `CatClaims::validate` → proof の label 設定の整合 →
  proof の署名 (`verify_signature`) → `cnf` の `ckt` / `jkt` 束縛
  (`verify_key_binding`) → `ath` の照合 → actx の type / action / namespace / track
  (→ 鮮度) → `CatClaims::authorize` → replay の確認・記録の順に実行する。全検証が
  成功したときだけ replay を記録し、途中で失敗した場合は記録しない
- `ath` は `CatToken::raw_token` が返す提示バイト列の SHA-256 と比較する。再エンコードや
  Base64 正規化をせず、同じ意味でも異なるバイト列のトークンには `ath` を流用できない
- actx の target 規則: SETUP は namespace / track の欠落を必須とし (要求側・proof 側の
  どちらかに存在すれば拒否)、`PublishNamespace` / `SubscribeNamespace` / `SubscribeTracks`
  は namespace を必須・track を不要、`Subscribe` / `RequestUpdate` / `Publish` / `Fetch` /
  `TrackStatus` は namespace と track の両方を必須とする。欠落と空の bytes / 空の
  namespace 配列は区別し、存在する target はフィールド列・バイト列を正規化せず完全一致で
  比較する
- 鮮度は `catdpop` の window (label 0)、無ければ呼び出し側が明示した既定値を使う。
  window は有限かつ非負で、`|reference_time - iat| <= window` の境界を受理する
- `CwtDpopReplayCache` は構築時に有限かつ正の `max_window_seconds` を固定し、proof の
  window がこれを超える場合は `ReplayWindowTooLarge` で拒否する。記録は
  `cti` → `iat + max_window_seconds` で保持し、参照時刻が保持期限と等しい間は保持して
  超えた後にだけ解放する。現在の proof の window で古い記録を早期破棄しないため、
  0346 の旧 JWT 側の不具合を繰り返さない。成功した検証だけを記録し、`honor_jti` が
  偽の場合は記録しない
- `moqt-reval` の再検証と拒否は `CatClaims::validate` の責務ではなく、この入口の対象外
  である (利用側の責務として doc に明記した)
- `cnf` を持たないトークンは proof を伴っても拒否し、`cnf` を持つ token の action を
  proof なしで認可しない。key 3 は IANA では `kid` であり、鍵束縛の根拠にしない。
  `cnf` による鍵束縛と `catreplay` の接続束縛が併存するトークンは、両者の意味を定義して
  いないため `CatReplayWithConfirmation` で拒否する
- `REQUEST_UPDATE` は transport draft のメッセージに FQN を持たないが、本 issue の target
  規則では namespace と track の両方を必須とする (namespace subscription の更新は本 API の
  対象外であり、呼び出し側が空の track を渡すことを前提にしない)
- テスト / PBT: 実 ES256 の鍵で ckt / jkt の成功、proof の署名改変・別鍵・別 token の
  `ath`・action / namespace / track の相違 (欠落・空・余分)・鮮度外と境界・replay・
  CAT 署名 / claims / scope の失敗と非記録・失敗後の再試行、異なる window の混在、
  SETUP の target 省略、cache の保持期間と window 上限、ES384 / EdDSA の proof、
  保持期限が有限でない場合の拒否を固定した。PBT に actx target の一致と replay cache の
  保持期間・window 境界を追加した

## 完了条件の検証

| 完了条件 | 検証 |
| --- | --- |
| proof 欠落、署名改変、別 token の ath、別鍵、action / namespace / track の相違、鮮度外・同一 cti replay を実暗号で拒否すること | `tests/test_c4m/dpop/cwt/verification.rs` の `cwt_dpop_rejects_*` / `cwt_dpop_ath_*` / `cwt_dpop_freshness_*` (proof 欠落は型と `verify_bearer_rejects_confirmation` で担保) |
| CAT 自体の署名・通常 claims・scope のいずれかが不正な場合も拒否し、CWT proof が正しくても replay 状態を記録しないこと | 同 `cwt_dpop_rejects_invalid_cat_*_without_recording` / `cwt_dpop_rejects_unauthorized_scope_without_recording` |
| 同じ意味でも異なる token bytes に同じ ath を流用できないこと | 同 `cwt_dpop_ath_requires_exact_token_bytes` |
| 異なる window の混在、失敗後の再試行、SETUP の target 省略を検証すること | 同 `cwt_dpop_replay_cache_*` / `cwt_dpop_setup_*` / `cwt_dpop_rejects_invalid_freshness_parameters` |
| 0346 の旧 JWT 向けの不具合を重複実装せず、必要な replay 保持期間の修正を新 CWT 側にも反映すること | `CwtDpopReplayCache` が `iat + max_window_seconds` で保持し、現在の proof の window で早期破棄しないこと (`cwt_dpop_replay_cache_*` を主根拠にする)。旧 `DpopReplayCache` は変更していない |

検証コマンド `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` /
`cargo test -p shiguredo_moqt` / `cargo test -p shiguredo_moqt --features aws-lc-rs --test test_c4m` /
`cargo test -p pbt` / `cargo test -p moq-sub` / `cargo test -p tokio-moq --lib with_auth_parameters` /
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` /
`cargo check --manifest-path fuzz/Cargo.toml --bins` はすべて成功した。
