use super::*;
use shiguredo_moqt::stream::datagram::validate_object_datagram_type;

#[test]
fn zero_length_normal_object_without_status_rejected() {
    let dg = ObjectDatagram {
        track_alias: 0,
        group_id: 0,
        object_id: 0,
        publisher_priority: None,
        properties_data: None,
        end_of_group: false,
        status: None,
    };
    let encoded = dg.encode().expect("正当なテスト入力の encode は成功する");
    assert!(matches!(
        ObjectDatagram::decode(&encoded),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn status_and_end_of_group_encode_error() {
    let dg = ObjectDatagram {
        track_alias: 1,
        group_id: 0,
        object_id: 0,
        publisher_priority: None,
        properties_data: None,
        end_of_group: true,
        status: Some(0x03),
    };
    assert!(matches!(
        dg.encode(),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn status_and_end_of_group_decode_error() {
    // type_byte = 0x20 | 0x02 = 0x22 (STATUS + END_OF_GROUP)
    let buf = vec![0x22, 0x01, 0x01];
    assert!(matches!(
        ObjectDatagram::decode(&buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn subgroup_type_decode_error() {
    // type_byte = 0x10 (SUBGROUP_HEADER)
    let buf = vec![0x10, 0x01, 0x01, 0x80];
    assert!(matches!(
        ObjectDatagram::decode(&buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn non_normal_status_with_properties_rejected() {
    // draft-ietf-moq-transport-21 §11.1.3 (Object Properties): status != Normal で Properties がある場合は
    // PROTOCOL_VIOLATION
    // type_byte = 0x21 (STATUS 0x20 + PROPERTIES 0x01)
    let mut buf = vec![0x21];
    shiguredo_moqt::varint::encode(1, &mut buf); // track_alias
    shiguredo_moqt::varint::encode(0, &mut buf); // group_id
    shiguredo_moqt::varint::encode(0, &mut buf); // object_id
    buf.push(128); // publisher_priority
    shiguredo_moqt::varint::encode(2, &mut buf); // properties length
    buf.extend_from_slice(&[0x01, 0x02]); // properties data (ダミー)
    shiguredo_moqt::varint::encode(0x03, &mut buf); // status = End of Group (非 Normal)
    assert!(matches!(
        ObjectDatagram::decode(&buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn unknown_object_status_rejected() {
    // draft-ietf-moq-transport-21 §11.1.2 (Object Status): 0x0, 0x3, 0x4 以外の status は不正
    // type_byte = 0x20 (STATUS)
    let mut buf = vec![0x20];
    shiguredo_moqt::varint::encode(1, &mut buf); // track_alias
    shiguredo_moqt::varint::encode(0, &mut buf); // group_id
    shiguredo_moqt::varint::encode(0, &mut buf); // object_id
    buf.push(128); // publisher_priority
    shiguredo_moqt::varint::encode(0x02, &mut buf); // status = 0x02 (無効)
    assert!(matches!(
        ObjectDatagram::decode(&buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn status_object_with_payload_rejected() {
    let dg = ObjectDatagram {
        track_alias: 3,
        group_id: 10,
        object_id: 1,
        publisher_priority: Some(200),
        properties_data: None,
        end_of_group: false,
        status: Some(0x03),
    };
    let mut encoded = dg.encode().expect("正当なテスト入力の encode は成功する");
    encoded.push(0xAA);
    assert!(matches!(
        ObjectDatagram::decode(&encoded),
        Err(MessageError::ProtocolViolation(_))
    ));
}

// ─── Properties Length のデコード境界 (draft-ietf-moq-transport-21 §11.2.1 (Object Datagram)) ────

/// PROPERTIES bit が立っているのに Properties Length = 0 のバイト列は PROTOCOL_VIOLATION。
/// checked_len 化後もこの datagram 固有チェック (subgroup では 0 が合法) が保持されること。
#[test]
fn properties_length_zero_rejected() {
    // type_byte = 0x0D (PROPERTIES 0x01 + ZERO_OBJECT_ID 0x04 + DEFAULT_PRIORITY 0x08)
    // track_alias=0, group_id=0, (object_id は ZERO_OBJECT_ID で省略),
    // (priority は DEFAULT_PRIORITY で省略), properties length=0
    let buf = vec![0x0Du8, 0x00, 0x00, 0x00];
    assert!(matches!(
        ObjectDatagram::decode(&buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

/// Properties Length がバッファ残量を超えるバイト列は、checked_len 化により他モジュールと
/// 同じ UnexpectedEof になる (32bit 環境での素キャスト切り詰めによる誤読を防ぐ)。
#[test]
fn properties_length_exceeds_buffer_unexpected_eof() {
    // type_byte = 0x0D、properties length=5 だが後続データなし
    let buf = vec![0x0Du8, 0x00, 0x00, 0x05];
    assert!(matches!(
        ObjectDatagram::decode(&buf),
        Err(MessageError::UnexpectedEof)
    ));
}

// ─── properties_data の Length 込み規約 (draft-ietf-moq-transport-21 §11.1.3 (Object Properties)) ────

/// encode は properties_data に Length を付け直さない (ワイヤ表現の固定)
///
/// 任意入力の往復 (decode が Length 込みを返すことを含む) は PBT がカバーする。
#[test]
fn encode_writes_properties_data_without_reprefixing_length() {
    let dg = ObjectDatagram {
        track_alias: 3,
        group_id: 10,
        object_id: 1,
        publisher_priority: Some(200),
        properties_data: Some(vec![0x02, 0xAA, 0xBB]),
        end_of_group: false,
        status: Some(0),
    };
    let encoded = dg.encode().expect("正当なテスト入力の encode は成功する");
    assert_eq!(
        encoded,
        vec![
            0x21, // type_byte: PROPERTIES 0x01 + STATUS 0x20
            0x03, // track_alias
            0x0A, // group_id
            0x01, // object_id
            0xC8, // publisher_priority
            0x02, 0xAA, 0xBB, // Properties Length = 2 + Properties 本体
            0x00, // status = Normal
        ],
        "Properties Length が二重に前置されないこと"
    );
}

/// 空スライス (Properties Length varint すら含まない) は encode で拒否される
#[test]
fn encode_rejects_empty_properties_data() {
    let dg = ObjectDatagram {
        track_alias: 3,
        group_id: 10,
        object_id: 1,
        publisher_priority: Some(200),
        properties_data: Some(Vec::new()),
        end_of_group: false,
        status: Some(0),
    };
    assert!(matches!(
        dg.encode(),
        Err(MessageError::ProtocolViolation(_))
    ));
}

/// Properties Length varint が途中で切れた blob は encode で拒否される
#[test]
fn encode_rejects_truncated_properties_length_varint() {
    let dg = ObjectDatagram {
        track_alias: 3,
        group_id: 10,
        object_id: 1,
        publisher_priority: Some(200),
        // 2 バイト varint の先頭バイトのみ
        properties_data: Some(vec![0x80]),
        end_of_group: false,
        status: Some(0),
    };
    assert!(matches!(
        dg.encode(),
        Err(MessageError::ProtocolViolation(_))
    ));
}

/// Properties Length = 0 は encode で拒否される (datagram では禁止)
#[test]
fn encode_rejects_properties_length_zero() {
    let dg = ObjectDatagram {
        track_alias: 3,
        group_id: 10,
        object_id: 1,
        publisher_priority: Some(200),
        properties_data: Some(vec![0x00]),
        end_of_group: false,
        status: Some(0),
    };
    assert!(matches!(
        dg.encode(),
        Err(MessageError::ProtocolViolation(_))
    ));
}

/// 宣言 Length と実データ長の不一致は encode で拒否される
#[test]
fn encode_rejects_properties_length_mismatch() {
    let dg = ObjectDatagram {
        track_alias: 3,
        group_id: 10,
        object_id: 1,
        publisher_priority: Some(200),
        // Properties Length = 2 だが本体は 1 バイト
        properties_data: Some(vec![0x02, 0xAA]),
        end_of_group: false,
        status: Some(0),
    };
    assert!(matches!(
        dg.encode(),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn undefined_bit_0x40_rejected() {
    // type に未定義 bit 0x40 が立つと PROTOCOL_VIOLATION
    // (draft-ietf-moq-transport-21 §11.2.1 (Object Datagram))
    let buf = vec![0x40, 0x01, 0x01];
    assert!(matches!(
        ObjectDatagram::decode(&buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn type_flags_mask_boundary_matrix() {
    // 定義済み bit のみの値は受理し、未定義 bit を含む値は拒否することを
    // 境界値で固定する (draft-ietf-moq-transport-21 §11.2.1 (Object Datagram))。
    // 旧 range 形式 (`0x00-0x0F` / `0x20-0x2F`) との等価性も兼ねる。
    // 受理: 定義済み bit のみ (0x2D は STATUS 付きだが END_OF_GROUP なしで正当)
    for valid in [0x00u64, 0x0F, 0x20, 0x2D] {
        assert!(
            validate_object_datagram_type(valid).is_ok(),
            "{valid:#X} は受理されること"
        );
    }
    // 拒否: 0x10 予約・ 0x30 台・ 0x40 未定義・ 128 以上・多バイト varint 値
    for invalid in [0x10u64, 0x1F, 0x30, 0x3F, 0x40, 0x7F, 0x80, 0x100, u64::MAX] {
        assert!(
            validate_object_datagram_type(invalid).is_err(),
            "{invalid:#X} は拒否されること"
        );
    }
}
