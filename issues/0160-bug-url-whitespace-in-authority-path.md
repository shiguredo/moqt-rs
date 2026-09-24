# URL の authority と path に含まれる空白を拒否する

- Created: 2026-09-24
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-url-whitespace
- Polished: {YYYY-MM-DD}

## 目的

`examples/moqt-transport/src/lib.rs` の `parse_url` が、URL の authority と path に含まれる空白文字 (スペース / タブ) を拒否するようにする。
現在は受理され、接続の途中で原因の分かりにくいエラーになる。

## 現状

`parse_url` は URL を区切り文字で分割するだけで、authority と path の文字種を検証しない。実測:

- `moqt://exa mple.com/app` は authority `exa mple.com` として受理され、
  接続時に `Fatal: failed to resolve 'exa mple.com': failed to lookup address information: nodename nor servname provided, or not known` になる
- `moqt://127.0.0.1:4433/a b` は path `/a b` として受理され、
  SETUP の作成時に `Fatal: session error 0x9: PATH does not conform to RFC 3986` になる

RFC 3986 §3.2 の host と §3.3 の path に空白は含まれない (`pchar` に空白は無い)。
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
