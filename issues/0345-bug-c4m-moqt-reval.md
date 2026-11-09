# moqt-reval 付きトークンを既定の検証経路で fail-open にしない

- Created: 2026-10-19
- Completed: 2026-11-10
- Branch: develop
- Polished: 2026-11-10

## 目的

draft-ietf-moq-c4m-02 §2.2 は次の 2 つの MUST を定める。

- "If the revalidation interval is smaller than the recipient is prepared or able to revalidate, the recipient MUST reject the token."
- "If a recipient is unable to revalidate tokens, it MUST reject all tokens with a "moqt-reval" claim."

`CatClaims::validate` は `moqt_reval` の有限性を検査するだけで、再検証の要否を判定せず Ok を返す。
アプリが `ClaimValidationOptions::default()` で `validate` を呼ぶと、`moqt_reval` を持つトークンを再検証なしで受理し、MUST 違反 (認可バイパス) になる。
`validate` を内部で呼ぶ bearer の検証入口 `CatToken::verify_bearer` と、CWT 形式の DPoP の一括検証入口 `CwtDpopProof::verify_against_cat_token` も同じく受理する。

## 現状

- `src/c4m/cat.rs` の `CatClaims::validate` は、参照時刻 / 許容ずれの妥当性、`exp` / `nbf` / `iss` / `aud`、非有限値を検証する。`moqt_reval` は有限性の検査だけを行い、非ゼロでも Ok を返す。doc コメントも「`moqt-reval` の再検証も行わず、非ゼロの `moqt-reval` でも成功する」と明記している。
- `ClaimValidationOptions` には再検証能力を宣言するフィールドが無い。
- `CatToken::verify_bearer` は `ClaimValidationOptions { reference_time_seconds, clock_tolerance_seconds: cache.leeway_seconds(), expected_issuers, expected_audiences }` を全フィールドのリテラルで組み立てて `validate` を呼ぶ。`BearerVerificationOptions` にも再検証能力を渡すフィールドが無い。
- `src/c4m/dpop.rs` の `CwtDpopVerification::claim_options` は `ClaimValidationOptions` をそのまま `validate` に渡す。`CwtDpopProof::verify_against_cat_token` の doc は「`moqt-reval` の再検証と拒否はこの入口の対象外であり、利用側の責務である ([`CatClaims::validate`] は `moqt-reval` を解釈しない)」とする (closed の 0418 で決めた契約)。
- `docs/c4m.md` の `moqt-reval` の節は「`CatClaims::validate` は `moqt-reval` の値の有限性だけを検査し、非ゼロの `moqt-reval` を既定では拒否しません」と警告し、拒否を利用側に委ねている。`README.md` の bearer の使用例も、`verify_bearer` の前に `token.claims().moqt_reval.is_some_and(|value| value != 0.0)` で手動拒否している。既定の API が fail-open のままなので、利用者が拒否を失念すると MUST 違反になる。
- `pbt/tests/prop_c4m/cat.rs` の `expected_validation_error` は `CatClaims::validate` の判定順を写している。`claims_validate_matches_model` は `ClaimValidationOptions` を全フィールドのリテラルで持ち、`ClaimValidationError` を wildcard なしで網羅 match してカウントする。フィールドや variant を追加すると、追随しない限りコンパイルできない。
- 影響: `moqt-reval` で再検証を要求するトークンが、再検証されないまま受理される。

## 設計方針

- `ClaimValidationOptions` に `pub revalidation_interval_seconds: Option<f64>` を追加する。既定は `None` とし、`None` は受信者が再検証できないことを表す (fail-closed)。
  - `revalidation_supported` と `revalidation_interval_seconds` の 2 フィールドに分けると、`revalidation_supported = true` と `revalidation_interval_seconds` の既定値の組み合わせで fail-open になり得る。そのため `Option<f64>` 1 フィールドに統一する。
- `Some(x)` は受信者が再検証できる最小間隔 (秒) を表す。`x` は有限かつ 0 以上でなければならない。
- `CatClaims::validate` の判定を次のようにする。既存の判定順を維持し、非有限値の検査 (catdpop の window_seconds を含む) の直後、`exp` / `nbf` の検査の前に再検証判定を追加する。非有限の `moqt_reval` は前段の非有限値の検査で `NonFiniteClaim("moqt-reval")` になるため、再検証判定に届く `moqt_reval` は無いか有限値である。再検証判定の中では次の順で判定する。
  1. `revalidation_interval_seconds` が `Some(x)` で `x` が非有限または負なら、クレームの有無にかかわらず `InvalidRevalidationInterval` で拒否する。`x` が NaN や負のまま比較すると、`moqt_reval < x` が偽になり fail-open になるため。
  2. `moqt_reval` が無ければ受理する。
  3. `moqt_reval` があり、`revalidation_interval_seconds` が `None` なら、値が `0` でも `RevalidationUnsupported` で拒否する。§2.2 の "it MUST reject all tokens with a "moqt-reval" claim" は値で限定していないため、字義どおり fail-closed にする。
  4. `moqt_reval` が `0` で `Some(x)` なら受理する。§2.2 の "When the value of this claim is zero, the token MUST NOT be revalidated." により、0 は再検証間隔ではなく「再検証しない」指示であり、再検証できる受信者はその指示に従えばよい。
  5. `moqt_reval` が非ゼロで `Some(x)` なら、`moqt_reval < x` のとき `RevalidationIntervalTooShort` で拒否し、`moqt_reval >= x` のとき受理する。§2.2 の "If the revalidation interval is smaller than the recipient is prepared or able to revalidate, the recipient MUST reject the token." に対応する。負の `moqt_reval` は `x >= 0` を下回るため、この規則で拒否される。
- `ClaimValidationError` に次の variant を追加し、`Display` を更新する。
  - `RevalidationUnsupported`: `moqt_reval` があり、受信者が再検証できない。
  - `RevalidationIntervalTooShort`: `moqt_reval` が非ゼロで、受信者の最小間隔を下回る。
  - `InvalidRevalidationInterval`: `revalidation_interval_seconds` が非有限または負。
- `BearerVerificationOptions` に `pub revalidation_interval_seconds: Option<f64>` を追加し、`CatToken::verify_bearer` は `validate` に渡す `ClaimValidationOptions` へそのまま転送する。
  `None` に固定すると、再検証できる受信者が bearer の入口で `moqt-reval` 付きトークンを受理できなくなり、`verify_with` / `validate` / `authorize` / `BearerReplayCache::check_and_bind` を個別に呼ぶしかなくなる。
  その場合、`verify_bearer` の手順 5 (`catreplay` 1 の `cti` / `exp` の必須化と保持期限の計算、`catreplay` 2 の拒否) を利用側が再実装することになる。
- `CwtDpopProof::verify_against_cat_token` は `CwtDpopVerification::claim_options` 経由で同じ判定が適用される。closed の 0418 が決めた「`moqt-reval` の再検証と拒否はこの入口の対象外」という契約は、本 issue で「拒否の判定はこの入口が行い、再検証の実行だけを利用側の責務とする」に改める。
- 再検証そのもの (定期的に検証し直すこと) は Sans-I/O の方針どおり利用側の責務のままとする。ライブラリが行うのは、宣言された再検証能力でトークンを受理してよいかの判定だけである。
- doc コメントを新しい挙動に合わせて更新する。対象は `CatClaims::validate`、`CatClaims::moqt_reval`、`ClaimValidationOptions`、`BearerVerificationOptions`、`CatToken::verify_bearer` の手順 3、`CwtDpopVerification::claim_options`、`CwtDpopProof::verify_against_cat_token` の手順 3 と「対象外」の段落である。

## 変更対象

- `src/c4m/cat.rs`: `ClaimValidationOptions` (フィールドと `Default`)、`ClaimValidationError` (variant と `Display`)、`CatClaims::validate`、`CatClaims::moqt_reval` の doc、`BearerVerificationOptions`、`CatToken::verify_bearer`
- `src/c4m/dpop.rs`: `CwtDpopVerification::claim_options` と `CwtDpopProof::verify_against_cat_token` の doc
- `tests/test_c4m/cat.rs`: `validation_vectors_decode_and_validate` の `ClaimValidationOptions` のリテラルを更新し、`validate` の判定のテストを追加する
- `tests/test_c4m/cat/bearer.rs`: `BearerVerificationOptions` のリテラルを更新し、bearer の入口のテストを追加する
- `tests/test_c4m/dpop/cwt/verification.rs`: `verify` ヘルパーの `ClaimValidationOptions` のリテラルを更新し、DPoP の入口のテストを追加する
- `tests/test_c4m/draft02.rs`: validation ベクタのループにある `ClaimValidationOptions` のリテラルを更新する
- `pbt/tests/prop_c4m/cat.rs`: `expected_validation_error`、`claims_validate_matches_model` のリテラルと網羅 match のカウンタ
- `README.md`: bearer の使用例の `BearerVerificationOptions` のリテラル (`src/lib.rs` の `#[doc = include_str!("../README.md")]` で doctest としてコンパイルされる) を更新し、`moqt_reval` の手動拒否を削除する
- `docs/c4m.md`: 実装範囲の `moqt-reval` の項目、「検証 API の責務」の表の `CatClaims::validate` / `CatToken::verify_bearer` / `CwtDpopProof::verify_against_cat_token` の行、`moqt-reval` の節の説明、`ClaimValidationOptions` / `BearerVerificationOptions` / `CwtDpopVerification` の使用例、「残る制約」の `moqt-reval` の項目
- `docs/missing-features.md`: 「C4M の `moqt-reval` 再検証」の行。既定で拒否するようになったことと、再検証の実行は利用側の責務であることに更新し、issue 列から本 issue への参照を外す
- `skills/shiguredo-moqt/SKILL.md`: `ClaimValidationOptions` / `BearerVerificationOptions` / `CwtDpopVerification` の使用例、`CwtDpopProof::verify_against_cat_token` の「`moqt-reval` の再検証はこの API の対象外である」、「残る制約」と注意事項の「非ゼロの `moqt-reval` は既定の `CatClaims::validate` では拒否されない」の 2 箇所

## 完了条件

- `ClaimValidationOptions::default()` を渡した `CatClaims::validate` が、`moqt_reval` を持つトークンを値 (`0` を含む有限値) にかかわらず `ClaimValidationError::RevalidationUnsupported` で拒否すること。
- `moqt_reval` が無いトークンを、既定でも、`x` が有限かつ 0 以上の `Some(x)` でも受理すること。
- `revalidation_interval_seconds = Some(x)` で `x` が有限かつ 0 以上のとき、有限の `moqt_reval` について、`moqt_reval == 0` と `moqt_reval >= x` を受理し、非ゼロの `moqt_reval < x` (負の値を含む) を `ClaimValidationError::RevalidationIntervalTooShort` で拒否すること。
- `revalidation_interval_seconds = Some(x)` で `x` が非有限または負の場合、`moqt_reval` が無いか有限であれば `ClaimValidationError::InvalidRevalidationInterval` で拒否すること。
- 非有限の `moqt_reval` は、`revalidation_interval_seconds` の値にかかわらず、従来どおり `ClaimValidationError::NonFiniteClaim("moqt-reval")` で拒否すること。
- `CatToken::verify_bearer` が、`BearerVerificationOptions::revalidation_interval_seconds` が `None` のとき `moqt_reval` 付きトークンを `CatError::Claims(ClaimValidationError::RevalidationUnsupported)` で拒否し、`Some(x)` を渡すと上記の判定で受理・拒否すること。
- `CwtDpopProof::verify_against_cat_token` が、`claim_options.revalidation_interval_seconds` に応じて上記の判定で受理・拒否すること。
- 上記を `tests/test_c4m/cat.rs`、`tests/test_c4m/cat/bearer.rs`、`tests/test_c4m/dpop/cwt/verification.rs` のテストで確認すること。
- `pbt/tests/prop_c4m/cat.rs` の `expected_validation_error`、`claims_validate_matches_model` のリテラルと新しい variant のカウンタを新しい判定に追随させること。PBT モデルの独立化は 0356 で扱うため、本 issue では実装に追随する最小限の更新でよい。
- `README.md`、`docs/c4m.md`、`docs/missing-features.md`、`skills/shiguredo-moqt/SKILL.md` に、「既定の検証で `moqt-reval` 付きトークンが受理される」「`moqt-reval` の拒否は検証入口の対象外」という記述が残っていないこと。
- 本 issue で追加・更新する `moqt-reval` の判定に関する doc コメントと文書が、§2.2 を draft-ietf-moq-c4m-02 として参照すること。互換 API や旧ベクタを指す既存の draft-01 参照 (`CatClaims::raw` の旧 label 327 / 328 の説明など) は変更しない。
- `markdownlint-cli2` が変更した Markdown で通ること。
- 次のコマンドが通ること。
  - `cargo test --workspace`
  - `cargo test -p shiguredo_moqt --features aws-lc-rs`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo clippy -p shiguredo_moqt --all-targets --features aws-lc-rs -- -D warnings`
  - `cargo build -p shiguredo_moqt --target thumbv7em-none-eabihf`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt --features aws-lc-rs`
  - `cargo fmt --all -- --check`

## 対象外

- `moqt-reval` を composition claims 内に置いたトークンの well-formed 性拒否は、`docs/missing-features.md` に記載のある別件とし、本 issue では扱わない。
- `moqt_reval` の再検証そのもの (定期的な再検証の実行) は Sans-I/O の方針どおり利用側の責務とし、本 issue では扱わない。
- 旧 JWT 形式の `DpopProof::verify_against_cat_token` は CAT のクレームを検証しない互換 API であり、呼び出し側が別途 `validate` を呼ぶ。本 issue の判定は `validate` 経由で効くため、この入口自体は変更しない。

## 解決方法

`ClaimValidationOptions` と `BearerVerificationOptions` に `revalidation_interval_seconds: Option<f64>` を追加し、`CatClaims::validate` で `moqt-reval` の受理可否を判定するようにした。
判定は非公開の `validate_moqt_reval` にまとめ、非有限値の検査の直後、`exp` / `nbf` の検査の前に呼ぶ。
`None` (既定) は値が 0 でも `RevalidationUnsupported` で拒否する。`Some(x)` は 0 を受理し、非ゼロは `x` 未満を `RevalidationIntervalTooShort` で拒否する。
`x` が非有限または負なら `InvalidRevalidationInterval` で拒否する。

`CatToken::verify_bearer` は `BearerVerificationOptions::revalidation_interval_seconds` を `validate` へ転送する。
`CwtDpopProof::verify_against_cat_token` は `claim_options` 経由で同じ判定が効く。doc の「`moqt-reval` はこの入口の対象外」を、受理可否はこの入口が判定し再検証の実行だけを利用側の責務とする記述に改めた。
再検証の責務は、非ゼロの `moqt-reval` を持つトークンを受理した場合に限ると明記した (0 は §2.2 で再検証してはならない)。

テストは次を追加・更新した。

- `tests/test_c4m/cat.rs`: `validate_moqt_reval_against_revalidation_capability` (境界値の表と、exp より先に判定する順序)、`revalidation_errors_display`
- `tests/test_c4m/cat/bearer.rs`: `verify_bearer_checks_moqt_reval_against_revalidation_capability` (拒否時に cache を変更しないことを含む)
- `tests/test_c4m/dpop/cwt/verification.rs`: `cwt_dpop_checks_moqt_reval_against_revalidation_capability` (拒否時に `cti` を記録しないことを含む)
- `pbt/tests/prop_c4m/cat.rs`: `expected_validation_error` に再検証判定を写し、受信者の再検証能力を生成して新しい 3 つの variant のカウンタを追加した。
  再検証判定が後段の検証に届くケースを減らすため、`claims_validate_matches_model` の実行回数を 512 回にし、非ゼロの等号境界 (50 / 100 / 200) を生成する
- `tests/test_c4m/draft02.rs`: validation ベクタのリテラルに `revalidation_interval_seconds: None` を追加した

`README.md` の bearer の使用例から `moqt_reval` の手動拒否を削除し、`docs/c4m.md`、`docs/missing-features.md`、`skills/shiguredo-moqt/SKILL.md` の記述と使用例を新しい挙動に合わせた。
`CODEBASE.md` の規定に従い、ブランチと PR を作らず develop に直接コミットし、`CHANGES.md` は更新していない。
