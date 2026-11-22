//! MOQMETRICS の Catalog Header / Metric Description / Capture Info / Metric Value と
//! Track Naming のテスト
//!
//! draft-jennings-moq-metrics-03 §4 (Metrics Data Model) / §5.1 (Track Naming) /
//! §5.2 (Granularity Levels) / §5.3 (Catalog Track) / §5.4 (Groups and Objects) と、
//! MSF-02 §10.4 (Metrics track catalog requirements) の整合を検証する。

use shiguredo_moqt::{
    error::MessageError,
    msf::{
        MsfCatalog, MsfCatalogDocument, MsfCompression, MsfPackaging, MsfTrack,
        metrics::{
            CATALOG_FORMAT_VERSION, CaptureInfo, CatalogHeader, METRICS_CATALOG_TRACK_NAME,
            METRICS_TRACK_PACKAGING, METRICS_TRACK_ROLE, MetricAttribute, MetricDescription,
            MetricGranularity, MetricType, MetricValue, ValueRepresentation,
            metrics_track_namespace, metrics_track_namespace_string,
        },
    },
    varint,
};

/// Catalog Header の wire 形式を任意の属性で組み立てる
fn catalog_header_wire(version: u64, attributes: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut buf = Vec::new();
    varint::encode(version, &mut buf);
    varint::encode(attributes.len() as u64, &mut buf);
    for (key, value) in attributes {
        varint::encode(key.len() as u64, &mut buf);
        buf.extend_from_slice(key);
        varint::encode(value.len() as u64, &mut buf);
        buf.extend_from_slice(value);
    }
    buf
}

/// Metric Description の wire 形式を任意の Metric Type / Value Representation で組み立てる
fn metric_description_wire(
    metric_type: u64,
    value_representation: u64,
    name: &[u8],
    unit: &[u8],
) -> Vec<u8> {
    let mut buf = Vec::new();
    varint::encode(metric_type, &mut buf);
    varint::encode(value_representation, &mut buf);
    varint::encode(name.len() as u64, &mut buf);
    buf.extend_from_slice(name);
    varint::encode(unit.len() as u64, &mut buf);
    buf.extend_from_slice(unit);
    buf
}

/// Capture Info の wire 形式を任意の値で組み立てる
fn capture_info_wire(capture_timestamp_offset: u64, catalog_version: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    varint::encode(capture_timestamp_offset, &mut buf);
    varint::encode(catalog_version, &mut buf);
    buf
}

/// §5.3 の Catalog Header のフィールド順と属性の往復を検証する
#[test]
fn catalog_header_roundtrip() {
    // draft-jennings-moq-metrics-03 §5.3 (Catalog Track) Figure 1:
    // Version / Number of Attributes / Attribute (Key Length / Key / Value Length / Value)。
    let header = CatalogHeader {
        version: CATALOG_FORMAT_VERSION,
        attributes: vec![
            MetricAttribute {
                key: String::from("location"),
                value: String::from("us-east-2"),
            },
            MetricAttribute {
                key: String::from("os"),
                value: String::from("ubuntu20.4"),
            },
        ],
    };
    let encoded = header.encode();
    assert_eq!(
        CatalogHeader::decode(&encoded).expect("エンコード結果をデコードできること"),
        header,
        "Catalog Header の往復"
    );

    // 属性なしは Version と Number of Attributes の 2 バイトになる。
    let empty = CatalogHeader {
        version: 1,
        attributes: Vec::new(),
    };
    assert_eq!(
        empty.encode(),
        vec![0x01, 0x00],
        "属性なしの Catalog Header"
    );
}

/// Catalog Header の属性が UTF-8 でない payload を拒否する
#[test]
fn catalog_header_rejects_invalid_utf8_attribute() {
    let invalid = catalog_header_wire(1, &[(&[0xFF], b"v")]);
    assert!(
        matches!(
            CatalogHeader::decode(&invalid),
            Err(MessageError::InvalidMsfPayload(_))
        ),
        "UTF-8 でない属性 key は拒否されること"
    );

    let invalid = catalog_header_wire(1, &[(b"k", &[0xFF])]);
    assert!(
        matches!(
            CatalogHeader::decode(&invalid),
            Err(MessageError::InvalidMsfPayload(_))
        ),
        "UTF-8 でない属性 value は拒否されること"
    );
}

/// §5.3 の Metric Description の往復と代表的な値の組み合わせを検証する
#[test]
fn metric_description_roundtrip() {
    // draft-jennings-moq-metrics-03 §5.10.1 (Catalog) の例に対応する 2 つの metric。
    let gauge_unsigned = MetricDescription {
        metric_index: 1,
        metric_type: MetricType::Gauge,
        value_representation: ValueRepresentation::Unsigned,
        name: String::from("cpu_usage_percentage"),
        unit: String::from("%"),
    };
    let encoded = gauge_unsigned
        .encode()
        .expect("テストフィクスチャは §5.3 を満たすこと");
    assert_eq!(
        MetricDescription::decode(&encoded, 1).expect("Gauge / unsigned の Metric Description"),
        gauge_unsigned
    );

    let gauge_float = MetricDescription {
        metric_index: 2,
        metric_type: MetricType::Gauge,
        value_representation: ValueRepresentation::Float,
        name: String::from("cpu_temperature"),
        unit: String::from("Cel"),
    };
    let encoded = gauge_float
        .encode()
        .expect("テストフィクスチャは §5.3 を満たすこと");
    assert_eq!(
        MetricDescription::decode(&encoded, 2).expect("Gauge / float の Metric Description"),
        gauge_float
    );

    // Metric Type 0x1 (Counter) / Value Representation 0x1 (signed) の wire 値を固定する。
    let counter_signed = MetricDescription {
        metric_index: 3,
        metric_type: MetricType::Counter,
        value_representation: ValueRepresentation::Signed,
        name: String::from("queue:depth"),
        unit: String::new(),
    };
    let encoded = counter_signed
        .encode()
        .expect("テストフィクスチャは §5.3 を満たすこと");
    assert_eq!(
        encoded,
        vec![
            0x01, 0x01, 0x0B, b'q', b'u', b'e', b'u', b'e', b':', b'd', b'e', b'p', b't', b'h',
            0x00
        ],
        "Metric Type / Value Representation / Name / Unit の順"
    );
}

/// Metric Index は Metric Description の payload に含まれない
#[test]
fn metric_description_index_is_not_encoded() {
    // draft-jennings-moq-metrics-03 §5.3 (Catalog Track): "It is not encoded in the
    // Metric Description itself."
    let base = MetricDescription {
        metric_index: 1,
        metric_type: MetricType::Gauge,
        value_representation: ValueRepresentation::Unsigned,
        name: String::from("cpu"),
        unit: String::from("%"),
    };
    let mut other = base.clone();
    other.metric_index = 42;
    assert_eq!(
        base.encode()
            .expect("Metric Description をエンコードできること"),
        other
            .encode()
            .expect("Metric Description をエンコードできること"),
        "Metric Index が異なっても payload は同じになること"
    );

    let decoded = MetricDescription::decode(
        &base
            .encode()
            .expect("Metric Description をエンコードできること"),
        42,
    )
    .expect("Metric Description をデコードできること");
    assert_eq!(
        decoded.metric_index, 42,
        "Object ID が Metric Index になること"
    );
}

/// Metric Type の未知値を拒否する
#[test]
fn unknown_metric_type_rejected() {
    // draft-jennings-moq-metrics-03 §5.3 (Catalog Track): "0x0 for Gauge, 0x1 for Counter".
    assert!(MetricType::try_from(0).is_ok());
    assert!(MetricType::try_from(1).is_ok());
    assert!(
        matches!(
            MetricType::try_from(2),
            Err(MessageError::InvalidMsfPayload(_))
        ),
        "Metric Type 2 は拒否されること"
    );

    let invalid = metric_description_wire(2, 0, b"cpu", b"");
    assert!(
        matches!(
            MetricDescription::decode(&invalid, 1),
            Err(MessageError::InvalidMsfPayload(_))
        ),
        "decode でも Metric Type 2 は拒否されること"
    );
}

/// Value Representation の未知値を拒否する
#[test]
fn unknown_value_representation_rejected() {
    // draft-jennings-moq-metrics-03 §5.3 (Catalog Track): 0x0 unsigned / 0x1 signed /
    // 0x2 floating point。
    assert!(ValueRepresentation::try_from(0).is_ok());
    assert!(ValueRepresentation::try_from(1).is_ok());
    assert!(ValueRepresentation::try_from(2).is_ok());
    assert!(
        matches!(
            ValueRepresentation::try_from(3),
            Err(MessageError::InvalidMsfPayload(_))
        ),
        "Value Representation 3 は拒否されること"
    );

    let invalid = metric_description_wire(0, 3, b"cpu", b"");
    assert!(
        matches!(
            MetricDescription::decode(&invalid, 1),
            Err(MessageError::InvalidMsfPayload(_))
        ),
        "decode でも Value Representation 3 は拒否されること"
    );
}

/// Metric Index 0 を拒否する
#[test]
fn metric_description_rejects_zero_index() {
    // draft-jennings-moq-metrics-03 §5.3 (Catalog Track): Object ID 0 は Catalog Header で
    // あり、Metric Description は N > 0 の Object ID に載る。
    let description = MetricDescription {
        metric_index: 0,
        metric_type: MetricType::Gauge,
        value_representation: ValueRepresentation::Unsigned,
        name: String::from("cpu"),
        unit: String::new(),
    };
    assert!(
        matches!(
            description.encode(),
            Err(MessageError::InvalidMsfPayload(_))
        ),
        "Metric Index 0 は encode で拒否されること"
    );

    let encoded = metric_description_wire(0, 0, b"cpu", b"");
    assert!(
        matches!(
            MetricDescription::decode(&encoded, 0),
            Err(MessageError::InvalidMsfPayload(_))
        ),
        "Metric Index 0 は decode で拒否されること"
    );
}

/// Metric 名の文字種違反を拒否する
#[test]
fn invalid_metric_name_rejected() {
    // draft-jennings-moq-metrics-03 §4 (Metrics Data Model): metric 名は ASCII の英字・
    // 数字・`_`・`:` から成る。
    let mut description = MetricDescription {
        metric_index: 1,
        metric_type: MetricType::Gauge,
        value_representation: ValueRepresentation::Unsigned,
        name: String::from("cpu usage"),
        unit: String::new(),
    };
    assert!(
        matches!(
            description.encode(),
            Err(MessageError::InvalidMsfPayload(_))
        ),
        "空白を含む Metric 名は encode で拒否されること"
    );

    description.name = String::from("cpu-usage");
    assert!(
        matches!(
            description.encode(),
            Err(MessageError::InvalidMsfPayload(_))
        ),
        "ハイフンを含む Metric 名は encode で拒否されること"
    );

    let invalid = metric_description_wire(0, 0, "cpu温度".as_bytes(), b"");
    assert!(
        matches!(
            MetricDescription::decode(&invalid, 1),
            Err(MessageError::InvalidMsfPayload(_))
        ),
        "非 ASCII の Metric 名は decode で拒否されること"
    );
}

/// §5.4 の Capture Info の往復と Capture Timestamp Offset の境界を検証する
#[test]
fn capture_info_roundtrip_and_boundaries() {
    // draft-jennings-moq-metrics-03 §5.4 (Groups and Objects): Capture Timestamp Offset は
    // Group ID (ミリ秒) に加算するナノ秒オフセットで、1,000,000 未満である。
    let info = CaptureInfo {
        capture_timestamp_offset: 123_456,
        catalog_version: 1,
    };
    let encoded = info
        .encode()
        .expect("テストフィクスチャは §5.4 を満たすこと");
    assert_eq!(
        encoded,
        capture_info_wire(123_456, 1),
        "Capture Timestamp Offset / Catalog Version の順"
    );
    assert_eq!(
        CaptureInfo::decode(&encoded).expect("Capture Info をデコードできること"),
        info
    );

    let max = CaptureInfo {
        capture_timestamp_offset: 999_999,
        catalog_version: 1,
    };
    let encoded = max.encode().expect("上限未満の Capture Timestamp Offset");
    assert_eq!(
        CaptureInfo::decode(&encoded).expect("上限未満の Capture Timestamp Offset"),
        max
    );

    let over = CaptureInfo {
        capture_timestamp_offset: 1_000_000,
        catalog_version: 1,
    };
    assert!(
        matches!(over.encode(), Err(MessageError::InvalidMsfPayload(_))),
        "1,000,000 以上の Capture Timestamp Offset は encode で拒否されること"
    );
    let invalid = capture_info_wire(1_000_000, 1);
    assert!(
        matches!(
            CaptureInfo::decode(&invalid),
            Err(MessageError::InvalidMsfPayload(_))
        ),
        "1,000,000 以上の Capture Timestamp Offset は decode で拒否されること"
    );
}

/// §5.4 の unsigned Metric Value は vi64 である
#[test]
fn metric_value_unsigned() {
    let value = MetricValue::Unsigned {
        metric_index: 1,
        value: 99,
    };
    let encoded = value.encode().expect("Metric Value をエンコードできること");
    assert_eq!(
        encoded,
        vec![0x63],
        "unsigned は vi64 でエンコードされること"
    );
    assert_eq!(
        MetricValue::decode(&encoded, 1, ValueRepresentation::Unsigned)
            .expect("unsigned の Metric Value をデコードできること"),
        value
    );
    assert_eq!(value.representation(), ValueRepresentation::Unsigned);
    assert_eq!(value.metric_index(), 1);
}

/// §5.4 の signed Metric Value は 64-bit two's complement の network byte order である
#[test]
fn metric_value_signed() {
    let value = MetricValue::Signed {
        metric_index: 2,
        value: -2,
    };
    let encoded = value.encode().expect("Metric Value をエンコードできること");
    assert_eq!(
        encoded,
        vec![0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFE],
        "signed は 64-bit two's complement の big-endian であること"
    );
    assert_eq!(
        MetricValue::decode(&encoded, 2, ValueRepresentation::Signed)
            .expect("signed の Metric Value をデコードできること"),
        value
    );
}

/// §5.4 の float Metric Value は IEEE 754 binary64 の network byte order である
#[test]
fn metric_value_float() {
    let value = MetricValue::Float {
        metric_index: 2,
        value: 45.1,
    };
    let encoded = value.encode().expect("Metric Value をエンコードできること");
    assert_eq!(
        encoded,
        45.1f64.to_be_bytes().to_vec(),
        "float は IEEE 754 binary64 の big-endian であること"
    );
    let decoded = MetricValue::decode(&encoded, 2, ValueRepresentation::Float)
        .expect("float の Metric Value をデコードできること");
    match decoded {
        MetricValue::Float {
            metric_index,
            value,
        } => {
            assert_eq!(metric_index, 2);
            assert_eq!(value.to_bits(), 45.1f64.to_bits(), "ビット列が一致すること");
        }
        other => panic!("Float として復号されること: {other:?}"),
    }
}

/// Metric Index は Metric Value の payload に含まれない
#[test]
fn metric_value_index_is_not_encoded() {
    // draft-jennings-moq-metrics-03 §5.4 (Groups and Objects): Object ID N (N > 0) の
    // payload 全体が Metric Value であり、Metric Index は payload に含まれない。
    let base = MetricValue::Unsigned {
        metric_index: 1,
        value: 42,
    };
    let other = MetricValue::Unsigned {
        metric_index: 7,
        value: 42,
    };
    assert_eq!(
        base.encode().expect("Metric Value をエンコードできること"),
        other.encode().expect("Metric Value をエンコードできること"),
        "Metric Index が異なっても payload は同じになること"
    );

    let decoded = MetricValue::decode(
        &base.encode().expect("Metric Value をエンコードできること"),
        7,
        ValueRepresentation::Unsigned,
    )
    .expect("Metric Value をデコードできること");
    assert_eq!(
        decoded.metric_index(),
        7,
        "data group の Object ID が Metric Index になること"
    );
}

/// Value Representation と payload 長が合わない Metric Value を拒否する
#[test]
fn metric_value_representation_mismatch_rejected() {
    // draft-jennings-moq-metrics-03 §5.4 (Groups and Objects): signed integer は 64-bit、
    // floating point は IEEE 754 binary64 である。unsigned の 1 バイト payload を
    // signed / float として解釈することはできない。
    let unsigned = MetricValue::Unsigned {
        metric_index: 1,
        value: 99,
    };
    let encoded = unsigned
        .encode()
        .expect("Metric Value をエンコードできること");
    for representation in [ValueRepresentation::Signed, ValueRepresentation::Float] {
        assert!(
            matches!(
                MetricValue::decode(&encoded, 1, representation),
                Err(MessageError::InvalidMsfPayload(_))
            ),
            "8 バイト未満の payload は {representation:?} として拒否されること"
        );
    }
}

/// Metric Index 0 を拒否する
#[test]
fn metric_value_rejects_zero_index() {
    // draft-jennings-moq-metrics-03 §5.4 (Groups and Objects): Object ID 0 は Capture Info
    // であり、Metric Value は N > 0 の Object ID に載る。
    let value = MetricValue::Unsigned {
        metric_index: 0,
        value: 1,
    };
    assert!(
        matches!(value.encode(), Err(MessageError::InvalidMsfPayload(_))),
        "Metric Index 0 は encode で拒否されること"
    );
    assert!(
        matches!(
            MetricValue::decode(&[0x01], 0, ValueRepresentation::Unsigned),
            Err(MessageError::InvalidMsfPayload(_))
        ),
        "Metric Index 0 は decode で拒否されること"
    );
}

/// Metric Type / Value Representation / catalog 形式 version の定数を固定する
#[test]
fn wire_value_constants() {
    // draft-jennings-moq-metrics-03 §5.3 (Catalog Track).
    assert_eq!(MetricType::Gauge.value(), 0x0);
    assert_eq!(MetricType::Counter.value(), 0x1);
    assert_eq!(ValueRepresentation::Unsigned.value(), 0x0);
    assert_eq!(ValueRepresentation::Signed.value(), 0x1);
    assert_eq!(ValueRepresentation::Float.value(), 0x2);
    assert_eq!(
        CATALOG_FORMAT_VERSION, 1,
        "本仕様が定義する catalog 形式 version"
    );
}

/// §5.1 / §5.2 の granularity と Track Name が往復すること
#[test]
fn granularity_track_names() {
    // draft-jennings-moq-metrics-03 §5.1 (Track Naming) / §5.2 (Granularity Levels):
    // data track の Track Name は 1 桁の ASCII 数字 ("0"〜"7") である。
    let cases = [
        (MetricGranularity::Emergency, "0"),
        (MetricGranularity::Alert, "1"),
        (MetricGranularity::Critical, "2"),
        (MetricGranularity::Error, "3"),
        (MetricGranularity::Warning, "4"),
        (MetricGranularity::Notice, "5"),
        (MetricGranularity::Informational, "6"),
        (MetricGranularity::Debug, "7"),
    ];
    for (granularity, track_name) in cases {
        assert_eq!(
            granularity.track_name(),
            track_name,
            "granularity の Track Name"
        );
        assert_eq!(
            MetricGranularity::from_track_name(track_name.as_bytes()).expect("既知の Track Name"),
            granularity,
            "Track Name から granularity を復元できること"
        );
    }

    for invalid in [&b""[..], b"00", b"8", b"a", b"-1", b" 0"] {
        assert!(
            matches!(
                MetricGranularity::from_track_name(invalid),
                Err(MessageError::InvalidMsfPayload(_))
            ),
            "1 桁の 0〜7 以外の Track Name は拒否されること"
        );
    }
}

/// §5.1 の Track Namespace tuple と MSF-02 §5.2.2 の文字列写像を固定する
#[test]
fn namespace_and_msf_string() {
    // draft-jennings-moq-metrics-03 §5.1 (Track Naming): 3 field の tuple であり、
    // ResourceID は 16 桁の小文字 16 進 ASCII である (§4.1)。
    let namespace = metrics_track_namespace(0x8f2c4a1b0e9d7c63);
    let fields = namespace.fields();
    assert_eq!(fields.len(), 3);
    assert_eq!(fields[0], b"metrics.moq.arpa".to_vec());
    assert_eq!(fields[1], b"v1".to_vec());
    assert_eq!(fields[2], b"8f2c4a1b0e9d7c63".to_vec());

    // draft-ietf-moq-msf-02 §5.2.2 (Track namespace): `/` 区切りで連結する。
    assert_eq!(
        metrics_track_namespace_string(0x8f2c4a1b0e9d7c63),
        "metrics.moq.arpa/v1/8f2c4a1b0e9d7c63"
    );
    // ResourceID 0 でも 16 桁にゼロ埋めされること。
    assert_eq!(
        metrics_track_namespace_string(0),
        "metrics.moq.arpa/v1/0000000000000000"
    );
    // draft-jennings-moq-metrics-03 §5.1 (Track Naming): catalog track の Track Name。
    assert_eq!(METRICS_CATALOG_TRACK_NAME, b"catalog");
}

/// MSF-02 §10.4 の packaging / role と定数が一致し、catalog にそのまま出力できること
#[test]
fn msf_catalog_requirement_values() {
    // draft-ietf-moq-msf-02 §10.4 (Metrics track catalog requirements): packaging
    // "moqmetrics" と role "metrics" を持たなければならない。
    assert_eq!(METRICS_TRACK_PACKAGING, MsfPackaging::MoqMetrics);
    assert_eq!(METRICS_TRACK_ROLE, "metrics");

    let mut track = MsfTrack::new(String::from("4"), METRICS_TRACK_PACKAGING, true);
    track.namespace = Some(metrics_track_namespace_string(0x8f2c4a1b0e9d7c63));
    track.role = Some(String::from(METRICS_TRACK_ROLE));
    let mut catalog = MsfCatalog::new();
    catalog.publish_tracks.push(track);
    let encoded = MsfCatalogDocument::Full(catalog)
        .encode(MsfCompression::None)
        .expect("moqmetrics publishTrack を持つカタログはエンコードできること");
    let text = core::str::from_utf8(&encoded).expect("カタログは UTF-8 の JSON であること");
    assert!(
        text.contains(r#""packaging":"moqmetrics""#),
        "catalog の packaging が moqmetrics であること"
    );
    assert!(
        text.contains(r#""role":"metrics""#),
        "catalog の role が metrics であること"
    );
    assert!(
        text.contains(r#""namespace":"metrics.moq.arpa/v1/8f2c4a1b0e9d7c63""#),
        "catalog の namespace が MOQMETRICS-03 §5.1 の tuple の写像であること"
    );
}
