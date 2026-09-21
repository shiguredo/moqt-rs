# MSF URI の track-identifier で percent-encoding を扱えない

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-uri-percent-encoding
- Polished: 2026-09-22

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

URI 層の `%XX` (RFC 3986 §2.1 (Percent-Encoding) の `pct-encoded`) と §11.1.2 の `.` + 小文字 hex 2 桁を別の層として扱い、track-identifier を 1 パスで走査してバイト列へデコードする。

- `src/name.rs` に `parse_name` の `%XX` 許容版を追加し、`src/msf/uri.rs` の `parse_msf_fragment` から使う。`parse_name` / `decode_field` の既存の挙動 (`%` を `InvalidEscape` で拒否) は変えず、他の呼び出し元に影響させない
- フィールドの分割は `parse_name` と同じ規則 (`-` 1 個 = namespace フィールド区切り、`--` = namespace と track name の境界、`-` 3 個以上 = 不正) を生の文字列の `-` のラン走査で確定する。`%XX` はデータバイトであり区切りを生成しない
- 各フィールドは 1 パスで走査してバイト列にする
  - リテラル (`a-z` / `A-Z` / `0-9` / `_`) はそのバイト
  - `.` は直後の 2 文字が hex であることをその場で要求し、2 文字を消費したら次の文字から走査を続ける。大文字 hex は `UppercaseHex`、hex 2 桁でなければ `InvalidEscape`、デコード結果がリテラルなら `RedundantEncoding`。`RedundantEncoding` は `.` エスケープだけに適用し、`%XX` の octet には適用しない (`%61` は 0x61 のデータバイトとして受理する)
  - `%` は直後の 2 文字が hex であることを要求し、その octet をデータバイトとして取り出す。hex の大文字小文字は問わない (RFC 3986 §2.1 は両者を等価とする)
  - ABNF が許す生の非リテラル文字 (`/` / `~` / `:` / `@` / `sub-delims-no-amp`) はその ASCII バイトをデータとして取り出す。§11.1.2 に `/` を区切りとする規則は無く、データの `/` は `.2f` と規定される
- `%XX` を文字列 (`a` や `.2f`) へ畳み込んでから再パースする方式にしてはならない。`.` の直後に `%XX` が続く入力を畳み込むと、入力に無い `.` + hex が生まれる。例: `msf:ns--.2%33` は生の `.` の直後が `2` と `%` のため `InvalidEscape` になるべきだが、畳み込むと `.23` になり 1 バイト 0x23 として受理される
- 同じ文字列を 2 回デコードしない。RFC 3986 §2.4 (When to Encode or Decode) は "Implementations must not percent-encode or decode the same string more than once, as decoding an already decoded string might lead to misinterpreting a percent data octet as the beginning of a percent-encoding,
  or vice versa in the case of percent-encoding an already percent-encoded string." と規定する
- 構造の確定を percent-decode より先に行う。RFC 3986 §2.4 の "the components and subcomponents significant to the scheme-specific dereferencing process (if any) must be parsed and separated
  before the percent-encoded octets within those components can be safely decoded, as otherwise the data may be mistaken for component delimiters." は、`-` のラン走査を生の文字列で行い `%XX` が区切りを生成しないことで満たす
- デコードしたバイトはデータとして扱い、区切り文字や新たなエスケープの開始として再解釈しない。`%2D` はデータバイト 0x2D (`-`) であり区切りではない。`%2E` はデータバイト 0x2E (`.`) であり `.` + hex の開始ではない。`%25` はデータバイト 0x25 (`%`) である
- 生の `?` は ABNF が禁じるため従来どおり拒否する。`%3F` と `%3f` は等価で、どちらも 0x3F のデータバイトになる (現行の `validate_pchar_no_amp` も `is_ascii_hexdigit` で両方受理する)
- 大文字 hex (`UppercaseHex`)、リテラル表現可能バイトの hex 化 (`RedundantEncoding`)、空フィールド、区切りの多重化といった既存規則はそのまま適用される。`UppercaseHex` / `RedundantEncoding` は生の文字列に素で書かれた `.` + hex にのみ関わる。`%4A` は 0x4A (`J`) というデータバイトになり `UppercaseHex` にはならない。素の `.4A` が `UppercaseHex`、素の `.61` が `RedundantEncoding` になる

URI 層の `%61` と MSF namespace-name 層の `.61` は別層の話である。前者は同じ octet の別表記として受理し、後者は §11.1.2 の表現として冗長なため従来どおり拒否する。

`&` 区切りのパラメータ列 (`parameter-list`) の percent-decode は本 issue の対象外とし、現状どおり生の文字列を保持する。同じ fragment を `&` / `=` で分解する `src/msf.rs` の `parse_fragment_pairs` / `resolve_catalog_variables` も「percent-decode は行わず、値をそのまま使う」ままにし、track-identifier だけをデコードする。同じ URI に 2 つの解釈が併存する点は本 issue では変更しない。

## 完了条件

- `msf:customer--catalog%3Fpart` を `parse_msf_fragment` が受理し、track name の該当バイトが 0x3F (`?`) になるテストが `tests/test_msf/uri.rs` に追加されていること (hex は大文字小文字を問わないため `%3f` も同じ 0x3F になる)
- `msf:ns--a/b` を `parse_msf_fragment` が受理し、track name の該当バイトが 0x2F (`/`) になるテストが追加されていること。ABNF が許す他の非リテラル文字も同じ規則であることを 1 文字 (`:` など) で代表させる
- `%2D` が区切りではなくデータバイト 0x2D として扱われるテストが追加されていること。namespace 部に置いた `msf:ns%2Da--catalog` が namespace 1 フィールド `ns-a` (0x6E 0x73 0x2D 0x61) になることを断言する (誤って `-` として扱うと namespace が 2 フィールド `ns` / `a` になる)
- `%2E` がデータバイト 0x2E (`.`) になり、`msf:ns--%2E2d` が 3 バイト 0x2E 0x32 0x64 (`.` `2` `d`) になるテストが追加されていること
- `.` の直後に `%XX` が続く入力が拒否されるテストが追加されていること。`msf:ns--.2%33` は `InvalidEscape` になる (文字列へ畳み込んでから再パースすると `.23` になり 1 バイト 0x23 として受理されてしまう)
- `%25` がデータバイト 0x25 (`%`) になるテストが追加されていること
- `%61` が `a`、`%4A` が 0x4A (`J`) として受理され、素の `.61` は `RedundantEncoding`、素の `.4A` は `UppercaseHex` で従来どおり拒否されるテストが追加されていること
- 生の `?` は従来どおり拒否されるテストが `parse_fragment_invalid_rejected` に維持されていること
- §11.1.2 表現の `msf:ns--a.2fb` と `name::serialize_name` の往復が維持されていること
- `pbt/tests/prop_msf_uri.rs` の `fragment_roundtrip` が成功すること
- `crate::name::parse_name` が `%` を含む文字列を従来どおり `InvalidEscape` で拒否すること (`%XX` 許容版の追加で既存 API の挙動が変わっていないことを固定する)
- `src/msf/uri.rs` のモジュール doc から「percent-decode は行わず、値をそのまま保持する」の記述が実装に合わせて更新されていること
- `make test` (`cargo test --workspace`) と `make clippy` と `make fmt` が通ること
