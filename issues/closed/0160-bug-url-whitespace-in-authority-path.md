# URL の authority と path に含まれる空白を拒否する

- Created: 2026-09-24
- Completed: 2026-09-30
- Branch: feature/fix-url-whitespace
- Polished: 2026-09-27

## 目的

`examples/moqt-transport/src/lib.rs` の `parse_url` が、URL の authority と path に含まれる空白文字 (スペース / タブ) を拒否するようにする。
現在は受理され、接続の途中で原因の分かりにくいエラーになる。

## 現状

`parse_url` は URL を区切り文字で分割するだけで、authority と path の文字種を検証しない。実測:

- `moqt://exa mple.com/app` は authority `exa mple.com` として受理され、
  接続時に `Fatal: failed to resolve 'exa mple.com': failed to lookup address information: nodename nor servname provided, or not known` になる
- `moqt://127.0.0.1:4433/a b` は path `/a b` として受理され、
  SETUP の作成時に `Fatal: session error 0x9: PATH does not conform to RFC 3986` になる

RFC 3986 §3.2.2 の host と §3.3 の path に空白は含まれない (`pchar` に空白は無い)。
`src/parameter.rs` の `validate_path` / `validate_authority` は接続時に拒否するが、
`parse_url` の時点で拒否すれば `--url` の検証として原因が伝わる。

## 設計方針

- authority と path に ASCII の空白 / タブ / 制御文字が含まれる場合は `parse_url` をエラーにする
- エラーメッセージは入力 URL と、どの成分のどの文字が原因かを英語で示す
- エラーが起きる位置は既存の authority 欠落の判定と同じく、fragment と authority の解離より後にする
- percent-encoding された `%20` は許可する (RFC 3986 §2.1 の pct-encoded は正しい表現)
- 既存の IPv6 リテラル、query 内の `://`、fragment の分離の挙動は変えない

## 完了条件

- `moqt://exa mple.com/app` / `moqt://127.0.0.1:4433/a b` / タブを含む URL が `parse_url` のエラーになること
- `moqt://example.com/a%20b` は受理され、path が `/a%20b` のままであること
- エラーメッセージに入力 URL が含まれること
- 既存の `parse_url` テスト (fragment、大文字 scheme、authority 空、IPv6 リテラル) が変わらないこと

## 解決方法

`examples/tokio-moq/src/lib.rs` の `parse_url` で authority と path の文字種を検証するようにした。

- `first_url_whitespace(authority, path)` を追加し、RFC 3986 §2 (Characters) の `host` / `path` に含まれない ASCII の空白 (SP / HTAB / LF / CR 等) と制御文字 (0x00-0x1F / 0x7F) を、authority → path の順に探して最初の 1 件を返す
- 見つかった場合は `invalid URL: {url} ('{成分}' contains a whitespace or control character at byte {位置})` を返す。成分名と位置を含めることで、どの入力が原因かをエラーだけで分かるようにした
- 検証の位置は fragment と authority の解離より後、fragment の解釈より前である。fragment は解釈する仕様が意味を定めるため対象外とした (fragment 内の空白はそのまま保持する)
- percent-encoding された `%20` は RFC 3986 §2.1 の `pct-encoded` として正当なので拒否しない
- `parse_url` の doc の `# Errors` に `invalid URL` を追加した

追加したテスト (`examples/tokio-moq/src/lib.rs` の `tests` モジュール):

- `parse_url_rejects_whitespace_in_authority_and_path`: `moqt://exa mple.com/app` / `moqt://127.0.0.1:4443/a b` / タブ / 改行 / NUL を含む URL を拒否し、メッセージに入力 URL が含まれること
- `parse_url_reports_whitespace_component`: エラーが `'authority' ... at byte 3` / `'path' ... at byte 2` のように成分と位置を報告すること
- `parse_url_accepts_percent_encoded_space_in_path`: `moqt://example.com/a%20b` を受理し path が `/a%20b` のままであること
- `parse_url_keeps_whitespace_in_fragment`: fragment 内の空白は URL の検証で拒否せず、値がそのまま保持されること

実機確認 (`moq-sub` を実行):

- `moqt://exa mple.com/app` → `argument '--url' has an invalid value "moqt://exa mple.com/app": invalid URL: moqt://exa mple.com/app ('authority' contains a whitespace or control character at byte 3)`
- `moqt://127.0.0.1:4433/a b` → `'path' contains a whitespace or control character at byte 2` を含む同じ形式のエラー
- `moqt://example.com/a%20b` は名前解決の段階へ進む (`Resolved example.com to [...]`)

`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ることを確認した。
