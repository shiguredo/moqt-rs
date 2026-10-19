# C4M の PBT モデルと fuzz のテストの穴を埋める

- Created: 2026-10-20
- Completed: {YYYY-MM-DD}
- Branch: feature/test-c4m-pbt-model-fuzz-gaps
- Polished: {YYYY-MM-DD}

## 目的

C4M (draft-ietf-moq-c4m-01) の PBT が実装の写経になっており、仕様から独立したモデルで検証できていない。fuzz も `CatClaims::validate` を対象にしていない。仕様に基づく独立モデルと fuzz で、テストが通っていても違反を検出できない状態を解消する。

## 現状

- `pbt/tests/prop_c4m/dpop.rs` の `resource_consistent` は、`src/c4m/dpop.rs` の `AuthorizationContext::verify_resource_consistency` の写経である。両者は `resource.split_once('?')` でクエリを取り出し、`query.split('&')` と `pair.split_once('=')` で `tns` / `tn` を集め、`tns` と `tn` を文字列比較する分岐が同一である。仕様から独立した判定になっていない。
- `pbt/tests/prop_c4m/cat.rs` の `expected_validation_error` は、doc に「`CatClaims::validate` の判定順をそのまま実装したモデル」と書き、`InvalidReferenceTime` → 非有限値 → `Expired` → `NotYetValid` → `IssuerMismatch` → `AudienceMismatch` の順序と条件が `src/c4m/cat.rs` の `CatClaims::validate` と同一である。
- `fuzz/fuzz_targets/fuzz_decode_c4m_cat_claims.rs` は `CatClaims::decode` の成功後に `claims.encode()` を呼んでいるため、`CatClaims::encode` は fuzz の対象になっている。一方、`CatClaims::validate` はどの fuzz ターゲットからも呼ばれていない。
- `fuzz/fuzz_targets/fuzz_c4m_dpop_verify.rs` と `fuzz/fuzz_targets/fuzz_verify_c4m_dpop_context.rs` は DPoP proof の検証 API を対象にしているが、`DpopProofBuilder::build` の encode 経路は対象にしていない。
  fuzz クレートは `aws-lc-rs` feature を有効にしていないため、`build` が要求する `CoseCrypto` を満たせず、現状の構成では encode 経路の fuzz を追加できない (`fuzz/Cargo.toml` の `shiguredo_moqt` 依存に feature 指定が無い)。
- `resource_consistent` は `resource` がクエリ無し (`moqt://<relay-endpoint>`) の場合を常に不一致とする。0346 は SETUP 用のクエリ無し resource を受理する変更を予定している。
- `CatClaims::validate` は 0345 で `moqt_reval` の検証が追加される予定である。
- 影響: 実装の分岐を写したモデルは実装と同じ誤りを共有するため、仕様違反を検出できない。`CatClaims::validate` は fuzz で任意入力の panic 耐性が確認されていない。

## 設計方針

- `resource_consistent` は draft-ietf-moq-c4m-01 §3.1.2 の「When the optional "resource" parameter is included, it MUST be consistent with the "tns" and "tn" parameters.」と §3.1.3 の resource URI 形式 (`moqt://<relay-endpoint>?tns=<namespace>&tn=<track>`) から、クエリの `tns` / `tn` を抽出して比較するモデルにする。実装の分岐をそのまま写さない。
- 重複した `tns` / `tn` は、仕様が各クエリパラメータと actx の対応フィールドの一致を求めるため、すべての値が一致する場合だけ整合とする。実装の「最後の値を使う」分岐は写さない。
- `expected_validation_error` は draft-ietf-moq-c4m-01 §2.2 (moqt-reval) と CWT / CAT の `exp` / `nbf` / `iss` / `aud` の規則から判定順を導く。0345 の `ClaimValidationOptions` 追加に追随する。
- `CatClaims::validate` を対象とする fuzz ターゲットを追加する。`CatClaims::decode` で得たクレームと任意の `ClaimValidationOptions` を `validate` に渡し、panic しないことを確認する。
- `DpopProofBuilder::build` の encode 経路の fuzz は、fuzz クレートに `aws-lc-rs` feature を追加しない限り実行できない。本 issue では対象外とし、必要なら別 issue で feature 追加とセットで扱う。
- 0345 / 0346 が未完了の間は、SETUP のクエリ無し resource と `moqt_reval` の期待値をモデルに含めず、依存をコメントで明記する。0346 の `verify_resource_consistency` の action 引数追加と、0345 の `ClaimValidationOptions` 追加に追随し、0345 / 0346 の完了後に期待値を取り込む。
- 独立モデルと実装が食い違った場合は、実装の修正を別 issue として切り出す。本 issue ではテストの追加・モデルの修正までとし、実装の挙動は変えない。
- 各テストが「実装を変えたら落ちる」ことを確認してから完了とする。

## 完了条件

- `resource_consistent` と `expected_validation_error` が、実装の写経でない独立モデルになっていること。
- `resource_consistent` のモデルが、クエリの `tns` / `tn` の有無、重複 (すべての値が一致する場合だけ整合)、欠落、追加パラメータ、パーセントエンコーディング無しの文字列比較を仕様から導いた条件で判定していること。
- `CatClaims::validate` を対象とする fuzz ターゲットが追加され、`cargo fuzz build <target>` が通ること。
- 0345 / 0346 の完了後に、SETUP のクエリ無し resource と `moqt_reval` の期待値をモデルに取り込んでいること (依存が未完了なら本 issue は完了しない)。
- `cargo test -p pbt` / `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
