use super::*;
use shiguredo_moqt::msf::uri::{
    MsfConnectionType, MsfLocationRange, MsfLocationRangeEnd, MsfTimeRange, parse_msf_fragment,
    parse_msf_uri, validate_moqt_endpoint_uri,
};

/// §11.1.3 (Example MSF URLs) のカタログ URL をパースできること
#[test]
fn parse_catalog_uri() {
    let uri = parse_msf_uri(
        "moqt://example.com/server/config?a=1&b=2#msf:customer-livestream-123--catalog",
    )
    .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(uri.authority, "example.com");
    assert_eq!(uri.path, "/server/config");
    assert_eq!(uri.query.as_deref(), Some("a=1&b=2"));
    assert_eq!(
        uri.fragment.namespace,
        shiguredo_moqt::message::common::TrackNamespace::new(vec![
            b"customer".to_vec(),
            b"livestream".to_vec(),
            b"123".to_vec(),
        ])
        .expect("テストフィクスチャの前提条件を満たす")
    );
    assert_eq!(uri.fragment.track_name, b"catalog");
}

/// connection パラメータが接続種別へ変換されること
#[test]
fn parse_connection_parameter() {
    let uri = parse_msf_uri(
        "moqt://example.com/relay-app/relayID#msf:customerID-broadcastID--catalog&connection=q",
    )
    .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        uri.fragment
            .connection_types()
            .expect("テストフィクスチャの前提条件を満たす"),
        vec![MsfConnectionType::Quic]
    );

    let uri = parse_msf_uri(
        "moqt://example.com/relay-app/relayID#msf:customerID-broadcastID--video&connection=wt",
    )
    .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        uri.fragment
            .connection_types()
            .expect("テストフィクスチャの前提条件を満たす"),
        vec![MsfConnectionType::WebTransport]
    );
}

/// connection パラメータの複数指定は出現順にすべて返ること
///
/// draft-ietf-moq-msf-02 §11.1.1 (Reserved fragment parameters) の union 規則は値域を対象とした
/// もので `connection` には適用しない。`q` と `wt` のように両立しない組み合わせの扱いは呼び出し側
/// が決めるため、ここでは検証せずすべて返す。
#[test]
fn parse_connection_parameter_with_multiple_values() {
    let uri = parse_msf_uri(
        "moqt://example.com/relay-app/relayID#msf:customerID-broadcastID--catalog&connection=q&connection=wt",
    )
    .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        uri.fragment
            .connection_types()
            .expect("テストフィクスチャの前提条件を満たす"),
        vec![MsfConnectionType::Quic, MsfConnectionType::WebTransport]
    );

    // 同じ値の重複もそのまま返す (呼び出し側は集合として扱う)
    let uri = parse_msf_uri(
        "moqt://example.com/relay-app/relayID#msf:customerID-broadcastID--catalog&connection=wt&connection=wt",
    )
    .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        uri.fragment
            .connection_types()
            .expect("テストフィクスチャの前提条件を満たす"),
        vec![
            MsfConnectionType::WebTransport,
            MsfConnectionType::WebTransport
        ]
    );
}

/// connection パラメータの不正値は reject されること
#[test]
fn parse_connection_invalid_value_rejected() {
    let fragment = parse_msf_fragment("msf:ns--catalog&connection=udp")
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(matches!(
        fragment.connection_types(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// wallclock-range / mediatime-range の closed / open をパースできること
#[test]
fn parse_time_ranges() {
    let fragment = parse_msf_fragment(
        "msf:ns--catalog&wallclock-range=1761759637565-1761759836189&mediatime-range=0-13421",
    )
    .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        fragment
            .wallclock_ranges()
            .expect("テストフィクスチャの前提条件を満たす"),
        vec![MsfTimeRange {
            start_ms: 1_761_759_637_565,
            end_ms: Some(1_761_759_836_189),
        }]
    );
    assert_eq!(
        fragment
            .mediatime_ranges()
            .expect("テストフィクスチャの前提条件を満たす"),
        vec![MsfTimeRange {
            start_ms: 0,
            end_ms: Some(13_421),
        }]
    );

    // dash と end を省略した open range
    let fragment = parse_msf_fragment("msf:ns--catalog&wallclock-range=1761751753894")
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        fragment
            .wallclock_ranges()
            .expect("テストフィクスチャの前提条件を満たす"),
        vec![MsfTimeRange {
            start_ms: 1_761_751_753_894,
            end_ms: None,
        }]
    );
    // "start-" も open range
    let fragment = parse_msf_fragment("msf:ns--catalog&mediatime-range=982-")
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        fragment
            .mediatime_ranges()
            .expect("テストフィクスチャの前提条件を満たす"),
        vec![MsfTimeRange {
            start_ms: 982,
            end_ms: None,
        }]
    );
}

/// time range の終端が開始より前なら reject されること
#[test]
fn parse_time_range_end_before_start_rejected() {
    let fragment = parse_msf_fragment("msf:ns--catalog&mediatime-range=100-50")
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(matches!(
        fragment.mediatime_ranges(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// location-range の GroupID.ObjectID / GroupID 形式をパースできること
#[test]
fn parse_location_ranges() {
    // §11.1.3 の `location-range=34-64`: 開始 group 34 から終端 group 64 全体
    let fragment = parse_msf_fragment("msf:ns--catalog&location-range=34-64")
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        fragment
            .location_ranges()
            .expect("テストフィクスチャの前提条件を満たす"),
        vec![MsfLocationRange {
            start_group_id: 34,
            start_object_id: None,
            end: Some(MsfLocationRangeEnd {
                group_id: 64,
                object_id: None,
            }),
        }]
    );

    // §11.1.1 の `location-range=34.0-2145.16`
    let fragment = parse_msf_fragment("msf:ns--catalog&location-range=34.0-2145.16")
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        fragment
            .location_ranges()
            .expect("テストフィクスチャの前提条件を満たす"),
        vec![MsfLocationRange {
            start_group_id: 34,
            start_object_id: Some(0),
            end: Some(MsfLocationRangeEnd {
                group_id: 2145,
                object_id: Some(16),
            }),
        }]
    );

    // §11.1.1 の `location-range=16.24` (open range)
    let fragment = parse_msf_fragment("msf:ns--catalog&location-range=16.24")
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        fragment
            .location_ranges()
            .expect("テストフィクスチャの前提条件を満たす"),
        vec![MsfLocationRange {
            start_group_id: 16,
            start_object_id: Some(24),
            end: None,
        }]
    );
}

/// 同一パラメータの複数指定は union として全件返されること
#[test]
fn parse_multiple_ranges() {
    let fragment =
        parse_msf_fragment("msf:ns--catalog&location-range=34-64&location-range=100.0-200.5")
            .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        fragment
            .location_ranges()
            .expect("テストフィクスチャの前提条件を満たす")
            .len(),
        2
    );
}

/// cat パラメータがそのまま返されること
#[test]
fn parse_cat_parameter() {
    let fragment = parse_msf_fragment("msf:ns--catalog&cat=gqhkYWxn")
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(fragment.cat_tokens(), vec!["gqhkYWxn"]);
}

/// 旧予約パラメータ `c4m` は認可トークンとして取り出さないこと
///
/// draft-ietf-moq-msf-02 Appendix A.1.7 が `c4m` の `cat` への改名を記録した。本実装は
/// 予約でないパラメータを認可トークンの供給元にしない fail-closed の方針を取る。
#[test]
fn parse_legacy_c4m_parameter_is_not_reserved() {
    let fragment = parse_msf_fragment("msf:ns--catalog&c4m=gqhkYWxn")
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        fragment.cat_tokens().is_empty(),
        "旧 c4m パラメータを認可トークンとして扱わないこと"
    );
    assert_eq!(
        fragment.parameter_values("c4m"),
        vec!["gqhkYWxn"],
        "旧 c4m は未知の汎用パラメータとして保持されること"
    );

    // 旧 c4m の値が Base64 として不正でも、予約パラメータとして検証しないためエラーにならない
    for value in ["!!!", ""] {
        let fragment = parse_msf_fragment(&format!("msf:ns--catalog&c4m={value}"))
            .expect("旧 c4m は汎用パラメータとして受理されること");
        assert!(
            fragment.cat_tokens().is_empty(),
            "旧 c4m={value:?} を認可トークンとして扱わないこと"
        );
    }
}

/// 未知パラメータは無視され、parameter_values で参照できること
#[test]
fn parse_unknown_parameter_retained() {
    let fragment = parse_msf_fragment("msf:ns--catalog&token=XYZ789")
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(fragment.parameter_values("token"), vec!["XYZ789"]);
}

/// fragment の形式不正は reject されること
#[test]
fn parse_fragment_invalid_rejected() {
    // msf: で始まらない
    assert!(matches!(
        parse_msf_fragment("customer--catalog"),
        Err(MessageError::InvalidCatalog(_))
    ));
    // track-identifier が空
    assert!(matches!(
        parse_msf_fragment("msf:"),
        Err(MessageError::InvalidCatalog(_))
    ));
    // パラメータに = が無い
    assert!(matches!(
        parse_msf_fragment("msf:ns--catalog&token"),
        Err(MessageError::InvalidCatalog(_))
    ));
    // パラメータ名が空
    assert!(matches!(
        parse_msf_fragment("msf:ns--catalog&=x"),
        Err(MessageError::InvalidCatalog(_))
    ));
    // track-identifier に `?` は使えない (pchar-no-amp)
    assert!(matches!(
        parse_msf_fragment("msf:ns--cata?log"),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// draft-ietf-moq-msf-02 §11.1 (URL construction and interpretation): scheme は
/// case-insensitive のため `MOQT://` も受理されること
#[test]
fn parse_uri_scheme_case_insensitive() {
    let uri = parse_msf_uri("MOQT://example.com/relay#msf:ns--catalog")
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(uri.authority, "example.com");
    assert_eq!(uri.path, "/relay");
    assert_eq!(uri.fragment.track_name, b"catalog");
}

/// location-range の終端が開始より前なら reject されること
#[test]
fn parse_location_range_end_before_start_rejected() {
    // Group ID の逆転
    let fragment = parse_msf_fragment("msf:ns--catalog&location-range=100-50")
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(matches!(
        fragment.location_ranges(),
        Err(MessageError::InvalidCatalog(_))
    ));
    // 同一 Group 内の Object ID の逆転
    let fragment = parse_msf_fragment("msf:ns--catalog&location-range=10.5-10.2")
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(matches!(
        fragment.location_ranges(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// draft-ietf-moq-msf-02 §11.1.1 (Reserved fragment parameters): location-range は
/// 終端省略時に dash も省略する MUST のため、末尾ダッシュは reject されること
#[test]
fn parse_location_range_trailing_dash_rejected() {
    let fragment = parse_msf_fragment("msf:ns--catalog&location-range=5-")
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(matches!(
        fragment.location_ranges(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// location-range の open range は dash なしで受理され、同一 Group の終端省略も受理されること
#[test]
fn parse_location_range_open_without_dash_accepted() {
    let fragment = parse_msf_fragment("msf:ns--catalog&location-range=16")
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        fragment
            .location_ranges()
            .expect("テストフィクスチャの前提条件を満たす"),
        vec![MsfLocationRange {
            start_group_id: 16,
            start_object_id: None,
            end: None,
        }]
    );
    // 同一 Group で終端の Object ID を省略した場合は Group 全体を含むため「前」ではない
    let fragment = parse_msf_fragment("msf:ns--catalog&location-range=10.5-10")
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        fragment
            .location_ranges()
            .expect("テストフィクスチャの前提条件を満たす"),
        vec![MsfLocationRange {
            start_group_id: 10,
            start_object_id: Some(5),
            end: Some(MsfLocationRangeEnd {
                group_id: 10,
                object_id: None,
            }),
        }]
    );
}

/// URI の形式不正は reject されること
#[test]
fn parse_uri_invalid_rejected() {
    // scheme 違い
    assert!(matches!(
        parse_msf_uri("https://example.com#msf:ns--catalog"),
        Err(MessageError::InvalidCatalog(_))
    ));
    // fragment なし
    assert!(matches!(
        parse_msf_uri("moqt://example.com/relay"),
        Err(MessageError::InvalidCatalog(_))
    ));
    // authority 空
    assert!(matches!(
        parse_msf_uri("moqt:///relay#msf:ns--catalog"),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// track-identifier の `%XX` (RFC 3986 §2.1 (Percent-Encoding)) をデータバイトへデコードすること
///
/// draft-ietf-moq-msf-02 §11.1 (URL construction and interpretation) の ABNF は
/// `track-identifier = 1*( pchar-no-amp / "/" )` であり、`pchar-no-amp` は `pct-encoded` を含む。
/// `?` は同節が `%3F` として percent-encode することを求める。
#[test]
fn parse_fragment_decodes_percent_encoded_track_identifier() {
    // `%3F` は 0x3F (`?`) のデータバイトになる (hex の大文字小文字は等価)
    let fragment = parse_msf_fragment("msf:customer--catalog%3Fpart")
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        fragment.track_name,
        b"catalog?part".to_vec(),
        "%3F が 0x3F としてデコードされること"
    );
    let lower = parse_msf_fragment("msf:customer--catalog%3fpart")
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        lower.track_name,
        b"catalog?part".to_vec(),
        "%3f も %3F と同じ 0x3F になること"
    );

    // `%25` は 0x25 (`%`) のデータバイトになる (2 回デコードしない)
    let fragment =
        parse_msf_fragment("msf:ns--a%25b").expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(fragment.track_name, b"a%b".to_vec());

    // `%61` は 0x61 (`a`)、`%4A` は 0x4A (`J`)。`.` + hex の冗長 / 大文字規則は適用しない
    let fragment =
        parse_msf_fragment("msf:ns--%61%4A").expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(fragment.track_name, vec![0x61, 0x4A]);
}

/// ABNF (`pchar-no-amp / "/"`) が生のまま許す非リテラル文字はその ASCII バイトをデータとして取り出すこと
///
/// §11.1.2 (MSF Namespace-Name String Encoding) はリテラル以外を `.` + 小文字 hex 2 桁で
/// 書くことを MUST とするため、これらは正規形ではないが、ABNF が許す表現として受理する。
/// `is_uri_data_byte` がデータとして扱う 14 文字を 1 文字ずつ通し、検証 (`is_pchar_no_amp_byte`)
/// とデコードで扱いがずれないことを固定する。リテラルと構造文字の `-` / `.` / `%` は
/// 別のテストで固定する。
#[test]
fn parse_fragment_accepts_raw_pchar_no_amp_characters() {
    // unreserved のうち本層で意味を持つ `-` (区切り) / `.` (エスケープ) / `%` を除く `~`
    // と、`pchar-no-amp` の `:` `@` `sub-delims-no-amp`、および ABNF が加える `/`
    for (ch, byte) in [
        ('~', 0x7E),
        ('!', 0x21),
        ('$', 0x24),
        ('\'', 0x27),
        ('(', 0x28),
        (')', 0x29),
        ('*', 0x2A),
        ('+', 0x2B),
        (',', 0x2C),
        (';', 0x3B),
        ('=', 0x3D),
        (':', 0x3A),
        ('@', 0x40),
        ('/', 0x2F),
    ] {
        let fragment = parse_msf_fragment(&format!("msf:ns--a{ch}b"))
            .unwrap_or_else(|e| panic!("生の {ch} は受理されること: {e:?}"));
        assert_eq!(
            fragment.track_name,
            vec![b'a', byte, b'b'],
            "生の {ch} は {byte:#04X} のデータバイトになること"
        );
    }

    // ABNF が除外する `&` は track-identifier の終端 (パラメータ区切り) になる。
    // データとしての `&` は `%26` で表す。
    let fragment =
        parse_msf_fragment("msf:ns--a&x=y").expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        fragment.track_name,
        b"a".to_vec(),
        "生の & は track-identifier の終端になること"
    );
    assert_eq!(
        fragment.parameter_values("x"),
        vec!["y"],
        "& の後ろはパラメータとして解釈されること"
    );
    let fragment =
        parse_msf_fragment("msf:ns--a%26b").expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        fragment.track_name,
        b"a&b".to_vec(),
        "%26 は 0x26 のデータバイトになること"
    );
}

/// `%2D` は区切りではなくデータバイト 0x2D として扱われること
///
/// 構造の確定 (`-` のラン走査) を percent-decode より先に行うため、`%2D` は namespace の
/// フィールド区切りを生成しない (RFC 3986 §2.4 (When to Encode or Decode))。
#[test]
fn parse_fragment_percent_encoded_hyphen_is_data() {
    let fragment =
        parse_msf_fragment("msf:ns%2Da--catalog").expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        fragment.namespace,
        shiguredo_moqt::message::common::TrackNamespace::new(vec![b"ns-a".to_vec()])
            .expect("テストフィクスチャの前提条件を満たす"),
        "%2D は区切りではなくデータバイト 0x2D であること"
    );
    assert_eq!(fragment.track_name, b"catalog".to_vec());
}

/// `%2E` はデータバイト 0x2E (`.`) になり、`.` + hex の開始として再解釈されないこと
///
/// RFC 3986 §2.4 (When to Encode or Decode) の「同じ文字列を 2 回 decode しない」を満たす。
#[test]
fn parse_fragment_percent_encoded_period_is_data() {
    let fragment =
        parse_msf_fragment("msf:ns--%2E2d").expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        fragment.track_name,
        vec![0x2E, 0x32, 0x64],
        "%2E は 0x2E のデータバイトであり .2d として再解釈されないこと"
    );
}

/// `.` の直後に `%XX` が続く入力は拒否されること
///
/// 文字列へ畳み込んでから再パースすると `.2%33` が `.23` になり 1 バイト 0x23 として
/// 受理されてしまう。1 パスで走査するため `.` の直後は hex 2 桁でなければならない。
#[test]
fn parse_fragment_escape_followed_by_percent_rejected() {
    let err = parse_msf_fragment("msf:ns--.2%33").expect_err("畳み込みは受理しないこと");
    assert!(
        matches!(&err, MessageError::InvalidCatalog(reason) if reason.contains("InvalidEscape")),
        ". の直後の % は InvalidEscape であること: {err:?}"
    );
}

/// `.` + hex の既存規則は percent-encoding 対応後も維持されること
#[test]
fn parse_fragment_keeps_dot_escape_rules() {
    // §11.1.2 の表現: `.2f` は `/`
    let fragment =
        parse_msf_fragment("msf:ns--a.2fb").expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(fragment.track_name, b"a/b".to_vec());

    // 素の `.61` は冗長、素の `.4A` は大文字 hex として拒否する
    let err = parse_msf_fragment("msf:ns--.61").expect_err("冗長エンコードは拒否すること");
    assert!(
        matches!(&err, MessageError::InvalidCatalog(reason) if reason.contains("RedundantEncoding")),
        ".61 は RedundantEncoding であること: {err:?}"
    );
    let err = parse_msf_fragment("msf:ns--.4A").expect_err("大文字 hex は拒否すること");
    assert!(
        matches!(&err, MessageError::InvalidCatalog(reason) if reason.contains("UppercaseHex")),
        ".4A は UppercaseHex であること: {err:?}"
    );
}

/// パラメータ値の検証エラーは実値を含まないこと
///
/// パラメータ値は認可トークンなどの機密値を含み得るため、エラーメッセージには実値を
/// 含めず、パラメータ名と 1 始まりの位置を示す。
#[test]
fn parse_fragment_parameter_value_errors_do_not_leak_value() {
    // percent-encoding の不正 (`%zz` は hex 2 桁でない)
    let err = parse_msf_fragment("msf:ns--catalog&cat=%zz")
        .expect_err("不正な percent-encoding は拒否すること");
    let reason = invalid_catalog_reason(&err);
    assert!(!reason.contains("%zz"), "値を含めないこと: {reason}");
    assert!(
        reason.contains("MSF fragment parameter 'cat' value (#1)"),
        "名前と位置を示すこと: {reason}"
    );

    // 文字種の不正 (空白)
    let err = parse_msf_fragment("msf:ns--catalog&cat=secret value")
        .expect_err("空白を含む値は拒否すること");
    let reason = invalid_catalog_reason(&err);
    assert!(!reason.contains("secret"), "値を含めないこと: {reason}");
    assert!(
        reason.contains("MSF fragment parameter 'cat' value (#1)"),
        "名前と位置を示すこと: {reason}"
    );

    // 2 個目のパラメータでも名前と位置を示すこと
    let err = parse_msf_fragment("msf:ns--catalog&connection=q&cat=%zz")
        .expect_err("不正な percent-encoding は拒否すること");
    let reason = invalid_catalog_reason(&err);
    assert!(!reason.contains("%zz"), "値を含めないこと: {reason}");
    assert!(
        reason.contains("MSF fragment parameter 'cat' value (#2)"),
        "名前と位置を示すこと: {reason}"
    );
}

/// `=` の無いパラメータのエラーは要素全体を含まないこと
///
/// `=` が無い要素は名前と値を区別できないため、要素全体を埋め込まず位置だけを示す。
#[test]
fn parse_fragment_missing_equal_reports_position_only() {
    let err = parse_msf_fragment("msf:ns--catalog&catSECRET")
        .expect_err("`=` の無いパラメータは拒否すること");
    let reason = invalid_catalog_reason(&err);
    assert!(
        !reason.contains("catSECRET"),
        "要素を含めないこと: {reason}"
    );
    assert!(
        reason.contains("MSF fragment parameter #1 is missing '='"),
        "位置を示すこと: {reason}"
    );
}

/// track-identifier とパラメータ名は機密値でないため実値を示すこと
#[test]
fn parse_fragment_identifier_errors_include_value() {
    let err =
        parse_msf_fragment("msf:ns--cata?log").expect_err("track-identifier の `?` は拒否すること");
    let reason = invalid_catalog_reason(&err);
    assert!(
        reason.contains("MSF track identifier") && reason.contains("ns--cata?log"),
        "track-identifier を示すこと: {reason}"
    );

    let err = parse_msf_fragment("msf:ns--catalog&ca t=SECRET")
        .expect_err("パラメータ名の空白は拒否すること");
    let reason = invalid_catalog_reason(&err);
    assert!(
        reason.contains("MSF fragment parameter name") && reason.contains("ca t"),
        "パラメータ名を示すこと: {reason}"
    );
    assert!(
        !reason.contains("SECRET"),
        "パラメータ名の検証エラーでも値を示さないこと: {reason}"
    );
}

/// `MessageError::InvalidCatalog` からエラーメッセージを取り出す
fn invalid_catalog_reason(err: &MessageError) -> &str {
    match err {
        MessageError::InvalidCatalog(reason) => reason,
        other => panic!("InvalidCatalog になること: {other:?}"),
    }
}

/// Track Name が空の MSF fragment を拒否すること
///
/// draft-ietf-moq-transport-23 §2.4.1 (Track Naming) は Track Name に 1 バイト以上を
/// MUST とする。§8.8 のシリアライズ表現としては `ns--` も構成できるが、
/// MSF fragment から購読対象の Full Track Name を作る経路では拒否する。
#[test]
fn parse_fragment_rejects_empty_track_name() {
    for fragment in ["msf:ns--", "msf:--"] {
        assert!(
            matches!(
                parse_msf_fragment(fragment),
                Err(shiguredo_moqt::error::MessageError::InvalidCatalog(_))
            ),
            "空の Track Name を持つ fragment {fragment:?} を拒否すること"
        );
    }
    // 0 フィールド namespace + 非空 Track Name は正当
    let fragment =
        parse_msf_fragment("msf:--catalog").expect("0 フィールド namespace は正当である");
    assert_eq!(fragment.track_name, b"catalog");
}

/// authority / path / query が RFC 3986 の構文に従うこと
///
/// draft-ietf-moq-msf-02 §11.1 (URL construction and interpretation) の ABNF は
/// RFC 3986 の `authority` / `path-abempty` / `query` を import する。
/// 不正な成分は transport 層へ渡さず `InvalidCatalog` にする。
#[test]
fn parse_uri_rejects_invalid_rfc3986_components() {
    for uri in [
        // port が数字でない
        "moqt://host:abc/p#msf:ns--catalog",
        // path に空白
        "moqt://host/a b#msf:ns--catalog",
        // query に空白
        "moqt://host/p?q=a b#msf:ns--catalog",
        // path の percent-encoding が不正
        "moqt://host/%ZZ#msf:ns--catalog",
        // IP-literal の `]` が無い
        "moqt://[::1/p#msf:ns--catalog",
        // host が空
        "moqt://:443/p#msf:ns--catalog",
    ] {
        assert!(
            matches!(
                parse_msf_uri(uri),
                Err(shiguredo_moqt::error::MessageError::InvalidCatalog(_))
            ),
            "不正な RFC 3986 成分を含む URI {uri:?} を拒否すること"
        );
    }

    // IPv6 リテラル + port、path / query の `@` は正当
    let uri = parse_msf_uri("moqt://[::1]:443/a@b?x=a@b#msf:ns--catalog")
        .expect("正当な RFC 3986 成分の URI を受理すること");
    assert_eq!(uri.authority, "[::1]:443");
    assert_eq!(uri.path, "/a@b");
    assert_eq!(uri.query.as_deref(), Some("x=a@b"));

    // 空 query (`?` の直後が fragment) は `query = *(...)` の範囲で正当
    parse_msf_uri("moqt://host/p?#msf:ns--catalog").expect("空の query を受理すること");
}

/// userinfo を含む authority を拒否し、エラーに実値を含めないこと
///
/// RFC 3986 §3.2.1 (User Information) は `user:password` 形式を deprecated とし、
/// 受け取った reference 中の userinfo を拒否する選択を許す。MSF §11.1 の authority は
/// "host and optional port" であるため、`@` を含む authority を拒否する。
/// パスワードをログや Debug に残さないため、エラーメッセージには authority を埋め込まない。
#[test]
fn parse_uri_rejects_userinfo_without_leaking() {
    let uri = "moqt://user:secret@host/p#msf:ns--catalog";
    let error = parse_msf_uri(uri).expect_err("userinfo を含む URI を拒否すること");
    let message = error.to_string();
    assert!(
        !message.contains("secret") && !message.contains("user@"),
        "エラーメッセージに userinfo の実値を含めないこと: {message}"
    );
}

/// MSF の connectionURI が要求する MOQT endpoint URI の受理例
///
/// draft-ietf-moq-msf-02 §5.2.37 (Connection URI) の例と、
/// draft-ietf-moq-transport-23 §6.1 (MOQT URI Scheme) の構文を満たす値を確認する。
#[test]
fn moqt_endpoint_uri_examples_are_valid() {
    for uri in [
        "moqt://logs.example.com",
        "moqt://metrics.example.com:8443",
        "moqt://logs.example.com/moqt?a=1&b=2",
        "MOQT://example.com",
        "moqt://example.com/path#msf:ns--catalog",
        "moqt://[::1]:443/path",
    ] {
        assert!(
            validate_moqt_endpoint_uri(uri).is_ok(),
            "有効な MOQT endpoint URI として受理すること: {uri}"
        );
    }
}

/// MSF の connectionURI が要求する MOQT endpoint URI の拒否例
#[test]
fn moqt_endpoint_uri_invalid_values_are_rejected() {
    for uri in [
        "https://example.com",
        "moqt://",
        "moqt:///path",
        "moqt://user:pass@example.com",
        "moqt://host:abc",
        "moqt://host/a b",
        "moqt://host?q=a b",
        "moqt://[::1/path",
        "moqt://host#badfragment",
        "moqt://host#Type:value",
        "moqt://host#type:",
        "moqt://host#type:value#extra",
    ] {
        assert!(
            matches!(
                validate_moqt_endpoint_uri(uri),
                Err(MessageError::InvalidCatalog(_))
            ),
            "不正な MOQT endpoint URI として拒否すること: {uri}"
        );
    }
}

/// 同じ URL に複数の wallclock-range がある場合、仕様どおり union されること
///
/// draft-ietf-moq-msf-02 §11.1.1 (Reserved fragment parameters)。
#[test]
fn wallclock_ranges_are_unioned() {
    let uri = parse_msf_uri(
        "moqt://example.com/path#msf:ns--catalog&wallclock-range=0-10&wallclock-range=5-20&wallclock-range=30-40",
    )
    .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        uri.fragment
            .wallclock_range_union()
            .expect("テストフィクスチャの前提条件を満たす"),
        vec![
            MsfTimeRange {
                start_ms: 0,
                end_ms: Some(20)
            },
            MsfTimeRange {
                start_ms: 30,
                end_ms: Some(40)
            }
        ]
    );
}

/// 隣接する範囲と終端なしの範囲も union されること
#[test]
fn adjacent_and_open_time_ranges_are_unioned() {
    let uri = parse_msf_uri(
        "moqt://example.com/path#msf:ns--catalog&mediatime-range=0-10&mediatime-range=11-20&mediatime-range=15-",
    )
    .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        uri.fragment
            .mediatime_range_union()
            .expect("テストフィクスチャの前提条件を満たす"),
        vec![MsfTimeRange {
            start_ms: 0,
            end_ms: None
        }]
    );
}

/// union の対象でも範囲の構文エラーは拒否すること
#[test]
fn invalid_time_range_union_is_rejected() {
    let uri = parse_msf_uri("moqt://example.com/path#msf:ns--catalog&wallclock-range=10-5")
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(matches!(
        uri.fragment.wallclock_range_union(),
        Err(MessageError::InvalidCatalog(_))
    ));
}

/// 同じ URL に複数の location-range がある場合、Group ID / Object ID の辞書順で union されること
#[test]
fn location_ranges_are_unioned() {
    let uri = parse_msf_uri(
        "moqt://example.com/path#msf:ns--catalog&location-range=34.0-2145.16&location-range=100-200",
    )
    .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        uri.fragment
            .location_range_union()
            .expect("テストフィクスチャの前提条件を満たす"),
        vec![MsfLocationRange {
            start_group_id: 34,
            start_object_id: None,
            end: Some(MsfLocationRangeEnd {
                group_id: 2145,
                object_id: Some(16)
            })
        }]
    );
}

/// 隣接する Group 全体と Object 単位の範囲も union されること
#[test]
fn adjacent_location_ranges_are_unioned() {
    let uri = parse_msf_uri(
        "moqt://example.com/path#msf:ns--catalog&location-range=16-24&location-range=25-30",
    )
    .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        uri.fragment
            .location_range_union()
            .expect("テストフィクスチャの前提条件を満たす"),
        vec![MsfLocationRange {
            start_group_id: 16,
            start_object_id: None,
            end: Some(MsfLocationRangeEnd {
                group_id: 30,
                object_id: None
            })
        }]
    );

    let uri = parse_msf_uri(
        "moqt://example.com/path#msf:ns--catalog&location-range=10.5-10.9&location-range=10.10-10.12",
    )
    .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        uri.fragment
            .location_range_union()
            .expect("テストフィクスチャの前提条件を満たす"),
        vec![MsfLocationRange {
            start_group_id: 10,
            start_object_id: Some(5),
            end: Some(MsfLocationRangeEnd {
                group_id: 10,
                object_id: Some(12)
            })
        }]
    );
}

/// 終端なしの location-range が後続の範囲を吸収すること
#[test]
fn open_location_range_absorbs_later_ranges() {
    let uri = parse_msf_uri(
        "moqt://example.com/path#msf:ns--catalog&location-range=16.24&location-range=17-20",
    )
    .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        uri.fragment
            .location_range_union()
            .expect("テストフィクスチャの前提条件を満たす"),
        vec![MsfLocationRange {
            start_group_id: 16,
            start_object_id: Some(24),
            end: None
        }]
    );
}
