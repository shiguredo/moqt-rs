# moqt-reval 付きトークンを既定の検証経路で fail-open にしない

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-c4m-moqt-reval
- Polished: 2026-10-21

## 目的

draft-ietf-moq-c4m-01 §2.2 は、再検証できない受信者は再検証を要求する `moqt-reval` 付きトークンを拒否する MUST、再検証間隔が自身の能力を下回る場合も拒否する MUST を定める。`CatClaims::validate` は `moqt_reval` の有限性を検査するだけで、再検証の要否を判定せず Ok を返す。アプリが `ClaimValidationOptions::default()` で `validate` を呼ぶと、非ゼロの `moqt_reval` を持つトークンを再検証なしで受理し、MUST 違反 (認可バイパス) になる。

## 現状

- `src/c4m/cat.rs` の `CatClaims::validate` は、参照時刻 / 許容ずれの妥当性、`exp` / `nbf` / `iss` / `aud`、非有限値を検証する。`moqt_reval` は有限性の検査だけを行い、非ゼロでも Ok を返す。
- `ClaimValidationOptions` には再検証能力を宣言するフィールドが無い。
- `docs/c4m.md` は「本ライブラリは Sans-I/O のため再検証を実行せず、拒否の判断は利用側が行います」と説明する。しかし既定の `validate` が非ゼロの `moqt_reval` を受理するため、利用者が拒否を失念しやすい。
- `pbt/tests/prop_c4m/cat.rs` の `expected_validation_error` は `CatClaims::validate` の判定順を写しており、`ClaimValidationOptions` の全フィールドリテラルを持つ。フィールド追加と判定変更に追随しないと `cargo test --workspace` が失敗する。
- 影響: 非ゼロの再検証間隔を要求するトークンが、再検証されないまま受理される。

## 設計方針

- `ClaimValidationOptions` に `pub revalidation_interval_seconds: Option<f64>` を追加する。既定は `None` とし、`None` は受信者が再検証できないことを表す (fail-closed)。
  - `revalidation_supported` と `revalidation_interval_seconds` の 2 フィールドに分けると、`revalidation_supported = true` と `revalidation_interval_seconds` の既定値の組み合わせで fail-open になり得る。そのため `Option<f64>` 1 フィールドに統一する。
- `Some(x)` は受信者が再検証できる最小間隔 (秒) を表す。`x` は有限かつ 0 以上でなければならない。
- `CatClaims::validate` の判定を次のようにする。既存の判定順を維持し、非有限値の検査 (catdpop の window_seconds を含む) の直後、`exp` / `nbf` の検査の前に `moqt_reval` の再検証判定を追加する。
  - `moqt_reval` が無い、または `0` の場合は、`revalidation_interval_seconds` が不正でない限り受理する。
    §2.2 の「When the value of this claim is zero, the token MUST NOT be revalidated. This is the default behaviour when the claim is not present.」により、0 は再検証を要求しない。
    §2.2 の「再検証できない受信者は `moqt-reval` 付きトークンをすべて拒否する MUST」は、再検証を要求する非ゼロのトークンに適用する。
  - `moqt_reval` が非ゼロで `revalidation_interval_seconds` が `None` の場合は拒否する。
  - `moqt_reval` が非ゼロで `Some(x)` の場合、`moqt_reval < x` なら拒否し、`moqt_reval >= x` なら受理する。§2.2 の「If the revalidation interval is smaller than the recipient is prepared or able to revalidate, the recipient MUST reject the token.」に対応する。
  - `revalidation_interval_seconds` が `Some(x)` で `x` が非有限または負の場合は、クレームの有無にかかわらず拒否する。`x` が不正なまま比較すると fail-open になるため。
- `ClaimValidationError` に次の variant を追加し、`Display` を更新する。
  - `RevalidationUnsupported`: `moqt_reval` が非ゼロで、受信者が再検証できない。
  - `RevalidationIntervalTooShort`: `moqt_reval` が非ゼロで、受信者の最小間隔を下回る。
  - `InvalidRevalidationInterval`: `revalidation_interval_seconds` が非有限または負。
- `CatClaims::validate`、`CatClaims::moqt_reval`、`ClaimValidationOptions` の doc コメントを更新する。Sans-I/O で再検証を実行しない点は維持しつつ、既定の `validate` が非ゼロの `moqt_reval` を拒否することを明記する。

## 変更対象

- `src/c4m/cat.rs`
- `tests/test_c4m/cat.rs`
- `pbt/tests/prop_c4m/cat.rs`
- `docs/c4m.md`
- `skills/shiguredo-moqt/SKILL.md` (使用例と API 一覧を確認し、必要なら `ClaimValidationOptions` の新フィールドを追記する)

## 完了条件

- `ClaimValidationOptions::default()` を渡した `CatClaims::validate` が、`moqt_reval` が有限の非ゼロのトークンを `ClaimValidationError::RevalidationUnsupported` で拒否すること。
- `moqt_reval` が無いトークン、および `moqt_reval == 0` のトークンを、既定でも `x` が有限かつ 0 以上の `Some(x)` でも受理すること。
- `revalidation_interval_seconds = Some(x)` で `x` が有限かつ 0 以上のとき、`moqt_reval >= x` を受理し、`moqt_reval < x` を `ClaimValidationError::RevalidationIntervalTooShort` で拒否すること。
- `revalidation_interval_seconds = Some(x)` で `x` が非有限または負の場合、`ClaimValidationError::InvalidRevalidationInterval` で拒否すること。
- 非有限の `moqt_reval` は従来どおり `ClaimValidationError::NonFiniteClaim("moqt-reval")` で拒否すること。
- `tests/test_c4m/cat.rs` に上記のテストを追加すること。
- `pbt/tests/prop_c4m/cat.rs` の `expected_validation_error`、`ClaimValidationOptions` のリテラル、新しい variant のカウンタを新しい判定に追随させ、`cargo test --workspace` が通ること。PBT モデルの独立化は 0356 で扱うため、本 issue では実装に追随する最小限の更新でよい。
- `docs/c4m.md` の `moqt-reval` の説明と `ClaimValidationOptions` の使用例を新しい挙動に合わせること。
- `skills/shiguredo-moqt/SKILL.md` の使用例が新しい API と矛盾しないこと (API 一覧への追記は必要に応じて行う)。
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
