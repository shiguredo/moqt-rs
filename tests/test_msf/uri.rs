use super::*;
use shiguredo_moqt::msf::uri::{
    MsfConnectionType, MsfLocationRange, MsfLocationRangeEnd, MsfTimeRange, parse_msf_fragment,
    parse_msf_uri,
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

/// c4m パラメータがそのまま返されること
#[test]
fn parse_c4m_parameter() {
    let fragment = parse_msf_fragment("msf:ns--catalog&c4m=gqhkYWxn")
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(fragment.c4m_tokens(), vec!["gqhkYWxn"]);
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

/// draft-ietf-moq-msf-01 §11.1 (URL construction and interpretation): scheme は
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

/// draft-ietf-moq-msf-01 §11.1.1 (Reserved fragment parameters): location-range は
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
