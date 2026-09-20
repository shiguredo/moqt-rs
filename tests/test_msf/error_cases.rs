use super::*;

#[test]
fn missing_version() {
    assert!(matches!(
        MsfCatalogDocument::decode(b"{\"tracks\":[]}"),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn version_zero() {
    assert!(matches!(
        MsfCatalogDocument::decode(b"{\"version\":\"0\",\"tracks\":[]}"),
        Err(MessageError::InvalidCatalogVersion(v)) if v == "0"
    ));
}

#[test]
fn version_99() {
    assert!(matches!(
        MsfCatalogDocument::decode(b"{\"version\":\"99\",\"tracks\":[]}"),
        Err(MessageError::InvalidCatalogVersion(v)) if v == "99"
    ));
}

#[test]
fn version_not_string() {
    // draft-ietf-moq-msf-01 §5.1.1 (MSF version): version は JSON Type: String
    assert!(matches!(
        MsfCatalogDocument::decode(b"{\"version\":1,\"tracks\":[]}"),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn missing_tracks() {
    assert!(matches!(
        MsfCatalogDocument::decode(b"{\"version\":\"draft-01\"}"),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn track_missing_name() {
    let json = br#"{"version":"draft-01","tracks":[{"packaging":"loc"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn track_missing_packaging() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","isLive":true}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn track_missing_is_live() {
    // draft-ietf-moq-msf-01 §5.2.7 (Is Live): isLive は必須
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn not_json_object() {
    assert!(matches!(
        MsfCatalogDocument::decode(b"[1,2,3]"),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn empty_input() {
    assert!(matches!(
        MsfCatalogDocument::decode(b""),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn trailing_data() {
    assert!(matches!(
        MsfCatalogDocument::decode(b"{\"version\":1,\"tracks\":[]}garbage"),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn media_timeline_not_array() {
    assert!(matches!(
        MsfMediaTimeline::decode(b"{}"),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn media_timeline_record_wrong_length() {
    // レコードが 2 要素 (3 要素必要)
    assert!(matches!(
        MsfMediaTimeline::decode(b"[[0,[0,0]]]"),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn media_timeline_location_wrong_length() {
    // ロケーションが 1 要素 (2 要素必要)
    assert!(matches!(
        MsfMediaTimeline::decode(b"[[0,[0],0]]"),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn event_timeline_missing_index() {
    assert!(matches!(
        MsfEventTimeline::decode(br#"[{"data":1}]"#),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn event_timeline_missing_data() {
    assert!(matches!(
        MsfEventTimeline::decode(br#"[{"t":1000}]"#),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn depends_entry_not_string() {
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"depends":[1,2]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn render_group_different_target_latency_rejected() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency): 同一 renderGroup 内の isLive=true トラックが宣言した targetLatency は一致必須 (省略は比較対象外)
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":500},
            {"name":"b","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":1000}
        ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn alt_group_different_target_latency_rejected() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency): 同一 altGroup 内の isLive=true トラックが宣言した targetLatency は一致必須 (省略は比較対象外)
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"altGroup":2,"targetLatency":500},
            {"name":"b","packaging":"loc","isLive":true,"altGroup":2,"targetLatency":1000}
        ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn render_group_same_target_latency_accepted() {
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":500},
            {"name":"b","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":500}
        ]}"#;
    assert!(MsfCatalogDocument::decode(json).is_ok());
}

#[test]
fn render_group_target_latency_omitted_accepted() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency): フィールドが無く isLive=true なら
    // player が遅延を選んでよい (MAY)。省略は宣言値との不一致として拒否しない
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":500},
            {"name":"b","packaging":"loc","isLive":true,"renderGroup":1}
        ]}"#;
    MsfCatalogDocument::decode(json).expect("省略された targetLatency は拒否されないこと");
}

#[test]
fn alt_group_target_latency_omitted_accepted() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency): altGroup でも省略は不一致としない
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"altGroup":2,"targetLatency":500},
            {"name":"b","packaging":"loc","isLive":true,"altGroup":2}
        ]}"#;
    MsfCatalogDocument::decode(json)
        .expect("altGroup でも省略された targetLatency は拒否されないこと");
}

#[test]
fn render_group_target_latency_and_buffers_split_accepted() {
    // draft-ietf-moq-msf-01 §5.2.8 / §5.2.9: 省略を比較対象外とするため、同一 group で
    // 一方が targetLatency のみ、他方が buffers のみを宣言する構成も受理される (本実装の解釈)
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":500},
            {"name":"b","packaging":"loc","isLive":true,"renderGroup":1,"buffers":{"target":1000}}
        ]}"#;
    MsfCatalogDocument::decode(json)
        .expect("group 内で targetLatency と buffers を使い分けても拒否されないこと");
}

#[test]
fn render_group_omitted_between_different_target_latency_rejected() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency): 省略トラックを挟んでも
    // 宣言された 2 値の不一致は検出すること
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":500},
            {"name":"b","packaging":"loc","isLive":true,"renderGroup":1},
            {"name":"c","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":1000}
        ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

// ─── lang BCP 47 軽量バリデーション ───────────────────────────────────────────

#[test]
fn lang_invalid_empty() {
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":""}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// primary language subtag が ASCII 英字でない場合 (数字) は拒否されることを確認する
#[test]
fn lang_invalid_numeric_primary() {
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"12"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// primary language subtag が 1 文字の場合は拒否されることを確認する (irregular 固定リストの `i-*` を除く)
///
/// irregular 固定リストの `i-*` だけが例外であり、`i-` 接頭辞一般が許されるわけではない
#[test]
fn lang_invalid_single_char() {
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"e"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn lang_invalid_special_chars() {
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"eng-!!"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn lang_invalid_leading_dash() {
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"-en"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn lang_invalid_trailing_dash() {
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"en-"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// RFC 5646 §2.1 (Syntax): language = 4ALPHA も 5*8ALPHA も正当な primary のため受理される
#[test]
fn lang_primary_four_to_eight_letters_accepted() {
    for lang in ["abcd", "abcde", "abcdefgh"] {
        let json = format!(
            r#"{{"version":"draft-01","tracks":[{{"name":"a","packaging":"loc","isLive":true,"lang":"{lang}"}}]}}"#
        );
        assert!(
            MsfCatalogDocument::decode(json.as_bytes()).is_ok(),
            "primary '{lang}' は受理されること"
        );
    }
}

/// RFC 5646 §2.1 (Syntax): primary が 9 文字以上なら拒否されること
#[test]
fn lang_invalid_primary_too_long() {
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"abcdefghi"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// RFC 5646 §2.1 (Syntax): privateuse tag ("x" + 1 個以上のサブタグ) は受理されること
#[test]
fn lang_privateuse_accepted() {
    for lang in ["x-foo", "x-foo-bar", "x-1"] {
        let json = format!(
            r#"{{"version":"draft-01","tracks":[{{"name":"a","packaging":"loc","isLive":true,"lang":"{lang}"}}]}}"#
        );
        assert!(
            MsfCatalogDocument::decode(json.as_bytes()).is_ok(),
            "privateuse '{lang}' は受理されること"
        );
    }
}

/// RFC 5646 §2.1 (Syntax): privateuse は最低 1 個のサブタグを必要とするため "x" 単独は拒否される
#[test]
fn lang_privateuse_without_subtag_rejected() {
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"x"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
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
            r#"{{"version":"draft-01","tracks":[{{"name":"a","packaging":"loc","isLive":true,"lang":"{lang}"}}]}}"#
        );
        assert!(
            MsfCatalogDocument::decode(json.as_bytes()).is_ok(),
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
            r#"{{"version":"draft-01","tracks":[{{"name":"a","packaging":"loc","isLive":true,"lang":"{lang}"}}]}}"#
        );
        assert!(
            MsfCatalogDocument::decode(json.as_bytes()).is_ok(),
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
            r#"{{"version":"draft-01","tracks":[{{"name":"a","packaging":"loc","isLive":true,"lang":"{lang}"}}]}}"#
        );
        assert!(
            MsfCatalogDocument::decode(json.as_bytes()).is_ok(),
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
            r#"{{"version":"draft-01","tracks":[{{"name":"a","packaging":"loc","isLive":true,"lang":"{lang}"}}]}}"#
        );
        assert!(
            matches!(
                MsfCatalogDocument::decode(json.as_bytes()),
                Err(MessageError::InvalidCatalog(reason))
                    if reason.contains("primary language subtag")
            ),
            "irregular 固定リストに無い '{lang}' は primary language subtag の形式検査で拒否されること"
        );
    }
}

/// privateuse の `x` は小文字のみを受理すること
///
/// 大文字小文字を無視するのは irregular 固定リストとの一致判定だけに限定する。
/// `X-FOO` は privateuse として扱われず、primary language subtag が 1 文字のため
/// その形式検査で拒否される
#[test]
fn lang_uppercase_privateuse_rejected() {
    for lang in ["X-FOO", "X-foo"] {
        let json = format!(
            r#"{{"version":"draft-01","tracks":[{{"name":"a","packaging":"loc","isLive":true,"lang":"{lang}"}}]}}"#
        );
        assert!(
            matches!(
                MsfCatalogDocument::decode(json.as_bytes()),
                Err(MessageError::InvalidCatalog(reason))
                    if reason.contains("primary language subtag")
            ),
            "大文字 X の privateuse '{lang}' は primary language subtag の形式検査で拒否されること"
        );
    }
    // 小文字 `x` は従来どおり受理される (対になる確認)
    let json =
        br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"x-foo"}]}"#;
    assert!(
        MsfCatalogDocument::decode(json).is_ok(),
        "小文字 x の privateuse は受理されること"
    );
}

/// delta add / clone 経路でも irregular タグが受理されること
#[test]
fn lang_irregular_in_delta_add_and_clone_accepted() {
    let add = br#"{"deltaUpdate":[{"op":"add","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"i-klingon"}]}]}"#;
    assert!(
        MsfCatalogDocument::decode(add).is_ok(),
        "delta add の irregular タグは受理されること"
    );
    let clone = br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"a","parentName":"parent","lang":"i-klingon"}]}]}"#;
    assert!(
        MsfCatalogDocument::decode(clone).is_ok(),
        "delta clone の irregular タグは受理されること"
    );
}

/// add operation 経路でも lang バリデーションが発火することを確認する
#[test]
fn lang_invalid_in_add_tracks() {
    let json = br#"{"deltaUpdate":[{"op":"add","tracks":[{"name":"a","packaging":"loc","isLive":true,"lang":"1"}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// clone operation 内の不正な lang も拒否されることを確認する
#[test]
fn lang_invalid_in_clone_track() {
    let json =
        br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"a","parentName":"parent","lang":"1"}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

// ─── targetLatency / buffers の相互制約 ───────────────────────────────────────

#[test]
fn target_latency_and_buffers_together_rejected_when_is_live_true() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency) / §5.2.9 (Buffers):
    // isLive=true の同一 track に targetLatency と buffers は共存できない
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"targetLatency":500,"buffers":{"target":1000}}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(reason))
        if reason == "targetLatency and buffers MUST NOT be present together"
    ));
}

#[test]
fn target_latency_and_buffers_together_rejected_when_is_live_false() {
    // draft-ietf-moq-msf-01 §5.2.8 / §5.2.9: isLive=false でも共存は reject されること
    // (本実装では presence 違反を優先する選択とする)
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":false,"targetLatency":500,"buffers":{"target":1000}}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(reason))
        if reason == "targetLatency and buffers MUST NOT be present together"
    ));
}

#[test]
fn target_latency_or_buffers_alone_ignored_when_is_live_false() {
    // draft-ietf-moq-msf-01 §5.2.8 / §5.2.9: 単独存在時は isLive=false で無視されること
    // (両単独とも確認する)
    for (name, json) in [
        (
            "targetLatency 単独",
            br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":false,"targetLatency":500}]}"#.as_slice(),
        ),
        (
            "buffers 単独",
            br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":false,"buffers":{"target":1000}}]}"#.as_slice(),
        ),
    ] {
        let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
        let MsfCatalogDocument::Full(cat) = doc else {
            panic!("Full catalog がデコードされること");
        };
        assert_eq!(cat.tracks[0].target_latency, None, "{name} は無視されること");
        assert_eq!(cat.tracks[0].buffers, None, "{name} は無視されること");
    }
}

#[test]
fn clone_target_latency_or_buffers_alone_ignored_when_is_live_false() {
    // draft-ietf-moq-msf-01 §5.2.8 / §5.2.9: clone の単独存在時も isLive=false で無視されること
    // (両単独とも確認する)
    for (name, json, field) in [
        (
            "targetLatency 単独",
            br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","isLive":false,"targetLatency":500}]}]}"#.as_slice(),
            "targetLatency",
        ),
        (
            "buffers 単独",
            br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","isLive":false,"buffers":{"target":1000}}]}]}"#.as_slice(),
            "buffers",
        ),
    ] {
        let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
        let MsfCatalogDocument::Delta(delta) = doc else {
            panic!("Delta update がデコードされること");
        };
        let MsfDeltaOperation::Clone { tracks } = &delta.operations[0] else {
            panic!("Clone 操作がデコードされること");
        };
        // 存在する側のみ無視を確認する (存在しない側の断定は空振りのため行わない)
        match field {
            "targetLatency" => assert_eq!(tracks[0].target_latency, None, "{name} は無視されること"),
            _ => assert_eq!(tracks[0].buffers, None, "{name} は無視されること"),
        }
    }
}

#[test]
fn target_latency_and_buffers_together_rejected_in_clone() {
    // draft-ietf-moq-msf-01 §5.2.8 / §5.2.9: clone でも共存は reject されること
    // (isLive 省略時を含む。理由文字列まで固定する)
    for (name, json) in [
        (
            "isLive:false",
            br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","isLive":false,"targetLatency":500,"buffers":{"target":1000}}]}]}"#.as_slice(),
        ),
        (
            "isLive 省略",
            br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","targetLatency":500,"buffers":{"target":1000}}]}]}"#.as_slice(),
        ),
    ] {
        assert!(
            matches!(
                MsfCatalogDocument::decode(json),
                Err(MessageError::InvalidCatalog(reason))
                if reason == "targetLatency and buffers MUST NOT be present together"
            ),
            "{name} の共存は reject されること"
        );
    }
}

#[test]
fn render_group_different_buffers_rejected() {
    // draft-ietf-moq-msf-01 §5.2.9 (Buffers): 同一 renderGroup 内の isLive=true トラックが宣言した buffers は一致必須 (省略は比較対象外)
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"buffers":{"target":500}},
            {"name":"b","packaging":"loc","isLive":true,"renderGroup":1,"buffers":{"target":1000}}
        ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn render_group_same_buffers_accepted() {
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"buffers":{"target":500}},
            {"name":"b","packaging":"loc","isLive":true,"renderGroup":1,"buffers":{"target":500}}
        ]}"#;
    assert!(MsfCatalogDocument::decode(json).is_ok());
}

#[test]
fn render_group_buffers_omitted_accepted() {
    // draft-ietf-moq-msf-01 §5.2.9 (Buffers): フィールドが無く isLive=true なら
    // player がバッファを選んでよい (MAY)。省略は宣言値との不一致として拒否しない
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"buffers":{"target":500}},
            {"name":"b","packaging":"loc","isLive":true,"renderGroup":1}
        ]}"#;
    MsfCatalogDocument::decode(json).expect("省略された buffers は拒否されないこと");
}

#[test]
fn alt_group_buffers_omitted_accepted() {
    // draft-ietf-moq-msf-01 §5.2.9 (Buffers): altGroup でも省略は不一致としない
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"altGroup":2,"buffers":{"target":500}},
            {"name":"b","packaging":"loc","isLive":true,"altGroup":2}
        ]}"#;
    MsfCatalogDocument::decode(json).expect("altGroup でも省略された buffers は拒否されないこと");
}

#[test]
fn render_group_omitted_between_different_buffers_rejected() {
    // draft-ietf-moq-msf-01 §5.2.9 (Buffers): 省略トラックを挟んでも
    // 宣言された 2 値の不一致は検出すること
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"buffers":{"target":500}},
            {"name":"b","packaging":"loc","isLive":true,"renderGroup":1},
            {"name":"c","packaging":"loc","isLive":true,"renderGroup":1,"buffers":{"target":1000}}
        ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn render_group_mixed_live_ignored_for_buffers() {
    // isLive=false のトラックは group 検証の対象外。ただし b は buffers を宣言していないため、
    // 本テストだけでは isLive 除外と省略除外を区別できない。省略除外は
    // render_group_buffers_omitted_accepted、isLive 除外は
    // encode_full_group_buffers_is_live_false_ignored がそれぞれ固定する
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"buffers":{"target":500}},
            {"name":"b","packaging":"loc","isLive":false,"renderGroup":1}
        ]}"#;
    MsfCatalogDocument::decode(json).expect("isLive=false の省略は拒否されないこと");
}

#[test]
fn render_group_mixed_live_ignored_for_target_latency() {
    // isLive=false のトラックは group 検証の対象外。ただし b は targetLatency を宣言していないため、
    // 本テストだけでは isLive 除外と省略除外を区別できない。省略除外は
    // render_group_target_latency_omitted_accepted、isLive 除外は
    // encode_full_group_target_latency_is_live_false_ignored がそれぞれ固定する
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"renderGroup":1,"targetLatency":500},
            {"name":"b","packaging":"loc","isLive":false,"renderGroup":1}
        ]}"#;
    MsfCatalogDocument::decode(json).expect("isLive=false の省略は拒否されないこと");
}

// ─── initDataList ─────────────────────────────────────────────────────────────

#[test]
fn init_data_list_duplicate_id_rejected() {
    // draft-ietf-moq-msf-01 §5.1.7 (Initialization Data List): id は catalog 内で一意
    let json = br#"{"version":"draft-01","tracks":[],"initDataList":[
            {"id":"init1","type":"inline","data":"AAEC"},
            {"id":"init1","type":"inline","data":"AAED"}
        ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn init_data_list_unknown_type_rejected() {
    let json = br#"{"version":"draft-01","tracks":[],"initDataList":[{"id":"init1","type":"unknown","data":"AAEC"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn init_ref_missing_id_rejected() {
    // draft-ietf-moq-msf-01 §5.2.13 (Initialization reference): initRef は
    // initDataList の id を指すため、一致する id が無ければ reject される
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"initRef":"missing"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn init_ref_matching_id_accepted() {
    // initRef が initDataList の id と一致すれば受理される
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"initRef":"init1"}],"initDataList":[{"id":"init1","type":"inline","data":"AAEC"}]}"#;
    assert!(MsfCatalogDocument::decode(json).is_ok());
}

// ─── delta update 新構造 ──────────────────────────────────────────────────────

#[test]
fn delta_update_empty_array_rejected() {
    // draft-ietf-moq-msf-01 §5.3 (Delta updates): 少なくとも 1 つの操作が必要
    let json = br#"{"deltaUpdate":[]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn delta_update_unknown_op_rejected() {
    let json = br#"{"deltaUpdate":[{"op":"unknown","tracks":[]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn delta_update_contains_version_rejected() {
    // draft-ietf-moq-msf-01 §5.3 (Delta updates): delta update に version は含めない
    let json = br#"{"deltaUpdate":[{"op":"add","tracks":[]}],"version":"draft-01"}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn delta_update_contains_tracks_rejected() {
    // draft-ietf-moq-msf-01 §5.3 (Delta updates): delta update に tracks は含めない
    let json = br#"{"deltaUpdate":[{"op":"add","tracks":[]}],"tracks":[]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn delta_operation_missing_op_rejected() {
    let json = br#"{"deltaUpdate":[{"tracks":[]}]}}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn delta_operation_missing_tracks_rejected() {
    let json = br#"{"deltaUpdate":[{"op":"add"}]}}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn init_data_list_missing_id_rejected() {
    let json =
        br#"{"version":"draft-01","tracks":[],"initDataList":[{"type":"inline","data":"AAEC"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn init_data_list_missing_type_rejected() {
    let json =
        br#"{"version":"draft-01","tracks":[],"initDataList":[{"id":"init1","data":"AAEC"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn init_data_list_missing_data_rejected() {
    let json =
        br#"{"version":"draft-01","tracks":[],"initDataList":[{"id":"init1","type":"inline"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn buffers_non_number_value_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"buffers":{"target":"fast"}}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn alt_group_different_buffers_rejected() {
    // draft-ietf-moq-msf-01 §5.2.9 (Buffers): 同一 altGroup 内の isLive=true トラックが宣言した buffers は一致必須 (省略は比較対象外)
    let json = br#"{"version":"draft-01","tracks":[
            {"name":"a","packaging":"loc","isLive":true,"altGroup":1,"buffers":{"target":500}},
            {"name":"b","packaging":"loc","isLive":true,"altGroup":1,"buffers":{"target":1000}}
        ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn clone_track_target_latency_and_buffers_together_rejected() {
    // draft-ietf-moq-msf-01 §5.2.8 / §5.2.9: clone の isLive:true 共存も reject されること
    let json = br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","isLive":true,"targetLatency":500,"buffers":{"target":1000}}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(reason))
        if reason == "targetLatency and buffers MUST NOT be present together"
    ));
}

#[test]
fn add_target_latency_and_buffers_together_rejected() {
    // draft-ietf-moq-msf-01 §5.2.8 / §5.2.9: delta add 経路の共存も reject されること
    let json = br#"{"deltaUpdate":[{"op":"add","tracks":[{"name":"t","packaging":"loc","isLive":false,"targetLatency":500,"buffers":{"target":1000}}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(reason))
        if reason == "targetLatency and buffers MUST NOT be present together"
    ));
}

#[test]
fn clone_omitted_kept() {
    // clone の isLive 省略時は単独値を保持すること
    let json = br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","targetLatency":500}]}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Delta(delta) = doc else {
        panic!("Delta update がデコードされること");
    };
    let MsfDeltaOperation::Clone { tracks } = &delta.operations[0] else {
        panic!("Clone 操作がデコードされること");
    };
    assert_eq!(tracks[0].target_latency, Some(500));
}

#[test]
fn clone_track_track_duration_when_is_live_true_rejected() {
    let json = br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","isLive":true,"trackDuration":1000}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn clone_track_event_type_with_non_eventtimeline_rejected() {
    let json = br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","packaging":"loc","eventType":"x"}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn event_timeline_missing_event_type_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"eventtimeline","isLive":true}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn media_timeline_missing_depends_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"mediatimeline","isLive":true,"mimeType":"application/json"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn media_timeline_wrong_mime_type_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"mediatimeline","isLive":true,"depends":["video"],"mimeType":"text/plain"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn event_type_with_non_eventtimeline_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"eventType":"x"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn eventtimeline_missing_depends_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"eventtimeline","isLive":true,"eventType":"cue","mimeType":"application/json"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn eventtimeline_wrong_mime_type_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"eventtimeline","isLive":true,"eventType":"cue","depends":["video"],"mimeType":"text/plain"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

// ─── codec / role に応じた必須フィールド ──────────────────────────────────────

/// role="video" は codec 必須 (draft-ietf-moq-msf-01 §5.2.18 (Codec))
#[test]
fn video_role_missing_codec_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"v","packaging":"loc","isLive":true,"role":"video","bitrate":1000}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) = MsfCatalogDocument::decode(json) else {
        panic!("role 由来の codec 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("codec") && reason.contains("role 'video'"),
        "role 由来の codec 要求を述べること: {reason}"
    );
}

/// role="video" は bitrate 必須 (draft-ietf-moq-msf-01 §5.2.22 (Maximum Bitrate))
#[test]
fn video_role_missing_bitrate_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"v","packaging":"loc","isLive":true,"role":"video","codec":"av01"}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) = MsfCatalogDocument::decode(json) else {
        panic!("role 由来の bitrate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("bitrate") && reason.contains("role 'video'"),
        "role 由来の bitrate 要求を述べること: {reason}"
    );
}

/// role="audiodescription" は audio として扱い samplerate / channelConfig を要求する
///
/// draft-ietf-moq-msf-01 §5.2.6 Table 4 は `audiodescription` を
/// "An audio description for visually impaired users" と定める。
#[test]
fn audiodescription_role_requires_audio_fields_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"role":"audiodescription","codec":"opus","bitrate":32000,"channelConfig":"2"}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) = MsfCatalogDocument::decode(json) else {
        panic!("role 由来の samplerate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("samplerate") && reason.contains("role 'audiodescription'"),
        "role 由来の samplerate 要求を述べること: {reason}"
    );
    let json = br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"role":"audiodescription","codec":"opus","bitrate":32000,"samplerate":48000}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) = MsfCatalogDocument::decode(json) else {
        panic!("role 由来の channelConfig 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("channelConfig") && reason.contains("role 'audiodescription'"),
        "role 由来の channelConfig 要求を述べること: {reason}"
    );
}

/// role="signlanguage" は visual track のため video として扱い codec / bitrate を要求する
///
/// draft-ietf-moq-msf-01 §5.2.6 Table 4 は `signlanguage` を
/// "A visual track for hearing impaired users." と定める。codec には登録外の文字列を使い、
/// codec 由来ではなく role 由来の要求であることを固定する。
#[test]
fn signlanguage_role_is_treated_as_video() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"v","packaging":"loc","isLive":true,"role":"signlanguage"}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) = MsfCatalogDocument::decode(json) else {
        panic!("role=signlanguage は codec を要求すること");
    };
    assert!(
        reason.contains("codec") && reason.contains("role 'signlanguage'"),
        "role 由来の codec 要求を述べること: {reason}"
    );
    let json = br#"{"version":"draft-01","tracks":[{"name":"v","packaging":"loc","isLive":true,"role":"signlanguage","codec":"unregistered"}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) = MsfCatalogDocument::decode(json) else {
        panic!("role=signlanguage は bitrate を要求すること");
    };
    assert!(
        reason.contains("bitrate") && reason.contains("role 'signlanguage'"),
        "role 由来の bitrate 要求を述べること: {reason}"
    );
    let json = br#"{"version":"draft-01","tracks":[{"name":"v","packaging":"loc","isLive":true,"role":"signlanguage","codec":"unregistered","bitrate":1000}]}"#;
    assert!(
        MsfCatalogDocument::decode(json).is_ok(),
        "role=signlanguage でも必須フィールドを満たせば受理されること"
    );
}

/// role="audio" は samplerate / channelConfig 必須 (§5.2.28 / §5.2.29)
#[test]
fn audio_role_missing_samplerate_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"role":"audio","codec":"opus","bitrate":32000,"channelConfig":"2"}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) = MsfCatalogDocument::decode(json) else {
        panic!("role 由来の samplerate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("samplerate") && reason.contains("role 'audio'"),
        "role 由来の samplerate 要求を述べること: {reason}"
    );
    let json = br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"role":"audio","codec":"opus","bitrate":32000,"samplerate":48000}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) = MsfCatalogDocument::decode(json) else {
        panic!("role 由来の channelConfig 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("channelConfig") && reason.contains("role 'audio'"),
        "role 由来の channelConfig 要求を述べること: {reason}"
    );
}

/// role="video" / "audio" が必須フィールドを満たせば受理される
#[test]
fn media_role_with_required_fields_accepted() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"v","packaging":"loc","isLive":true,"role":"video","codec":"av01","bitrate":1000},{"name":"a","packaging":"loc","isLive":true,"role":"audio","codec":"opus","bitrate":32000,"samplerate":48000,"channelConfig":"2"}]}"#;
    assert!(MsfCatalogDocument::decode(json).is_ok());
}

/// codec も role も無いトラックは種別を判定できないため codec / bitrate を要求しない
///
/// draft-ietf-moq-msf-01 §5.2.18 (Codec) の
/// "It is not required for raw data tracks or event streams." と整合する。
#[test]
fn missing_role_does_not_require_media_fields() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true}]}"#;
    assert!(MsfCatalogDocument::decode(json).is_ok());
}

/// encode 時にも role に応じた必須フィールドが検証される
#[test]
fn media_role_missing_fields_rejected_on_encode() {
    let mut track = MsfTrack::new("v".to_string(), MsfPackaging::Loc, true);
    track.role = Some("video".to_string());
    track.codec = Some("av01".to_string());
    // bitrate を欠いたまま encode するとエラーになる
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        tracks: vec![track],
        ..MsfCatalog::new()
    });
    let Err(MessageError::InvalidCatalog(reason)) = doc.encode() else {
        panic!("role 由来の bitrate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("bitrate") && reason.contains("role 'video'"),
        "role 由来の bitrate 要求を述べること: {reason}"
    );
}

/// role 省略 + codec `opus` は samplerate 必須 (§5.2.28 (Audio sample rate))
#[test]
fn codec_audio_missing_samplerate_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"codec":"opus","bitrate":32000,"channelConfig":"2"}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) = MsfCatalogDocument::decode(json) else {
        panic!("codec 由来の samplerate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("samplerate") && reason.contains("codec 'opus'"),
        "codec 由来の samplerate 要求を述べること: {reason}"
    );
}

/// role 省略 + codec `opus` は channelConfig 必須 (§5.2.29 (Channel configuration))
#[test]
fn codec_audio_missing_channel_config_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"codec":"opus","bitrate":32000,"samplerate":48000}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) = MsfCatalogDocument::decode(json) else {
        panic!("codec 由来の channelConfig 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("channelConfig") && reason.contains("codec 'opus'"),
        "codec 由来の channelConfig 要求を述べること: {reason}"
    );
}

/// role 省略 + codec `opus` は bitrate 必須 (§5.2.22 (Maximum Bitrate))
#[test]
fn codec_audio_missing_bitrate_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"codec":"opus","samplerate":48000,"channelConfig":"2"}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) = MsfCatalogDocument::decode(json) else {
        panic!("codec 由来の bitrate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("bitrate") && reason.contains("codec 'opus'"),
        "codec 由来の bitrate 要求を述べること: {reason}"
    );
}

/// role 省略 + codec `av01.0.08M.08` は bitrate 必須 (§5.2.22 (Maximum Bitrate))
#[test]
fn codec_video_missing_bitrate_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"v","packaging":"loc","isLive":true,"codec":"av01.0.08M.08"}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) = MsfCatalogDocument::decode(json) else {
        panic!("codec 由来の bitrate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("bitrate") && reason.contains("codec 'av01.0.08M.08'"),
        "codec 由来の bitrate 要求を述べること: {reason}"
    );
}

/// レジストリの登録名だけの codec `av01` も video と判定して bitrate を要求する
///
/// draft-ietf-moq-msf-01 §5.6.2 のカタログ例は `"codec":"av01"` を使う。
#[test]
fn registry_name_only_video_codec_missing_bitrate_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"v","packaging":"loc","isLive":true,"codec":"av01"}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) = MsfCatalogDocument::decode(json) else {
        panic!("codec 由来の bitrate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("bitrate") && reason.contains("codec 'av01'"),
        "codec 由来の bitrate 要求を述べること: {reason}"
    );
}

/// role 省略 + codec `opus` で bitrate / samplerate / channelConfig を揃えれば受理される
#[test]
fn codec_audio_with_required_fields_accepted() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"a","packaging":"loc","isLive":true,"codec":"opus","bitrate":32000,"samplerate":48000,"channelConfig":"2"}]}"#;
    assert!(MsfCatalogDocument::decode(json).is_ok());
}

/// role 省略 + codec `av01.0.08M.08` で bitrate を揃えれば受理される
#[test]
fn codec_video_with_required_fields_accepted() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"v","packaging":"loc","isLive":true,"codec":"av01.0.08M.08","bitrate":1000}]}"#;
    assert!(MsfCatalogDocument::decode(json).is_ok());
}

/// 登録名に一致しない codec は audio / video と判定せず、codec に基づく要求を行わない
#[test]
fn unregistered_codec_does_not_require_media_fields() {
    for codec in ["opusx", "opus.2", "vp8x", "flacx"] {
        let json = format!(
            r#"{{"version":"draft-01","tracks":[{{"name":"t","packaging":"loc","isLive":true,"codec":"{codec}"}}]}}"#
        );
        assert!(
            MsfCatalogDocument::decode(json.as_bytes()).is_ok(),
            "登録外の codec '{codec}' は codec に基づく要求を行わない"
        );
    }
}

/// WEBCODECS-CODEC-REGISTRY の audio 登録名を audio と判定すること
///
/// bitrate だけを与え、audio 固有の samplerate 要求で拒否されることで audio 判定を固定する
/// (video と取り違えていれば bitrate だけで受理される)。
/// 登録内容は WEBCODECS-CODEC-REGISTRY (Registry Draft, 2026-02-12) §3 (Audio Codec Registry)。
#[test]
fn registry_audio_codec_names_are_classified() {
    // `*` 付き登録名は登録名単独と可変サフィックス付きの両方を渡す
    let audio_codecs = [
        "flac",
        "mp3",
        "mp4a",
        "mp4a.40.2",
        "opus",
        "vorbis",
        "ulaw",
        "alaw",
        "pcm",
        "pcm-s16",
    ];
    for codec in audio_codecs {
        let json = format!(
            r#"{{"version":"draft-01","tracks":[{{"name":"t","packaging":"loc","isLive":true,"codec":"{codec}","bitrate":32000}}]}}"#
        );
        let Err(MessageError::InvalidCatalog(reason)) = MsfCatalogDocument::decode(json.as_bytes())
        else {
            panic!("audio の登録名 '{codec}' は samplerate を要求すること");
        };
        assert!(
            reason.contains("samplerate") && reason.contains(&format!("codec '{codec}'")),
            "codec 由来の samplerate 要求を述べること: {reason}"
        );
    }
}

/// WEBCODECS-CODEC-REGISTRY の video 登録名を video と判定すること
///
/// bitrate を与えれば受理され (audio と取り違えていない)、bitrate を欠けば video 判定により
/// 拒否される (未判定なら受理されてしまう) の両方向で video 判定を固定する。
/// 登録内容は WEBCODECS-CODEC-REGISTRY (Registry Draft, 2026-02-12) §4 (Video Codec Registry)。
#[test]
fn registry_video_codec_names_are_classified() {
    // `*` 付き登録名は登録名単独と可変サフィックス付きの両方を渡す
    let video_codecs = [
        "av01",
        "av01.0.08M.08",
        "avc1",
        "avc1.640028",
        "avc3",
        "avc3.640028",
        "hev1",
        "hev1.1.6.L120.B0",
        "hvc1",
        "hvc1.1.6.L120.B0",
        "vp8",
        "vp09",
        "vp09.0.10.08",
    ];
    for codec in video_codecs {
        let json = format!(
            r#"{{"version":"draft-01","tracks":[{{"name":"t","packaging":"loc","isLive":true,"codec":"{codec}","bitrate":1000}}]}}"#
        );
        assert!(
            MsfCatalogDocument::decode(json.as_bytes()).is_ok(),
            "video の登録名 '{codec}' は bitrate だけで受理されること (audio と取り違えない)"
        );
        let json = format!(
            r#"{{"version":"draft-01","tracks":[{{"name":"t","packaging":"loc","isLive":true,"codec":"{codec}"}}]}}"#
        );
        let Err(MessageError::InvalidCatalog(reason)) = MsfCatalogDocument::decode(json.as_bytes())
        else {
            panic!(
                "video の登録名 '{codec}' は bitrate を要求すること (未判定なら受理されてしまう)"
            );
        };
        assert!(
            reason.contains("bitrate") && reason.contains(&format!("codec '{codec}'")),
            "codec 由来の bitrate 要求を述べること: {reason}"
        );
    }
}

/// `*` 付き登録名の区切り文字だけで可変サフィックスが空でも一致とすること
///
/// 登録表記の `*` に隣接する区切り文字までを導入記号として前方一致するため、サフィックスが
/// 空の `名前.` / `名前-` も一致とみなす。登録上は完全修飾形ではないが、一致とみなす側に
/// 倒れても「より厳しく拒否する」方向にしか働かない。
#[test]
fn codec_separator_only_suffix_is_classified() {
    // audio 側は bitrate を与えても samplerate を欠けば拒否される (audio 判定を固定)
    for codec in ["mp4a.", "pcm-"] {
        let json = format!(
            r#"{{"version":"draft-01","tracks":[{{"name":"t","packaging":"loc","isLive":true,"codec":"{codec}","bitrate":32000}}]}}"#
        );
        let Err(MessageError::InvalidCatalog(reason)) = MsfCatalogDocument::decode(json.as_bytes())
        else {
            panic!("登録名 '{codec}' は samplerate を要求すること");
        };
        assert!(
            reason.contains("samplerate") && reason.contains(&format!("codec '{codec}'")),
            "codec 由来の samplerate 要求を述べること: {reason}"
        );
    }
    // video 側は bitrate だけで受理される (video 判定を固定)
    for codec in ["av01.", "avc1."] {
        let json = format!(
            r#"{{"version":"draft-01","tracks":[{{"name":"t","packaging":"loc","isLive":true,"codec":"{codec}","bitrate":1000}}]}}"#
        );
        assert!(
            MsfCatalogDocument::decode(json.as_bytes()).is_ok(),
            "登録名 '{codec}' は bitrate だけで受理されること (audio と取り違えない)"
        );
    }
}

/// 登録名の直後が区切り文字でない codec は audio / video と判定しないこと
///
/// 前方一致だけで判定すると `mp4ax` / `pcmx` / `av01x` などを誤判定する。
#[test]
fn codec_boundary_separator_is_required() {
    for codec in [
        "mp4ax", "pcmx", "av01x", "avc1x", "avc3x", "hev1x", "hvc1x", "vp09x",
    ] {
        let json = format!(
            r#"{{"version":"draft-01","tracks":[{{"name":"t","packaging":"loc","isLive":true,"codec":"{codec}"}}]}}"#
        );
        assert!(
            MsfCatalogDocument::decode(json.as_bytes()).is_ok(),
            "区切り文字が無い codec '{codec}' は codec に基づく要求を行わない"
        );
    }
}

/// role と codec の判定が食い違う場合は両方の要求を重ねて適用する
///
/// role=`video` + codec=`opus` は video の bitrate と audio の samplerate /
/// channelConfig をすべて要求する。bitrate を満たしても samplerate / channelConfig を
/// 欠けば、codec を根拠に拒否される。
#[test]
fn role_and_codec_requirements_are_combined() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"role":"video","codec":"opus","bitrate":32000}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) = MsfCatalogDocument::decode(json) else {
        panic!("samplerate 欠如は codec を根拠に拒否されること");
    };
    assert!(
        reason.contains("codec 'opus'") && reason.contains("samplerate"),
        "codec 由来の samplerate 要求を述べること: {reason}"
    );
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"role":"video","codec":"opus","bitrate":32000,"samplerate":48000}]}"#;
    let Err(MessageError::InvalidCatalog(reason)) = MsfCatalogDocument::decode(json) else {
        panic!("channelConfig 欠如は codec を根拠に拒否されること");
    };
    assert!(
        reason.contains("codec 'opus'") && reason.contains("channelConfig"),
        "codec 由来の channelConfig 要求を述べること: {reason}"
    );
    // 両方の要求を満たせば受理される
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"role":"video","codec":"opus","bitrate":32000,"samplerate":48000,"channelConfig":"2"}]}"#;
    assert!(
        MsfCatalogDocument::decode(json).is_ok(),
        "role と codec の要求を満たすカタログは受理されること"
    );
}

/// role 省略 + codec `opus` の bitrate 欠如は encode 経路でも拒否される
#[test]
fn codec_audio_missing_bitrate_rejected_on_encode() {
    let mut track = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    track.codec = Some("opus".to_string());
    track.samplerate = Some(48_000);
    track.channel_config = Some("2".to_string());
    // bitrate を欠いたまま encode するとエラーになる
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        tracks: vec![track],
        ..MsfCatalog::new()
    });
    let Err(MessageError::InvalidCatalog(reason)) = doc.encode() else {
        panic!("codec 由来の bitrate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("bitrate") && reason.contains("codec 'opus'"),
        "codec 由来の bitrate 要求を述べること: {reason}"
    );
}

/// role 省略 + codec `opus` の samplerate 欠如は encode 経路でも拒否される
#[test]
fn codec_audio_missing_samplerate_rejected_on_encode() {
    let mut track = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    track.codec = Some("opus".to_string());
    track.bitrate = Some(32_000);
    track.channel_config = Some("2".to_string());
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        tracks: vec![track],
        ..MsfCatalog::new()
    });
    let Err(MessageError::InvalidCatalog(reason)) = doc.encode() else {
        panic!("codec 由来の samplerate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("samplerate") && reason.contains("codec 'opus'"),
        "codec 由来の samplerate 要求を述べること: {reason}"
    );
}

/// role 省略 + codec `opus` の channelConfig 欠如は encode 経路でも拒否される
#[test]
fn codec_audio_missing_channel_config_rejected_on_encode() {
    let mut track = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    track.codec = Some("opus".to_string());
    track.bitrate = Some(32_000);
    track.samplerate = Some(48_000);
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        tracks: vec![track],
        ..MsfCatalog::new()
    });
    let Err(MessageError::InvalidCatalog(reason)) = doc.encode() else {
        panic!("codec 由来の channelConfig 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("channelConfig") && reason.contains("codec 'opus'"),
        "codec 由来の channelConfig 要求を述べること: {reason}"
    );
}

/// role 省略 + codec `av01` の bitrate 欠如は encode 経路でも拒否される
#[test]
fn registry_name_only_video_codec_missing_bitrate_rejected_on_encode() {
    let mut track = MsfTrack::new("v".to_string(), MsfPackaging::Loc, true);
    track.codec = Some("av01".to_string());
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        tracks: vec![track],
        ..MsfCatalog::new()
    });
    let Err(MessageError::InvalidCatalog(reason)) = doc.encode() else {
        panic!("codec 由来の bitrate 欠如は InvalidCatalog であること");
    };
    assert!(
        reason.contains("bitrate") && reason.contains("codec 'av01'"),
        "codec 由来の bitrate 要求を述べること: {reason}"
    );
}

/// role も codec も無いトラックを持つカタログは encode 経路でも受理される
///
/// codec に基づく要求は codec を持つトラックにのみ課されるため、raw data トラックは
/// bitrate を要求しない (draft-ietf-moq-msf-01 §5.2.18 (Codec))。
#[test]
fn raw_track_without_codec_accepted_on_encode() {
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        tracks: vec![MsfTrack::new("raw".to_string(), MsfPackaging::Loc, true)],
        ..MsfCatalog::new()
    });
    assert!(
        doc.encode().is_ok(),
        "codec も role も無いトラックは codec に基づく要求を行わないこと"
    );
}

/// 通常トラックに parentName が含まれると不正カタログとして拒否される
///
/// draft-ietf-moq-msf-01 §5.2.33 (Parent name): "This field MUST only be included
/// inside a clone operation in a delta update Section 5.1.6."
#[test]
fn parent_name_in_track_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"parentName":"p"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(ref reason))
            if reason == "parentName MUST only be included inside a clone operation in a delta update"
    ));
}

/// 通常トラックに parentNamespace が含まれると不正カタログとして拒否される
///
/// draft-ietf-moq-msf-01 §5.2.34 (Parent namespace): "This field MUST only be included
/// inside a clone operation in a delta update Section 5.1.6." `parentName` と対称。
#[test]
fn parent_namespace_in_track_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"parentNamespace":"ns"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(ref reason))
            if reason == "parentNamespace MUST only be included inside a clone operation in a delta update"
    ));
}

// ─── framerate の非有限値拒否 ─────────────────────────────────────────────────

/// framerate に infinity (1e400 は f64 で infinity に丸められる) を指定すると拒否される
#[test]
fn framerate_infinity_rejected() {
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"framerate":1e400}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

// ─── authInfo value_raw の encode 時検証 ──────────────────────────────────────

/// 手組みの非 UTF-8 value_raw は encode で拒否され、panic しない
///
/// draft-ietf-moq-msf-01 §5.2.42 (Authorization Info)
#[test]
fn auth_info_non_utf8_value_rejected_on_encode() {
    let mut track = MsfTrack::new("video".to_string(), MsfPackaging::Loc, true);
    track.auth_info = Some(vec![MsfAuthInfo {
        scheme: "x".to_string(),
        value_raw: vec![0xFF, 0xFE],
    }]);
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        version: MSF_VERSION.to_string(),
        generated_at: None,
        is_complete: false,
        tracks: vec![track],
        publish_tracks: Vec::new(),
        removed_tracks: Default::default(),
        init_data_list: Vec::new(),
    });
    assert!(
        matches!(doc.encode(), Err(MessageError::InvalidCatalog(_))),
        "非 UTF-8 の value_raw は InvalidCatalog で拒否されること"
    );
}

/// publishTracks 側の手組み不正 value_raw も encode で拒否され、panic しない
///
/// draft-ietf-moq-msf-01 §5.1.5 (Publish tracks) / §5.2.42 (Authorization Info)
#[test]
fn auth_info_non_utf8_value_in_publish_tracks_rejected_on_encode() {
    let mut track = MsfTrack::new("video".to_string(), MsfPackaging::Loc, true);
    track.auth_info = Some(vec![MsfAuthInfo {
        scheme: "x".to_string(),
        value_raw: vec![0xFF, 0xFE],
    }]);
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        version: MSF_VERSION.to_string(),
        generated_at: None,
        is_complete: false,
        tracks: Vec::new(),
        publish_tracks: vec![track],
        removed_tracks: Default::default(),
        init_data_list: Vec::new(),
    });
    assert!(
        matches!(doc.encode(), Err(MessageError::InvalidCatalog(_))),
        "publishTracks の非 UTF-8 value_raw は InvalidCatalog で拒否されること"
    );
}

/// 手組みの非 JSON 値 value_raw は encode で拒否される
///
/// draft-ietf-moq-msf-01 §5.2.42 (Authorization Info): 値は単独の JSON 値
#[test]
fn auth_info_non_json_value_rejected_on_encode() {
    let mut track = MsfTrack::new("video".to_string(), MsfPackaging::Loc, true);
    track.auth_info = Some(vec![MsfAuthInfo {
        scheme: "x".to_string(),
        value_raw: b"{invalid".to_vec(),
    }]);
    let doc = MsfCatalogDocument::Full(MsfCatalog {
        version: MSF_VERSION.to_string(),
        generated_at: None,
        is_complete: false,
        tracks: vec![track],
        publish_tracks: Vec::new(),
        removed_tracks: Default::default(),
        init_data_list: Vec::new(),
    });
    assert!(
        matches!(doc.encode(), Err(MessageError::InvalidCatalog(_))),
        "単独の JSON 値でない value_raw は InvalidCatalog で拒否されること"
    );
}

// ─── encode 時の完全カタログ検証 (decode_full_catalog と同等) ──────────────────

/// テスト用の Full カタログドキュメントを組み立てる
fn full_doc(
    tracks: Vec<MsfTrack>,
    publish_tracks: Vec<MsfTrack>,
    init_data_list: Vec<MsfInitData>,
) -> MsfCatalogDocument {
    MsfCatalogDocument::Full(MsfCatalog {
        version: MSF_VERSION.to_string(),
        generated_at: None,
        is_complete: false,
        tracks,
        publish_tracks,
        removed_tracks: Default::default(),
        init_data_list,
    })
}

/// tracks 内の (namespace, name) 重複は encode でも拒否されること
#[test]
fn encode_full_duplicate_name_within_tracks_rejected() {
    // draft-ietf-moq-msf-01 §5.2.3 (Track name)
    let a = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    let b = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    assert!(matches!(
        full_doc(vec![a, b], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// tracks と publishTracks をまたぐ (namespace, name) 重複は encode でも拒否されること
#[test]
fn encode_full_duplicate_name_across_tracks_and_publish_tracks_rejected() {
    // draft-ietf-moq-msf-01 §5.2.3 (Track name): "Within the catalog"
    let a = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    let b = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    assert!(matches!(
        full_doc(vec![a], vec![b], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// initDataList の id 重複は encode でも拒否されること
#[test]
fn encode_full_duplicate_init_data_list_id_rejected() {
    // draft-ietf-moq-msf-01 §5.1.7 (Initialization Data List)
    let entry = || MsfInitData {
        id: "init1".to_string(),
        kind: MsfInitDataKind::Inline,
        data: "AAEC".to_string(),
    };
    assert!(matches!(
        full_doc(vec![], vec![], vec![entry(), entry()]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// 一致しない initRef は encode でも拒否されること
#[test]
fn encode_full_missing_init_ref_rejected() {
    // draft-ietf-moq-msf-01 §5.2.13 (Initialization reference)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.init_ref = Some("missing".to_string());
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// targetLatency と buffers の共存は encode でも拒否されること
#[test]
fn encode_full_target_latency_and_buffers_together_rejected() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency) / §5.2.9 (Buffers)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.target_latency = Some(500);
    track.buffers = Some(MsfBuffers {
        target: Some(1000),
        min: None,
        max: None,
    });
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(reason))
            if reason == "targetLatency and buffers MUST NOT be present together"
    ));
}

/// 同一 renderGroup の targetLatency 不一致は encode でも拒否されること
#[test]
fn encode_full_group_target_latency_mismatch_rejected() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency)
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.render_group = Some(1);
    a.target_latency = Some(500);
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.render_group = Some(1);
    b.target_latency = Some(1000);
    assert!(matches!(
        full_doc(vec![a, b], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// publishTracks 内の同一 altGroup の buffers 不一致は encode でも拒否されること
#[test]
fn encode_full_publish_tracks_group_buffers_mismatch_rejected() {
    // draft-ietf-moq-msf-01 §5.2.9 (Buffers)
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::MoqLog, true);
    a.alt_group = Some(1);
    a.buffers = Some(MsfBuffers {
        target: Some(500),
        min: None,
        max: None,
    });
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::MoqLog, true);
    b.alt_group = Some(1);
    b.buffers = Some(MsfBuffers {
        target: Some(1000),
        min: None,
        max: None,
    });
    assert!(matches!(
        full_doc(vec![], vec![a, b], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// 同一 renderGroup で片方だけ targetLatency を宣言した手組みカタログは encode でも受理されること
#[test]
fn encode_full_group_target_latency_omitted_accepted() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency): 省略は player の裁量 (MAY) であり不一致ではない
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.render_group = Some(1);
    a.target_latency = Some(500);
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.render_group = Some(1);
    full_doc(vec![a, b], vec![], vec![])
        .encode()
        .expect("省略された targetLatency は encode できること");
}

/// 同一 renderGroup で片方だけ buffers を宣言した手組みカタログは encode でも受理されること
#[test]
fn encode_full_group_buffers_omitted_accepted() {
    // draft-ietf-moq-msf-01 §5.2.9 (Buffers): 省略は player の裁量 (MAY) であり不一致ではない
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.render_group = Some(1);
    a.buffers = Some(MsfBuffers {
        target: Some(500),
        min: None,
        max: None,
    });
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.render_group = Some(1);
    full_doc(vec![a, b], vec![], vec![])
        .encode()
        .expect("省略された buffers は encode できること");
}

/// 同一 altGroup で片方だけ buffers を宣言した手組みカタログは encode でも受理されること
#[test]
fn encode_full_alt_group_buffers_omitted_accepted() {
    // draft-ietf-moq-msf-01 §5.2.9 (Buffers): altGroup でも省略は不一致としない
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.alt_group = Some(2);
    a.buffers = Some(MsfBuffers {
        target: Some(500),
        min: None,
        max: None,
    });
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.alt_group = Some(2);
    full_doc(vec![a, b], vec![], vec![])
        .encode()
        .expect("altGroup でも省略された buffers は encode できること");
}

/// 同一 altGroup で片方だけ targetLatency を宣言した手組みカタログは encode でも受理されること
#[test]
fn encode_full_alt_group_target_latency_omitted_accepted() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency): altGroup でも省略は不一致としない
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.alt_group = Some(2);
    a.target_latency = Some(500);
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.alt_group = Some(2);
    full_doc(vec![a, b], vec![], vec![])
        .encode()
        .expect("altGroup でも省略された targetLatency は encode できること");
}

/// renderGroup と altGroup を同時に持つトラックでも renderGroup の不一致は拒否されること
#[test]
fn encode_full_group_render_conflict_rejected() {
    // renderGroup の不一致が altGroup の相違に隠されないこと (altGroup は衝突しない)
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.render_group = Some(1);
    a.alt_group = Some(2);
    a.target_latency = Some(500);
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.render_group = Some(1);
    b.alt_group = Some(2);
    b.target_latency = Some(500);
    let mut c = MsfTrack::new("c".to_string(), MsfPackaging::Loc, true);
    c.render_group = Some(1);
    c.alt_group = Some(3);
    c.target_latency = Some(1000);
    // c は renderGroup 1 で a / b と衝突する (altGroup は異なる)
    assert!(matches!(
        full_doc(vec![a, b, c], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(reason)) if reason.contains("renderGroup 1")
    ));
}

/// renderGroup が衝突しない構成でも altGroup の不一致は拒否されること
#[test]
fn encode_full_group_alt_conflict_rejected() {
    // altGroup の不一致が renderGroup の相違に隠されないこと
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.render_group = Some(1);
    a.alt_group = Some(2);
    a.target_latency = Some(500);
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.render_group = Some(2);
    b.alt_group = Some(2);
    b.target_latency = Some(1000);
    // renderGroup は 1 と 2 で衝突せず、altGroup 2 のみが衝突する
    assert!(matches!(
        full_doc(vec![a, b], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(reason)) if reason.contains("altGroup 2")
    ));
}

/// group 値 0 も通常の group 値として宣言値の一致を検証すること
#[test]
fn encode_full_group_zero_target_latency_mismatch_rejected() {
    // group 値は 0 も有効な値であり、省略 (None) と混同して比較対象外にしないこと
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.render_group = Some(0);
    a.target_latency = Some(500);
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.render_group = Some(0);
    b.target_latency = Some(1000);
    assert!(matches!(
        full_doc(vec![a, b], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// group 値 0 の buffers でも通常の group 値として宣言値の一致を検証すること
#[test]
fn encode_full_group_zero_buffers_mismatch_rejected() {
    // targetLatency 側だけでなく buffers 側でも 0 を省略と混同しないこと
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.render_group = Some(0);
    a.buffers = Some(MsfBuffers {
        target: Some(500),
        min: None,
        max: None,
    });
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.render_group = Some(0);
    b.buffers = Some(MsfBuffers {
        target: Some(1000),
        min: None,
        max: None,
    });
    assert!(matches!(
        full_doc(vec![a, b], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// group 未所属のトラックは宣言値が異なっても group 検証で拒否されないこと
#[test]
fn encode_full_without_group_different_target_latency_accepted() {
    // group 値が無い (None) トラック同士は group 検証の対象外であること
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.target_latency = Some(500);
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.target_latency = Some(1000);
    full_doc(vec![a, b], vec![], vec![])
        .encode()
        .expect("group 未所属のトラックは group 検証の対象外であること");
}

/// publishTracks 内で片方だけ targetLatency を宣言した手組みカタログは encode でも受理されること
#[test]
fn encode_full_publish_tracks_group_target_latency_omitted_accepted() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency): tracks と publishTracks は分離して検証するが
    // 省略を比較対象外とする規則は publishTracks 側にも適用される
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::MoqLog, true);
    a.render_group = Some(1);
    a.target_latency = Some(500);
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::MoqLog, true);
    b.render_group = Some(1);
    full_doc(vec![], vec![a, b], vec![])
        .encode()
        .expect("publishTracks でも省略された targetLatency は encode できること");
}

/// 省略トラックを挟んでも宣言された targetLatency の不一致は encode でも拒否されること
#[test]
fn encode_full_group_omitted_between_different_target_latency_rejected() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency)
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.render_group = Some(1);
    a.target_latency = Some(500);
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.render_group = Some(1);
    let mut c = MsfTrack::new("c".to_string(), MsfPackaging::Loc, true);
    c.render_group = Some(1);
    c.target_latency = Some(1000);
    assert!(matches!(
        full_doc(vec![a, b, c], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// 省略トラックを挟んでも宣言された buffers の不一致は encode でも拒否されること
#[test]
fn encode_full_group_omitted_between_different_buffers_rejected() {
    // draft-ietf-moq-msf-01 §5.2.9 (Buffers)
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.render_group = Some(1);
    a.buffers = Some(MsfBuffers {
        target: Some(500),
        min: None,
        max: None,
    });
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, true);
    b.render_group = Some(1);
    let mut c = MsfTrack::new("c".to_string(), MsfPackaging::Loc, true);
    c.render_group = Some(1);
    c.buffers = Some(MsfBuffers {
        target: Some(1000),
        min: None,
        max: None,
    });
    assert!(matches!(
        full_doc(vec![a, b, c], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// isLive=false のトラックの targetLatency は手組みカタログの encode でも比較対象外であること
#[test]
fn encode_full_group_target_latency_is_live_false_ignored() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency): isLive=false では targetLatency は無視される。
    // decode 経路は decode_track が isLive=false の値を None に正規化するため、
    // 正規化を通らない手組みの MsfCatalog で除外そのものを固定する
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.render_group = Some(1);
    a.target_latency = Some(500);
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, false);
    b.render_group = Some(1);
    b.target_latency = Some(1000);
    full_doc(vec![a, b], vec![], vec![])
        .encode()
        .expect("isLive=false の targetLatency は encode の比較対象外であること");
}

/// isLive=false のトラックの buffers は手組みカタログの encode でも比較対象外であること
#[test]
fn encode_full_group_buffers_is_live_false_ignored() {
    // draft-ietf-moq-msf-01 §5.2.9 (Buffers): isLive=false では buffers は無視される
    let mut a = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    a.render_group = Some(1);
    a.buffers = Some(MsfBuffers {
        target: Some(500),
        min: None,
        max: None,
    });
    let mut b = MsfTrack::new("b".to_string(), MsfPackaging::Loc, false);
    b.render_group = Some(1);
    b.buffers = Some(MsfBuffers {
        target: Some(1000),
        min: None,
        max: None,
    });
    full_doc(vec![a, b], vec![], vec![])
        .encode()
        .expect("isLive=false の buffers は encode の比較対象外であること");
}

/// 非 eventtimeline での eventType は encode でも拒否されること
#[test]
fn encode_full_event_type_with_non_eventtimeline_rejected() {
    // draft-ietf-moq-msf-01 §5.2.5 (Event timeline type)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.event_type = Some("x".to_string());
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// eventtimeline で eventType が無い場合は encode でも拒否されること
#[test]
fn encode_full_event_timeline_missing_event_type_rejected() {
    // draft-ietf-moq-msf-01 §5.2.5 (Event timeline type)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::EventTimeline, true);
    track.depends = vec!["v".to_string()];
    track.mime_type = Some("application/json".to_string());
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// mediatimeline で depends が無い場合は encode でも拒否されること
#[test]
fn encode_full_media_timeline_missing_depends_rejected() {
    // draft-ietf-moq-msf-01 §7.2 (Media Timeline Catalog requirements)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::MediaTimeline, true);
    track.mime_type = Some("application/json".to_string());
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// encryptionScheme 指定時の cipherSuite 欠如は encode でも拒否されること
#[test]
fn encode_full_encryption_scheme_missing_cipher_suite_rejected() {
    // draft-ietf-moq-msf-01 §5.2.39 (Cipher suite)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.encryption_scheme = Some("com.example.custom".to_string());
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// moq-secure-objects の必須組欠如は encode でも拒否されること
#[test]
fn encode_full_secure_objects_incomplete_rejected() {
    // draft-ietf-moq-msf-01 §4.3.3 (Recommended encryption scheme)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.encryption_scheme = Some("moq-secure-objects".to_string());
    track.cipher_suite = Some("aes-128-gcm-sha256".to_string());
    // keyId / trackBaseKey を欠く
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// 不正な lang は encode でも拒否されること
#[test]
fn encode_full_invalid_lang_rejected() {
    // draft-ietf-moq-msf-01 §5.2.32 (Language)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.lang = Some("1".to_string());
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// irregular タグは encode でも受理され、decode で同じ値に戻ること
#[test]
fn encode_full_irregular_lang_roundtrip() {
    // draft-ietf-moq-msf-01 §5.2.32 (Language)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.lang = Some("i-klingon".to_string());
    let bytes = full_doc(vec![track], vec![], vec![])
        .encode()
        .expect("irregular タグは encode できること");
    let decoded = MsfCatalogDocument::decode(&bytes).expect("encode 結果は decode できること");
    let MsfCatalogDocument::Full(catalog) = decoded else {
        panic!("Full catalog に戻ること");
    };
    assert_eq!(
        catalog.tracks[0].lang.as_deref(),
        Some("i-klingon"),
        "encode / decode で lang が保持されること"
    );
}

/// 通常トラックの parentName は encode でも拒否されること
#[test]
fn encode_full_parent_name_rejected() {
    // draft-ietf-moq-msf-01 §5.2.33 (Parent name)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.parent_name = Some("p".to_string());
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// isLive=true の trackDuration は encode でも拒否されること
#[test]
fn encode_full_track_duration_when_is_live_true_rejected() {
    // draft-ietf-moq-msf-01 §5.2.35 (Track duration)
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::Loc, true);
    track.track_duration = Some(1000);
    assert!(matches!(
        full_doc(vec![track], vec![], vec![]).encode(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

// ─── encode 時のデルタ更新検証 (decode_delta と同等) ──────────────────────────

/// 空の operations は encode でも拒否されること
#[test]
fn encode_delta_empty_operations_rejected() {
    // draft-ietf-moq-msf-01 §5.3 (Delta updates)
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: None,
        operations: Vec::new(),
    });
    assert!(matches!(doc.encode(), Err(MessageError::InvalidCatalog(_))));
}

/// delta add の不正トラックは encode でも拒否されること
#[test]
fn encode_delta_add_invalid_track_rejected() {
    // draft-ietf-moq-msf-01 §7.2 (Media Timeline Catalog requirements): depends 必須
    let mut track = MsfTrack::new("t".to_string(), MsfPackaging::MediaTimeline, true);
    track.mime_type = Some("application/json".to_string());
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: None,
        operations: vec![MsfDeltaOperation::Add {
            tracks: vec![track],
        }],
    });
    assert!(matches!(doc.encode(), Err(MessageError::InvalidCatalog(_))));
}

/// delta clone の targetLatency / buffers 共存は encode でも拒否されること
#[test]
fn encode_delta_clone_target_latency_and_buffers_rejected() {
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency) / §5.2.9 (Buffers)
    let mut clone = MsfCloneTrack::new("c".to_string(), "p".to_string());
    clone.target_latency = Some(500);
    clone.buffers = Some(MsfBuffers {
        target: Some(1000),
        min: None,
        max: None,
    });
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: None,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    });
    assert!(matches!(
        doc.encode(),
        Err(MessageError::InvalidCatalog(reason))
            if reason == "targetLatency and buffers MUST NOT be present together"
    ));
}

/// delta clone の isLive=true + trackDuration は encode でも拒否されること
#[test]
fn encode_delta_clone_track_duration_when_is_live_true_rejected() {
    // draft-ietf-moq-msf-01 §5.2.35 (Track duration)
    let mut clone = MsfCloneTrack::new("c".to_string(), "p".to_string());
    clone.is_live = Some(true);
    clone.track_duration = Some(1000);
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: None,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    });
    assert!(matches!(doc.encode(), Err(MessageError::InvalidCatalog(_))));
}

/// delta clone の非 eventtimeline packaging + eventType は encode でも拒否されること
#[test]
fn encode_delta_clone_event_type_with_non_eventtimeline_rejected() {
    // draft-ietf-moq-msf-01 §5.2.5 (Event timeline type)
    let mut clone = MsfCloneTrack::new("c".to_string(), "p".to_string());
    clone.packaging = Some(MsfPackaging::Loc);
    clone.event_type = Some("x".to_string());
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: None,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    });
    assert!(matches!(doc.encode(), Err(MessageError::InvalidCatalog(_))));
}

/// delta clone の不正 lang は encode でも拒否されること
#[test]
fn encode_delta_clone_invalid_lang_rejected() {
    // draft-ietf-moq-msf-01 §5.2.32 (Language)
    let mut clone = MsfCloneTrack::new("c".to_string(), "p".to_string());
    clone.lang = Some("1".to_string());
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: None,
        operations: vec![MsfDeltaOperation::Clone {
            tracks: vec![clone],
        }],
    });
    assert!(matches!(doc.encode(), Err(MessageError::InvalidCatalog(_))));
}

/// delta add / clone の irregular タグは encode でも受理され、decode で同じ値に戻ること
#[test]
fn encode_delta_irregular_lang_roundtrip() {
    // draft-ietf-moq-msf-01 §5.2.32 (Language)
    let mut add = MsfTrack::new("a".to_string(), MsfPackaging::Loc, true);
    add.lang = Some("i-klingon".to_string());
    let mut clone = MsfCloneTrack::new("c".to_string(), "p".to_string());
    // 大文字表記も encode / decode で正規化されずそのまま保持されることを確認する
    clone.lang = Some("I-AMI".to_string());
    let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
        generated_at: None,
        operations: vec![
            MsfDeltaOperation::Add { tracks: vec![add] },
            MsfDeltaOperation::Clone {
                tracks: vec![clone],
            },
        ],
    });
    let bytes = doc.encode().expect("irregular タグは encode できること");
    let decoded = MsfCatalogDocument::decode(&bytes).expect("encode 結果は decode できること");
    assert_eq!(decoded, doc, "encode / decode で lang が保持されること");
}
