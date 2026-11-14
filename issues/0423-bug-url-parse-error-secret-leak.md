# URL と MSF fragment の解析エラーのメッセージに機密値を埋め込まない

- Created: 2026-11-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-url-parse-error-secret-leak
- Polished: 2026-11-14

## 目的

URL と MSF fragment の解析に失敗したときのエラーメッセージに、入力のパラメータ値や userinfo がそのまま埋め込まれる。
fragment のパラメータ値は C4M トークンなどの認可情報を含み得て (draft-ietf-moq-msf-01 §11.1.1 の `c4m`、§11.4.2)、エラーはログや CLI の標準エラーに出るため、機密情報が漏れる。エラーメッセージから値を除く。

## 現状

- `src/msf/uri.rs` の `parse_msf_fragment` は、`=` の無いパラメータを `MessageError::InvalidCatalog(format!("MSF fragment parameter '{element}' is missing '='"))` で拒否し、要素全体 (値を含む) を埋め込む。
- `src/msf/uri.rs` の `validate_pchar_no_amp` は、パラメータ名と値の不正な percent-encoding / 文字を
  `format!("invalid percent-encoding in MSF fragment component '{value}'")` /
  `format!("invalid character in MSF fragment component '{value}'")` で拒否し、
  コンポーネント (パラメータ名・値・track-identifier) を埋め込む。同関数は track-identifier の検証にも使われる。
- `crates/tokio-moq/src/lib.rs` の `invalid_authority` は `format!("invalid server address '{authority}': {reason}")` で authority をそのまま埋め込む。`authority_parts` は userinfo を含む authority を `"userinfo is not supported"` で拒否するが、
  このとき `user:password@host` のパスワードがエラーに出る (RFC 3986 §3.2.1 "Applications should not render as clear text any data after the first colon (":") character found within a userinfo subcomponent")。
- `crates/tokio-moq/src/lib.rs` の `parse_url` は、`format!("unsupported URL scheme: {url} (use moqt://)")` /
  `format!("moqt:// URL requires authority: {url} (e.g. moqt://localhost:4443)")` /
  `format!("invalid URL: {url} ('{component}' contains a whitespace or control character at byte {index})")` /
  `format!("invalid moqt URI fragment: {url} ('#' inside a fragment must be percent-encoded)")` /
  `format!("invalid moqt URI fragment: {url} (must be '<type>:<value>')")` /
  `format!("invalid moqt URI fragment type: {url} (must be non-empty and consist of ASCII lowercase letters, digits, and hyphens)")` /
  `format!("invalid MSF fragment in {url}: {e}")` /
  `format!("invalid c4m parameter in {url}: not a non-empty Base64 token")` のエラーに入力 URL 全体を埋め込む。
  補助関数の `parse_moqt_fragment` / `parse_msf_value` は `url` をエラーに埋め込み、`parse_c4m_tokens` は `decode_c4m_token` に
  `url` を渡し、`decode_c4m_token` も `url` をエラーに埋め込む。URL には query と fragment が含まれ、§11.4.2 の token や
  `c4m` の値が入り得るため、`parse_msf_fragment` を直しても `parse_url` のエラーから値が漏れる。
- `src/msf.rs` の `resolve_catalog_variables` のエラーは変数名やバイト位置だけを示し、値を埋め込まない。

## 設計方針

- MSF fragment の構文解析エラー (`parse_msf_fragment` / `validate_pchar_no_amp`) は、パラメータ値を埋め込まない。
  パラメータ名と track-identifier は機密値ではないため表示してよい。`=` の無い要素は名前と値を区別できないため、
  要素全体を埋め込まず、何番目の要素かだけを示す。`validate_pchar_no_amp` は track-identifier にも使うため、
  呼び出し側から対象 (track-identifier / パラメータ名 / パラメータ値) と位置を渡し、パラメータ値のときだけ実値を表示しない。
- MSF fragment の値の変換エラー (`connection_types` / `wallclock_ranges` / `mediatime_ranges` / `location_ranges`) は、接続種別や数値であり機密値ではないため、従来どおり値を表示してよい。
- `invalid_authority` は、userinfo を含む authority を拒否するときは authority を埋め込まない。それ以外の authority の拒否 (host が空、ポートが不正など) は従来どおり authority を表示してよい (userinfo を含まないため)。
- `parse_url` 系のエラーは URL 全体を埋め込まない。`parse_url` 自身のエラーは、原因を特定できる非機密の成分
  (scheme / component 名 / fragment type など) だけを示す。`parse_moqt_fragment` / `parse_msf_value` /
  `parse_c4m_tokens` / `decode_c4m_token` から `url` 引数を外し、エラーに URL を埋め込まない。closed 0159 は
  `parse_url` のエラーに URL を含める設計だった。URL には query / fragment が含まれ、§11.4.2 の token が入り得るため、
  本 issue では URL 全体ではなく非機密の成分だけを示す方針に改める。
- `parse_url` の `# Errors` と、入力 URL 全体を含むことを前提にした既存テスト (`parse_url_rejects_whitespace_in_authority_and_path` など) を新しい方針に合わせる。
- エラーの種類 (`MessageError::InvalidCatalog` / `TransportError::InvalidAuthority` / `parse_url` 系の `String`) と拒否の条件は変えない。

## 変更対象

- `src/msf/uri.rs`: `parse_msf_fragment` / `validate_pchar_no_amp` のエラーメッセージ
- `crates/tokio-moq/src/lib.rs`: `parse_url` / `parse_moqt_fragment` / `parse_msf_value` / `parse_c4m_tokens` / `decode_c4m_token` / `invalid_authority` / `authority_parts` のエラーメッセージと `# Errors`
- `tests/test_msf/uri.rs`: `parse_msf_fragment` のエラーメッセージのテスト
- `crates/tokio-moq/src/lib.rs` の `#[cfg(test)] mod tests`: `parse_url` / `parse_moqt_fragment` / `parse_msf_value` / `decode_c4m_token` / `authority_parts` のエラーメッセージのテスト

## 完了条件

- `parse_msf_fragment` / `validate_pchar_no_amp` のエラーメッセージに、入力のパラメータ値が含まれないこと。パラメータ名と track-identifier は表示してよい。
- userinfo を含む authority の拒否のエラーメッセージに、userinfo (特にパスワード) が含まれないこと。
- `parse_url` / `parse_moqt_fragment` / `parse_msf_value` / `decode_c4m_token` のエラーメッセージに、入力 URL の query / fragment の値が含まれないこと。scheme / component 名 / fragment type などの非機密の成分は表示してよい。
- 既存の拒否条件とエラーの種類が変わらないこと。
- `parse_url` の `# Errors` と、入力 URL 全体を含むことを前提にした既存テストを新しい方針に合わせること。
- 上記をテストで固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
