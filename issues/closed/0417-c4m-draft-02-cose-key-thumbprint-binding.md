# cnf の ckt による COSE Key Thumbprint 束縛を追加する

- Created: 2026-11-03
- Completed: 2026-11-04
- Branch: develop
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
- 先行: [0416](closed/0416-c4m-draft-02-cwt-dpop-proof-codec.md)

## 差分の確認範囲

- `Confirmation` の `ckt` (5) の型付き保持と decode / encode の検証
- RFC 9679 の COSE Key Thumbprint (SHA-256) の計算と、公開 `CoseKey` から JWK への変換
- `CwtDpopProof::verify_key_binding` (ckt / jkt の束縛検証)
- テスト / PBT / fuzz target の追随と RFC 9679 の参照追加
- テストベクタの `ES256_PRIVATE_KEY_HEX` の doc 更新 (JWK 変換テストでも使うため)

`cnf` を使った受理判定の統合 (署名・鮮度・ath・replay を含む一連の検証) は 0418 で行う。
key 3 の旧 jkt 解釈は 0346 の担当であり、CWT の新しい束縛検証では扱わない。docs / README /
skills の更新は 0420 で行う。CHANGES.md は CODEBASE の方針により更新しない。

## 解決方法

draft-02 §4.1 / §4.3 の `cnf` の `ckt` (COSE Key Thumbprint) による鍵束縛を追加した。

- `src/c4m/cat.rs` の `Confirmation` に `cose_key_thumbprint` (confirmation key 5) を追加し、
  decode で bstr・32 バイト (SHA-256) を検証する。encode も 32 バイト以外を
  `InvalidCoseKeyThumbprintLength` で拒否する。`ckt` accessor と
  `CatTokenBuilder::cose_key_thumbprint` を追加した
- `src/c4m/crypto.rs` に `CoseKey::cose_key_thumbprint_sha256` を追加した。RFC 9679 §3 の
  とおり、必須パラメータだけの COSE_Key map (EC2 は kty / crv / 非圧縮 x / y、OKP は
  kty / crv / x) を決定論的 CBOR に符号化し、SHA-256 を計算する。`kid` / `alg` /
  `key_ops` / 秘密鍵は含めない (RFC 9679 §5.1)。対称鍵は対象外である
- `src/c4m/jwk.rs` に `Jwk::from_cose_key` を追加した。CWT proof の公開鍵から等価な
  公開 JWK を作り、`jkt` (RFC 7638) の計算に使う。base64url はパディング無しに正規化し、
  秘密鍵は含めない
- `src/c4m/dpop.rs` の `CwtDpopProof::verify_key_binding` を追加した。`ckt` がある場合は
  proof の kccs 公開鍵のサムプリントと一致することを要求し、不一致を `jkt` への
  fallback で救済しない。`jkt` (323) がある場合は等価 JWK のサムプリントと一致することを
  要求する。両方ある場合は両方の一致を要求する (RFC 8747 §3.1 の単一 PoP 鍵の要件)。
  `ckt` も `jkt` も無い場合 (未知 method だけ / 空 / key 3 だけ) は拒否し、未知 method は
  認可の根拠にしない
- テスト / PBT: RFC 9679 §6 の固定ベクタ (EC2 P-256) と draft §4.2.1 の例、独立計算した
  OKP のベクタ、任意属性の有無でサムプリントが変わらないこと、異なる鍵の拒否、
  ckt / jkt の各組み合わせ、不正な ckt の型・長さ、未知 method だけの拒否、実 ES256 /
  ES384 / EdDSA proof と実 cnf の束縛、draft §4.2.1 と付録 A.4 の固定値での束縛、
  解釈しない COSE_Key method の無視、サムプリントの決定性と任意パラメータ非影響の
  PBT を追加した。`Confirmation::encode` の変更に合わせて fuzz target も更新した

## 完了条件の検証

| 完了条件 | 検証 |
| --- | --- |
| EC2 と対応する OKP 鍵で独立した既知ベクタと一致すること | `tests/test_c4m/cose.rs` の `cose_key_thumbprint_matches_rfc9679_example` / `cose_key_thumbprint_matches_independent_okp_vector` / `cose_key_thumbprint_matches_c4m_example` |
| 任意 COSE key 属性の有無で値が変わらず、異なる鍵の proof を拒否すること | 同 `cose_key_thumbprint_*`、PBT の `cose_key_thumbprint_*`、`tests/test_c4m/dpop/cwt.rs` の `known_vector_cnf_binds_to_es256_proof` / `es384_and_eddsa_proofs_bind_to_a_cnf_claim` / `eddsa_proof_binds_with_jkt` |
| ckt と jkt をそれぞれの方式で検証し、併存時は同じ鍵で両方の一致を要求すること。片方だけの一致、不正な ckt、未知 confirmation method だけの cnf を認可成功にしないこと | 同 `cnf_binding_requires_every_present_thumbprint_to_match` / `cnf_binding_rejects_unknown_confirmation_methods`、`tests/test_c4m/cat.rs` の `confirmation_ckt_*` |
| 公開鍵を表すデータだけを使い、秘密情報をテスト出力や文書へ追加しないこと | `CoseKey::to_cose_key_map` / `Jwk::from_cose_key` が秘密鍵を出力しないこと、`tests/test_c4m/cose.rs` の `cose_key_thumbprint_matches_rfc9679_example`、`tests/test_c4m/jwk.rs` の `from_cose_key_*`、`pbt` の `cose_key_thumbprint_*` |

検証コマンド `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` /
`cargo test -p shiguredo_moqt` / `cargo test -p shiguredo_moqt --features aws-lc-rs --test test_c4m` /
`cargo test -p pbt` / `cargo test -p moq-sub` / `cargo test -p tokio-moq --lib with_auth_parameters` /
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` /
`cargo check --manifest-path fuzz/Cargo.toml --bins` はすべて成功した。
