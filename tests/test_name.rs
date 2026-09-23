//! draft-ietf-moq-transport-21 §8.8 (Representing Namespace and Track Names) / draft-ietf-moq-transport-21 §8.8.1 (Parsing Serialized Names): Namespace / Track Name のシリアライズ表現とパースの単体テスト。
//! PBT (pbt/tests/prop_name.rs) が網羅するラウンドトリップとは別に、PBT で生成しにくい
//! 不正入力・境界・特異値と、`NameParseError` が `core::error::Error` を実装していることを検証する。

use shiguredo_moqt::{
    message::common::TrackNamespace, name::NameParseError, name::parse_name,
    name::parse_name_with_percent_encoding, name::serialize_name,
};

/// フィールドリストから TrackNamespace を作る
fn ns(fields: &[&[u8]]) -> TrackNamespace {
    TrackNamespace::new(fields.iter().map(|f| f.to_vec()).collect())
        .expect("正当な namespace である")
}

mod draft_example {
    use super::*;

    #[test]
    fn example_serialize_and_parse() {
        // draft-ietf-moq-transport-21 §8.8.1 (Parsing Serialized Names) の例
        // example.net の `.` (0x2e) はリテラル不可のため `.2e` になる (冗長エンコードではない)
        let namespace = ns(&[b"example.net", b"team2", b"project_x"]);
        let track = b"report";
        let serialized = serialize_name(&namespace, track);
        assert_eq!(serialized, "example.2enet-team2-project_x--report");

        let (parsed_ns, parsed_track) =
            parse_name(&serialized).expect("正当なシリアライズ名である");
        assert_eq!(parsed_ns, namespace);
        assert_eq!(parsed_track, track);
    }
}

mod separator_invariant {
    use super::*;

    #[test]
    fn hyphen_and_period_always_hex_encoded() {
        // 0x2d (`-`) / 0x2e (`.`) はリテラル範囲外なので必ず `.2d` / `.2e` にエンコードされる
        let namespace = ns(&[b"a-b"]);
        let serialized = serialize_name(&namespace, b"x.y");
        assert_eq!(serialized, "a.2db--x.2ey");
        let (parsed_ns, parsed_track) =
            parse_name(&serialized).expect("正当なシリアライズ名である");
        assert_eq!(parsed_ns, namespace);
        assert_eq!(parsed_track, b"x.y");
    }
}

mod boundary_cases {
    use super::*;

    #[test]
    fn zero_namespace_fields() {
        // namespace 0 個 → 先頭が `--`
        let namespace = ns(&[]);
        let serialized = serialize_name(&namespace, b"report");
        assert_eq!(serialized, "--report");
        let (parsed_ns, parsed_track) = parse_name("--report").expect("正当なシリアライズ名である");
        assert_eq!(parsed_ns, namespace);
        assert_eq!(parsed_track, b"report");
    }

    #[test]
    fn empty_track_name() {
        // track name 空 → 末尾が `--`
        let namespace = ns(&[b"ns"]);
        let serialized = serialize_name(&namespace, b"");
        assert_eq!(serialized, "ns--");
        let (parsed_ns, parsed_track) = parse_name("ns--").expect("正当なシリアライズ名である");
        assert_eq!(parsed_ns, namespace);
        assert_eq!(parsed_track, b"");
    }

    #[test]
    fn both_empty() {
        // 0 フィールド namespace + 空 track name
        let namespace = ns(&[]);
        let serialized = serialize_name(&namespace, b"");
        assert_eq!(serialized, "--");
        let (parsed_ns, parsed_track) = parse_name("--").expect("正当なシリアライズ名である");
        assert_eq!(parsed_ns, namespace);
        assert_eq!(parsed_track, b"");
    }
}

mod parse_errors {
    use super::*;

    #[test]
    fn uppercase_hex_rejected() {
        assert_eq!(parse_name("x--.2E"), Err(NameParseError::UppercaseHex));
    }

    #[test]
    fn redundant_encoding_rejected() {
        // .61 = 'a' はリテラル表現可能なので冗長
        assert_eq!(parse_name("x--.61"), Err(NameParseError::RedundantEncoding));
        // .5f = '_' (0x5f) もリテラル表現可能なので冗長 (literal 集合に `_` を含む境界)
        assert_eq!(parse_name("x--.5f"), Err(NameParseError::RedundantEncoding));
    }

    #[test]
    fn trailing_period_rejected() {
        assert_eq!(parse_name("x--a."), Err(NameParseError::InvalidEscape));
    }

    #[test]
    fn single_hex_digit_rejected() {
        assert_eq!(parse_name("x--.6"), Err(NameParseError::InvalidEscape));
    }

    #[test]
    fn non_hex_after_period_rejected() {
        assert_eq!(parse_name("x--.gg"), Err(NameParseError::InvalidEscape));
    }

    #[test]
    fn leading_hyphen_empty_field_rejected() {
        // 先頭ハイフン由来の空 namespace フィールド
        assert_eq!(
            parse_name("-a--b"),
            Err(NameParseError::EmptyNamespaceField)
        );
    }

    #[test]
    fn hyphen_run_three_rejected() {
        assert_eq!(parse_name("a---b"), Err(NameParseError::MultipleSeparators));
    }

    #[test]
    fn multiple_boundary_rejected() {
        assert_eq!(
            parse_name("a--b--c"),
            Err(NameParseError::MultipleSeparators)
        );
    }

    #[test]
    fn missing_separator_rejected() {
        assert_eq!(parse_name("abc"), Err(NameParseError::MissingSeparator));
        assert_eq!(parse_name("a-b-c"), Err(NameParseError::MissingSeparator));
    }

    #[test]
    fn bare_hyphen_in_track_name_rejected() {
        // track name 部に裸の `-` が残る不正入力は track 部のバイト検証で弾かれる
        assert_eq!(parse_name("a--b-c"), Err(NameParseError::InvalidEscape));
    }

    #[test]
    fn full_name_too_long_rejected() {
        // namespace 4090 + track 10 = 4100 > 4096
        let serialized = format!("{}--{}", "a".repeat(4090), "b".repeat(10));
        assert_eq!(
            parse_name(&serialized),
            Err(NameParseError::FullNameTooLong)
        );
    }

    #[test]
    fn too_many_namespace_fields_rejected() {
        // 33 フィールド (> 32)
        let ns_part = vec!["a"; 33].join("-");
        let serialized = format!("{ns_part}--x");
        assert_eq!(
            parse_name(&serialized),
            Err(NameParseError::InvalidNamespace)
        );
    }

    // `Display` が variant ごとの失敗内容を表す
    #[test]
    fn display_describes_each_variant() {
        // namespace 4090 + track 10 = 4100 > 4096
        let too_long = format!("{}--{}", "a".repeat(4090), "b".repeat(10));
        // 33 フィールド (> 32)
        let too_many_fields = format!("{}--x", vec!["a"; 33].join("-"));

        let cases = [
            ("x--.2E", "hex escape must use lowercase hex digits"),
            ("x--.61", "hex escape for a literal byte is redundant"),
            ("x--a.", "invalid escape sequence or unexpected byte"),
            ("-a--b", "namespace field must not be empty"),
            ("abc", "missing namespace and track name separator"),
            (
                "a---b",
                "too many separators or a hyphen run of three or more",
            ),
            (
                too_long.as_str(),
                "full track name is too long (max 4096 bytes)",
            ),
            (
                too_many_fields.as_str(),
                "namespace is invalid (max 32 fields)",
            ),
        ];

        for (input, expected) in cases {
            let err = parse_name(input).expect_err("不正な名前はエラーになること");
            assert_eq!(
                err.to_string(),
                expected,
                "Display が失敗の内容を表すこと: {input}"
            );
        }
    }
}

/// `NameParseError` を `Box<dyn std::error::Error + Send + Sync>` へ `?` で変換できる
///
/// 名前を解析する公開 API のエラーを、利用側が自身のエラー型に包んで伝播させるために
/// `?` を使えることを確認する。
#[test]
fn name_parse_error_converts_into_boxed_error() {
    /// 不正な名前を `?` で伝播させる
    fn parse_invalid() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // ピリオド後の hex は小文字でなければならない
        // (draft-ietf-moq-transport-21 §8.8.1 (Parsing Serialized Names))
        let _ = parse_name("x--.2E")?;
        Ok(())
    }

    let err = parse_invalid().expect_err("大文字 hex を使う名前はエラーになること");
    assert_eq!(
        err.to_string(),
        "hex escape must use lowercase hex digits",
        "Display が失敗の内容を表すこと"
    );
    assert!(err.source().is_none(), "内包するエラーは無いこと");
}

/// `parse_name` は URI 層の `%XX` を受理しない (§8.8.1 の表現のみを扱う)
///
/// percent-decoding は MSF URI 用の `parse_name_with_percent_encoding` だけが行う。
#[test]
fn parse_name_rejects_percent_encoding() {
    assert_eq!(parse_name("x--a%3Fb"), Err(NameParseError::InvalidEscape));
    assert_eq!(parse_name("x--%61"), Err(NameParseError::InvalidEscape));
    assert_eq!(parse_name("x%2Da--b"), Err(NameParseError::InvalidEscape));
}

/// `parse_name_with_percent_encoding` は `%XX` をデータバイトとして受理すること
///
/// draft-ietf-moq-msf-01 §11.1 (URL construction and interpretation) の track-identifier は
/// `pct-encoded` を含む。`.` + hex の規則 (`UppercaseHex` / `RedundantEncoding`) は
/// `%XX` には適用せず、素の `.` エスケープには従来どおり適用する。
#[test]
fn parse_name_with_percent_encoding_decodes_octets() {
    let (namespace, track_name) = parse_name_with_percent_encoding("ns--a%3Fb")
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(namespace.fields(), &[b"ns".to_vec()]);
    assert_eq!(track_name, b"a?b".to_vec());

    // `%61` は 0x61 のデータバイト (素の `.61` は冗長として拒否)
    let (_, track_name) =
        parse_name_with_percent_encoding("ns--%61").expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(track_name, vec![0x61]);
    assert_eq!(
        parse_name_with_percent_encoding("ns--.61"),
        Err(NameParseError::RedundantEncoding)
    );

    // `.` の直後は hex 2 桁でなければならない (畳み込みは行わない)
    assert_eq!(
        parse_name_with_percent_encoding("ns--.2%33"),
        Err(NameParseError::InvalidEscape)
    );

    // `%` の後が hex 2 桁でない場合は InvalidEscape
    assert_eq!(
        parse_name_with_percent_encoding("ns--a%"),
        Err(NameParseError::InvalidEscape)
    );
    assert_eq!(
        parse_name_with_percent_encoding("ns--a%3"),
        Err(NameParseError::InvalidEscape)
    );
    assert_eq!(
        parse_name_with_percent_encoding("ns--a%zz"),
        Err(NameParseError::InvalidEscape)
    );
    // 2 文字揃っていても hex でなければ InvalidEscape (`%zzz` / `%3z` は長さ検査を通過する)
    assert_eq!(
        parse_name_with_percent_encoding("ns--a%zzz"),
        Err(NameParseError::InvalidEscape)
    );
    assert_eq!(
        parse_name_with_percent_encoding("ns--a%3z"),
        Err(NameParseError::InvalidEscape)
    );
}
