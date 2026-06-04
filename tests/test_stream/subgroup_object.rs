use super::*;

#[test]
fn status_required_when_payload_zero() {
    let obj = SubgroupObject {
        object_id_delta: 0,
        payload_length: 0,
        status: None,
    };
    let mut buf = Vec::new();
    assert!(matches!(
        obj.encode(false, None, &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn status_forbidden_when_payload_nonzero() {
    let obj = SubgroupObject {
        object_id_delta: 0,
        payload_length: 10,
        status: Some(0x01),
    };
    let mut buf = Vec::new();
    assert!(matches!(
        obj.encode(false, None, &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn properties_zero_length_is_valid() {
    // draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header): Subgroup object では Properties Length = 0 は合法
    let mut buf = Vec::new();
    shiguredo_moqt::varint::encode(0, &mut buf); // object_id_delta
    shiguredo_moqt::varint::encode(0, &mut buf); // prop_len = 0 (合法)
    shiguredo_moqt::varint::encode(5, &mut buf); // payload_length
    let header_len = buf.len();
    let (obj, _, consumed) =
        SubgroupObject::decode(&buf, true).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(consumed, header_len);
    assert_eq!(obj.payload_length, 5);
}

#[test]
fn non_normal_status_with_properties_rejected() {
    // draft-ietf-moq-transport-21 §11.1.3 (Object Properties): status != Normal で Properties がある場合は
    // PROTOCOL_VIOLATION
    let mut buf = Vec::new();
    shiguredo_moqt::varint::encode(0, &mut buf); // object_id_delta
    shiguredo_moqt::varint::encode(2, &mut buf); // prop_len = 2
    buf.extend_from_slice(&[0x01, 0x02]); // properties data (ダミー)
    shiguredo_moqt::varint::encode(0, &mut buf); // payload_length = 0 (status あり)
    shiguredo_moqt::varint::encode(0x03, &mut buf); // status = End of Group (非 Normal)
    assert!(matches!(
        SubgroupObject::decode(&buf, true),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn has_properties_true_but_no_data_rejected() {
    // has_properties が true なのに properties_data が None の場合はエラー
    let obj = SubgroupObject {
        object_id_delta: 0,
        payload_length: 64,
        status: None,
    };
    let mut buf = Vec::new();
    assert!(matches!(
        obj.encode(true, None, &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn has_properties_false_but_data_present_rejected() {
    // has_properties が false なのに properties_data がある場合はエラー
    let obj = SubgroupObject {
        object_id_delta: 0,
        payload_length: 64,
        status: None,
    };
    let props_data = vec![0x00]; // Properties Length = 0
    let mut buf = Vec::new();
    assert!(matches!(
        obj.encode(false, Some(&props_data), &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn non_normal_status_with_properties_encode_rejected() {
    // encode 側でも status != Normal + Properties の組み合わせを拒否する
    let obj = SubgroupObject {
        object_id_delta: 0,
        payload_length: 0,
        status: Some(0x03), // End of Group (非 Normal)
    };
    let props_data = vec![0x02, 0x01, 0x02]; // Properties Length = 2 + ダミーデータ
    let mut buf = Vec::new();
    assert!(matches!(
        obj.encode(true, Some(&props_data), &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn unknown_object_status_rejected() {
    // draft-ietf-moq-transport-21 §11.1.2 (Object Status): 0x0, 0x3, 0x4 以外の status は不正
    let mut buf = Vec::new();
    shiguredo_moqt::varint::encode(0, &mut buf); // object_id_delta
    shiguredo_moqt::varint::encode(0, &mut buf); // payload_length = 0 (status あり)
    shiguredo_moqt::varint::encode(0x01, &mut buf); // status = 0x01 (無効)
    assert!(matches!(
        SubgroupObject::decode(&buf, false),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn valid_object_status_values() {
    // 有効な status 値はすべて受理される
    for status_val in [0x0, 0x3, 0x4] {
        let mut buf = Vec::new();
        shiguredo_moqt::varint::encode(0, &mut buf); // object_id_delta
        shiguredo_moqt::varint::encode(0, &mut buf); // payload_length = 0
        shiguredo_moqt::varint::encode(status_val, &mut buf);
        assert!(
            SubgroupObject::decode(&buf, false).is_ok(),
            "status {status_val:#x} should be valid"
        );
    }
}
