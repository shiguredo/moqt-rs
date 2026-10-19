# moqt-reval 付きトークンを既定の検証経路で fail-open にしない

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-c4m-moqt-reval
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-c4m-01 §2.2 は「再検証できない受信者は moqt-reval 付きトークンをすべて拒否する MUST」「再検証間隔が自身の能力を下回る場合も拒否する MUST」を定める。`CatClaims::validate` は `moqt_reval` を解釈せず Ok を返すため、アプリが既定の検証 API を使うと MUST 違反 (認可バイパス) になる。

## 現状

- `src/c4m/cat.rs` の `CatClaims::validate` は exp / nbf / iss / aud と非有限値だけを検証し、`moqt_reval` は有限性の検査しかしない。
- `ClaimValidationOptions` に再検証能力を宣言する手段が無い。
- doc は「Sans-I/O のため再検証の実行は行わず、値の解釈と拒否の判断は利用側が行う」とするが、既定の `validate` が成功するため、利用者が拒否を失念しやすい。
- 影響: 短い再検証間隔を要求するトークンが、再検証されないまま受理される。

## 設計方針

- `ClaimValidationOptions` に `revalidation_supported` / `revalidation_interval_seconds` を追加し、既定では非ゼロの `moqt_reval` を拒否する (fail-closed)。
- または `moqt_reval` を返す専用 API を必須経路にして `validate` から成功を返さない。
- `moqt_reval` が 0 のときは再検証不要として受理する。

## 完了条件

- 既定の検証 API が非ゼロ `moqt_reval` を拒否すること。
- 再検証可能と宣言した場合に、許容間隔と比較して受理 / 拒否できること。
- 上記を `tests/test_c4m` で固定し、`cargo test --workspace` / `cargo test --workspace --features aws-lc-rs` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
