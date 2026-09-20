use super::*;

#[test]
fn wrong_type_error() {
    let buf = vec![0x10, 0x01]; // SUBGROUP_HEADER 型
    assert!(matches!(
        FetchHeader::decode(&buf),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn large_request_id() {
    let hdr = FetchHeader {
        request_id: 1_073_741_824,
    };
    let encoded = hdr.encode();
    let (decoded, consumed) =
        FetchHeader::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(consumed, encoded.len());
    assert_eq!(decoded, hdr);
}
