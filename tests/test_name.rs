//! draft-ietf-moq-transport-21 §8.8 (Representing Namespace and Track Names) / draft-ietf-moq-transport-21 §8.8.1 (Parsing Serialized Names): Namespace / Track Name のシリアライズ表現とパースの単体テスト。
//! PBT (pbt/tests/prop_name.rs) が網羅するラウンドトリップとは別に、PBT で生成しにくい
//! 不正入力・境界・特異値を検証する。

use shiguredo_moqt::{
    message::common::TrackNamespace, name::NameParseError, name::parse_name, name::serialize_name,
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
}
