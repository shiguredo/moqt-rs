use super::*;

// ─── lang BCP 47 軽量バリデーション ───────────────────────────────────────────

#[test]
fn lang_invalid_empty() {
    let json =
        br#"{"version":"draft-02","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":""}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// primary language subtag が ASCII 英字でない場合 (数字) は拒否されることを確認する
#[test]
fn lang_invalid_numeric_primary() {
    let json =
        br#"{"version":"draft-02","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"12"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// primary language subtag が 1 文字の場合は拒否されることを確認する (irregular 固定リストの `i-*` を除く)
///
/// irregular 固定リストの `i-*` だけが例外であり、`i-` 接頭辞一般が許されるわけではない
#[test]
fn lang_invalid_single_char() {
    let json =
        br#"{"version":"draft-02","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"e"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn lang_invalid_special_chars() {
    let json =
        br#"{"version":"draft-02","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"eng-!!"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn lang_invalid_leading_dash() {
    let json =
        br#"{"version":"draft-02","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"-en"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn lang_invalid_trailing_dash() {
    let json =
        br#"{"version":"draft-02","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"en-"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// RFC 5646 §2.1 (Syntax): language = 4ALPHA も 5*8ALPHA も正当な primary のため受理される
#[test]
fn lang_primary_four_to_eight_letters_accepted() {
    for lang in ["abcd", "abcde", "abcdefgh"] {
        let json = format!(
            r#"{{"version":"draft-02","tracks":[{{"name":"a","packaging":"loc","isLive":true,"lang":"{lang}"}}]}}"#
        );
        assert!(
            MsfCatalogDocument::decode(json.as_bytes(), shiguredo_moqt::msf::MsfCompression::None)
                .is_ok(),
            "primary '{lang}' は受理されること"
        );
    }
}

/// RFC 5646 §2.1 (Syntax): primary が 9 文字以上なら拒否されること
#[test]
fn lang_invalid_primary_too_long() {
    let json =
        br#"{"version":"draft-02","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"abcdefghi"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// RFC 5646 §2.1 (Syntax): privateuse tag ("x" + 1 個以上のサブタグ) は受理されること
#[test]
fn lang_privateuse_accepted() {
    for lang in ["x-foo", "x-foo-bar", "x-1"] {
        let json = format!(
            r#"{{"version":"draft-02","tracks":[{{"name":"a","packaging":"loc","isLive":true,"lang":"{lang}"}}]}}"#
        );
        assert!(
            MsfCatalogDocument::decode(json.as_bytes(), shiguredo_moqt::msf::MsfCompression::None)
                .is_ok(),
            "privateuse '{lang}' は受理されること"
        );
    }
}

/// RFC 5646 §2.1 (Syntax): privateuse は最低 1 個のサブタグを必要とするため "x" 単独は拒否される
#[test]
fn lang_privateuse_without_subtag_rejected() {
    let json =
        br#"{"version":"draft-02","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"x"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// RFC 5646 §2.1 (Syntax): irregular (grandfathered) タグの固定リスト 17 件を受理すること
///
/// 値は仕様の ABNF の正規表記のまま渡す。このうち `i-*` の 13 件は primary language subtag が
/// 1 文字のため固定リストとの一致でなければ受理されず、`en-GB-oed` と `sgn-*` の 4 件は
/// primary が 2〜3 文字で後続も英数字のため汎用規則でも受理される
/// (固定リストへの登録が結果を変えるのは 13 件である)。大文字小文字を無視した一致は
/// `lang_irregular_case_insensitive_accepted` が検証する
#[test]
fn lang_irregular_tags_accepted() {
    for lang in [
        "en-GB-oed",
        "i-ami",
        "i-bnn",
        "i-default",
        "i-enochian",
        "i-hak",
        "i-klingon",
        "i-lux",
        "i-mingo",
        "i-navajo",
        "i-pwn",
        "i-tao",
        "i-tay",
        "i-tsu",
        "sgn-BE-FR",
        "sgn-BE-NL",
        "sgn-CH-DE",
    ] {
        let json = format!(
            r#"{{"version":"draft-02","tracks":[{{"name":"a","packaging":"loc","isLive":true,"lang":"{lang}"}}]}}"#
        );
        assert!(
            MsfCatalogDocument::decode(json.as_bytes(), shiguredo_moqt::msf::MsfCompression::None)
                .is_ok(),
            "irregular '{lang}' は受理されること"
        );
    }
}

/// RFC 5646 §2.1.1 (Formatting of Language Tags): 大文字小文字は区別しないため
/// "I-AMI" は "i-ami" と等価であり、大文字表記も受理されること
#[test]
fn lang_irregular_case_insensitive_accepted() {
    // primary subtag が 1 文字の `i-*` は irregular の一致判定でなければ受理されない
    for lang in ["I-AMI", "I-KLINGON", "I-Default", "I-ENOCHIAN", "I-NAVAJO"] {
        let json = format!(
            r#"{{"version":"draft-02","tracks":[{{"name":"a","packaging":"loc","isLive":true,"lang":"{lang}"}}]}}"#
        );
        assert!(
            MsfCatalogDocument::decode(json.as_bytes(), shiguredo_moqt::msf::MsfCompression::None)
                .is_ok(),
            "irregular '{lang}' は大文字小文字を問わず受理されること"
        );
    }
}

/// RFC 5646 §2.1 (Syntax): regular (grandfathered) タグは `langtag` の規則に一致するため
/// 受理されること
#[test]
fn lang_regular_grandfathered_tags_accepted() {
    for lang in [
        "art-lojban",
        "cel-gaulish",
        "no-bok",
        "no-nyn",
        "zh-guoyu",
        "zh-hakka",
        "zh-min",
        "zh-min-nan",
        "zh-xiang",
    ] {
        let json = format!(
            r#"{{"version":"draft-02","tracks":[{{"name":"a","packaging":"loc","isLive":true,"lang":"{lang}"}}]}}"#
        );
        assert!(
            MsfCatalogDocument::decode(json.as_bytes(), shiguredo_moqt::msf::MsfCompression::None)
                .is_ok(),
            "regular '{lang}' は受理されること"
        );
    }
}

/// irregular 固定リストに一致しない 1 文字 primary は拒否されること
///
/// `i-` 接頭辞を一般的な規則として受理しないことと、固定リストとのタグ全体の一致のみ
/// (ASCII 大文字小文字は無視) を受理することを確認する。
/// すべて primary language subtag の形式検査で拒否される
#[test]
fn lang_unknown_single_char_primary_rejected() {
    for lang in [
        "i",
        "i-not-a-tag",
        "i-klingonish",
        "i-klingon-x",
        "i-klingon-",
        // 後続サブタグが付いた irregular タグも完全一致しないため拒否される
        "I-KLINGON-X",
    ] {
        let json = format!(
            r#"{{"version":"draft-02","tracks":[{{"name":"a","packaging":"loc","isLive":true,"lang":"{lang}"}}]}}"#
        );
        assert!(
            matches!(
                MsfCatalogDocument::decode(
                    json.as_bytes(),
                    shiguredo_moqt::msf::MsfCompression::None
                ),
                Err(MessageError::InvalidCatalog(_))
            ),
            "irregular 固定リストに無い '{lang}' は構文検査で拒否されること"
        );
    }
}

/// privateuse の `x` は大文字小文字を区別しないこと
///
/// RFC 5646 §2.1.1 により ABNF は ASCII 大文字小文字を区別しないため、
/// `X-FOO` / `X-foo` も privateuse として受理する。
#[test]
fn lang_uppercase_privateuse_accepted() {
    for lang in ["X-FOO", "X-foo"] {
        let json = format!(
            r#"{{"version":"draft-02","tracks":[{{"name":"a","packaging":"loc","isLive":true,"lang":"{lang}"}}]}}"#
        );
        MsfCatalogDocument::decode(json.as_bytes(), shiguredo_moqt::msf::MsfCompression::None)
            .expect("大文字 X の privateuse を受理すること");
    }
    // 小文字 `x` は従来どおり受理される (対になる確認)
    let json =
        br#"{"version":"draft-02","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"x-foo"}]}"#;
    assert!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None).is_ok(),
        "小文字 x の privateuse は受理されること"
    );
}

/// delta add / clone 経路でも irregular タグが受理されること
#[test]
fn lang_irregular_in_delta_add_and_clone_accepted() {
    let add = br#"{"generatedAt":1,"deltaUpdate":[{"op":"add","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"i-klingon"}]}]}"#;
    assert!(
        MsfCatalogDocument::decode(add, shiguredo_moqt::msf::MsfCompression::None).is_ok(),
        "delta add の irregular タグは受理されること"
    );
    let clone = br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"a","parentName":"parent","lang":"i-klingon"}]}]}"#;
    assert!(
        MsfCatalogDocument::decode(clone, shiguredo_moqt::msf::MsfCompression::None).is_ok(),
        "delta clone の irregular タグは受理されること"
    );
}

/// add operation 経路でも lang バリデーションが発火することを確認する
#[test]
fn lang_invalid_in_add_tracks() {
    let json = br#"{"generatedAt":1,"deltaUpdate":[{"op":"add","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"1"}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// clone operation 内の不正な lang も拒否されることを確認する
#[test]
fn lang_invalid_in_clone_track() {
    let json =
        br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"a","parentName":"parent","lang":"1"}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}
