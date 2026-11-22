//! MOQLOG の Log Record / Track Naming のテスト
//!
//! draft-jennings-moq-log-04 §5.1 (Track Naming) / §5.3 (Log Record) と、
//! MSF-02 §9.4 (Log track catalog requirements) の整合を検証する。

use shiguredo_moqt::{
    error::MessageError,
    msf::{
        MSF_VERSION, MsfCatalog, MsfCatalogDocument, MsfCompression, MsfPackaging, MsfTrack,
        log::{
            LOG_TRACK_PACKAGING, LOG_TRACK_ROLE, LogRecord, LogRecordAttribute, LogSeverity,
            log_track_namespace, log_track_namespace_string,
        },
    },
    varint,
};

/// 任意のフィールドから Log Record の wire 形式を組み立てる
///
/// encode が MUST 違反を拒否するため、decode の検証には手組みの wire を使う。
fn wire(
    timestamp_offset: u64,
    facility: u64,
    identity: [&[u8]; 4],
    message: &[u8],
    attributes: &[(&[u8], &[u8])],
) -> Vec<u8> {
    let mut buf = Vec::new();
    varint::encode(timestamp_offset, &mut buf);
    varint::encode(facility, &mut buf);
    for field in identity.into_iter().chain([message]) {
        varint::encode(field.len() as u64, &mut buf);
        buf.extend_from_slice(field);
    }
    varint::encode(attributes.len() as u64, &mut buf);
    for (key, value) in attributes {
        varint::encode(key.len() as u64, &mut buf);
        buf.extend_from_slice(key);
        varint::encode(value.len() as u64, &mut buf);
        buf.extend_from_slice(value);
    }
    buf
}

/// §5.3 のフィールド順と各フィールドの wire 表現を固定する
#[test]
fn exact_wire_format() {
    // draft-jennings-moq-log-04 §5.3 (Log Record): Timestamp Offset / Facility /
    // Hostname / App Name / Proc ID / Msg ID / Message / Number of Attributes / Attribute
    // の順である。
    let record = LogRecord {
        timestamp_offset: 0,
        facility: 1,
        hostname: String::from("h"),
        app_name: String::from("a"),
        proc_id: String::from("p"),
        msg_id: String::from("m"),
        message: String::from("hi"),
        attributes: vec![LogRecordAttribute {
            key: String::from("k"),
            value: String::from("v"),
        }],
    };
    let encoded = record
        .encode()
        .expect("テストフィクスチャは §5.3 を満たすこと");
    assert_eq!(
        encoded,
        vec![
            0x00, // Timestamp Offset
            0x01, // Facility
            0x01, b'h', // Hostname Length / Hostname
            0x01, b'a', // App Name Length / App Name
            0x01, b'p', // Proc ID Length / Proc ID
            0x01, b'm', // Msg ID Length / Msg ID
            0x02, b'h', b'i', // Message Length / Message
            0x01, // Number of Attributes
            0x01, b'k', // Key Length / Key
            0x01, b'v', // Value Length / Value
        ],
        "§5.3 のフィールド順でエンコードされること"
    );

    let decoded = LogRecord::decode(&encoded).expect("エンコード結果をデコードできること");
    assert_eq!(decoded, record, "復号結果が元の Log Record と一致すること");
}

/// Timestamp Offset の上限値 999,999,999 は受理し、1,000,000,000 は拒否する
#[test]
fn timestamp_offset_boundaries() {
    // draft-jennings-moq-log-04 §5.3 (Log Record): "The value is less than 1000000000."
    let mut record = LogRecord {
        timestamp_offset: 999_999_999,
        facility: 0,
        hostname: String::new(),
        app_name: String::new(),
        proc_id: String::new(),
        msg_id: String::new(),
        message: String::new(),
        attributes: Vec::new(),
    };
    let encoded = record
        .encode()
        .expect("上限未満の Timestamp Offset は受理されること");
    assert_eq!(
        LogRecord::decode(&encoded).expect("上限未満の Timestamp Offset は復号できること"),
        record
    );

    record.timestamp_offset = 1_000_000_000;
    assert!(
        matches!(record.encode(), Err(MessageError::InvalidMsfPayload(_))),
        "1,000,000,000 以上の Timestamp Offset は encode で拒否されること"
    );
    let invalid = wire(1_000_000_000, 0, [b"", b"", b"", b""], b"", &[]);
    assert!(
        matches!(
            LogRecord::decode(&invalid),
            Err(MessageError::InvalidMsfPayload(_))
        ),
        "1,000,000,000 以上の Timestamp Offset は decode で拒否されること"
    );
}

/// Facility の上限値 23 は受理し、24 は拒否する
#[test]
fn facility_boundaries() {
    // draft-jennings-moq-log-04 §5.3 (Log Record): "from 0 to 23".
    let mut record = LogRecord {
        timestamp_offset: 0,
        facility: 23,
        hostname: String::new(),
        app_name: String::new(),
        proc_id: String::new(),
        msg_id: String::new(),
        message: String::new(),
        attributes: Vec::new(),
    };
    let encoded = record.encode().expect("Facility 23 は受理されること");
    assert_eq!(
        LogRecord::decode(&encoded).expect("Facility 23 は復号できること"),
        record
    );

    record.facility = 24;
    assert!(
        matches!(record.encode(), Err(MessageError::InvalidMsfPayload(_))),
        "Facility 24 は encode で拒否されること"
    );
    let invalid = wire(0, 24, [b"", b"", b"", b""], b"", &[]);
    assert!(
        matches!(
            LogRecord::decode(&invalid),
            Err(MessageError::InvalidMsfPayload(_))
        ),
        "Facility 24 は decode で拒否されること"
    );
}

/// Hostname / App Name / Proc ID / Msg ID の非 ASCII 値を拒否する
#[test]
fn non_ascii_identity_fields_rejected() {
    // draft-jennings-moq-log-04 §5.3 (Log Record): HOSTNAME / APP-NAME / PROCID / MSGID は
    // ASCII 文字列である。
    let mut record = LogRecord {
        timestamp_offset: 0,
        facility: 0,
        hostname: String::from("ホスト"),
        app_name: String::new(),
        proc_id: String::new(),
        msg_id: String::new(),
        message: String::new(),
        attributes: Vec::new(),
    };
    assert!(
        matches!(record.encode(), Err(MessageError::InvalidMsfPayload(_))),
        "非 ASCII の Hostname は encode で拒否されること"
    );

    let invalid = wire(0, 0, ["ホスト".as_bytes(), b"", b"", b""], b"", &[]);
    assert!(
        matches!(
            LogRecord::decode(&invalid),
            Err(MessageError::InvalidMsfPayload(_))
        ),
        "非 ASCII の Hostname は decode で拒否されること"
    );

    record.hostname = String::new();
    record.app_name = String::from("アプリ");
    assert!(
        matches!(record.encode(), Err(MessageError::InvalidMsfPayload(_))),
        "非 ASCII の App Name は encode で拒否されること"
    );

    let invalid = wire(0, 0, [b"", "アプリ".as_bytes(), b"", b""], b"", &[]);
    assert!(
        matches!(
            LogRecord::decode(&invalid),
            Err(MessageError::InvalidMsfPayload(_))
        ),
        "非 ASCII の App Name は decode で拒否されること"
    );
}

/// Message が UTF-8 でない payload を拒否する
#[test]
fn invalid_utf8_message_rejected() {
    // draft-jennings-moq-log-04 §5.3 (Log Record): Message は UTF-8 文字列である。
    let invalid = wire(0, 0, [b"", b"", b"", b""], b"\xFF", &[]);
    assert!(
        matches!(
            LogRecord::decode(&invalid),
            Err(MessageError::InvalidMsfPayload(_))
        ),
        "UTF-8 でない Message は decode で拒否されること"
    );
}

/// Attribute の key / value が UTF-8 でない payload を拒否する
#[test]
fn invalid_utf8_attribute_rejected() {
    // draft-jennings-moq-log-04 §5.3 (Log Record): Attribute の Key と Value は UTF-8 である。
    let invalid = wire(0, 0, [b"", b"", b"", b""], b"", &[(&[0xFF], b"v")]);
    assert!(
        matches!(
            LogRecord::decode(&invalid),
            Err(MessageError::InvalidMsfPayload(_))
        ),
        "UTF-8 でない Attribute の key は decode で拒否されること"
    );

    let invalid = wire(0, 0, [b"", b"", b"", b""], b"", &[(b"k", &[0xFF])]);
    assert!(
        matches!(
            LogRecord::decode(&invalid),
            Err(MessageError::InvalidMsfPayload(_))
        ),
        "UTF-8 でない Attribute の value は decode で拒否されること"
    );
}

/// §5.1 の Table 1 の severity と Track Name が往復すること
#[test]
fn severity_track_names() {
    // draft-jennings-moq-log-04 §5.1 (Track Naming): severity は Track Name の 1 桁の
    // ASCII 数字 ("0"〜"7") で表現される。
    let cases = [
        (LogSeverity::Emergency, "0"),
        (LogSeverity::Alert, "1"),
        (LogSeverity::Critical, "2"),
        (LogSeverity::Error, "3"),
        (LogSeverity::Warning, "4"),
        (LogSeverity::Notice, "5"),
        (LogSeverity::Informational, "6"),
        (LogSeverity::Debug, "7"),
    ];
    for (severity, track_name) in cases {
        assert_eq!(severity.track_name(), track_name, "severity の Track Name");
        assert_eq!(
            LogSeverity::from_track_name(track_name.as_bytes()).expect("既知の Track Name"),
            severity,
            "Track Name から severity を復元できること"
        );
    }

    for invalid in [&b""[..], b"00", b"8", b"a", b"-1", b" 0"] {
        assert!(
            matches!(
                LogSeverity::from_track_name(invalid),
                Err(MessageError::InvalidMsfPayload(_))
            ),
            "1 桁の 0〜7 以外の Track Name は拒否されること"
        );
    }
}

/// §5.1 の Track Namespace tuple と MSF-02 §5.2.2 の文字列写像を固定する
#[test]
fn namespace_and_msf_string() {
    // draft-jennings-moq-log-04 §5.1 (Track Naming): 3 field の tuple であり、ResourceID は
    // 16 桁の小文字 16 進 ASCII である (§4)。
    let namespace = log_track_namespace(0x8f2c4a1b0e9d7c63);
    let fields = namespace.fields();
    assert_eq!(fields.len(), 3);
    assert_eq!(fields[0], b"logs.moq.arpa".to_vec());
    assert_eq!(fields[1], b"v1".to_vec());
    assert_eq!(fields[2], b"8f2c4a1b0e9d7c63".to_vec());

    // draft-ietf-moq-msf-02 §5.2.2 (Track namespace): `/` 区切りで連結する。
    assert_eq!(
        log_track_namespace_string(0x8f2c4a1b0e9d7c63),
        "logs.moq.arpa/v1/8f2c4a1b0e9d7c63"
    );
    // ResourceID 0 でも 16 桁にゼロ埋めされること。
    assert_eq!(
        log_track_namespace_string(0),
        "logs.moq.arpa/v1/0000000000000000"
    );
}

/// MSF-02 §9.4 の packaging / role と定数が一致し、catalog にそのまま出力できること
#[test]
fn msf_catalog_requirement_values() {
    // draft-ietf-moq-msf-02 §9.4 (Log track catalog requirements): packaging "moqlog" と
    // role "log" を持たなければならない。
    assert_eq!(LOG_TRACK_PACKAGING, MsfPackaging::MoqLog);
    assert_eq!(LOG_TRACK_ROLE, "log");

    let mut track = MsfTrack::new(String::from("6"), LOG_TRACK_PACKAGING, true);
    track.namespace = Some(log_track_namespace_string(0x8f2c4a1b0e9d7c63));
    track.role = Some(String::from(LOG_TRACK_ROLE));
    let mut catalog = MsfCatalog::new();
    catalog.publish_tracks.push(track);
    let encoded = MsfCatalogDocument::Full(catalog)
        .encode(MsfCompression::None)
        .expect("moqlog publishTrack を持つカタログはエンコードできること");
    let text = core::str::from_utf8(&encoded).expect("カタログは UTF-8 の JSON であること");
    assert!(
        text.contains(r#""packaging":"moqlog""#),
        "catalog の packaging が moqlog であること"
    );
    assert!(
        text.contains(r#""role":"log""#),
        "catalog の role が log であること"
    );
    assert!(
        text.contains(r#""namespace":"logs.moq.arpa/v1/8f2c4a1b0e9d7c63""#),
        "catalog の namespace が MOQLOG-04 §5.1 の tuple の写像であること"
    );
    assert_eq!(MSF_VERSION, "draft-02", "MSF の対応バージョン");
}
