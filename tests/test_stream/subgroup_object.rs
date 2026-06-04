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

// draft-ietf-moq-transport-21 §11.1.2 (Object Status): 未知 status は protocol error。
// 検証を書き込み前に行い、失敗時に buf へ部分バイトを残さない
#[test]
fn invalid_status_does_not_partially_write() {
    let obj = SubgroupObject {
        object_id_delta: 1,
        payload_length: 0,
        status: Some(0x01),
    };
    let mut buf = vec![0xAA, 0xBB, 0xCC];
    assert!(matches!(
        obj.encode(false, None, &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
    assert_eq!(buf, vec![0xAA, 0xBB, 0xCC], "失敗時に buf が変化しないこと");

    // properties を伴う経路でも検証が書き込み前に走る
    let mut buf = vec![0xAA, 0xBB];
    assert!(matches!(
        obj.encode(true, Some(&[0x00]), &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
    assert_eq!(buf, vec![0xAA, 0xBB], "失敗時に buf が変化しないこと");
}

// 空スライスは Properties Length varint を含まない契約違反入力として拒否する
#[test]
fn empty_properties_slice_rejected() {
    let obj = SubgroupObject {
        object_id_delta: 0,
        payload_length: 0,
        status: Some(0x00),
    };
    let mut buf = Vec::new();
    assert!(matches!(
        obj.encode(true, Some(&[]), &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
    assert!(buf.is_empty(), "拒否時に buf が変化しないこと");
}

// has_properties=true でも Properties Length = 0 を含むデータは正常にエンコードできる
#[test]
fn properties_length_zero_encoded() {
    let obj = SubgroupObject {
        object_id_delta: 0,
        payload_length: 0,
        status: Some(0x00),
    };
    let mut buf = Vec::new();
    obj.encode(true, Some(&[0x00]), &mut buf)
        .expect("Properties Length = 0 は合法");
    let (decoded, props, _) =
        SubgroupObject::decode(&buf, true).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded.object_id_delta, 0);
    assert_eq!(decoded.payload_length, 0);
    assert_eq!(decoded.status, Some(0x00));
    assert_eq!(props.as_deref(), Some(&[0x00][..]));
}

// 非 Normal status でも Properties Length = 0 を含むデータは正常にエンコードできる
// (draft-ietf-moq-transport-21 §11.3.1: PROPERTIES bit が立ち非 Normal なら Length = 0 で表現する)
#[test]
fn non_normal_status_with_zero_length_properties_encoded() {
    let obj = SubgroupObject {
        object_id_delta: 0,
        payload_length: 0,
        status: Some(0x03),
    };
    let mut buf = Vec::new();
    obj.encode(true, Some(&[0x00]), &mut buf)
        .expect("非 Normal status でも Properties Length = 0 は合法");
    let (decoded, props, _) =
        SubgroupObject::decode(&buf, true).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded.status, Some(0x03));
    assert_eq!(props.as_deref(), Some(&[0x00][..]));
}

// Properties Length varint が途中で切れている blob は拒否する
#[test]
fn truncated_properties_length_rejected() {
    let obj = SubgroupObject {
        object_id_delta: 0,
        payload_length: 5,
        status: None,
    };
    let mut buf = vec![0xAA, 0xBB];
    assert!(matches!(
        obj.encode(true, Some(&[0x80]), &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
    assert_eq!(buf, vec![0xAA, 0xBB], "失敗時に buf が変化しないこと");
}

// Properties Length と実データ長が一致しない blob は拒否する
#[test]
fn properties_length_mismatch_rejected() {
    // Length = 64 を宣言して後続データ 0 バイト
    let obj = SubgroupObject {
        object_id_delta: 0,
        payload_length: 5,
        status: None,
    };
    let mut buf = vec![0xAA, 0xBB];
    assert!(matches!(
        obj.encode(true, Some(&[0x40]), &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
    assert_eq!(buf, vec![0xAA, 0xBB], "失敗時に buf が変化しないこと");

    // Length = 0 を宣言して余分な 1 バイト (Normal status)
    let obj = SubgroupObject {
        object_id_delta: 0,
        payload_length: 0,
        status: Some(0x00),
    };
    let mut buf = Vec::new();
    assert!(matches!(
        obj.encode(true, Some(&[0x00, 0xAA]), &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));

    // Length = 0 を宣言して余分な 1 バイト (非 Normal status) も拒否する
    let obj = SubgroupObject {
        object_id_delta: 0,
        payload_length: 0,
        status: Some(0x03),
    };
    let mut buf = Vec::new();
    assert!(matches!(
        obj.encode(true, Some(&[0x00, 0xAA]), &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

// Length が u64::MAX の 9 バイト varint でも桁あふれで panic せず拒否する
#[test]
fn properties_length_u64_max_rejected() {
    let obj = SubgroupObject {
        object_id_delta: 0,
        payload_length: 5,
        status: None,
    };
    let mut buf = vec![0xAA];
    assert!(matches!(
        obj.encode(true, Some(&[0xFF; 9]), &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
    assert_eq!(buf, vec![0xAA], "失敗時に buf が変化しないこと");
}

// Length が一致する非空 Properties は正常にエンコードできる
#[test]
fn properties_length_matching_non_empty_encoded() {
    let obj = SubgroupObject {
        object_id_delta: 0,
        payload_length: 5,
        status: None,
    };
    let props = [0x02, 0x02, 0x00]; // Properties Length = 2 + KVP (prop_type 0x02 / VarInt 0)
    let mut buf = Vec::new();
    obj.encode(true, Some(&props), &mut buf)
        .expect("Length が一致する非空 Properties は合法");
    let (decoded, props_out, consumed) =
        SubgroupObject::decode(&buf, true).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(consumed, buf.len());
    assert_eq!(decoded.payload_length, 5);
    assert_eq!(props_out.as_deref(), Some(&props[..]));
}

// 非最小エンコーディングの Length (2 バイトで 0) も長さが一致すれば受理する
#[test]
fn non_minimal_properties_length_accepted() {
    let obj = SubgroupObject {
        object_id_delta: 0,
        payload_length: 5,
        status: None,
    };
    // &[0x80, 0x00] は Length = 0 の非最小表現 (draft §8.1 は非最小を許容する)
    let mut buf = Vec::new();
    obj.encode(true, Some(&[0x80, 0x00]), &mut buf)
        .expect("非最小エンコーディングの Length は合法");
    let (_, props_out, consumed) =
        SubgroupObject::decode(&buf, true).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(consumed, buf.len());
    assert_eq!(props_out.as_deref(), Some(&[0x80, 0x00][..]));
}
