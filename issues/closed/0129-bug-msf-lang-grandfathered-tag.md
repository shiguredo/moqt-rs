# lang 検証が grandfathered irregular タグを拒否する

- Created: 2026-09-21
- Completed: 2026-09-23
- Branch: feature/fix-msf-lang-grandfathered-tag
- Polished: 2026-09-22

## 目的

draft-ietf-moq-msf-01 §5.2.32 (Language) は、lang の値を RFC 5646 (BCP 47) の言語タグに限定する。

> A string defining the dominant language of the track.  The string MUST be one of the standard Tags for Identifying Languages as defined by [LANG].

RFC 5646 §2.1 (Syntax) の `Language-Tag` は `langtag` / `privateuse` / `grandfathered` の 3 つから成り、`grandfathered` には `langtag` の規則に一致しない irregular タグの固定リストが含まれる。

> ```text
>  Language-Tag  = langtag             ; normal language tags
>                / privateuse          ; private use tag
>                / grandfathered       ; grandfathered tags
> ```

> ```text
>  grandfathered = irregular           ; non-redundant tags registered
>                / regular             ; during the RFC 3066 era
>
>  irregular     = "en-GB-oed"         ; irregular tags do not match
>                / "i-ami"             ; the 'langtag' production and
>                / "i-bnn"             ; would not otherwise be
>                / "i-default"         ; considered 'well-formed'
>                / "i-enochian"        ; These tags are all valid,
>                / "i-hak"             ; but most are deprecated
>                / "i-klingon"         ; in favor of more modern
>                / "i-lux"             ; subtags or subtag
>                / "i-mingo"           ; combination
>                / "i-navajo"
>                / "i-pwn"
>                / "i-tao"
>                / "i-tay"
>                / "i-tsu"
>                / "sgn-BE-FR"
>                / "sgn-BE-NL"
>                / "sgn-CH-DE"
> ```

`i-klingon` などの irregular タグは妥当な言語タグであるにもかかわらず、現状は primary subtag の形式検査で拒否され、そのトラックを含むカタログ全体の decode が失敗する。

## 現状

`src/msf.rs` の `validate_lang_tag` は RFC 5646 §2.1 のサブセットを独自に実装しており、受理する形式を次の 3 つに限定している。

- `x` に 1 個以上の `-` 区切りサブタグが続く privateuse
- primary language subtag が 2 〜 8 文字の ASCII 英字
- 後続サブタグが 1 文字以上の ASCII 英数字

`i-klingon` の primary subtag は 1 文字の `i` なので 2 番目の規則で拒否される。実際に lang を `i-klingon` にしたカタログの `MsfCatalogDocument::decode` は `InvalidCatalog` になり、大文字小文字を変えた `I-KLINGON` も同じ理由で拒否される。

`validate_lang_tag` は `validate_full_track` / `validate_clone_track_fragment` / `decode_common_track_fields` の 3 箇所から呼ばれる。
`decode_common_track_fields` は `decode_track` と cloneTracks の decoder が共用し、`validate_clone_track_fragment` は `validate_delta_for_encode` の Clone 枝 (encode 前検証) から呼ばれる。
このため full カタログの encode / decode に加えて delta の add / clone が encode / decode の両方向で同じ理由により拒否され、1 トラックの lang が原因でカタログ全体を解釈できなくなる。

grandfathered のうち `regular` (art-lojban / cel-gaulish / no-bok / no-nyn / zh-guoyu / zh-hakka / zh-min / zh-min-nan / zh-xiang) と `en-GB-oed` / `sgn-BE-FR` / `sgn-BE-NL` / `sgn-CH-DE` は primary subtag が 2 〜 3 文字の ASCII 英字で後続サブタグも英数字のため現状でも受理される。`i-` で始まる irregular タグは primary が 1 文字のため拒否される。
privateuse は現行実装が小文字 `x` のみを受理するため、`X-FOO` のような大文字 `X` の privateuse も拒否される (`RFC 5646 §2.1.1` の大文字小文字無視には本 issue では合わせない)。

## 設計方針

`validate_lang_tag` の primary subtag 検査より前に、RFC 5646 §2.1 の `irregular` 固定リストとの一致判定を追加する。ABNF は大文字小文字を区別しないため、ASCII 大文字小文字を無視して比較する (RFC 5646 §2.1.1 (Formatting of Language Tags))。

> The ABNF syntax also does not distinguish between upper- and lowercase: the uppercase US-ASCII letters in the range 'A' through 'Z' are always considered equivalent and mapped directly to their US-ASCII lowercase equivalents in the range 'a' through 'z'.
> So the tag "I-AMI" is considered equivalent to that value "i-ami" in the 'irregular' production.

受理する形式は次のとおりに限定する。

- `irregular` の 17 タグ: `en-GB-oed` / `i-ami` / `i-bnn` / `i-default` / `i-enochian` / `i-hak` / `i-klingon` / `i-lux` / `i-mingo` / `i-navajo` / `i-pwn` / `i-tao` / `i-tay` / `i-tsu` / `sgn-BE-FR` / `sgn-BE-NL` / `sgn-CH-DE` (大文字小文字を問わない)
- 既存の 3 規則: privateuse、primary language subtag が 2 〜 8 文字の ASCII 英字、後続サブタグが 1 文字以上の ASCII 英数字
- 大文字小文字を無視するのは `irregular` リストとの一致判定だけにする。privateuse の `x` は現行どおり小文字のみを受理し、`X-FOO` を受理させる変更は本 issue の対象外とする (RFC 5646 §2.1.1 の大文字小文字無視に合わせた全面対応は別途扱う)

`irregular` リストは RFC 5646 §2.1 が "These tags were registered under [RFC3066] and are a fixed list that can never change." と述べる固定リストであり、IANA Language Subtag Registry の更新では増えない。定数として持てば将来の追従は不要である。

完全な BCP 47 検証 (IANA Language Subtag Registry との照合、`extlang` / `variant` の長さ制約、`extension` の構造) は本 issue の対象外とし、既存の軽量構文検査の範囲を維持する。
`validate_lang_tag` の doc コメントには、受理する形式として irregular タグ (primary が 1 文字の `i-` を含む) を追記し、既存の「primary language subtag: 2〜3 文字 / 4 文字 / 5〜8 文字の ASCII 英字」の記述に irregular の例外があることが分かる形に直す。

## 完了条件

- lang が `i-klingon` のカタログを `MsfCatalogDocument::decode` が受理するテストが `tests/test_msf/error_cases.rs` に追加されていること
- lang が `i-klingon` のトラックを持つカタログの encode / decode 往復が成功するテストが追加されていること
- 大文字小文字を変えた `I-KLINGON` を受理するテストが追加されていること
- `irregular` の 17 タグすべてを受理するテストが追加されていること
- delta の add / clone 経路でも irregular タグが受理されるテストが追加されていること
- `regular` の grandfathered タグ (`zh-min-nan` / `art-lojban` など primary が 2 〜 3 文字) を受理するテストを追加し、従来どおり受理されることを固定すること (現状の受理を検証する既存テストは無い)
- 既存の拒否テスト (`lang_invalid_empty` / `lang_invalid_numeric_primary` / `lang_invalid_single_char` / `lang_invalid_special_chars` / `lang_invalid_leading_dash` / `lang_invalid_trailing_dash` / `lang_invalid_primary_too_long` / `lang_privateuse_without_subtag_rejected`) が維持されていること。
  `lang_invalid_single_char` の doc コメント「primary language subtag が 1 文字の場合は拒否される」は irregular タグ (`i-` 始まり) の例外を含む形に直す
- `make test` (`cargo test --workspace`) と `make clippy` と `make fmt` が通ること

## 解決方法

`validate_lang_tag` に RFC 5646 §2.1 (Syntax) の `irregular` (grandfathered) タグの固定リストを追加し、primary language subtag の形式検査より先に ASCII 大文字小文字を無視して一致判定するようにした。

1. `src/msf.rs` に `LANG_IRREGULAR_TAGS` (17 タグ) と `is_lang_irregular` を追加した。値は RFC 5646 §2.1 の ABNF の正規表記のまま持ち、一次資料と目視で照合できるようにする。比較は `eq_ignore_ascii_case` によるタグ全体の一致なので、`i-klingon` / `I-KLINGON` / `I-AMI` のいずれも受理する (RFC 5646 §2.1.1 (Formatting of Language Tags) が "I-AMI" は "i-ami" と等価と述べる)
2. 判定順序は privateuse の早期 return → `irregular` の一致判定 → primary language subtag の形式検査 → 後続サブタグの検査とした。`i-klingon` のように primary が 1 文字のタグは 3 番目の規則で拒否されるため、2 番目の判定で受理する
   - 固定リストは "These tags were registered under [RFC3066] and are a fixed list that can never change." と仕様が述べるとおり IANA Language Subtag Registry の更新で増えないため定数として持つ
   - この文は `regular` と `irregular` を合わせた grandfathered タグ全体について述べており、`irregular` はその一部である (doc コメントにも明記した)
3. 大文字小文字を無視するのは `irregular` との一致判定だけに限定した。privateuse の `x` は従来どおり小文字のみを受理し、`X-FOO` のような大文字 `X` の扱いは変えていない
4. 完全な BCP 47 検証 (IANA Language Subtag Registry との照合、primary language subtag 以外のサブタグの最大長の検査、`extlang` / `variant` / `extension` の構造検査) を行わない方針は維持し、`validate_lang_tag` の doc コメントに irregular タグ (primary が 1 文字の `i-` を含む) と、primary language subtag の検査に例外があること、検査しない範囲を追記した

テスト:

- `tests/test_msf/error_cases.rs` に 8 本追加した
  - `lang_irregular_tags_accepted`: `irregular` の 17 タグを RFC の正規表記で decode が受理すること。このうち `i-*` の 13 件は primary が 1 文字のため固定リストとの一致でなければ受理されず、`en-GB-oed` と `sgn-*` の 4 件は primary が 2〜3 文字のため汎用規則でも受理される (テストの doc コメントに内訳を明記した)
  - `lang_irregular_case_insensitive_accepted`: `I-AMI` / `I-KLINGON` / `I-Default` / `I-ENOCHIAN` / `I-NAVAJO` を受理すること (§2.1.1)。`i-*` は大文字小文字無視がなければ受理されない値だけを選んでいる
  - `lang_regular_grandfathered_tags_accepted`: `regular` の 9 タグ (`zh-min-nan` / `art-lojban` など) を受理すること (従来どおりの受理を固定する既存テストは無かった)
  - `lang_unknown_single_char_primary_rejected`: `i` / `i-not-a-tag` / `i-klingonish` / `i-klingon-x` / `i-klingon-` / `I-KLINGON-X` を拒否すること。`i-` 接頭辞一般ではなく固定リストとのタグ全体の一致のみを受理することと、拒否理由が primary language subtag の形式検査であることを固定する
  - `lang_uppercase_privateuse_rejected`: 大文字 `X` の privateuse (`X-FOO` / `X-foo`) を拒否し、対になる `x-foo` を受理すること (大文字小文字無視を irregular の一致判定に限定したことの固定)
  - `lang_irregular_in_delta_add_and_clone_accepted`: delta の add / clone の decode で `i-klingon` を受理すること
  - `encode_full_irregular_lang_roundtrip`: full カタログの encode / decode 往復で `i-klingon` が保持されること
  - `encode_delta_irregular_lang_roundtrip`: delta の add / clone の encode / decode 往復で `i-klingon` / `I-AMI` がそのまま保持されること (大文字表記を正規化しないことの固定)
  - `lang_invalid_single_char` の doc コメントを irregular の例外を含む形に修正
- `pbt/tests/prop_msf.rs` の `sample_lang` が 1/8 の重み (全体では約 1/20) で grandfathered タグ (`i-klingon` / `I-AMI` / `i-default` / `en-GB-oed` / `zh-min-nan`) を返すようにし、`full_catalog_roundtrip` / `delta_roundtrip` の encode / decode 往復で irregular タグを検証するようにした
- 既存の拒否テスト 8 本 (`lang_invalid_empty` / `lang_invalid_numeric_primary` / `lang_invalid_single_char` / `lang_invalid_special_chars` / `lang_invalid_leading_dash` / `lang_invalid_trailing_dash` / `lang_invalid_primary_too_long` / `lang_privateuse_without_subtag_rejected`) は変更せず維持している
- 変異実験で検出力を確認した (一致判定を `==` に変更すると 2 本、`irregular` の判定を primary 検査の後ろへ移動すると 5 本、`i-` 接頭辞の一致へ緩めると 1 本、privateuse を大文字小文字無視にすると 1 本、固定リストから `i-*` の 13 件を削除すると 5 本、`en-GB-oed` と `sgn-*` の 4 件を削除すると 0 本 = doc コメントに書いた内訳どおりのテストが失敗する)

`CHANGES.md` の `## develop` の `[FIX]` 群の末尾に `[FIX]` を追加し、`docs/IMPLEMENTATION.md` の検証規則の記述に grandfathered タグを含むこと (`irregular` は固定リスト、`regular` は langtag 規則で受理する) を追記した。
