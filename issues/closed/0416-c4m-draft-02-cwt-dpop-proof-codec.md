# CWT 形式の DPoP proof を生成・解析する

- Created: 2026-11-03
- Completed: 2026-11-04
- Branch: develop
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

- 先行: [0414](closed/0414-c4m-draft-02-action-codes.md)
- 関連: [0418](0418-c4m-draft-02-cwt-dpop-validation.md)

## 差分の確認範囲

- CWT 形式 DPoP proof の型・発行・解析・署名検証 API、actx の bytes 表現、claim label 設定
- COSE protected header の kccs (label 14) の解析と crit の理解、COSE_Key の CBOR マップ変換
- テスト / PBT / fuzz target の追加

CAT トークンとの束縛 (ckt / jkt)、ath の照合、scope・type・action・target の受理判定、
鮮度 (catdpop window)、cti の replay 検出は 0418 で行う。docs / README / skills の
CWT proof の説明更新は 0420 で行う。CHANGES.md は CODEBASE の方針により更新しない。
`refs/cbor/rfc9528.txt` はリポジトリに無いため、kccs の構造は draft §4.2.1 の例
(14: {8: {1: COSE_Key}}) に従った。

## 解決方法

draft-02 §4.2 の CWT 形式 DPoP proof (`dpop-proof+cwt`) の codec を追加した。

- 型: `CwtDpopProofCodec` (actx / ath の claim label を保持)、`CwtDpopProofClaims`、
  `CwtAuthorizationContext`、`CwtDpopProofHeader`、`CwtDpopProof`。発行は
  `CwtDpopProofCodec::build`、解析は `decode`、署名検証は `verify_signature` で行う。
  COSE_Sign1 (タグ 18) を使い、CWT タグ 61 は付けない (解析はタグ 61 付きも受理する)
- claim label は draft が `TBD_ACTX` / `TBD_ATH` のままで IANA 割当も未確定なため、
  呼び出し側が明示する。`CwtDpopProofCodec::new(actx_label, ath_label)` は両者が同じ値、
  または CWT の登録済み claim key (1〜8) と衝突する場合を拒否する。正式 label は
  コードに置かない
- payload は cti (7、bstr) / iat (6、int) / actx / ath (bstr) を必須とする。actx は
  {0: "moqt", 1: 数値 action, ?2: [* bstr], ?3: bstr} を扱い、namespace と track は
  bytes のまま保持して JSON 文字列へ変換しない。action は整数のまま保持し、未知の
  値も保持する。未知の通常 claim は `raw` に保持して無視し、必須 claim の代替には
  しない。重複 claim は拒否する (O(n log n) で検査する)
- protected header は alg (非対称のみ) / typ = "dpop-proof+cwt" / kccs を必須とし、
  protected 以外の配置や誤った typ、COSE_Sign1 以外の形式を拒否する。kccs (label 14)
  を理解済みの critical header として扱い、未理解の critical label は拒否する
- kccs は CWT Claims Set の map とし、`cnf` (8) → `COSE_Key` (1) から公開鍵を取り出す。
  COSE_Key は EC2 (kty=2, crv=1/2/3) と OKP (kty=1, crv=6) を扱い、秘密鍵パラメータ
  `d` と対称鍵を拒否する。RFC 9052 §7 の CDDL が `tstr / int` を許すため、`kty` / `crv` /
  `alg` は整数とレジストリ名の両方を受理し、`key_ops` は `"verify"` と整数 2 の両方を
  署名検証の許可として扱う。`alg` がある場合は proof の alg と一致することを検証する。
  `kid` / Base IV / 未知のメンバーは RFC 8747 §3.2 が他のメンバーの同梱を許すため無視する
- 発行時は actx.type が "moqt" であることを検証し、秘密鍵を含まない鍵では発行しない
- テスト / PBT / fuzz: ES256 / ES384 / EdDSA の実署名ラウンドトリップ、対称 alg
  (COSE_Mac0 と、COSE_Sign1 に対称 alg を入れた両方) の拒否、protected 必須項目の
  欠落・誤配置、crit 14 の受理と未理解 crit の拒否、COSE_Key の秘密鍵・未知 kty /
  crv・型エラー、重複 claim、actx の bytes 保持と任意フィールド、label 衝突、
  署名改ざん・別鍵、claims の roundtrip を固定した。PBT に claims の roundtrip と
  COSE_Key マップ変換、fuzz に decode の任意入力耐性を追加した

## 完了条件の検証

| 完了条件 | 検証 |
| --- | --- |
| ES256 の実署名で発行・解析でき、対称 alg、protected 必須項目の欠落・誤配置、誤った typ / kccs / COSE 形式を拒否すること | `tests/test_c4m/dpop/cwt.rs` の `es256_round_trip_preserves_bytes` / `rejects_*` / `accepts_tagged_untagged_and_cwt_tagged_sign1` |
| SETUP / namespace / track action ごとの任意フィールドを表現し、JSON 文字列へ変換せず bytes を保持すること | 同 `es256_round_trip_preserves_bytes` / `claims_round_trip_preserves_bytes_and_unknown_claims` / `claims_allow_namespace_and_track_to_be_absent`、PBT の `cwt_claims_roundtrip` |
| 未知の通常 claim を保持・無視し、重複 claim と未理解 critical header を拒否すること。cti / iat の型違反、label 設定の衝突、署名領域を検証すること | 同 `accepts_understood_critical_header` / `proof_rejects_duplicate_claims` / `claims_require_well_typed_members` / `codec_rejects_conflicting_claim_labels` / `rejects_tampered_*` / `build_*` |
| モック暗号を使わず、実装済み `CoseCrypto` と aws-lc-rs feature で確認すること | 同 `es256_round_trip_preserves_bytes` / `es384_and_eddsa_round_trip` / `rejects_tampered_or_wrong_key_signature`。COSE_Key は `tests/test_c4m/cose.rs` の `cose_key_*` で補足する |

検証コマンド `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` /
`cargo test -p shiguredo_moqt` / `cargo test -p shiguredo_moqt --features aws-lc-rs --test test_c4m` /
`cargo test -p pbt` / `cargo test -p moq-sub` / `cargo test -p tokio-moq --lib with_auth_parameters` /
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` /
`cargo check --manifest-path fuzz/Cargo.toml --bins` はすべて成功した。
