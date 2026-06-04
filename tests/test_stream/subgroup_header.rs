use super::*;

#[test]
fn reserved_subgroup_id_mode_returns_error() {
    // type_byte = 0x10 | 0x06 = 0x16 (SUBGROUP_ID_MODE = 0b11, 予約済み)
    let buf = vec![0x16, 0x01, 0x01, 0x80];
    assert!(matches!(
        SubgroupHeader::decode(&buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn wrong_type_no_bit4() {
    // bit4 が立っていない (OBJECT_DATAGRAM 型)
    let buf = vec![0x00, 0x01, 0x01, 0x80];
    assert!(matches!(
        SubgroupHeader::decode(&buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn first_object_bit_in_type_byte() {
    let hdr = SubgroupHeader {
        track_alias: 0,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Zero,
        publisher_priority: Some(0),
        has_properties: false,
        end_of_group: false,
        first_object: true,
    };
    let encoded = hdr.encode();
    // type_byte 先頭: 0x10 (bit4) | 0x40 (bit6, FIRST_OBJECT)
    assert_eq!(encoded[0], 0x50);
}

#[test]
fn first_object_false_does_not_set_bit() {
    let hdr = SubgroupHeader {
        track_alias: 0,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Zero,
        publisher_priority: Some(0),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    let encoded = hdr.encode();
    // type_byte 先頭: 0x10 (bit4) のみ
    assert_eq!(encoded[0] & 0x40, 0);
}

#[test]
fn undefined_bits_at_or_above_128_rejected() {
    // type に 128 以上の未定義 bit が立つと PROTOCOL_VIOLATION
    // (draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header))
    // 0x90 = bit4 (必須) + bit7 (未定義)。1 バイトに収まらないため
    // varint::encode で構築する (raw バイト列では別値に解釈される)。
    let mut buf = Vec::new();
    shiguredo_moqt::varint::encode(0x90, &mut buf);
    shiguredo_moqt::varint::encode(1, &mut buf); // track_alias
    shiguredo_moqt::varint::encode(1, &mut buf); // group_id
    assert!(matches!(
        SubgroupHeader::decode(&buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn type_flags_known_mask_boundary() {
    // mode 0b11 (予約) を除いた正当値の最大値 0x7D は受理され、0x80 は拒否される
    // (draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header))。
    // 0x7F は mask 清浄だが mode 0b11 (予約) を含むため正当なヘッダにならない。
    let hdr = SubgroupHeader {
        track_alias: 1,
        group_id: 2,
        subgroup_id: SubgroupIdMode::Explicit(9),
        publisher_priority: None,
        has_properties: true,
        end_of_group: true,
        first_object: true,
    };
    let encoded = hdr.encode();
    assert_eq!(encoded[0], 0x7D, "定義済み bit の最大値で 0x7D になること");
    let (decoded, consumed) = SubgroupHeader::decode(&encoded).expect("0x7D は受理されること");
    assert_eq!(consumed, encoded.len());
    assert_eq!(decoded, hdr);
    // 0x80 (bit7 のみ。bit4 なし) は bit4 必須検査で拒否される
    // (mask 検査には到達しない。mask 経路の拒否は 0x90 テストで担保する)
    let mut buf = Vec::new();
    shiguredo_moqt::varint::encode(0x80, &mut buf);
    assert!(matches!(
        SubgroupHeader::decode(&buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}
