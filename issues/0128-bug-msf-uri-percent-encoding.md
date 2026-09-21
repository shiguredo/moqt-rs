# MSF URI の track-identifier で percent-encoding を扱えない

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-uri-percent-encoding
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-01 §11.1 (URL construction and interpretation) の ABNF は、track-identifier に `pchar-no-amp` と `/` を許し、`?` は `%3F` として percent-encode すると規定する。

> ```text
> track-identifier  = 1*( pchar-no-amp / "/" )
>                     ; MSF namespace-name string
>                     ; MUST NOT contain '&' or '?'
>                     ; '?' MUST be percent-encoded as %3F within this component
> ```

`pchar-no-amp` は RFC 3986 の `pct-encoded` (`%` HEXDIG HEXDIG) を含む。

> ```text
> pchar-no-amp      = unreserved / pct-encoded / sub-delims-no-amp / ":" / "@"
> ```

したがって `#msf:ns--catalog%3Fpart` のような URI は仕様上合法であり、library はこれを解釈できなければならない。現状は `src/msf/uri.rs` の ABNF 検査を通った値を `crate::name::parse_name` が拒否するため、合法な URI を解釈できない。

## 現状

`src/msf/uri.rs` の `parse_msf_fragment` は次の順に処理する。

1. `msf:` を剥がし、`&` で track-identifier とパラメータ列に分割する
2. `validate_pchar_no_amp` で track-identifier の文字種を検査する。`is_pchar_no_amp_byte` は `unreserved` / `sub-delims-no-amp` / `:` / `@` を許し、`%` の後は `pct-encoded` として hex 2 桁を要求する。第 2 引数 `allow_slash` が true のため `/` も許す
3. `crate::name::parse_name` に生の文字列を渡す。`src/name.rs` の `decode_field` はリテラル (`a-z` / `A-Z` / `0-9` / `_`) と `.` + hex 2 桁以外のバイトを拒否し、`%` と `/` は `NameParseError::InvalidEscape` になる。`parse_name` は `.` のエスケープをバイトへデコードするだけで percent-decode は行わない

このため次のようになる。

- `msf:ns--catalog%3Fpart` は `InvalidCatalog` になる (`invalid MSF track identifier: InvalidEscape`)
- `msf:ns--a/b` も同じく `InvalidCatalog` になる
- `msf:ns--a.2fb` (§11.1.2 の表現) は成功し、track name の該当バイトが `/` (0x2F) になる

`src/msf/uri.rs` のモジュール doc は「percent-decode は行わず、値をそのまま保持する」と記述しており、この記述も実装変更に合わせて更新が必要になる。

なお §11.1 は track-identifier を MSF namespace-name string とし、その表現を §11.1.2 (MSF Namespace-Name String Encoding) に委ねる。§11.1.2 は「percent-encoded」という語を `.` + 小文字 hex 2 桁の意味で使っており、ABNF の `pct-encoded` (`%XX`) と語が衝突している。両者を別の層として区別する必要がある。

```text
-  Unreserved characters (a-z, A-Z, 0-9, _) are represented literally.
-  All other byte values (including hyphens and periods used as data) MUST be percent-encoded using a period (.) followed by two lowercase hexadecimal digits (e.g., a literal hyphen in a name becomes .2d).
```

## 設計方針

URI 層の `%XX` (RFC 3986 §2.1 (Percent-Encoding) の `pct-encoded`) と §11.1.2 の `.` + 小文字 hex 2 桁を別の層として扱い、デコード順序を固定する。

1. 構造の分離を先に行う。`-` (namespace フィールド区切り) と `--` (namespace と track name の境界) は percent-decode 前の文字列で確定する。
   RFC 3986 §2.4 (When to Encode or Decode) は "the components and subcomponents significant to the scheme-specific dereferencing process (if any) must be parsed and separated
   before the percent-encoded octets within those components can be safely decoded, as otherwise the data may be mistaken for component delimiters." と規定する
2. 分離した各フィールドで `%XX` をデータバイトへ 1 回だけデコードする。同じ文字列を 2 回デコードしない (RFC 3986 §2.4 の "
   Implementations must not percent-encode or decode the same string more than once, as decoding an already decoded string might lead to misinterpreting a percent data octet as the beginning of a percent-encoding,
   or vice versa in the case of percent-encoding an already percent-encoded string.")
3. デコードしたバイトはデータとして扱い、区切り文字や新たなエスケープの開始として再解釈しない。`%2D` はデータバイト 0x2D (`-`) であり区切りではない。`%2E` はデータバイト 0x2E (`.`) であり `.` + hex の開始ではない。`%25` はデータバイト 0x25 (`%`) である
4. デコード結果を §11.1.2 の表現へ寄せてから `crate::name::parse_name` に渡す。リテラル表現可能なバイト (`a-z` / `A-Z` / `0-9` / `_`) はリテラルのまま、それ以外は `.` + 小文字 hex 2 桁にする。RFC 3986 §2.4 は unreserved 集合に対応する percent-encoded octet をいつでもデコードできるとし、§11.1.2 は unreserved をリテラルで表すと規定するため、`%61` は `a` になる
5. 生の `/` はデータバイト 0x2F として扱い `.2f` に正規化する。§11.1 の ABNF は `pchar-no-amp` と並んで `/` を許すが、§11.1.2 に `/` を区切りとする規則は無く、データの `/` は `.2f` と規定される
6. 生の `?` は ABNF が禁じるため従来どおり拒否する。`?` の表現は `%3F` だけになる
7. 正規化後の文字列を `crate::name::parse_name` に渡すため、大文字 hex (`UppercaseHex`)、リテラル表現可能バイトの hex 化 (`RedundantEncoding`)、空フィールド、区切りの多重化などの既存規則はそのまま適用される

URI 層の `%61` と MSF namespace-name 層の `.61` は別層の話である。前者は同じ octet の別表記として受理し、後者は §11.1.2 の表現として冗長なため従来どおり拒否する。

`&` 区切りのパラメータ列 (`parameter-list`) の percent-decode は本 issue の対象外とし、現状どおり生の文字列を保持する。

## 完了条件

- `msf:customer--catalog%3Fpart` を `parse_msf_fragment` が受理し、track name の該当バイトが 0x3F (`?`) になるテストが `tests/test_msf/uri.rs` に追加されていること
- `msf:ns--a/b` を `parse_msf_fragment` が受理し、track name の該当バイトが 0x2F (`/`) になるテストが追加されていること
- `%2D` が区切りではなくデータバイト 0x2D として扱われるテストが追加されていること (namespace のフィールド数が `%2D` の前後で増えない)
- `%2E` が `.` + hex の開始として再解釈されず、データバイト 0x2E になるテストが追加されていること
- `%25` がデータバイト 0x25 (`%`) になるテストが追加されていること
- `%61` が `a` として受理され、`.61` は従来どおり拒否されるテストが追加されていること
- 生の `?` は従来どおり拒否されるテストが `parse_fragment_invalid_rejected` に維持されていること
- §11.1.2 表現の `msf:ns--a.2fb` と `name::serialize_name` の往復が維持されていること
- `pbt/tests/prop_msf_uri.rs` の `fragment_roundtrip` が成功すること
- `src/msf/uri.rs` のモジュール doc から「percent-decode は行わず、値をそのまま保持する」の記述が実装に合わせて更新されていること
- `make test` (`cargo test --workspace`) と `make clippy` と `make fmt` が通ること
