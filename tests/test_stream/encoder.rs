//! FetchStreamEncoder の外部テスト
//!
//! エンコーダで生成したバイト列をデコーダでラウンドトリップ検証する。
use super::*;

/// エンコーダで生成したバイト列をデコーダで検証するヘルパー
fn encode_and_decode(
    request_id: u64,
    inputs: &[(FetchObjectInput, &[u8])], // (input, payload)
) -> Vec<DecodedFetchEntry> {
    let mut encoder = FetchStreamEncoder::new(request_id);
    let mut stream = encoder.encode_header();

    for (input, payload) in inputs {
        encoder
            .encode_object(input, None, &mut stream)
            .expect("テストフィクスチャの前提条件を満たす");
        stream.extend_from_slice(payload);
    }

    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&stream);
    let header = decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(header.request_id, request_id);

    let mut entries = Vec::new();
    while let Some(entry) = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
    {
        if let DecodedFetchEntry::Object(ref obj) = entry
            && obj.payload_length > 0
        {
            // ペイロードが揃うまで待つ (テストでは一括 push なので常に揃っている)
            decoder.try_read_payload().expect("payload が取れる");
        }
        entries.push(entry);
    }
    entries
}

#[test]
fn test_single_object() {
    let entries = encode_and_decode(
        1,
        &[(
            FetchObjectInput {
                group_id: 10,
                subgroup_id: 0,
                object_id: 0,
                publisher_priority: 128,
                has_properties: false,
                is_datagram_origin: false,
                payload_length: 3,
            },
            b"abc",
        )],
    );

    assert_eq!(entries.len(), 1);
    match &entries[0] {
        DecodedFetchEntry::Object(obj) => {
            assert_eq!(obj.group_id, 10);
            assert_eq!(obj.subgroup_id, 0);
            assert_eq!(obj.object_id, 0);
            assert_eq!(obj.publisher_priority, 128);
        }
        _ => panic!("Object が期待された"),
    }
}

#[test]
fn test_delta_compression_same_group() {
    // 同一 group, 同一 subgroup, 連続 object_id → デルタ圧縮される
    let entries = encode_and_decode(
        1,
        &[
            (
                FetchObjectInput {
                    group_id: 0,
                    subgroup_id: 0,
                    object_id: 0,
                    publisher_priority: 128,
                    has_properties: false,
                    is_datagram_origin: false,
                    payload_length: 1,
                },
                b"a",
            ),
            (
                FetchObjectInput {
                    group_id: 0,
                    subgroup_id: 0,
                    object_id: 1,
                    publisher_priority: 128,
                    has_properties: false,
                    is_datagram_origin: false,
                    payload_length: 1,
                },
                b"b",
            ),
            (
                FetchObjectInput {
                    group_id: 0,
                    subgroup_id: 0,
                    object_id: 2,
                    publisher_priority: 128,
                    has_properties: false,
                    is_datagram_origin: false,
                    payload_length: 1,
                },
                b"c",
            ),
        ],
    );

    assert_eq!(entries.len(), 3);
    for (i, entry) in entries.iter().enumerate() {
        match entry {
            DecodedFetchEntry::Object(obj) => {
                assert_eq!(obj.group_id, 0);
                assert_eq!(obj.subgroup_id, 0);
                assert_eq!(obj.object_id, i as u64);
                assert_eq!(obj.publisher_priority, 128);
            }
            _ => panic!("Object が期待された"),
        }
    }
}

#[test]
fn test_group_change() {
    let entries = encode_and_decode(
        1,
        &[
            (
                FetchObjectInput {
                    group_id: 0,
                    subgroup_id: 0,
                    object_id: 0,
                    publisher_priority: 128,
                    has_properties: false,
                    is_datagram_origin: false,
                    payload_length: 1,
                },
                b"a",
            ),
            (
                FetchObjectInput {
                    group_id: 1, // group 変更
                    subgroup_id: 0,
                    object_id: 0,
                    publisher_priority: 128,
                    has_properties: false,
                    is_datagram_origin: false,
                    payload_length: 1,
                },
                b"b",
            ),
        ],
    );

    assert_eq!(entries.len(), 2);
    match &entries[1] {
        DecodedFetchEntry::Object(obj) => {
            assert_eq!(obj.group_id, 1);
            assert_eq!(obj.subgroup_id, 0);
            assert_eq!(obj.object_id, 0);
        }
        _ => panic!("Object が期待された"),
    }
}

#[test]
fn test_priority_change_in_same_subgroup_is_rejected() {
    let mut encoder = FetchStreamEncoder::new(1);
    let mut stream = encoder.encode_header();

    encoder
        .encode_object(
            &FetchObjectInput {
                group_id: 0,
                subgroup_id: 0,
                object_id: 0,
                publisher_priority: 128,
                has_properties: false,
                is_datagram_origin: false,
                payload_length: 1,
            },
            None,
            &mut stream,
        )
        .expect("テストフィクスチャの前提条件を満たす");

    assert!(matches!(
        encoder.encode_object(
            &FetchObjectInput {
                group_id: 0,
                subgroup_id: 0,
                object_id: 1,
                publisher_priority: 64,
                has_properties: false,
                is_datagram_origin: false,
                payload_length: 1,
            },
            None,
            &mut stream,
        ),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn test_non_sequential_object_id() {
    // object_id が +1 でない場合は明示される
    let entries = encode_and_decode(
        1,
        &[
            (
                FetchObjectInput {
                    group_id: 0,
                    subgroup_id: 0,
                    object_id: 0,
                    publisher_priority: 128,
                    has_properties: false,
                    is_datagram_origin: false,
                    payload_length: 1,
                },
                b"a",
            ),
            (
                FetchObjectInput {
                    group_id: 0,
                    subgroup_id: 0,
                    object_id: 5, // +1 ではない → 明示
                    publisher_priority: 128,
                    has_properties: false,
                    is_datagram_origin: false,
                    payload_length: 1,
                },
                b"b",
            ),
        ],
    );

    match &entries[1] {
        DecodedFetchEntry::Object(obj) => {
            assert_eq!(obj.object_id, 5);
        }
        _ => panic!("Object が期待された"),
    }
}

#[test]
fn test_end_of_range_then_object() {
    let mut encoder = FetchStreamEncoder::new(1);
    let mut stream = encoder.encode_header();

    // End of Non-Existent Range
    encoder
        .encode_end_of_non_existent_range(5, 10, &mut stream)
        .expect("テストフィクスチャの前提条件を満たす");

    // Object (NoPriorActualObject 文脈)
    let input = FetchObjectInput {
        group_id: 5, // End of Range の group_id と同じ → 継承可能だが NoPriorActualObject
        subgroup_id: 0,
        object_id: 11,
        publisher_priority: 64,
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 1,
    };
    encoder
        .encode_object(&input, None, &mut stream)
        .expect("テストフィクスチャの前提条件を満たす");
    stream.extend_from_slice(b"x");

    // デコードで検証
    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&stream);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");

    let entry = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert!(matches!(
        entry,
        DecodedFetchEntry::EndOfNonExistentRange {
            group_id: 5,
            object_id: 10
        }
    ));

    let entry = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    match entry {
        DecodedFetchEntry::Object(obj) => {
            assert_eq!(obj.group_id, 5);
            assert_eq!(obj.object_id, 11);
            assert_eq!(obj.publisher_priority, 64);
        }
        _ => panic!("Object が期待された"),
    }
}

#[test]
fn test_encode_header() {
    let encoder = FetchStreamEncoder::new(42);
    let header_bytes = encoder.encode_header();

    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&header_bytes);
    let header = decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(header.request_id, 42);
}

// 同一 group 内で subgroup だけ変更された場合、Object ID Delta フィールドは
// delta としてエンコードされなければならない (draft-ietf-moq-transport-21 §11.4.1.1:
// "When the Group ID Delta field is not present, the Object ID is the prior Object's ID
// plus the Object ID Delta if present")。
// 修正前は subgroup_changed が true のとき絶対値を乗せていたため、decoder 側で
// delta として誤解釈されラウンドトリップが壊れていた。

#[test]
fn test_subgroup_change_with_object_id_delta() {
    // 同一 group, subgroup 変更, object_id が prior+1 ではない → delta 計算経路
    // 修正前: encoder が絶対値 10 を書き込み、decoder は 5 + 10 + 1 = 16 と誤解釈した
    let entries = encode_and_decode(
        1,
        &[
            (
                FetchObjectInput {
                    group_id: 0,
                    subgroup_id: 0,
                    object_id: 5,
                    publisher_priority: 128,
                    has_properties: false,
                    is_datagram_origin: false,
                    payload_length: 1,
                },
                b"a",
            ),
            (
                FetchObjectInput {
                    group_id: 0,
                    subgroup_id: 1, // subgroup 変更
                    object_id: 10,  // prior(5) + 1 ではない → delta = 10 - 5 - 1 = 4
                    publisher_priority: 128,
                    has_properties: false,
                    is_datagram_origin: false,
                    payload_length: 1,
                },
                b"b",
            ),
        ],
    );

    assert_eq!(entries.len(), 2);
    match &entries[1] {
        DecodedFetchEntry::Object(obj) => {
            assert_eq!(obj.group_id, 0);
            assert_eq!(obj.subgroup_id, 1);
            assert_eq!(obj.object_id, 10);
        }
        _ => panic!("Object が期待された"),
    }
}

/// draft-ietf-moq-transport-21 §11.4.1.2 (End of Range):
/// encode_end_of_unknown_range で生成したバイト列がデコーダで
/// DecodedFetchEntry::EndOfUnknownRange に復号され、後続 Object も
/// 正しくラウンドトリップすること
#[test]
fn end_of_unknown_range_then_object() {
    let mut encoder = FetchStreamEncoder::new(1);
    let mut stream = encoder.encode_header();

    // End of Unknown Range
    encoder
        .encode_end_of_unknown_range(5, 10, &mut stream)
        .expect("テストフィクスチャの前提条件を満たす");

    // Object (NoPriorActualObject 文脈)
    let input = FetchObjectInput {
        group_id: 5, // End of Unknown Range の group_id と同じ → 継承可能だが NoPriorActualObject
        subgroup_id: 0,
        object_id: 11,
        publisher_priority: 64,
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 1,
    };
    encoder
        .encode_object(&input, None, &mut stream)
        .expect("テストフィクスチャの前提条件を満たす");
    stream.extend_from_slice(b"x");

    // デコードで検証
    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&stream);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");

    // End of Unknown Range として復号されること
    let entry = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert!(matches!(
        entry,
        DecodedFetchEntry::EndOfUnknownRange {
            group_id: 5,
            object_id: 10
        }
    ));

    // 後続 Object も正しく復号されること
    let entry = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    match entry {
        DecodedFetchEntry::Object(obj) => {
            assert_eq!(obj.group_id, 5);
            assert_eq!(obj.object_id, 11);
            assert_eq!(obj.publisher_priority, 64);
        }
        _ => panic!("Object が期待された"),
    }
}

#[test]
fn test_subgroup_change_with_object_id_absent() {
    // 同一 group, subgroup 変更, object_id == prior+1 → Object ID Delta absent 経路
    // 修正前: encoder が絶対値 6 を書き込み、decoder は 5 + 6 + 1 = 12 と誤解釈した
    let entries = encode_and_decode(
        1,
        &[
            (
                FetchObjectInput {
                    group_id: 0,
                    subgroup_id: 0,
                    object_id: 5,
                    publisher_priority: 128,
                    has_properties: false,
                    is_datagram_origin: false,
                    payload_length: 1,
                },
                b"a",
            ),
            (
                FetchObjectInput {
                    group_id: 0,
                    subgroup_id: 1, // subgroup 変更
                    object_id: 6,   // prior(5) + 1 → Object ID Delta absent
                    publisher_priority: 128,
                    has_properties: false,
                    is_datagram_origin: false,
                    payload_length: 1,
                },
                b"b",
            ),
        ],
    );

    assert_eq!(entries.len(), 2);
    match &entries[1] {
        DecodedFetchEntry::Object(obj) => {
            assert_eq!(obj.group_id, 0);
            assert_eq!(obj.subgroup_id, 1);
            assert_eq!(obj.object_id, 6);
        }
        _ => panic!("Object が期待された"),
    }
}

/// draft-ietf-moq-transport-21 §11.4.1 (FETCH stream) Table 7:
/// encode_end_of_timed_out_range で生成したバイト列がデコーダで
/// DecodedFetchEntry::EndOfTimedOutRange としてアプリに通知されること
#[test]
fn end_of_timed_out_range_notifies_app() {
    let mut encoder = FetchStreamEncoder::new(1);
    let mut stream = encoder.encode_header();

    // End of Timed-Out Range
    encoder
        .encode_end_of_timed_out_range(5, 10, &mut stream)
        .expect("テストフィクスチャの前提条件を満たす");

    // デコードで検証
    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&stream);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");

    // End of Timed-Out Range として通知されること
    let entry = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert!(matches!(
        entry,
        DecodedFetchEntry::EndOfTimedOutRange {
            group_id: 5,
            object_id: 10
        }
    ));
}

/// Group Order を指定する公開コンストラクタが decoder と対称に値域検証すること
///
/// draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter):
/// Ascending (0x01) と Descending (0x02) のみ許可される。
#[test]
fn test_new_with_group_order_validates_value() {
    assert!(
        FetchStreamEncoder::new_with_group_order(1, 0x01).is_ok(),
        "Ascending (0x01) は有効な Group Order であること"
    );
    assert!(
        FetchStreamEncoder::new_with_group_order(1, 0x02).is_ok(),
        "Descending (0x02) は有効な Group Order であること"
    );
    assert!(
        matches!(
            FetchStreamEncoder::new_with_group_order(1, 0x00),
            Err(MessageError::ProtocolViolation(_))
        ),
        "0x00 は ProtocolViolation になること"
    );
    assert!(
        matches!(
            FetchStreamEncoder::new_with_group_order(1, 0x03),
            Err(MessageError::ProtocolViolation(_))
        ),
        "0x03 は ProtocolViolation になること"
    );
}

/// Descending (0x02) を指定したエンコーダの出力が、同じ Group Order の
/// デコーダで正しく復号できること
#[test]
fn test_descending_group_order_roundtrip() {
    let mut encoder =
        FetchStreamEncoder::new_with_group_order(1, 0x02).expect("Descending は有効な Group Order");
    let mut stream = encoder.encode_header();

    // Descending では group_id が減少する向きにデルタ圧縮される
    for group_id in [5u64, 4] {
        encoder
            .encode_object(
                &FetchObjectInput {
                    group_id,
                    subgroup_id: 0,
                    object_id: 0,
                    publisher_priority: 128,
                    has_properties: false,
                    is_datagram_origin: false,
                    payload_length: 0,
                },
                None,
                &mut stream,
            )
            .expect("Descending 順のオブジェクトは encode できる");
    }

    let mut decoder =
        FetchStreamDecoder::new_with_group_order(0x02).expect("Descending は有効な Group Order");
    decoder.push(&stream);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");

    let mut groups = Vec::new();
    while let Some(entry) = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
    {
        match entry {
            DecodedFetchEntry::Object(obj) => groups.push(obj.group_id),
            _ => panic!("Object が期待された"),
        }
    }
    assert_eq!(groups, vec![5, 4], "Group ID が復元されること");
}

/// draft-ietf-moq-transport-21 §11.4.1 (FETCH stream) Table 7:
/// End of Timed-Out Range の後に Object が正しくラウンドトリップすること
/// (NoPriorActualObject 文脈の相互作用を検証する)
#[test]
fn end_of_timed_out_range_then_object() {
    let mut encoder = FetchStreamEncoder::new(1);
    let mut stream = encoder.encode_header();

    // End of Timed-Out Range
    encoder
        .encode_end_of_timed_out_range(5, 10, &mut stream)
        .expect("テストフィクスチャの前提条件を満たす");

    // Object (NoPriorActualObject 文脈)
    let input = FetchObjectInput {
        group_id: 5,
        subgroup_id: 0,
        object_id: 11,
        publisher_priority: 64,
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 1,
    };
    encoder
        .encode_object(&input, None, &mut stream)
        .expect("テストフィクスチャの前提条件を満たす");
    stream.extend_from_slice(b"x");

    // デコードで検証
    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&stream);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");

    // End of Timed-Out Range として復号されること
    let entry = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert!(matches!(
        entry,
        DecodedFetchEntry::EndOfTimedOutRange {
            group_id: 5,
            object_id: 10
        }
    ));

    // 後続 Object も正しく復号されること
    let entry = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    match entry {
        DecodedFetchEntry::Object(obj) => {
            assert_eq!(obj.group_id, 5);
            assert_eq!(obj.object_id, 11);
            assert_eq!(obj.publisher_priority, 64);
        }
        _ => panic!("Object が期待された"),
    }
}
