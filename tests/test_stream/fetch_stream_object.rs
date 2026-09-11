use super::*;

#[test]
fn datagram_origin_with_explicit_subgroup_rejected() {
    // Datagram 起源のオブジェクトは Subgroup ID を持てない
    let obj = FetchStreamObject {
        group_id: Some(1),
        subgroup_id: FetchSubgroupIdMode::Explicit(42),
        object_id: Some(5),
        publisher_priority: Some(200),
        has_properties: false,
        is_datagram_origin: true,
        payload_length: 100,
    };
    let mut buf = Vec::new();
    assert!(matches!(
        obj.encode(None, FetchPriorContext::HasPriorObject, &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn datagram_origin_with_previous_same_rejected() {
    // Datagram 起源では PreviousSame も不正
    let obj = FetchStreamObject {
        group_id: Some(1),
        subgroup_id: FetchSubgroupIdMode::PreviousSame,
        object_id: Some(5),
        publisher_priority: Some(200),
        has_properties: false,
        is_datagram_origin: true,
        payload_length: 100,
    };
    let mut buf = Vec::new();
    assert!(matches!(
        obj.encode(None, FetchPriorContext::HasPriorObject, &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn datagram_origin_ignores_lsb() {
    // bit 0x40 が立っている場合、下位 2 bit を無視して subgroup_id は Zero になる
    // 手動で flags = 0x5F (0x40 | 0x10 | 0x08 | 0x04 | 0x03) を構築する
    let mut buf = Vec::new();
    shiguredo_moqt::varint::encode(0x5F, &mut buf); // flags: datagram + priority + group + object + LSB=0b11
    shiguredo_moqt::varint::encode(2, &mut buf); // group_id
    shiguredo_moqt::varint::encode(3, &mut buf); // object_id
    buf.push(100); // publisher_priority
    shiguredo_moqt::varint::encode(50, &mut buf); // payload_length

    let (entry, consumed) = FetchStreamEntry::decode(&buf, FetchPriorContext::HasPriorObject)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(consumed, buf.len());
    if let FetchStreamEntry::Object(decoded) = entry {
        assert!(decoded.is_datagram_origin);
        // 下位 2 bit は無視されるので Explicit ではなく Zero になる
        assert_eq!(decoded.subgroup_id, FetchSubgroupIdMode::Zero);
    } else {
        panic!("FetchStreamEntry::Object が期待された");
    }
}

/// draft-ietf-moq-transport-21 §11.4.1 (FETCH stream) Table 7:
/// End of Timed-Out Range (0x20C) のラウンドトリップ
#[test]
fn end_of_timed_out_range() {
    let entry = FetchStreamEntry::EndOfTimedOutRange {
        group_id: 4,
        object_id: 9,
    };
    let mut buf = Vec::new();
    entry
        .encode(None, FetchPriorContext::HasPriorObject, &mut buf)
        .expect("テストフィクスチャの前提条件を満たす");
    let (decoded, consumed) = FetchStreamEntry::decode(&buf, FetchPriorContext::HasPriorObject)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(consumed, buf.len());
    assert_eq!(
        decoded,
        FetchStreamEntry::EndOfTimedOutRange {
            group_id: 4,
            object_id: 9,
        }
    );
}

/// 0x8C / 0x10C / 0x20C の混在ストリームが順に decode できる
///
/// draft-ietf-moq-transport-21 §11.4.1 (FETCH stream) Table 7:
/// 3 種の End of Range はいずれも既知値として受理される
/// (128 以上の未知値拒否の一般則は別途扱う)。
#[test]
fn mixed_end_of_range_kinds_decode_in_order() {
    let entries = [
        FetchStreamEntry::EndOfNonExistentRange {
            group_id: 1,
            object_id: 2,
        },
        FetchStreamEntry::EndOfUnknownRange {
            group_id: 3,
            object_id: 4,
        },
        FetchStreamEntry::EndOfTimedOutRange {
            group_id: 5,
            object_id: 6,
        },
    ];
    let mut buf = Vec::new();
    for entry in &entries {
        entry
            .encode(None, FetchPriorContext::HasPriorObject, &mut buf)
            .expect("テストフィクスチャの前提条件を満たす");
    }
    let mut pos = 0;
    for expected in &entries {
        let (decoded, consumed) =
            FetchStreamEntry::decode(&buf[pos..], FetchPriorContext::HasPriorObject)
                .expect("テストフィクスチャの前提条件を満たす");
        pos += consumed;
        assert_eq!(&decoded, expected);
    }
    assert_eq!(pos, buf.len());
}

#[test]
fn properties_zero_length_is_valid() {
    // draft-ietf-moq-transport-21 §11.4.1 (Fetch Header): Fetch object でも Properties Length = 0 は合法
    // flags = 0x2C (PROPERTIES 0x20 + OBJECT_ID 0x04 + GROUP_ID 0x08)
    let mut buf = Vec::new();
    shiguredo_moqt::varint::encode(0x2C, &mut buf); // flags
    shiguredo_moqt::varint::encode(1, &mut buf); // group_id
    shiguredo_moqt::varint::encode(0, &mut buf); // object_id
    shiguredo_moqt::varint::encode(0, &mut buf); // prop_len = 0 (合法)
    shiguredo_moqt::varint::encode(3, &mut buf); // payload_length
    let header_len = buf.len();
    let (entry, consumed) = FetchStreamEntry::decode(&buf, FetchPriorContext::HasPriorObject)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(consumed, header_len);
    assert!(matches!(entry, FetchStreamEntry::Object(_)));
}

#[test]
fn encode_has_properties_without_data_rejected() {
    // has_properties = true だが properties_data = None は不正
    let obj = FetchStreamObject {
        group_id: Some(1),
        subgroup_id: FetchSubgroupIdMode::Zero,
        object_id: Some(0),
        publisher_priority: Some(100),
        has_properties: true,
        is_datagram_origin: false,
        payload_length: 32,
    };
    let mut buf = Vec::new();
    assert!(matches!(
        obj.encode(None, FetchPriorContext::HasPriorObject, &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn encode_properties_data_without_flag_rejected() {
    // has_properties = false だが properties_data = Some は不正
    let obj = FetchStreamObject {
        group_id: Some(1),
        subgroup_id: FetchSubgroupIdMode::Zero,
        object_id: Some(0),
        publisher_priority: Some(100),
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 32,
    };
    let mut buf = Vec::new();
    assert!(matches!(
        obj.encode(
            Some(&[0x01, 0xFF]),
            FetchPriorContext::HasPriorObject,
            &mut buf
        ),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn encode_first_object_with_prior_subgroup_rejected() {
    // First 文脈で PreviousSame は不正
    let obj = FetchStreamObject {
        group_id: Some(1),
        subgroup_id: FetchSubgroupIdMode::PreviousSame,
        object_id: Some(0),
        publisher_priority: Some(100),
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 0,
    };
    let mut buf = Vec::new();
    assert!(matches!(
        obj.encode(None, FetchPriorContext::First, &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn encode_first_object_without_group_id_rejected() {
    // First 文脈で group_id = None は不正
    let obj = FetchStreamObject {
        group_id: None,
        subgroup_id: FetchSubgroupIdMode::Zero,
        object_id: Some(0),
        publisher_priority: Some(100),
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 0,
    };
    let mut buf = Vec::new();
    assert!(matches!(
        obj.encode(None, FetchPriorContext::First, &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn encode_first_object_without_object_id_rejected() {
    // First 文脈で object_id = None は不正
    let obj = FetchStreamObject {
        group_id: Some(1),
        subgroup_id: FetchSubgroupIdMode::Zero,
        object_id: None,
        publisher_priority: Some(100),
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 0,
    };
    let mut buf = Vec::new();
    assert!(matches!(
        obj.encode(None, FetchPriorContext::First, &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn encode_first_object_without_priority_rejected() {
    // First 文脈で publisher_priority = None は不正
    let obj = FetchStreamObject {
        group_id: Some(1),
        subgroup_id: FetchSubgroupIdMode::Zero,
        object_id: Some(0),
        publisher_priority: None,
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 0,
    };
    let mut buf = Vec::new();
    assert!(matches!(
        obj.encode(None, FetchPriorContext::First, &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn encode_no_prior_actual_object_with_prior_subgroup_rejected() {
    // NoPriorActualObject 文脈で PreviousPlusOne は不正
    let obj = FetchStreamObject {
        group_id: Some(1),
        subgroup_id: FetchSubgroupIdMode::PreviousPlusOne,
        object_id: Some(0),
        publisher_priority: Some(100),
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 0,
    };
    let mut buf = Vec::new();
    assert!(matches!(
        obj.encode(None, FetchPriorContext::NoPriorActualObject, &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn encode_no_prior_actual_object_without_priority_rejected() {
    // NoPriorActualObject 文脈で publisher_priority = None は不正
    let obj = FetchStreamObject {
        group_id: Some(1),
        subgroup_id: FetchSubgroupIdMode::Zero,
        object_id: Some(0),
        publisher_priority: None,
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 0,
    };
    let mut buf = Vec::new();
    assert!(matches!(
        obj.encode(None, FetchPriorContext::NoPriorActualObject, &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn encode_no_prior_actual_object_allows_group_id_omission() {
    // NoPriorActualObject 文脈では group_id = None は合法
    // (prior Group ID は End of Range の値を使う)
    let obj = FetchStreamObject {
        group_id: None,
        subgroup_id: FetchSubgroupIdMode::Zero,
        object_id: None,
        publisher_priority: Some(100),
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 0,
    };
    let mut buf = Vec::new();
    obj.encode(None, FetchPriorContext::NoPriorActualObject, &mut buf)
        .expect("テストフィクスチャの前提条件を満たす");
    let (entry, consumed) = FetchStreamEntry::decode(&buf, FetchPriorContext::NoPriorActualObject)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(consumed, buf.len());
    assert_eq!(entry, FetchStreamEntry::Object(obj));
}

#[test]
fn decode_no_prior_actual_object_with_prior_subgroup_rejected() {
    // NoPriorActualObject 文脈で PreviousSame (mode 0x01) をデコードすると PROTOCOL_VIOLATION
    // flags = 0x1D (0x10 | 0x08 | 0x04 | 0x01) = priority + group + object + PreviousSame
    let mut buf = Vec::new();
    shiguredo_moqt::varint::encode(0x1D, &mut buf); // flags
    shiguredo_moqt::varint::encode(1, &mut buf); // group_id
    shiguredo_moqt::varint::encode(0, &mut buf); // object_id
    buf.push(100); // publisher_priority
    shiguredo_moqt::varint::encode(0, &mut buf); // payload_length
    assert!(matches!(
        FetchStreamEntry::decode(&buf, FetchPriorContext::NoPriorActualObject),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn decode_no_prior_actual_object_without_priority_rejected() {
    // NoPriorActualObject 文脈で priority 未指定をデコードすると PROTOCOL_VIOLATION
    // flags = 0x0C (0x08 | 0x04) = group + object のみ (priority なし)
    let mut buf = Vec::new();
    shiguredo_moqt::varint::encode(0x0C, &mut buf); // flags
    shiguredo_moqt::varint::encode(1, &mut buf); // group_id
    shiguredo_moqt::varint::encode(0, &mut buf); // object_id
    shiguredo_moqt::varint::encode(0, &mut buf); // payload_length
    assert!(matches!(
        FetchStreamEntry::decode(&buf, FetchPriorContext::NoPriorActualObject),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn unknown_flags_at_or_above_128_rejected() {
    // Table 7 の 3 値 (0x8C / 0x10C / 0x20C) 以外の 128 以上は PROTOCOL_VIOLATION
    // (draft-ietf-moq-transport-21 §11.4.1 (FETCH stream): "Any other value is a PROTOCOL_VIOLATION")
    // 0x8D は 128 以上かつ特殊値ではない。First 文脈で decode し、prior 文脈違反
    // ではなく未知 flags としての拒否であることをメッセージで断定する
    // (未知値チェックが prior チェックより先行することの固定)。
    let mut buf = Vec::new();
    shiguredo_moqt::varint::encode(0x8D, &mut buf);
    assert!(matches!(
        FetchStreamEntry::decode(&buf, FetchPriorContext::First),
        Err(MessageError::ProtocolViolation(
            "invalid fetch serialization flags"
        ))
    ));
}

// 空スライスは Properties Length varint を含まない契約違反入力として拒否し、
// 失敗時に buf へ部分バイトを残さない
#[test]
fn encode_has_properties_with_empty_slice_rejected() {
    let obj = FetchStreamObject {
        group_id: Some(1),
        subgroup_id: FetchSubgroupIdMode::Zero,
        object_id: Some(0),
        publisher_priority: Some(100),
        has_properties: true,
        is_datagram_origin: false,
        payload_length: 32,
    };
    let mut buf = vec![0xAA, 0xBB, 0xCC];
    assert!(matches!(
        obj.encode(Some(&[]), FetchPriorContext::HasPriorObject, &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
    assert_eq!(buf, vec![0xAA, 0xBB, 0xCC], "失敗時に buf が変化しないこと");
}

// has_properties=true でも Properties Length = 0 を含むデータは正常にエンコードできる
#[test]
fn encode_properties_length_zero_roundtrip() {
    let obj = FetchStreamObject {
        group_id: Some(1),
        subgroup_id: FetchSubgroupIdMode::Zero,
        object_id: Some(0),
        publisher_priority: Some(100),
        has_properties: true,
        is_datagram_origin: false,
        payload_length: 32,
    };
    let mut buf = Vec::new();
    obj.encode(Some(&[0x00]), FetchPriorContext::HasPriorObject, &mut buf)
        .expect("Properties Length = 0 は合法");
    let (entry, consumed) = FetchStreamEntry::decode(&buf, FetchPriorContext::HasPriorObject)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(consumed, buf.len());
    assert_eq!(entry, FetchStreamEntry::Object(obj));
}

// 非空の Properties は Properties Length と実データが一致していれば正常にエンコードできる
#[test]
fn encode_non_empty_properties_roundtrip() {
    let obj = FetchStreamObject {
        group_id: Some(1),
        subgroup_id: FetchSubgroupIdMode::Zero,
        object_id: Some(0),
        publisher_priority: Some(100),
        has_properties: true,
        is_datagram_origin: false,
        payload_length: 5,
    };
    let props = [0x02, 0x02, 0x00]; // Properties Length = 2 + KVP (prop_type 0x02 / VarInt 0)
    let mut buf = Vec::new();
    obj.encode(Some(&props), FetchPriorContext::HasPriorObject, &mut buf)
        .expect("Length が一致する非空 Properties は合法");
    let (entry, consumed) = FetchStreamEntry::decode(&buf, FetchPriorContext::HasPriorObject)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(consumed, buf.len());
    assert_eq!(entry, FetchStreamEntry::Object(obj));
}

// Properties Length varint が途中で切れている blob は拒否する
#[test]
fn encode_truncated_properties_length_rejected() {
    let obj = FetchStreamObject {
        group_id: Some(1),
        subgroup_id: FetchSubgroupIdMode::Zero,
        object_id: Some(0),
        publisher_priority: Some(100),
        has_properties: true,
        is_datagram_origin: false,
        payload_length: 32,
    };
    let mut buf = vec![0xAA, 0xBB];
    assert!(matches!(
        obj.encode(Some(&[0x80]), FetchPriorContext::HasPriorObject, &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
    assert_eq!(buf, vec![0xAA, 0xBB], "失敗時に buf が変化しないこと");
}

// Properties Length と実データ長が一致しない blob は拒否する
#[test]
fn encode_properties_length_mismatch_rejected() {
    let obj = FetchStreamObject {
        group_id: Some(1),
        subgroup_id: FetchSubgroupIdMode::Zero,
        object_id: Some(0),
        publisher_priority: Some(100),
        has_properties: true,
        is_datagram_origin: false,
        payload_length: 32,
    };
    // Length = 64 を宣言して後続データ 0 バイト
    let mut buf = vec![0xAA, 0xBB];
    assert!(matches!(
        obj.encode(Some(&[0x40]), FetchPriorContext::HasPriorObject, &mut buf),
        Err(MessageError::ProtocolViolation(_))
    ));
    assert_eq!(buf, vec![0xAA, 0xBB], "失敗時に buf が変化しないこと");
    // Length = 0 を宣言して余分な 1 バイト
    let mut buf = Vec::new();
    assert!(matches!(
        obj.encode(
            Some(&[0x00, 0xAA]),
            FetchPriorContext::HasPriorObject,
            &mut buf
        ),
        Err(MessageError::ProtocolViolation(_))
    ));
}

// Length が u64::MAX の 9 バイト varint でも桁あふれで panic せず拒否する
#[test]
fn encode_properties_length_u64_max_rejected() {
    let obj = FetchStreamObject {
        group_id: Some(1),
        subgroup_id: FetchSubgroupIdMode::Zero,
        object_id: Some(0),
        publisher_priority: Some(100),
        has_properties: true,
        is_datagram_origin: false,
        payload_length: 32,
    };
    let mut buf = vec![0xAA];
    assert!(matches!(
        obj.encode(
            Some(&[0xFF; 9]),
            FetchPriorContext::HasPriorObject,
            &mut buf
        ),
        Err(MessageError::ProtocolViolation(_))
    ));
    assert_eq!(buf, vec![0xAA], "失敗時に buf が変化しないこと");
}
