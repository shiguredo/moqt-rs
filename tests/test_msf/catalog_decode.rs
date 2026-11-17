use super::*;

#[test]
fn full_catalog_minimal() {
    let json = br#"{"version":"draft-02","tracks":[]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full が期待された");
    };
    assert_eq!(cat.version, "draft-02");
    assert_eq!(cat.generated_at, None);
    assert!(!cat.is_complete);
    assert!(cat.tracks.is_empty());
}

#[test]
fn full_catalog_with_all_root_fields() {
    let json =
        br#"{"version":"draft-02","generatedAt":1700000000000,"isComplete":true,"tracks":[]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full が期待された");
    };
    assert_eq!(cat.generated_at, Some(1_700_000_000_000));
    assert!(cat.is_complete);
}

#[test]
fn full_catalog_unknown_fields_ignored() {
    // パーサは未知フィールドを無視する (draft-ietf-moq-msf-02 §5 (Catalog))
    let json = br#"{"version":"draft-02","tracks":[],"unknownField":42,"another":"value"}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    assert!(matches!(doc, MsfCatalogDocument::Full(_)));
}

#[test]
fn unknown_packaging_rejected() {
    // draft-ietf-moq-msf-02 §5.2.4 (Packaging): 許容値は loc / mediatimeline / eventtimeline / moqlog / moqmetrics のみ
    let json =
        br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"custom","isLive":true}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn moqlog_packaging_accepted() {
    // draft-ietf-moq-msf-02 §5.2.4 (Packaging): moqlog は許容値
    let json =
        br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"moqlog","isLive":true}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full が期待された");
    };
    assert_eq!(cat.tracks[0].packaging, MsfPackaging::MoqLog);
}

#[test]
fn moqmetrics_packaging_accepted() {
    // draft-ietf-moq-msf-02 §5.2.4 (Packaging): moqmetrics は許容値
    let json =
        br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"moqmetrics","isLive":true}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full が期待された");
    };
    assert_eq!(cat.tracks[0].packaging, MsfPackaging::MoqMetrics);
}

#[test]
fn delta_minimal() {
    let json = br#"{"generatedAt":1,"deltaUpdate":[{"op":"remove","tracks":[{"name":"x"}]}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Delta(d) = doc else {
        panic!("Delta が期待された");
    };
    assert_eq!(d.generated_at, 1);
    assert_eq!(d.operations.len(), 1);
    let MsfDeltaOperation::Remove { tracks } = &d.operations[0] else {
        panic!("Remove が期待された");
    };
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].name, "x");
}

#[test]
fn remove_tracks_extra_field_rejected() {
    // draft-ietf-moq-msf-02 §5.1.6 (Delta update): remove operation の track object に name/namespace 以外は禁止
    let json = br#"{"generatedAt":1,"deltaUpdate":[{"op":"remove","tracks":[{"name":"x","packaging":"loc"}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn delta_update_non_array_rejected() {
    // draft-ietf-moq-msf-02 §5.1.6 (Delta update): deltaUpdate は operation object の配列
    let json = br#"{"generatedAt":1,"deltaUpdate":false,"version":"draft-02","tracks":[]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn duplicate_track_name_rejected() {
    // draft-ietf-moq-msf-02 §5.2.3 (Track name): track name は namespace ごとに一意
    let json = br#"{"version":"draft-02","tracks":[
            {"name":"audio","packaging":"loc","isLive":true},
            {"name":"audio","packaging":"loc","isLive":true}
        ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn same_name_different_namespace_allowed() {
    // 異なる namespace 内の同名トラックは許容される
    let json = br#"{"version":"draft-02","tracks":[
            {"name":"audio","packaging":"loc","namespace":"ns1","isLive":true},
            {"name":"audio","packaging":"loc","namespace":"ns2","isLive":true}
        ]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    assert!(matches!(doc, MsfCatalogDocument::Full(_)));
}

#[test]
fn is_complete_false_rejected() {
    // draft-ietf-moq-msf-02 §5.1.3 (Is Complete): isComplete が FALSE ならフィールド自体を含めてはならない
    let json = br#"{"version":"draft-02","isComplete":false,"tracks":[]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn clone_track_minimal_rfc_example() {
    // draft-ietf-moq-msf-02 §5.1.6 (Delta update): packaging/isLive 省略の clone track
    let json = br#"{
            "generatedAt": 1,
            "deltaUpdate": [
                {
                    "op": "clone",
                    "tracks": [
                        {
                            "parentName": "video-1080",
                            "name": "video-720",
                            "width": 1280,
                            "height": 720,
                            "bitrate": 600000
                        }
                    ]
                }
            ]
        }"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Delta(d) = doc else {
        panic!("Delta が期待された");
    };
    assert_eq!(d.operations.len(), 1);
    let MsfDeltaOperation::Clone { tracks } = &d.operations[0] else {
        panic!("Clone が期待された");
    };
    assert_eq!(tracks.len(), 1);
    let ct = &tracks[0];
    assert_eq!(ct.name, "video-720");
    assert_eq!(ct.parent_name, "video-1080");
    assert!(ct.packaging.is_none());
    assert!(ct.is_live.is_none());
    assert_eq!(ct.width, Some(1280));
    assert_eq!(ct.height, Some(720));
    assert_eq!(ct.bitrate, Some(600000));
}

#[test]
fn clone_track_with_packaging_and_is_live() {
    // packaging/isLive を明示的に指定した clone track
    let json = br#"{
            "generatedAt": 1,
            "deltaUpdate": [
                {
                    "op": "clone",
                    "tracks": [
                        {
                            "parentName": "parent",
                            "name": "child",
                            "packaging": "loc",
                            "isLive": true
                        }
                    ]
                }
            ]
        }"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Delta(d) = doc else {
        panic!("Delta が期待された");
    };
    let MsfDeltaOperation::Clone { tracks } = &d.operations[0] else {
        panic!("Clone が期待された");
    };
    assert_eq!(tracks[0].packaging, Some(MsfPackaging::Loc));
    assert_eq!(tracks[0].is_live, Some(true));
}

#[test]
fn clone_track_missing_parent_name_rejected() {
    // clone operation 内で parentName がないとエラー
    let json = br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"x"}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// packaging=eventtimeline で eventType を省略した clone は受理されること
///
/// draft-ietf-moq-msf-02 §5.1.6 (Delta update): "The cloned track inherits all attributes
/// from the parent except the Track Name which MUST be new. Attributes redefined in the
/// track object override inherited values." 親が eventtimeline (親は §5.2.5 の必須により
/// eventType を持つ) ならば正当な継承表現であり、decode 時点では親の属性を知らないため
/// 必須違反と断定できない。逆に親が非 eventtimeline ならば実効トラックは §5.2.5 違反に
/// なり得るが、それも decode 時点では判定不能であり、delta update の適用時 (親解決後) の
/// 検証対象となる。
#[test]
fn clone_track_eventtimeline_without_event_type_accepted() {
    let json = br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"child","parentName":"parent","packaging":"eventtimeline"}]}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Delta(d) = doc else {
        panic!("Delta が期待された");
    };
    let MsfDeltaOperation::Clone { tracks } = &d.operations[0] else {
        panic!("Clone が期待された");
    };
    assert_eq!(tracks[0].packaging, Some(MsfPackaging::EventTimeline));
    assert_eq!(tracks[0].event_type, None);
}

/// v02 の `/` 区切り namespace が MOQT の Track Namespace に写像され、
/// §8.8 の URL 形式と往復一致すること
///
/// draft-ietf-moq-msf-02 §5.2.2 (Track namespace) / §11.2.4 (Relationship to the serialized
/// form): カタログ文字列をまずバイト列へ写像し、そのバイト列に MOQT の直列化を適用する。
#[test]
fn catalog_namespace_roundtrips_through_url_form() {
    let namespace = parse_catalog_namespace("customer/livestream/123")
        .expect("v02 の namespace として解決できること");
    assert_eq!(
        namespace.fields(),
        &[
            b"customer".to_vec(),
            b"livestream".to_vec(),
            b"123".to_vec(),
        ],
        "各 substring が順に Track Namespace Field になること"
    );

    let url = shiguredo_moqt::name::serialize_name(&namespace, b"catalog");
    assert_eq!(
        url, "customer-livestream-123--catalog",
        "バイト値に §8.8 の直列化を適用すること"
    );
    let (parsed, track_name) =
        shiguredo_moqt::name::parse_name(&url).expect("URL 形式として解決できること");
    assert_eq!(parsed, namespace, "URL 形式から同じ namespace に戻ること");
    assert_eq!(track_name, b"catalog", "track name が一致すること");
}

/// 不正なカタログ namespace を拒否すること
///
/// draft-ietf-moq-msf-02 §5.2.2 (Track namespace): 1 〜 32 fields、各 field 非空、全体 4096
/// バイト以下。先頭・末尾・連続する `/` は空 field になるため不正。
#[test]
fn invalid_catalog_namespace_rejected() {
    for value in ["", "a//b", "/a", "a/", "a\0b"] {
        assert!(
            matches!(
                parse_catalog_namespace(value),
                Err(MessageError::InvalidCatalog(_))
            ),
            "不正な namespace {value:?} を拒否すること"
        );
    }

    let too_many_fields = (0..33).map(|_| "a").collect::<Vec<_>>().join("/");
    assert!(
        matches!(
            parse_catalog_namespace(&too_many_fields),
            Err(MessageError::InvalidCatalog(_))
        ),
        "33 fields の namespace を拒否すること"
    );

    let exactly_32 = (0..32).map(|_| "a").collect::<Vec<_>>().join("/");
    assert!(
        parse_catalog_namespace(&exactly_32).is_ok(),
        "32 fields の namespace は受理すること"
    );

    let too_long = "a".repeat(4097);
    assert!(
        matches!(
            parse_catalog_namespace(&too_long),
            Err(MessageError::InvalidCatalog(_))
        ),
        "4097 バイトの namespace を拒否すること"
    );
}

/// 不正な track name を decode で拒否すること
#[test]
fn invalid_catalog_track_name_rejected() {
    for json in [
        br#"{"version":"draft-02","tracks":[{"name":"","packaging":"loc","isLive":true}]}"#
            .as_slice(),
        br#"{"version":"draft-02","tracks":[{"name":"a\u0000b","packaging":"loc","isLive":true}]}"#
            .as_slice(),
    ] {
        assert!(
            matches!(
                MsfCatalogDocument::decode(json),
                Err(MessageError::InvalidCatalog(_))
            ),
            "不正な track name を持つカタログを拒否すること: {}",
            String::from_utf8_lossy(json)
        );
    }

    let empty_parent =
        br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":""}]}]}"#;
    assert!(
        matches!(
            MsfCatalogDocument::decode(empty_parent),
            Err(MessageError::InvalidCatalog(_))
        ),
        "空の parentName を拒否すること"
    );

    let empty_remove =
        br#"{"generatedAt":1,"deltaUpdate":[{"op":"remove","tracks":[{"name":""}]}]}"#;
    assert!(
        matches!(
            MsfCatalogDocument::decode(empty_remove),
            Err(MessageError::InvalidCatalog(_))
        ),
        "空の removeTracks name を拒否すること"
    );
}

/// depends の String 要素も track name の写像規則で検証すること
#[test]
fn depends_elements_use_track_name_mapping() {
    let empty = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"depends":[""]}]}"#;
    assert!(
        matches!(
            MsfCatalogDocument::decode(empty),
            Err(MessageError::InvalidCatalog(_))
        ),
        "空の depends 要素を拒否すること"
    );

    let nul = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"depends":["a\u0000b"]}]}"#;
    assert!(
        matches!(
            MsfCatalogDocument::decode(nul),
            Err(MessageError::InvalidCatalog(_))
        ),
        "NUL を含む depends 要素を拒否すること"
    );
}

/// Full Track Name の一意性が継承 namespace を解決して判定されること
///
/// draft-ietf-moq-msf-02 §5.2.3 (Track name): 一意性は明示 namespace と省略 namespace を
/// 解決した後の Full Track Name で判定する。
#[test]
fn track_name_uniqueness_resolves_inherited_namespace() {
    let mut catalog = MsfCatalog::new();
    let mut explicit = MsfTrack::new("video".to_string(), MsfPackaging::Loc, true);
    explicit.namespace = Some("customer/livestream/123".to_string());
    let omitted = MsfTrack::new("video".to_string(), MsfPackaging::Loc, true);
    catalog.tracks.push(explicit.clone());
    catalog.tracks.push(omitted.clone());

    let base = parse_catalog_namespace("customer/livestream/123")
        .expect("テスト用 namespace を解決できること");
    assert!(
        matches!(
            catalog.validate_track_names(Some(&base)),
            Err(MessageError::InvalidCatalog(_))
        ),
        "継承を解決すると同じ Full Track Name になるため拒否すること"
    );

    let mut different = MsfCatalog::new();
    different.tracks.push(explicit);
    let mut other_namespace = omitted;
    other_namespace.namespace = Some("customer/livestream/456".to_string());
    different.tracks.push(other_namespace);
    assert!(
        different.validate_track_names(Some(&base)).is_ok(),
        "namespace が異なる同名 track は別の Full Track Name として受理すること"
    );
}

/// カタログ文書の先頭 BOM と未対サロゲートを拒否すること
///
/// draft-ietf-moq-msf-02 §11.2.1 (Producing bytes from a JSON string): カタログ文書は BOM で
/// 始まってはならず、名前に未対サロゲートを含めてはならない。
#[test]
fn bom_and_unpaired_surrogate_rejected() {
    let mut with_bom = vec![0xef, 0xbb, 0xbf];
    with_bom.extend_from_slice(br#"{"version":"draft-02","tracks":[]}"#);
    assert!(
        matches!(
            MsfCatalogDocument::decode(&with_bom),
            Err(MessageError::InvalidCatalog(_))
        ),
        "先頭 BOM 付きのカタログを拒否すること"
    );

    let surrogate =
        br#"{"version":"draft-02","tracks":[{"name":"a\uDEADb","packaging":"loc","isLive":true}]}"#;
    assert!(
        matches!(
            MsfCatalogDocument::decode(surrogate),
            Err(MessageError::InvalidCatalog(_))
        ),
        "未対サロゲートを含む name を拒否すること"
    );
}

/// 名前中の U+FEFF は BOM ではなく通常の文字として受理すること
#[test]
fn zero_width_no_break_space_in_name_accepted() {
    let json = "{\"version\":\"draft-02\",\"tracks\":[{\"name\":\"a\u{feff}b\",\"packaging\":\"loc\",\"isLive\":true}]}";
    let doc =
        MsfCatalogDocument::decode(json.as_bytes()).expect("名前中の U+FEFF は受理されること");
    let MsfCatalogDocument::Full(catalog) = doc else {
        panic!("Full が期待された");
    };
    assert_eq!(catalog.tracks[0].name, "a\u{feff}b");
}

/// clone / remove / publishTracks の名前・namespace を decode で拒否すること
///
/// draft-ietf-moq-msf-02 §5.2.2 / §5.2.3 / §5.2.34 / §5.2.35 / §11.2: track object の
/// 種類によらず、名前と namespace は同じ写像規則で検証する。
#[test]
fn clone_and_remove_invalid_names_rejected() {
    for json in [
        // clone 自身の name が空 / NUL
        br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"","parentName":"p"}]}]}"#.as_slice(),
        br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"a\u0000b","parentName":"p"}]}]}"#
            .as_slice(),
        // parentName が NUL
        br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"a\u0000b"}]}]}"#
            .as_slice(),
        // parentNamespace が空 field / NUL
        br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","parentNamespace":"a//b"}]}]}"#.as_slice(),
        br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","parentNamespace":"a\u0000b"}]}]}"#.as_slice(),
        // clone 自身の namespace が空 field
        br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","namespace":"a//b"}]}]}"#.as_slice(),
        // clone の depends 要素が空 / NUL
        br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","depends":[""]}]}]}"#.as_slice(),
        br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","depends":["a\u0000b"]}]}]}"#.as_slice(),
        // remove の name が NUL / namespace が空 field
        br#"{"generatedAt":1,"deltaUpdate":[{"op":"remove","tracks":[{"name":"a\u0000b"}]}]}"#.as_slice(),
        br#"{"generatedAt":1,"deltaUpdate":[{"op":"remove","tracks":[{"name":"x","namespace":"a//b"}]}]}"#.as_slice(),
        // publishTracks の name が空 / namespace が空 field
        br#"{"version":"draft-02","tracks":[],"publishTracks":[{"name":"","packaging":"loc","isLive":true}]}"#.as_slice(),
        br#"{"version":"draft-02","tracks":[],"publishTracks":[{"name":"p","namespace":"a//b","packaging":"loc","isLive":true}]}"#.as_slice(),
    ] {
        assert!(
            matches!(
                MsfCatalogDocument::decode(json),
                Err(MessageError::InvalidCatalog(_))
            ),
            "不正な名前または namespace を拒否すること: {}",
            String::from_utf8_lossy(json)
        );
    }
}

/// JSON エスケープの有無が同じ Full Track Name に写像されること
///
/// draft-ietf-moq-msf-02 §11.2.1 (Producing bytes from a JSON string): エスケープは JSON の
/// パース時に解決され、写像は Unicode scalar value の UTF-8 符号化で行う。`"café"` と
/// `"caf\u00e9"` は同じバイト列になる。
#[test]
fn escaped_and_literal_names_are_the_same_full_track_name() {
    let json = "{\"version\":\"draft-02\",\"tracks\":[\
            {\"name\":\"caf\u{e9}\",\"packaging\":\"loc\",\"isLive\":true},\
            {\"name\":\"caf\\u00e9\",\"packaging\":\"loc\",\"isLive\":true}\
        ]}";
    assert!(
        matches!(
            MsfCatalogDocument::decode(json.as_bytes()),
            Err(MessageError::InvalidCatalog(_))
        ),
        "エスケープ表記が違っても同じ Full Track Name は重複として拒否すること"
    );
}

/// Unicode 正規化をしないこと
///
/// draft-ietf-moq-msf-02 §11.2.1: NFC / NFD / NFKC / NFKD などの正規化は行わない。
/// `"café"` (U+00E9) と `"cafe\u0301"` (U+0065 U+0301) は異なる Track Name である。
#[test]
fn unicode_normalization_is_not_applied() {
    let json = "{\"version\":\"draft-02\",\"tracks\":[\
            {\"name\":\"caf\u{e9}\",\"packaging\":\"loc\",\"isLive\":true},\
            {\"name\":\"cafe\\u0301\",\"packaging\":\"loc\",\"isLive\":true}\
        ]}";
    let doc = MsfCatalogDocument::decode(json.as_bytes())
        .expect("正規化しないため 2 つの異なる Track Name として受理されること");
    let MsfCatalogDocument::Full(catalog) = doc else {
        panic!("Full が期待された");
    };
    assert_eq!(catalog.tracks.len(), 2);
    assert_eq!(catalog.tracks[0].name, "café");
    assert_eq!(catalog.tracks[1].name, "cafe\u{0301}");
}

/// 単一 field の `-` を namespaces の区切りとして解釈しないこと
///
/// draft-ietf-moq-msf-02 §5.2.2: v02 で区切りは `/` だけであり、`-` は field の内容である。
#[test]
fn hyphen_is_not_a_namespace_delimiter() {
    let namespace =
        parse_catalog_namespace("a-b").expect("単一 field の namespace として解決できること");
    assert_eq!(namespace.fields(), &[b"a-b".to_vec()]);
    assert_eq!(
        serialize_catalog_namespace(&namespace).expect("カタログ形式に戻せること"),
        "a-b"
    );
}

/// namespace 単体と Full Track Name の 4,096 バイト境界を検証すること
///
/// draft-ietf-moq-msf-02 §5.2.2 (Track namespace) / draft-ietf-moq-transport-23 §8.7
/// (Track Namespace Structure): namespace は 4,096 バイト以下、Full Track Name も 4,096
/// バイト以下である。
#[test]
fn namespace_and_full_track_name_length_boundaries() {
    // namespace 単体は 4,096 バイトちょうどまで受理する
    let max_namespace = "a".repeat(4096);
    assert!(
        parse_catalog_namespace(&max_namespace).is_ok(),
        "4,096 バイトの namespace は単体では受理すること"
    );

    // namespace 4,095 バイト + name 1 バイト = Full Track Name 4,096 バイトちょうど
    let namespace = "a".repeat(4095);
    let ok_json = format!(
        r#"{{"version":"draft-02","tracks":[{{"name":"b","namespace":"{namespace}","packaging":"loc","isLive":true}}]}}"#
    );
    assert!(
        MsfCatalogDocument::decode(ok_json.as_bytes()).is_ok(),
        "Full Track Name 4,096 バイトちょうどは受理すること"
    );

    // namespace 4,096 バイト + name 1 バイト = 4,097 バイトは拒否する
    let too_long_json = format!(
        r#"{{"version":"draft-02","tracks":[{{"name":"b","namespace":"{max_namespace}","packaging":"loc","isLive":true}}]}}"#
    );
    assert!(
        matches!(
            MsfCatalogDocument::decode(too_long_json.as_bytes()),
            Err(MessageError::InvalidCatalog(_))
        ),
        "Full Track Name 4,097 バイトは拒否すること"
    );

    // namespace を省略しても、name 単体が 4,096 バイトを超えれば拒否する
    let long_name = "a".repeat(4097);
    let long_name_json = format!(
        r#"{{"version":"draft-02","tracks":[{{"name":"{long_name}","packaging":"loc","isLive":true}}]}}"#
    );
    assert!(
        matches!(
            MsfCatalogDocument::decode(long_name_json.as_bytes()),
            Err(MessageError::InvalidCatalog(_))
        ),
        "4,097 バイトの Track Name は拒否すること"
    );
}

/// delta の add / clone / remove でも Full Track Name の長さ制限を検証すること
///
/// draft-ietf-moq-transport-23 §8.7 (Track Namespace Structure): Full Track Name は 4,096
/// バイト以下。完全カタログだけでなく delta の各操作でも同じ制約を適用する。
#[test]
fn delta_operations_reject_overlong_full_track_names() {
    let namespace = "a".repeat(4096);

    let add = format!(
        r#"{{"generatedAt":1,"deltaUpdate":[{{"op":"add","tracks":[{{"name":"b","namespace":"{namespace}","packaging":"loc","isLive":true}}]}}]}}"#
    );
    assert!(
        matches!(
            MsfCatalogDocument::decode(add.as_bytes()),
            Err(MessageError::InvalidCatalog(_))
        ),
        "delta add の Full Track Name 4,097 バイトを拒否すること"
    );

    let clone = format!(
        r#"{{"generatedAt":1,"deltaUpdate":[{{"op":"clone","tracks":[{{"name":"b","parentName":"p","namespace":"{namespace}"}}]}}]}}"#
    );
    assert!(
        matches!(
            MsfCatalogDocument::decode(clone.as_bytes()),
            Err(MessageError::InvalidCatalog(_))
        ),
        "delta clone の Full Track Name 4,097 バイトを拒否すること"
    );

    let long_parent_name = "p".repeat(4097);
    let clone_parent = format!(
        r#"{{"generatedAt":1,"deltaUpdate":[{{"op":"clone","tracks":[{{"name":"b","parentName":"{long_parent_name}"}}]}}]}}"#
    );
    assert!(
        matches!(
            MsfCatalogDocument::decode(clone_parent.as_bytes()),
            Err(MessageError::InvalidCatalog(_))
        ),
        "delta clone の parentName 4,097 バイトを拒否すること"
    );

    // clone 自身の namespace を省略し parentNamespace を継承する場合、clone の Full Track Name は
    // parentNamespace + clone name になる。親は 4,096 バイトちょうど、clone は超過する。
    let parent_namespace = "a".repeat(4095);
    let clone_inheriting = format!(
        r#"{{"generatedAt":1,"deltaUpdate":[{{"op":"clone","tracks":[{{"name":"bb","parentName":"p","parentNamespace":"{parent_namespace}"}}]}}]}}"#
    );
    assert!(
        matches!(
            MsfCatalogDocument::decode(clone_inheriting.as_bytes()),
            Err(MessageError::InvalidCatalog(_))
        ),
        "clone が親 namespace を継承した Full Track Name 4,097 バイトを拒否すること"
    );

    let remove = format!(
        r#"{{"generatedAt":1,"deltaUpdate":[{{"op":"remove","tracks":[{{"name":"b","namespace":"{namespace}"}}]}}]}}"#
    );
    assert!(
        matches!(
            MsfCatalogDocument::decode(remove.as_bytes()),
            Err(MessageError::InvalidCatalog(_))
        ),
        "delta remove の Full Track Name 4,097 バイトを拒否すること"
    );
}

/// decode 経由でも各不正な namespace を拒否すること
///
/// `parse_catalog_namespace` の直接テストだけでなく、track object の `namespace` として
/// decode したときにも同じ拒否になることを表駆動で固定する。
#[test]
fn decode_rejects_each_invalid_namespace_form() {
    let mut cases = vec![
        String::new(),
        "/a".to_string(),
        "a/".to_string(),
        "a//b".to_string(),
        "a\\u0000b".to_string(),
        (0..33).map(|_| "a").collect::<Vec<_>>().join("/"),
        "a".repeat(4097),
    ];
    // JSON の \u0000 は Rust の文字列ではバックスラッシュを残したままにする
    cases.push("a\\u0000b".to_string());
    for namespace in cases {
        let json = format!(
            r#"{{"version":"draft-02","tracks":[{{"name":"t","namespace":"{namespace}","packaging":"loc","isLive":true}}]}}"#
        );
        assert!(
            matches!(
                MsfCatalogDocument::decode(json.as_bytes()),
                Err(MessageError::InvalidCatalog(_))
            ),
            "decode で不正な namespace {namespace:?} を拒否すること"
        );
    }
}

/// delta でも Full Track Name 4,096 バイトちょうどは受理し、4,097 バイトは拒否すること
#[test]
fn delta_full_track_name_boundaries() {
    let ok_name = "a".repeat(4096);
    let too_long_name = "a".repeat(4097);

    // add: namespace を省略した name 単体
    let add_ok = format!(
        r#"{{"generatedAt":1,"deltaUpdate":[{{"op":"add","tracks":[{{"name":"{ok_name}","packaging":"loc","isLive":true}}]}}]}}"#
    );
    assert!(
        MsfCatalogDocument::decode(add_ok.as_bytes()).is_ok(),
        "add の name 単体 4,096 バイトは受理すること"
    );
    let add_ng = format!(
        r#"{{"generatedAt":1,"deltaUpdate":[{{"op":"add","tracks":[{{"name":"{too_long_name}","packaging":"loc","isLive":true}}]}}]}}"#
    );
    assert!(
        matches!(
            MsfCatalogDocument::decode(add_ng.as_bytes()),
            Err(MessageError::InvalidCatalog(_))
        ),
        "add の name 単体 4,097 バイトを拒否すること"
    );

    // clone: 自身の name 単体 (parentName は 1 バイト)
    let clone_ok = format!(
        r#"{{"generatedAt":1,"deltaUpdate":[{{"op":"clone","tracks":[{{"name":"{ok_name}","parentName":"p"}}]}}]}}"#
    );
    assert!(
        MsfCatalogDocument::decode(clone_ok.as_bytes()).is_ok(),
        "clone の name 単体 4,096 バイトは受理すること"
    );
    let clone_ng = format!(
        r#"{{"generatedAt":1,"deltaUpdate":[{{"op":"clone","tracks":[{{"name":"{too_long_name}","parentName":"p"}}]}}]}}"#
    );
    assert!(
        matches!(
            MsfCatalogDocument::decode(clone_ng.as_bytes()),
            Err(MessageError::InvalidCatalog(_))
        ),
        "clone の name 単体 4,097 バイトを拒否すること"
    );

    // remove: name 単体
    let remove_ok = format!(
        r#"{{"generatedAt":1,"deltaUpdate":[{{"op":"remove","tracks":[{{"name":"{ok_name}"}}]}}]}}"#
    );
    assert!(
        MsfCatalogDocument::decode(remove_ok.as_bytes()).is_ok(),
        "remove の name 単体 4,096 バイトは受理すること"
    );
    let remove_ng = format!(
        r#"{{"generatedAt":1,"deltaUpdate":[{{"op":"remove","tracks":[{{"name":"{too_long_name}"}}]}}]}}"#
    );
    assert!(
        matches!(
            MsfCatalogDocument::decode(remove_ng.as_bytes()),
            Err(MessageError::InvalidCatalog(_))
        ),
        "remove の name 単体 4,097 バイトを拒否すること"
    );
}

/// clone の親 Full Track Name の長さを clone 自身とは独立に検証すること
///
/// parentNamespace 4,095 バイト + parentName 2 バイト = 4,097 バイトの親は不正だが、
/// clone 自身の name 1 バイト + 継承 namespace 4,095 バイトは 4,096 バイトで正当である。
/// 親側の検証が clone 自身の検証に吸収されないことを固定する。
#[test]
fn clone_parent_full_track_name_length_is_checked_separately() {
    let parent_namespace = "a".repeat(4095);
    let json = format!(
        r#"{{"generatedAt":1,"deltaUpdate":[{{"op":"clone","tracks":[{{"name":"b","parentName":"pp","parentNamespace":"{parent_namespace}"}}]}}]}}"#
    );
    assert!(
        matches!(
            MsfCatalogDocument::decode(json.as_bytes()),
            Err(MessageError::InvalidCatalog(_))
        ),
        "親の Full Track Name 4,097 バイトを拒否すること"
    );
}

/// packaging 値 `catalog` を受理すること
///
/// draft-ietf-moq-msf-02 §5.2.4 (Packaging) の Table 5 は、別のカタログトラックを参照する
/// カタログ用に `catalog` を追加した。media 向けの必須フィールドは要求しない。
#[test]
fn catalog_packaging_accepted() {
    let json =
        br#"{"version":"draft-02","tracks":[{"name":"catalog","packaging":"catalog","isLive":true}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full が期待された");
    };
    assert_eq!(cat.tracks[0].packaging, MsfPackaging::Catalog);
}

/// packagingMetadata の構文と accessor を固定すること
///
/// draft-ietf-moq-msf-02 §5.2.5 (Packaging metadata): `;` 区切りの `key=value` で、
/// 同じ key は最初の出現を使い、未知の key は無視する。
#[test]
fn packaging_metadata_accessor_uses_first_occurrence() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"packagingMetadata":"streamMapping=object;unknown=x;streamMapping=first"}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(catalog) = doc else {
        panic!("Full が期待された");
    };
    let track = &catalog.tracks[0];
    assert_eq!(
        track.packaging_metadata.as_deref(),
        Some("streamMapping=object;unknown=x;streamMapping=first"),
        "wire の値をそのまま保持すること"
    );
    assert_eq!(
        track.packaging_metadata_value("streamMapping"),
        Some("object"),
        "同じ key は最初の出現を使うこと"
    );
    assert_eq!(
        track.packaging_metadata_value("unknown"),
        Some("x"),
        "未知の key も accessor では取得できること"
    );
    assert_eq!(
        track.packaging_metadata_value("missing"),
        None,
        "存在しない key は None になること"
    );
}

/// 不正な packagingMetadata を decode で拒否すること
#[test]
fn invalid_packaging_metadata_rejected() {
    for metadata in [
        "", "a=b;;c=d", "key", "=value", "key=", "a b=c", "a=b c", "a=b=c", "a=b;c",
    ] {
        let json = format!(
            r#"{{"version":"draft-02","tracks":[{{"name":"t","packaging":"loc","isLive":true,"packagingMetadata":"{metadata}"}}]}}"#
        );
        assert!(
            matches!(
                MsfCatalogDocument::decode(json.as_bytes()),
                Err(MessageError::InvalidCatalog(_))
            ),
            "不正な packagingMetadata {metadata:?} を拒否すること"
        );
    }

    // clone でも同じ構文検証を行う
    let json = br#"{"generatedAt":1,"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","packagingMetadata":"a=b;;c=d"}]}]}"#;
    assert!(
        matches!(
            MsfCatalogDocument::decode(json),
            Err(MessageError::InvalidCatalog(_))
        ),
        "clone の不正な packagingMetadata を拒否すること"
    );
}

/// depends の track name / renderGroup 参照が解決されること
#[test]
fn depends_resolution_accepts_track_name_and_render_group() {
    let json = br#"{"version":"draft-02","tracks":[
        {"name":"video","packaging":"loc","isLive":true,"renderGroup":1},
        {"name":"audio","packaging":"loc","isLive":true},
        {"name":"timeline","packaging":"mediatimeline","isLive":true,"depends":["audio",1],"mimeType":"application/json"}
    ]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("依存先が実在するため受理されること");
    let MsfCatalogDocument::Full(catalog) = doc else {
        panic!("Full が期待された");
    };
    assert_eq!(
        catalog.tracks[2].depends,
        vec![
            MsfDependency::TrackName("audio".to_string()),
            MsfDependency::RenderGroup(1)
        ]
    );
}

/// 存在しない track name / renderGroup への依存が拒否されること
#[test]
fn depends_resolution_rejects_unresolved_references() {
    for depends in [r#"["missing"]"#, "[7]"] {
        let json = format!(
            r#"{{"version":"draft-02","tracks":[{{"name":"x","packaging":"loc","isLive":true,"depends":{depends}}}]}}"#
        );
        assert!(
            matches!(
                MsfCatalogDocument::decode(json.as_bytes()),
                Err(MessageError::InvalidCatalog(_))
            ),
            "未解決の depends {depends} を拒否すること"
        );
    }
}

/// 自己参照と同名 track への依存が拒否されること
#[test]
fn depends_resolution_rejects_self_and_ambiguous_names() {
    let self_ref = br#"{"version":"draft-02","tracks":[{"name":"x","packaging":"loc","isLive":true,"depends":["x"]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(self_ref),
        Err(MessageError::InvalidCatalog(_))
    ));

    // namespace が違えば track name の一意性は満たすが、depends の String は 2 件に一致する
    let ambiguous = br#"{"version":"draft-02","tracks":[
        {"name":"dup","namespace":"a","packaging":"loc","isLive":true},
        {"name":"dup","namespace":"b","packaging":"loc","isLive":true},
        {"name":"x","packaging":"loc","isLive":true,"depends":["dup"]}
    ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(ambiguous),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// 複数の track が同じ renderGroup を共有していても 1 つのグループとして解決されること
#[test]
fn depends_resolution_accepts_shared_render_group() {
    let json = br#"{"version":"draft-02","tracks":[
        {"name":"a","packaging":"loc","isLive":true,"renderGroup":5},
        {"name":"b","packaging":"loc","isLive":true,"renderGroup":5},
        {"name":"c","packaging":"loc","isLive":true,"depends":[5]}
    ]}"#;
    assert!(
        MsfCatalogDocument::decode(json).is_ok(),
        "同じ renderGroup を共有する track への依存を受理すること"
    );
}

/// tracks と publishTracks が相互に依存できること
#[test]
fn depends_resolution_allows_publish_tracks_cross_reference() {
    let json = br#"{"version":"draft-02","tracks":[
        {"name":"normal","packaging":"loc","isLive":true,"depends":["published"]}
    ],"publishTracks":[
        {"name":"published","packaging":"loc","isLive":true,"depends":["normal"]}
    ]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("相互参照を受理すること");
    let encoded = doc.encode().expect("encode できること");
    assert_eq!(
        MsfCatalogDocument::decode(&encoded).expect("roundtrip できること"),
        doc
    );
}

/// media timeline が renderGroup の全 track を個別名で列挙する場合に拒否されること
#[test]
fn media_timeline_all_tracks_named_without_render_group_rejected() {
    let json = br#"{"version":"draft-02","tracks":[
        {"name":"v1","packaging":"loc","isLive":true,"renderGroup":1},
        {"name":"v2","packaging":"loc","isLive":true,"renderGroup":1},
        {"name":"timeline","packaging":"mediatimeline","isLive":true,"depends":["v1","v2"]}
    ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// media timeline が renderGroup と所属 track の個別名を併記する場合に拒否されること
#[test]
fn media_timeline_render_group_and_member_name_rejected() {
    let json = br#"{"version":"draft-02","tracks":[
        {"name":"v1","packaging":"loc","isLive":true,"renderGroup":1},
        {"name":"v2","packaging":"loc","isLive":true,"renderGroup":1},
        {"name":"timeline","packaging":"mediatimeline","isLive":true,"depends":[1,"v1"]}
    ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// mimeType を持たない media timeline が受理されること
#[test]
fn media_timeline_without_mime_type_accepted() {
    let json = br#"{"version":"draft-02","tracks":[
        {"name":"video","packaging":"loc","isLive":true},
        {"name":"timeline","packaging":"mediatimeline","isLive":true,"depends":["video"]}
    ]}"#;
    assert!(MsfCatalogDocument::decode(json).is_ok());
}

/// moq-secure-objects で cipherSuite が無い場合は decode で拒否されること
#[test]
fn secure_objects_without_cipher_suite_rejected() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"encryptionScheme":"moq-secure-objects","keyId":"key-1","trackBaseKey":"AAEC"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// accessibleWindowTime の保持と altGroup 内の一致を検証すること
#[test]
fn accessible_window_time_alt_group_rules() {
    // 同じ値なら受理
    let same = br#"{"version":"draft-02","tracks":[
        {"name":"a","packaging":"loc","isLive":true,"altGroup":1,"accessibleWindowTime":60000},
        {"name":"b","packaging":"loc","isLive":false,"altGroup":1,"accessibleWindowTime":60000}
    ]}"#;
    let doc = MsfCatalogDocument::decode(same).expect("同じ altGroup の同じ値は受理されること");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full が期待された");
    };
    assert_eq!(cat.tracks[0].accessible_window_time, Some(60000));

    // 値が異なる場合は isLive=false でも拒否
    let different = br#"{"version":"draft-02","tracks":[
        {"name":"a","packaging":"loc","isLive":false,"altGroup":1,"accessibleWindowTime":60000},
        {"name":"b","packaging":"loc","isLive":false,"altGroup":1,"accessibleWindowTime":120000}
    ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(different),
        Err(MessageError::InvalidCatalog(_))
    ));

    // 一部だけが持つ場合は拒否
    let partial = br#"{"version":"draft-02","tracks":[
        {"name":"a","packaging":"loc","isLive":true,"altGroup":1,"accessibleWindowTime":60000},
        {"name":"b","packaging":"loc","isLive":true,"altGroup":1}
    ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(partial),
        Err(MessageError::InvalidCatalog(_))
    ));

    // 全 track が持たない場合は受理
    let none = br#"{"version":"draft-02","tracks":[
        {"name":"a","packaging":"loc","isLive":true,"altGroup":1},
        {"name":"b","packaging":"loc","isLive":true,"altGroup":1}
    ]}"#;
    assert!(MsfCatalogDocument::decode(none).is_ok());
}

/// publishTracks の altGroup にも一致規則が適用されること
#[test]
fn accessible_window_time_publish_tracks_alt_group_rules() {
    let json = br#"{"version":"draft-02","tracks":[],"publishTracks":[
        {"name":"a","packaging":"loc","isLive":true,"altGroup":1,"accessibleWindowTime":60000},
        {"name":"b","packaging":"loc","isLive":true,"altGroup":1,"accessibleWindowTime":120000}
    ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// track-property / object-property の initData を受理すること
#[test]
fn init_data_property_types_accepted() {
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"initRef":"init1"}],"initDataList":[
        {"id":"init1","type":"track-property","data":"0x0D"},
        {"id":"init2","type":"object-property","data":"0x78"}
    ]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("property type の initData を受理すること");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full が期待された");
    };
    assert_eq!(cat.init_data_list[0].kind, MsfInitDataKind::TrackProperty);
    assert_eq!(cat.init_data_list[1].kind, MsfInitDataKind::ObjectProperty);
}

/// property type の hex notation が不正なら decode で拒否すること
#[test]
fn init_data_property_type_invalid_hex_rejected() {
    for data in [
        "0D",
        "0x",
        "0xZZ",
        "0x4000000000000000",
        "0x10000000000000000",
    ] {
        let json = format!(
            r#"{{"version":"draft-02","tracks":[],"initDataList":[{{"id":"i","type":"track-property","data":"{data}"}}]}}"#
        );
        assert!(
            matches!(
                MsfCatalogDocument::decode(json.as_bytes()),
                Err(MessageError::InvalidCatalog(_))
            ),
            "不正な hex {data:?} を拒否すること"
        );
    }

    // 2^62 - 1 は受理する
    let json = br#"{"version":"draft-02","tracks":[],"initDataList":[{"id":"i","type":"track-property","data":"0x3FFFFFFFFFFFFFFF"}]}"#;
    assert!(MsfCatalogDocument::decode(json).is_ok());
}

/// update 操作を decode できること
#[test]
fn delta_update_decodes() {
    let json = br#"{"generatedAt":1,"deltaUpdate":[{"op":"update","tracks":[{"name":"v","parentName":"v","label":"new","isLive":true}]}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("update 操作を decode できること");
    let MsfCatalogDocument::Delta(delta) = doc else {
        panic!("Delta が期待された");
    };
    let MsfDeltaOperation::Update { tracks } = &delta.operations[0] else {
        panic!("Update が期待された");
    };
    assert_eq!(tracks[0].track.name, "v");
    assert_eq!(tracks[0].track.parent_name, "v");
    assert_eq!(tracks[0].track.label.as_deref(), Some("new"));
}

/// update の name が parentName と一致しない場合は拒否されること
#[test]
fn delta_update_name_mismatch_rejected() {
    let json = br#"{"generatedAt":1,"deltaUpdate":[{"op":"update","tracks":[{"name":"x","parentName":"v","label":"new"}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// update の namespace が parentNamespace と一致しない場合は拒否されること
#[test]
fn delta_update_namespace_mismatch_rejected() {
    let json = br#"{"generatedAt":1,"deltaUpdate":[{"op":"update","tracks":[{"name":"v","parentName":"v","parentNamespace":"a","namespace":"b","label":"new"}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// generatedAt を持たない delta update が拒否されること
#[test]
fn delta_missing_generated_at_rejected() {
    let json = br#"{"deltaUpdate":[{"op":"remove","tracks":[{"name":"x"}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// delta update が他の Root Catalog フィールドを持てないこと
#[test]
fn delta_forbidden_root_fields_rejected() {
    for field in [
        r#""isComplete":true"#,
        r#""publishTracks":[]"#,
        r#""initDataList":[]"#,
        r#""version":"draft-02""#,
        r#""tracks":[]"#,
    ] {
        let json = format!(
            r#"{{"generatedAt":1,"deltaUpdate":[{{"op":"remove","tracks":[{{"name":"x"}}]}}],{field}}}"#
        );
        assert!(
            matches!(
                MsfCatalogDocument::decode(json.as_bytes()),
                Err(MessageError::InvalidCatalog(_))
            ),
            "delta update の {field} を拒否すること"
        );
    }
}

/// delta update の未知フィールドは無視されること
#[test]
fn delta_unknown_root_field_accepted() {
    let json = br#"{"generatedAt":1,"deltaUpdate":[{"op":"remove","tracks":[{"name":"x"}]}],"custom":true}"#;
    assert!(MsfCatalogDocument::decode(json).is_ok());
}

/// draft-01 の完全カタログは受理しないこと
#[test]
fn full_catalog_draft_01_rejected() {
    let json = br#"{"version":"draft-01","tracks":[]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalogVersion(version)) if version == "draft-01"
    ));
}

/// draft-03 の完全カタログは受理しないこと
#[test]
fn full_catalog_draft_03_rejected() {
    let json = br#"{"version":"draft-03","tracks":[]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalogVersion(version)) if version == "draft-03"
    ));
}

/// version を持たない delta update は version ゲートの対象外であること
#[test]
fn delta_without_version_is_not_version_gated() {
    let json = br#"{"generatedAt":1,"deltaUpdate":[{"op":"remove","tracks":[{"name":"x"}]}]}"#;
    let doc =
        MsfCatalogDocument::decode(json).expect("delta は version を持たなくても受理されること");
    assert!(matches!(doc, MsfCatalogDocument::Delta(_)));
}
