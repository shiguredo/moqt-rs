# lang 検証が grandfathered irregular タグを拒否する

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
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
