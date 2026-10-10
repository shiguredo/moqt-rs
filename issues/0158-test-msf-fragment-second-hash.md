# MSF URI の fragment 内の 2 個目の `#` の拒否をテストで固定する

- Created: 2026-09-24
- Completed: {YYYY-MM-DD}
- Branch: feature/test-msf-fragment-second-hash
- Polished: 2026-09-28
- Updated: 2026-10-10

## 目的

`shiguredo_moqt::msf::uri::parse_msf_uri` が fragment 内の 2 個目の `#` を拒否することをテストで固定し、
利用者に `#` をそのまま fragment に置けないこと (track-identifier では `.23`、`&` 区切りのパラメータ値では `%23` と書く必要があること) が伝わるエラーメッセージにする。

## 現状

`src/msf/uri.rs` の `parse_msf_uri` は `rest.split_once('#')` で最初の `#` だけを区切りにするため、
`moqt://example.com/path#msf:track#foo` の fragment 文字列は `msf:track#foo` になる。

`parse_msf_fragment` は `validate_pchar_no_amp` で `track-identifier` を検証し、
`#` は pchar-no-amp に含まれないため結果的に拒否される。
実測 (`parse_msf_uri("moqt://example.com/path#msf:track#foo")`) では
`InvalidCatalog("invalid character in MSF fragment component 'track#foo'")` になる。

根拠: RFC 3986 §3.5 の `fragment = *( pchar / "/" / "?" )` に `#` は含まれない。加えて draft-ietf-moq-msf-01 §11.1 は
track-identifier を MSF namespace-name string と規定し、§11.1.2 は非リテラルのバイト値を「`.` + 小文字 hex 2 桁」で書く MUST を
定めるため、track-identifier 中の `#` の正規形は `.23` である (`%23` は ABNF が許す非正規形)。
`&` 区切りのパラメータ値は RFC 3986 の percent-encoding に従い `%23` と書く。

挙動は RFC に適合しているが、拒否は `#` を明示した検査ではなく pchar 検証の副作用であり、
`tests/test_msf/uri.rs` にも 2 個目の `#` を持つ URI のテストが無いため、`split_once('#')` の扱いを変えると通ってしまう。
example 経路 (`examples/tokio-moq/src/lib.rs`) には `#` の明示検査があるため (0133、closed)、
明示検査が無いのは `parse_msf_uri` である。

## 設計方針

- `split_once('#')` の直後に fragment 文字列へ `#` が残っていないかを検査し、専用のエラーを返す
- メッセージは `#` をそのまま fragment に置けないことを英語で伝える。track-identifier では `.23` (例: `msf:track.23foo`)、
  パラメータ値では `%23` と書くことを示す
- `parse_msf_fragment` の pchar 検証はそのまま残し、二重の防御にする
- `tests/test_msf/uri.rs` に 2 個目の `#` を持つ URI の拒否テストを追加する

## 完了条件

- `parse_msf_uri("moqt://example.com/path#msf:track#foo")` がエラーになり、メッセージが `.23` (track-identifier) と `%23` (パラメータ値) に触れること
- `tests/test_msf/uri.rs` に 2 個目の `#` の拒否テストがあり、メッセージを固定していること
- 既存の MSF URI テスト (scheme 大文字小文字、query、authority 空、fragment 形式) が変わらないこと
- RFC 3986 §3.5 と draft-ietf-moq-msf-01 §11.1 / §11.1.2 の引用をコメントに残すこと
