# URL と MSF fragment の解析エラーのメッセージに機密値を埋め込まない

- Created: 2026-11-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-url-parse-error-secret-leak
- Polished: {YYYY-MM-DD}

## 目的

URL と MSF fragment の解析に失敗したときのエラーメッセージに、入力のパラメータ値や userinfo がそのまま埋め込まれる。
fragment のパラメータ値は C4M トークンなどの認可情報を含み得て (draft-ietf-moq-msf-01 §11.1.1 の `c4m`、§11.4.2)、エラーはログや CLI の標準エラーに出るため、機密情報が漏れる。エラーメッセージから値を除く。

## 現状

- `src/msf/uri.rs` の `parse_msf_fragment` は、`=` の無いパラメータを `MessageError::InvalidCatalog(format!("MSF fragment parameter '{element}' is missing '='"))` で拒否し、要素全体 (値を含む) を埋め込む。
- `src/msf/uri.rs` の `validate_pchar_no_amp` は、パラメータ名と値の不正な percent-encoding / 文字を `format!("invalid percent-encoding in MSF fragment component '{value}'")` / `format!("invalid character in MSF fragment component '{value}'")` で拒否し、値そのものを埋め込む。
- `crates/tokio-moq/src/lib.rs` の `invalid_authority` は `format!("invalid server address '{authority}': {reason}")` で authority をそのまま埋め込む。`authority_parts` は userinfo を含む authority を `"userinfo is not supported"` で拒否するが、
  このとき `user:password@host` のパスワードがエラーに出る (RFC 3986 §3.2.1 "Applications should not render as clear text any data after the first colon (":") character found within a userinfo subcomponent")。
- `src/msf.rs` の `resolve_catalog_variables` のエラーは変数名やバイト位置だけを示し、値を埋め込まない。

## 設計方針

- MSF fragment の解析エラーは、値を埋め込まずに不正の種類と位置 (何番目のパラメータか、パラメータ名か値か) だけを示す。パラメータ名は機密値ではないが、`=` の無い要素は名前と値を区別できないため、要素全体を埋め込まない。
- `invalid_authority` は、userinfo を含む authority を拒否するときは authority を埋め込まない。それ以外の authority の拒否 (host が空、ポートが不正など) は従来どおり authority を表示してよい (userinfo を含まないため)。
- エラーの種類 (`MessageError::InvalidCatalog` / `TransportError::InvalidAuthority`) と拒否の条件は変えない。

## 完了条件

- `parse_msf_fragment` / `validate_pchar_no_amp` のエラーメッセージに、入力のパラメータ値が含まれないこと。
- userinfo を含む authority の拒否のエラーメッセージに、userinfo (特にパスワード) が含まれないこと。
- 既存の拒否条件とエラーの種類が変わらないこと。
- 上記をテストで固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
