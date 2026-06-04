use super::*;

#[test]
fn resolve_basic_substitution() {
    // `%id%` が fragment 値で置換されること
    let json = br#"{"version":"draft-01","tracks":[{"name":"cmcdv2-%id%","packaging":"loc","isLive":true}]}"#;
    let resolved =
        resolve_catalog_variables(json, "id=bob").expect("テストフィクスチャの前提条件を満たす");
    let text = String::from_utf8(resolved).expect("テストフィクスチャの前提条件を満たす");
    assert!(text.contains("cmcdv2-bob"));
    assert!(!text.contains("%id%"));
    let doc =
        MsfCatalogDocument::decode(text.as_bytes()).expect("テストフィクスチャの前提条件を満たす");
    assert!(matches!(doc, MsfCatalogDocument::Full(_)));
}

#[test]
fn resolve_with_reserved_params_ignored() {
    // §11.1 形の予約パラメータ混じり fragment でも変数は解決されること
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"authInfo":{"cat":"%token%"}}]}"#;
    let resolved =
        resolve_catalog_variables(json, "msf:ns--catalog&token=abc&location-range=16.24")
            .expect("テストフィクスチャの前提条件を満たす");
    let text = String::from_utf8(resolved).expect("テストフィクスチャの前提条件を満たす");
    assert!(text.contains("\"cat\":\"abc\""));
}

#[test]
fn resolve_at_value() {
    // 値の `@` は受理され置換されること
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"%id%","packaging":"loc","isLive":true}]}"#;
    let resolved =
        resolve_catalog_variables(json, "id=a@b").expect("テストフィクスチャの前提条件を満たす");
    let text = String::from_utf8(resolved).expect("テストフィクスチャの前提条件を満たす");
    assert!(text.contains("\"a@b\""));
}
#[test]
fn resolve_section_5_2_43_full_form() {
    // §5.2.43 例の fragment 形 (先頭の `=` なし要素付き) でも解決できること
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"authInfo":{"cat":"%token%"}}]}"#;
    let resolved = resolve_catalog_variables(json, "namespace--name&token=XYZ789")
        .expect("テストフィクスチャの前提条件を満たす");
    let text = String::from_utf8(resolved).expect("テストフィクスチャの前提条件を満たす");
    assert!(text.contains("\"cat\":\"XYZ789\""));
}

#[test]
fn resolve_first_wins() {
    // 重複キーは先勝ちで解決されること
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"%id%","packaging":"loc","isLive":true}]}"#;
    let resolved =
        resolve_catalog_variables(json, "id=a&id=b").expect("テストフィクスチャの前提条件を満たす");
    let text = String::from_utf8(resolved).expect("テストフィクスチャの前提条件を満たす");
    assert!(text.contains("\"a\""));
}

#[test]
fn resolve_name_case_sensitive() {
    // 変数名の大文字小文字は区別されること
    let json = br#"{"version":"draft-01","tracks":[{"name":"%id%-%ID%","packaging":"loc","isLive":true}]}"#;
    let resolved =
        resolve_catalog_variables(json, "id=a&ID=b").expect("テストフィクスチャの前提条件を満たす");
    let text = String::from_utf8(resolved).expect("テストフィクスチャの前提条件を満たす");
    assert!(text.contains("\"a-b\""));
}

#[test]
fn resolve_msf_prefixed_fragment() {
    // `msf:` 付き fragment 全体でも変数は解決されること (先頭要素は無視される)
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"authInfo":{"cat":"%token%"}}]}"#;
    let resolved = resolve_catalog_variables(json, "msf:ns--catalog&token=XYZ789")
        .expect("テストフィクスチャの前提条件を満たす");
    let text = String::from_utf8(resolved).expect("テストフィクスチャの前提条件を満たす");
    assert!(text.contains("\"cat\":\"XYZ789\""));
}

#[test]
fn resolve_error_kinds() {
    // 未閉じと不正参照でメッセージ種別が異なること
    let unclosed = br#"{"name":"%id"}"#;
    let Err(MessageError::InvalidCatalog(reason)) = resolve_catalog_variables(unclosed, "id=bob")
    else {
        panic!("未閉じで失敗すること");
    };
    assert!(reason.contains("unclosed"), "未閉じであること: {reason}");
    let invalid = br#"{"name":"%a b%"}"#;
    let Err(MessageError::InvalidCatalog(reason)) = resolve_catalog_variables(invalid, "id=bob")
    else {
        panic!("不正参照で失敗すること");
    };
    assert!(
        reason.contains("invalid variable reference"),
        "不正参照であること: {reason}"
    );
}

#[test]
fn resolve_error_contains_position() {
    // 不正参照エラーは開始バイト位置を含むこと
    let json = br#"{"name":"%a b%"}"#;
    let Err(MessageError::InvalidCatalog(reason)) = resolve_catalog_variables(json, "id=bob")
    else {
        panic!("不正参照で失敗すること");
    };
    assert!(reason.contains("byte 9"), "位置付きであること: {reason}");
}
#[test]
fn resolve_section_5_2_43_example() {
    // §5.2.43 例: token=XYZ789 が "cat": "%token%" に代入されること
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"authInfo":{"cat":"%token%"}}]}"#;
    let resolved = resolve_catalog_variables(json, "token=XYZ789")
        .expect("テストフィクスチャの前提条件を満たす");
    let text = String::from_utf8(resolved).expect("テストフィクスチャの前提条件を満たす");
    assert!(text.contains("\"cat\":\"XYZ789\""));
}

#[test]
fn resolve_undefined_kept() {
    // 未定義変数の参照は保持されること
    let json = br#"{"version":"draft-01","tracks":[{"name":"%missing%","packaging":"loc","isLive":true}]}"#;
    let resolved =
        resolve_catalog_variables(json, "id=bob").expect("テストフィクスチャの前提条件を満たす");
    let text = String::from_utf8(resolved).expect("テストフィクスチャの前提条件を満たす");
    assert!(text.contains("%missing%"));
}

#[test]
fn resolve_name_charset_and_case() {
    // 変数名の `-`・`_` と大文字小文字の区別
    let json = br#"{"version":"draft-01","tracks":[{"name":"%my-id_X%-%MY-ID%","packaging":"loc","isLive":true}]}"#;
    let resolved = resolve_catalog_variables(json, "my-id_X=a&MY-ID=b")
        .expect("テストフィクスチャの前提条件を満たす");
    let text = String::from_utf8(resolved).expect("テストフィクスチャの前提条件を満たす");
    assert!(text.contains("a-b"));
}

#[test]
fn resolve_multiple_variables() {
    // §5.6.14 形の複数変数同時置換 (token / id / event)。3 変数すべて置換されること
    // (§5.6.14 自体は track 直下 c4m を使う例だが、ここでは本実装の authInfo 形に適応する)
    let json = br#"{"version":"draft-01","tracks":[{"name":"%event%","namespace":"%id%","packaging":"loc","isLive":true,"authInfo":{"cat":"%token%"}}]}"#;
    let resolved = resolve_catalog_variables(json, "token=1234&id=bob&event=xyz")
        .expect("テストフィクスチャの前提条件を満たす");
    let text = String::from_utf8(resolved).expect("テストフィクスチャの前提条件を満たす");
    assert!(!text.contains("%token%"));
    assert!(!text.contains("%id%"));
    assert!(!text.contains("%event%"));
    let doc =
        MsfCatalogDocument::decode(text.as_bytes()).expect("テストフィクスチャの前提条件を満たす");
    assert!(matches!(doc, MsfCatalogDocument::Full(_)));
}

#[test]
fn resolve_propagates_fragment_error() {
    // 参照される値の文字種外は resolve から伝播すること
    // (参照されない予約パラメータ等は不問とする)
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"%id%","packaging":"loc","isLive":true}]}"#;
    assert!(matches!(
        resolve_catalog_variables(json, "id=a,b"),
        Err(MessageError::InvalidCatalog(_))
    ));
    assert!(matches!(
        resolve_catalog_variables(json, "#id=bob"),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn fragment_pairs_edge_cases() {
    // 重複キーは両方保持され、空値は空文字列として受理されること
    let pairs =
        parse_fragment_pairs("id=a&id=b&empty=").expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        pairs,
        vec![
            ("id".to_string(), "a".to_string()),
            ("id".to_string(), "b".to_string()),
            ("empty".to_string(), String::new())
        ]
    );
    // 空の変数名は reject されること
    assert!(matches!(
        parse_fragment_pairs("=x"),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn fragment_pairs_parse() {
    // `=` を含まない要素は無視されること
    let pairs = parse_fragment_pairs("id=bob&flag&event=xyz")
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        pairs,
        vec![
            ("id".to_string(), "bob".to_string()),
            ("event".to_string(), "xyz".to_string())
        ]
    );
    // 値文字種外は reject されること
    assert!(matches!(
        parse_fragment_pairs("id=a,b"),
        Err(MessageError::InvalidCatalog(_))
    ));
    // 空 fragment は空になること
    assert_eq!(
        parse_fragment_pairs("").expect("テストフィクスチャの前提条件を満たす"),
        Vec::new()
    );
}

#[test]
fn resolve_invalid_percent_rejected() {
    // 不正 % は種類ごとに reject されること
    for (name, json) in [
        (
            "閉じない %",
            br#"{"version":"draft-01","tracks":[{"name":"%id","packaging":"loc","isLive":true}]}"#
                .as_slice(),
        ),
        (
            "空名",
            br#"{"version":"draft-01","tracks":[{"name":"%%","packaging":"loc","isLive":true}]}"#
                .as_slice(),
        ),
        (
            "変数名不正文字",
            br#"{"version":"draft-01","tracks":[{"name":"%a b%","packaging":"loc","isLive":true}]}"#
                .as_slice(),
        ),
        (
            "リテラル %",
            br#"{"version":"draft-01","tracks":[{"name":"100%","packaging":"loc","isLive":true}]}"#
                .as_slice(),
        ),
    ] {
        assert!(
            matches!(
                resolve_catalog_variables(json, "id=bob"),
                Err(MessageError::InvalidCatalog(_))
            ),
            "{name} は reject されること"
        );
    }
}
