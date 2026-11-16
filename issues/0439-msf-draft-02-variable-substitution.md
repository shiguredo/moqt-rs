# MSF の変数置換の文字種を v02 に合わせる

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §5.4.1 (Variable Syntax) は変数値に使える文字を拡大し、変数名の文字種制限を削除した。§5.4.2 (Variable Resolution) は各 pair を最初の `=` で分割すると定め、§11.2 の mapping は置換後に適用すると定める。
現行実装は Base64 の `+` `/` `=` を含む認可トークンを変数値として扱えず、v02 の URL で `cat` トークンを渡すと変数解決に失敗する。

## 現状

- `src/msf.rs` の `is_variable_value_char` は英数字・`-`・`_`・`@` だけを許可し、v02 が許可する `+` `/` `=` `.` を拒否する。
- `is_variable_name_char` は英数字・`-`・`_` だけを許可する。v02 は変数名の文字種を列挙していない。
- `parse_fragment_pairs` と `resolve_catalog_variables` は同じ文字種検証を使う。`split_fragment_pairs_unchecked` は `split_once('=')` で分割しており、最初の `=` で分割する規則は満たす。
- `refs/moq/draft-ietf-moq-msf-02.txt` §5.4.1 は許可文字として `plus sign, forward slash, equals, period and the at sign (@)` を挙げ、`,` `;` `"` `&` などを禁じる。
- `tests/test_msf/variable_substitution.rs` の `resolve_name_charset_and_case` は旧文字種を固定している。
- `crates/moq-pub` の `C4M_AUTH_INFO_VALUE` は `%c4m%` 形式で、値に Base64 が入る。`+` `/` `=` を含むトークンは現状拒否される。

## 設計方針

- 変数値の許可文字に `+` `/` `=` `.` を追加する。v02 が列挙する禁止文字 (`,` `;` `"` `&` など) は現状どおり拒否する。
- 変数名は v02 が文字種を列挙していないため、fragment の param-name として表現できる文字に緩和する。`%` を含めないことと、空でないことを検証する。既存の英数字・`-`・`_` だけの制限は外す。
- 置換後の値が §11.2 の mapping 対象になることは既存の実装順 (生 JSON に置換してから decode) で満たしているため、順序は変えない。
- `tests/test_msf/variable_substitution.rs` の旧文字種を固定しているテストを v02 の規則に置き換える。

## 完了条件

- `+` `/` `=` `.` を含む値 (例: Base64 の `eyJhbGciOi...+/=` 形式) が変数として解決され、`authInfo` の値に反映されること。
- `,` `;` `"` `&` を含む値が `MessageError::InvalidCatalog` で拒否されること。
- `%cat-token%` のように `-` を含む名前と、`%a.b%` のように英数字以外を含む名前が解決されること。
- 最初の `=` で分割する挙動 (`k=a=b` を key `k` / value `a=b` にする) がテストで固定されていること。
- 上記を `tests/test_msf/variable_substitution.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
