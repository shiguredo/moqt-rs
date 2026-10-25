# C4M の PBT モデルと fuzz のテストの穴を埋める

- Created: 2026-10-20
- Completed: {YYYY-MM-DD}
- Branch: feature/test-c4m-pbt-model-fuzz-gaps
- Polished: 2026-10-25

## 目的

C4M (draft-ietf-moq-c4m-01) の PBT が実装の写経になっており、仕様から独立したモデルで検証できていない。fuzz も `CatClaims::validate` を対象にしていない。仕様に基づく独立モデルと fuzz で、テストが通っていても違反を検出できない状態を解消する。

## 現状

- `pbt/tests/prop_c4m/dpop.rs` の `resource_consistent` は、`src/c4m/dpop.rs` の `AuthorizationContext::verify_resource_consistency` の写経である。両者は `resource.split_once('?')` でクエリを取り出し、`query.split('&')` と `pair.split_once('=')` で `tns` / `tn` を集め、`tns` と `tn` を文字列比較する分岐が同一である。仕様から独立した判定になっていない。
- `pbt/tests/prop_c4m/cat.rs` の `expected_validation_error` は、doc に「`CatClaims::validate` の判定順をそのまま実装したモデル」と書き、`InvalidReferenceTime` → 非有限値 → `Expired` → `NotYetValid` → `IssuerMismatch` → `AudienceMismatch` の順序と条件が `src/c4m/cat.rs` の `CatClaims::validate` と同一である。
- `fuzz/fuzz_targets/fuzz_decode_c4m_cat_claims.rs` は `CatClaims::decode` の成功後に `claims.encode()` を呼んでいるため、`CatClaims::encode` は fuzz の対象になっている。一方、`CatClaims::validate` はどの fuzz ターゲットからも呼ばれていない。
- `fuzz/fuzz_targets/fuzz_c4m_authorization_context.rs` / `fuzz/fuzz_targets/fuzz_c4m_dpop_verify.rs` / `fuzz/fuzz_targets/fuzz_verify_c4m_dpop_context.rs` は DPoP proof の検証 API を対象にしているが、`DpopProofBuilder::build` の encode 経路は対象にしていない。
  fuzz クレートは `aws-lc-rs` feature を有効にしていないため、fuzz クレートから利用できる `CoseCrypto` の実装が無く、現状の構成では encode 経路の fuzz を追加できない (`fuzz/Cargo.toml` の `shiguredo_moqt` 依存に feature 指定が無い)。`CoseCrypto` のスタブ実装は規約で禁止されているため、feature を有効にするか、実装済みの `CoseCrypto` を fuzz クレートへ渡す必要がある。
- `resource_consistent` は `resource` がクエリ無し (`moqt://<relay-endpoint>`) の場合を常に不一致とする。0346 は SETUP 用のクエリ無し resource を受理する変更を予定している。
- `CatClaims::validate` は 0345 で `moqt_reval` の検証が追加される予定である。
- 影響: 実装の分岐を写したモデルは実装と同じ誤りを共有するため、仕様違反を検出できない。`CatClaims::validate` は fuzz で任意入力の panic 耐性が確認されていない。

## 設計方針

- `resource_consistent` は draft-ietf-moq-c4m-01 §3.1.2 の「When the optional "resource" parameter is included, it MUST be consistent with the "tns" and "tn" parameters.」と §3.1.3 の 3 つの resource URI 形式から、クエリの `tns` / `tn` を抽出して比較するモデルにする。実装の分岐をそのまま写さない。
  - 接続確立: `moqt://<relay-endpoint>` (クエリ無し)
  - namespace 操作: `moqt://<relay-endpoint>?tns=<namespace>`
  - track 操作: `moqt://<relay-endpoint>?tns=<namespace>&tn=<track>`
- 0346 の action 引数追加に追随し、`resource_consistent` は action を受け取る。0346 の完了後は、次の action 別の規則で判定する。
  - SETUP: クエリ無しの `moqt://<authority>` を整合とする。
  - namespace 操作: `tns` の一致を要求する。actx に `tn` がありクエリに `tn` がある場合だけ `tn` の一致を要求する。
  - track 操作: `tns` を比較し、actx に `tn` がある場合は `tn` も比較する。
  - 0346 の完了前は、現行の `verify_resource_consistency` に合わせてクエリの `tns` / `tn` だけを比較し、SETUP のクエリ無し resource はモデルに含めない。
- 重複した `tns` / `tn` は、仕様が各クエリパラメータと actx の対応フィールドの一致を求めるという解釈から、すべての値が一致する場合だけ整合とする。これは仕様が重複時の意味を明示していないため、保守的な解釈である。実装の「最後の値を使う」分岐は写さない。生成器は同じ値の重複と異なる値の重複の両方を生成する。
- 重複の解釈が実装と食い違うため、実装の修正は `issues/0377-bug-c4m-resource-duplicate-query.md` で行う。本 issue ではテストの追加・モデルの修正までとし、実装の挙動は変えない。0377 の完了後に `cargo test -p pbt` が通ることを本 issue の完了条件に含める。
- `expected_validation_error` は draft-ietf-moq-c4m-01 §2.2 (moqt-reval) と CWT / CAT の `exp` / `nbf` / `iss` / `aud` の規則から条件を導く。
- 判定順は仕様で定まらない (RFC 8392 §7.2 は「The order of the steps is not significant ...」としている) ため、ライブラリの契約として現行の順序 (参照時刻 / 許容ずれの妥当性 → 非有限値 → `Expired` → `NotYetValid` → `IssuerMismatch` → `AudienceMismatch`) をモデルにも明記する。
- `InvalidReferenceTime` と `NonFiniteClaim` は仕様のクレームではなく、公開 API の前提条件としてモデルに含める。
- 0345 の `ClaimValidationOptions` 追加に追随し、`revalidation_interval_seconds` と新しいエラー variant をモデルに取り込む。
- `CatClaims::validate` を対象とする fuzz ターゲットを追加する。`CatClaims::decode` で得たクレームと任意の `ClaimValidationOptions` を `validate` に渡し、panic しないことを確認する。ターゲットは `fuzz/fuzz_targets/fuzz_c4m_cat_validate.rs` とし、`fuzz/Cargo.toml` の `[[bin]]` に登録する。
- `DpopProofBuilder::build` の encode 経路の fuzz は、fuzz クレートに `aws-lc-rs` feature を追加しない限り実行できない。本 issue では対象外とし、必要なら別 issue で feature 追加とセットで扱う。
- 0345 / 0346 が未完了の間は、SETUP のクエリ無し resource と `moqt_reval` の期待値をモデルに含めず、依存をコメントで明記する。コメントには issue 番号を書かず、依存理由 (例: 「接続確立の resource はクエリを持たないため、SETUP の検証は実装側の対応後にモデルへ取り込む」) を書く。0345 / 0346 の完了後に期待値を取り込む。
- 追加・修正する PBT の各 property が、対応する実装の分岐を変えたら落ちることを確認してから完了とする。fuzz ターゲットは panic 耐性だけを確認するため、この確認の対象外とする。
- パーセントエンコーディングは解釈せず、クエリの `tns` / `tn` を文字列として比較する。これは実装の `verify_resource_consistency` と同じ前提であり、モデルの独立性は判定条件 (重複・action 別の形式) に向ける。パーセントエンコーディングの解釈は本 issue の対象外とする。

## 変更対象

- `pbt/tests/prop_c4m/dpop.rs`
- `pbt/tests/prop_c4m/cat.rs`
- `fuzz/fuzz_targets/fuzz_c4m_cat_validate.rs` (新規)
- `fuzz/Cargo.toml`

重複の検証の実装修正は `issues/0377-bug-c4m-resource-duplicate-query.md` で行う。その他の実装の挙動を変える必要が生じた場合は、本 issue では変更せず、別 issue で `src/c4m/dpop.rs` / `src/c4m/cat.rs` と `tests/test_c4m/` を修正する。

## 完了条件

- `resource_consistent` と `expected_validation_error` が、実装の写経でない独立モデルになっていること。
- `resource_consistent` のモデルが、action 別の resource 形式、クエリの `tns` / `tn` の有無、重複 (すべての値が一致する場合だけ整合)、欠落、追加パラメータを仕様から導いた条件で判定し、パーセントエンコーディング無しの文字列比較を実装と同じ前提で行っていること。生成器が同じ値の重複と異なる値の重複を生成し、モデルの重複分岐を踏むこと。
- `expected_validation_error` のモデルが、仕様から導いた条件と、ライブラリの契約としての判定順・API 前提条件を明記していること。
- `CatClaims::validate` を対象とする fuzz ターゲット (`fuzz/fuzz_targets/fuzz_c4m_cat_validate.rs`) が追加され、`fuzz/Cargo.toml` の `[[bin]]` に登録され、`cargo fuzz list` に表示され、`cargo +nightly fuzz build fuzz_c4m_cat_validate` が通ること。
- 0345 / 0346 の完了後に、SETUP のクエリ無し resource、namespace 操作の `tns` / `tn` の扱い、`moqt_reval` の期待値をモデルに取り込んでいること (依存が未完了なら本 issue は完了しない)。
- `issues/0377-bug-c4m-resource-duplicate-query.md` の完了後に `cargo test -p pbt` が通ること。
- `cargo test -p pbt` / `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
