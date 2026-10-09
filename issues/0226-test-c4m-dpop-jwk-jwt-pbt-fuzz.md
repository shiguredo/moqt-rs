# C4M の DPoP / JWS / JWK と CAT トークン経路に PBT と fuzzing を追加する

- Created: 2026-10-09
- Completed: {YYYY-MM-DD}
- Branch: feature/add-c4m-dpop-pbt-fuzz
- Polished: {YYYY-MM-DD}

## 目的

C4M (draft-ietf-moq-c4m-01) の認可トークン経路は、PBT が CBOR / COSE / CAT クレーム / `moqt` クレームの 4 本しかなく、DPoP proof の検証、JWS compact、JWK、CAT トークンのデコード経路が PBT で固定されていない。

これらの経路は入力の組み合わせで分岐が決まる (actx の `resource` と `tns` / `tn` の整合、リプレイキャッシュの保持期間、JWS ヘッダの `crit`、JWK の正規化 JSON など) ため、単体テストの固定ケースだけでは回帰を検出できない。任意入力に対するクラッシュ耐性も fuzzing でしか確認できない。

## 現状

- `pbt/tests/prop_c4m/` は `cbor.rs` / `cose.rs` / `cat.rs` / `moqt.rs` の 4 本で、対象は CBOR のラウンドトリップ、COSE メッセージのラウンドトリップ、`CatClaims` のラウンドトリップ、`MoqtScope` の認可判定である
- `cargo llvm-cov -p pbt --tests` で計測すると、`src/c4m/dpop.rs` / `src/c4m/jwt.rs` / `src/c4m/jwk.rs` は行カバレッジ 0%、`src/c4m/cat.rs` は 31.78% (150/472 行) である。`src/c4m/cat.rs` の未到達は `CatToken::decode` / `decode_compact` / `decode_cose` と `CatClaims::validate`、ビルダーが中心である
- `fuzz/fuzz_targets/` の c4m ターゲットは `fuzz_decode_c4m_cbor` / `fuzz_decode_c4m_cat_token` / `fuzz_decode_c4m_cose_token` / `fuzz_decode_c4m_dpop_proof` / `fuzz_verify_c4m_dpop_context` の 5 本である。JWS compact、JWS ヘッダ、CBOR からのクレームデコード、COSE メッセージ単体、Authorization Context、リプレイキャッシュには target が無い
- `src/c4m/dpop.rs` の `verify_against_token` と `DpopReplayCache::check_and_record` は `CoseCrypto` を必要としない検証 (actx / 鮮度 / リプレイ) を含むが、`pbt/` の既定 feature では `aws-lc-rs` が無効であり署名検証は対象にできない。`pbt/tests/prop_c4m/main.rs` の方針どおり、署名 / 検証を使う property は `tests/test_c4m/` に置く

## 設計方針

PBT は `pbt/tests/prop_c4m/` に追加し、`pbt/tests/prop_c4m/main.rs` にモジュールを登録する。すべて noprop の `Runner` とサンプラーで入力を作り、カバレッジゲートで対象の分岐が実際に踏まれたことを確認する。生成器は仕様から直接導いたモデルと突き合わせる。

- `dpop.rs`: `AuthorizationContext::decode` の取り出しと `verify_context_type` / `verify_action` / `verify_target` / `verify_resource_consistency` の判定を、draft-ietf-moq-c4m-01 §3.1.2 / §3.1.3 と draft-nandakumar-moq-generic-dpop-proof-00 §5.1 の規則をそのまま実装したモデルと比較する。`resource` のクエリは `tns` / `tn` の有無、重複、欠落、パーセントエンコーディング無しの文字列比較まで生成対象にする
- `dpop.rs`: `DpopProofClaims::decode` の必須 / 任意クレームを検証し、`DpopProof::decode` はテスト側で `base64url(header).base64url(payload).base64url(signature)` を組み立てて通す (署名の暗号検証は行わない)
- `dpop.rs`: `DpopReplayCache::check_and_record` の系列を、ウィンドウ内の保持と `Replayed` の判定を独立に実装したモデルと比較する。`len()` の一致まで含める
- `dpop.rs`: `verify_freshness` の `ProofExpired` / `ProofNotYetValid` / 非有限値の判定を境界値込みでモデルと比較する
- `jwt.rs`: `JwsHeader::decode` の `alg` / `typ` / `kid` / `jwk` と `crit` 拒否、重複メンバー拒否を検証し、`JwsCompact::decode` は生成した compact 形式で `signing_input` / `payload` / `signature` を検証する。パディング付き base64url の受理も対象にする
- `jwk.rs`: `Jwk::decode` のメンバー取り出しと秘密鍵メンバー拒否、`canonical_json` の RFC 7638 §3.2 の必須メンバー順 / 空白無し / パディング無し正規化、`to_cose_key` と `matches_public_key` の一致判定、`default_signing_algorithm` の対応を検証する。サムプリントは `CoseCrypto` が要るため対象外とする
- `cat.rs`: compact 形式と COSE 形式のトークンをテスト側で組み立て、`CatToken::decode` / `decode_compact` / `decode_cose` の形式判別、`format` / `raw_token` / `signing_input` / `claims` を検証する。`CatClaims::validate` は `ClaimValidationOptions` の `exp` / `nbf` / `iss` / `aud` / クロックスキューをモデルと比較する

fuzzing は `fuzz/fuzz_targets/` に追加し、`fuzz/Cargo.toml` の `[[bin]]` に登録する。既存ターゲットと同じく第一の性質は panic しないこととし、デコードに成功した場合は再エンコードなどの契約も確認する。

- `fuzz_decode_c4m_jws.rs`: 任意の UTF-8 を `JwsCompact::decode` と `JwsHeader::decode` に渡す
- `fuzz_decode_c4m_cat_claims.rs`: 任意バイトを CBOR としてデコードし、`CatClaims` / `MoqtClaim` / `MoqtScope` / `CatDpop` / `Confirmation` の decode に渡す。成功したら encode して再デコードする
- `fuzz_decode_c4m_cose_message.rs`: 任意バイトを `CoseMessage::decode` に渡し、`header` と `signing_input` を呼ぶ
- `fuzz_c4m_authorization_context.rs`: 任意の UTF-8 を `AuthorizationContext::decode` に渡し、成功したら `verify_context_type` / `verify_action` / `verify_target` / `verify_resource_consistency` を呼ぶ
- `fuzz_c4m_dpop_verify.rs`: 任意の UTF-8 を `DpopProof::decode` に渡し、成功したら `verify_freshness` と `verify_authorization_context` と `DpopReplayCache::check_and_record` を呼ぶ

## 完了条件

- `pbt/tests/prop_c4m/` に上記の property が追加され、対象の分岐ごとにカバレッジゲートがあること
- `make pbt` が通り、追加した property が 256 ケースで安定して成功すること
- 追加した fuzz ターゲットが `cargo fuzz list` に表示され、各ターゲットが `cargo +nightly fuzz run <target> -- -max_total_time=30` で panic しないこと
- `make pbt` / `make test` / `make clippy` / `make fmt` が通ること
- ソースコードに issue 番号や issue への言及を書かないこと

## 解決方法

{未着手}
