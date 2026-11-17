# MSF の変数置換の文字種を v02 に合わせる

- Created: 2026-11-16
- Completed: 2026-11-17
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §5.4.1 (Variable Syntax) は変数値に使える文字を拡大し、変数名の文字種制限を削除した。§5.4.2 (Variable Resolution) は各 pair を最初の `=` で分割すると定め、§11.2 の mapping は置換後に適用すると定める。

現行実装の `resolve_catalog_variables` は Base64 の `+` `/` `=` を含む値を拒否するため、v02 の変数値を受け付けられない。本 issue は `src/msf.rs` の変数置換と fragment パースの受理範囲を v02 に合わせる。

## 現状

- `src/msf.rs` の `is_variable_name_char` は英数字・`-`・`_` だけを許可する。`is_variable_value_char` は `is_variable_name_char(b) || b == b'@'` と、名前の判定に従属している。
- この従属のため、名前の制限を緩めると値の許可文字も同時に広がる。v02 が値に禁じる `,` `;` `"` `'` `~` などが値に入り得る。
- `parse_fragment_pairs` と `split_fragment_pairs_unchecked` は `split_once('=')` で分割しており、最初の `=` で分割する規則は満たす。
- `&` は §5.4.2 の pair 区切りであり、値に現れることはない。`parse_fragment_pairs("id=a&b")` は `[("id", "a")]` になる。
- 現行テスト `resolve_name_charset_and_case` は `-` `_` と大文字小文字の区別を確認するだけで、`+` `/` `=` `.` の拒否は固定していない。
- `crates/moq-pub` はカタログに `"%c4m%"` を書くが、`resolve_catalog_variables` を呼ぶ経路は `crates/` にない。`crates/moq-sub` は `MsfCatalogDocument::decode` を直接呼ぶ。`%c4m%` から `cat` トークンへの end-to-end な解決は本 issue の範囲外である。

## 設計方針

- 変数値の許可文字を名前の判定から独立させ、英数字・`-`・`_`・`+`・`/`・`=`・`.`・`@` だけを許可する。`,` `;` `"` `'` `~` などそれ以外は `MessageError::InvalidCatalog` で拒否する。
- 変数名は v02 が文字種を列挙していないため、fragment の param-name として表現できる文字に緩和する。空でないこと、`%` を含まないことを検証する。値の許可文字とは独立に判定する。
- `=` の分割は現行の `split_once('=')` を維持する。
- `&` の扱いは区切りとして固定し、拒否対象にはしない。`id=a&b` が `[("id", "a")]` になることをテストで確認する。
- 置換後の値が §11.2 の mapping 対象になることは、既存の実装順 (生 JSON に置換してから decode) で満たす。名前の mapping 検証は 0438 が担当する。
- `%c4m%` / `%cat%` の end-to-end な解決は、0440 の改名と `crates/moq-sub` の変数置換経路の実装に依存する。本 issue では `resolve_catalog_variables` が `+` `/` `=` を含む値を解決できるところまでを対象とする。

## 完了条件

- `+` `/` `=` `.` を含む値 (例: Base64 の `eyJhbGciOi...+/=` 形式) が変数として解決され、置換後の JSON に反映されること。
- `,` `;` `"` `'` `~` を含む値が `MessageError::InvalidCatalog` で拒否されること。
- `&` が区切りとして扱われ、`id=a&b` が `[("id", "a")]` になること。
- `%cat-token%` のように `-` を含む名前と、`%a.b%` のように英数字以外を含む名前が解決されること。空名と `%` を含む名前が拒否されること。
- 最初の `=` で分割する挙動 (`k=a=b` を key `k` / value `a=b` にする) がテストで固定されていること。
- 既存の `resolve_name_charset_and_case` は残し、`-` `_` と大小区別のカバレッジを失わないこと。`+` `/` `=` `.` の受理は別テストで追加すること。
- 上記を `tests/test_msf/variable_substitution.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- `src/msf.rs` の変数値の許可文字を名前の判定から独立させ、draft-ietf-moq-msf-02 §5.4.1 の列挙どおり英数字・`-`・`_`・`+`・`/`・`=`・`.`・`@` だけを受理するようにした。`,` `;` `"` `'` `~` などの記号は `InvalidCatalog` で拒否する。
- 変数名は v02 が文字種を列挙しないため、fragment の `param-name` (`pchar-no-amp / "/"`) に緩和した。`pchar-no-amp` の判定は `src/msf/uri.rs` の `is_pchar_no_amp_byte` を `pub(crate)` にして共有し、fragment パーサと変数パーサで受理範囲がずれないようにした。`%` は変数参照の区切りと衝突するため拒否し、`pct-encoded` は percent-decode しない方針のため受理しない。
- `&` を pair の区切り、最初の `=` を key/value の分割として維持し、テストで固定した。`=` を含む参照名は fragment key として到達不能なため未定義参照として保持することを doc とテストに明記した。
- `tests/test_msf/variable_substitution.rs` に、Base64 の `+` `/` `=` `.` を含む値の解決、禁止記号の拒否、`&` 区切り、`-` と `.` を含む名前の解決、`%` / `pct-encoded` / 空白を含む名前の拒否、最初の `=` での分割を追加した。既存の `resolve_name_charset_and_case` は残した。

確認コマンド: `cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` が通ることを確認した。
