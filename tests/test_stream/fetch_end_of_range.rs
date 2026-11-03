//! Fetch ストリームの End of Range のデルタ符号化 (draft-ietf-moq-transport-23 §11.4.2) の外部テスト
//!
//! バイト列は仕様の規則から手計算した golden として固定する。エンコーダが生成する
//! バイト列と、そのバイト列をデコーダが解決した絶対 Location の両方を検証する。
use super::*;

/// テスト用の FETCH_HEADER: type 0x05 + Request ID 1
const FETCH_HEADER_BYTES: [u8; 2] = [0x05, 0x01];

/// golden バイト列を先頭から順にデコードし、Object のペイロードも読み出す
///
/// End of Range はペイロードを持たないため何も消費しない。
fn decode_golden_stream(bytes: &[u8], group_order: u8) -> Vec<DecodedFetchEntry> {
    let mut decoder = FetchStreamDecoder::new_with_group_order(group_order)
        .expect("0x01 / 0x02 は有効な Group Order");
    decoder.push(bytes);
    let header = decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(
        header.request_id, 1,
        "FETCH_HEADER の Request ID が復号されること"
    );

    let mut entries = Vec::new();
    while let Some(entry) = decoder
        .try_decode_entry()
        .expect("golden バイト列はデコードできる")
    {
        if let DecodedFetchEntry::Object(ref object) = entry
            && object.payload_length > 0
        {
            decoder
                .try_read_payload()
                .expect("golden バイト列のペイロードを読み出せる");
        }
        entries.push(entry);
    }
    decoder
        .finish()
        .expect("エントリ境界で終端したストリームは finish で受容される");
    entries
}

/// 期待する Object エントリを作る
///
/// Subgroup ID 0 / Publisher Priority 128 / Datagram 起源でない / Properties なしを固定する。
fn expected_object(group_id: u64, object_id: u64, payload_length: u64) -> DecodedFetchEntry {
    DecodedFetchEntry::Object(DecodedFetchObject {
        group_id,
        subgroup_id: 0,
        object_id,
        publisher_priority: 128,
        is_datagram_origin: false,
        payload_length,
        properties_bytes: None,
    })
}

/// FETCH_HEADER 直後の End of Range は絶対 ID、Object の直後は直前 Location のデルタになること
///
/// draft-ietf-moq-transport-23 §11.4.2.1 (ID Decoding): 最初の entry の
/// Group ID Delta / Object ID Delta は絶対 ID で、それ以外は直前の Object または
/// End of Range の Location からのデルタである。同じ 4 バイトが位置によって
/// 異なる Location に解決されることを固定する。
#[test]
fn first_end_of_range_after_header_is_absolute_and_after_object_is_delta() {
    // FETCH_HEADER 直後: Group 3 / Object 4 の絶対 ID
    let mut first = FETCH_HEADER_BYTES.to_vec();
    first.extend_from_slice(&[
        0x80, 0x8C, // End of Non-Existent Range (0x8C) の Serialization Flags
        0x03, // Group ID Delta = 3 (絶対 Group ID)
        0x04, // Object ID Delta = 4 (絶対 Object ID)
    ]);
    assert_eq!(
        decode_golden_stream(&first, 0x01),
        vec![DecodedFetchEntry::EndOfNonExistentRange {
            group_id: 3,
            object_id: 4
        }],
        "FETCH_HEADER 直後の End of Range は絶対 ID として解決されること"
    );

    // Object (Group 1 / Object 0) の直後: Group 1 + 3 = 4、Group が変わるため Object ID は絶対値 4
    let mut after_object = FETCH_HEADER_BYTES.to_vec();
    after_object.extend_from_slice(&[
        0x1F, // flags: Group ID (0x08) + Object ID (0x04) + Priority (0x10) + Subgroup ID (0x03)
        0x01, // Group ID Delta = 1 (最初の Object なので絶対 Group ID)
        0x00, // Subgroup ID = 0
        0x00, // Object ID Delta = 0 (最初の Object なので絶対 Object ID)
        0x80, // Publisher Priority = 128
        0x00, // Object Payload Length = 0
        0x80, 0x8C, // End of Non-Existent Range (0x8C)
        0x03, // Group ID Delta = 3 (prior Group ID 1 + 3)
        0x04, // Object ID Delta = 4 (Group が変わるので新しい Group の絶対 Object ID)
    ]);
    assert_eq!(
        decode_golden_stream(&after_object, 0x01),
        vec![
            expected_object(1, 0, 0),
            DecodedFetchEntry::EndOfNonExistentRange {
                group_id: 4,
                object_id: 4
            },
        ],
        "Object の直後の End of Range は直前 Location のデルタとして解決されること"
    );
}

/// 連続する End of Range が直前の End of Range の Location を基準に解決されること
///
/// draft-ietf-moq-transport-23 §11.4.2.1 (ID Decoding): 基準になる Location は
/// 「直前の Object または End of Range」であり、Object がなくても最初の
/// End of Range の Location を基準にできる。Group ID Delta が 0 のときだけ
/// Object ID が prior + delta + 1 になり、Group が変わる場合は絶対 Object ID になる。
#[test]
fn consecutive_end_of_range_indicators_chain_previous_locations() {
    let mut bytes = FETCH_HEADER_BYTES.to_vec();
    bytes.extend_from_slice(&[
        // End of Non-Existent Range: 先頭なので絶対 ID (5, 10)
        0x80, 0x8C, 0x05, 0x0A,
        // End of Unknown Range: 同一 Group (delta 0) で Object ID は 10 + 1 + 1 = 12
        0x81, 0x0C, 0x00, 0x01,
        // End of Timed-Out Range: Group 5 + 3 = 8、Group が変わるので Object ID は絶対値 0
        0x82, 0x0C, 0x03, 0x00,
        // End of Non-Existent Range: 同一 Group (8) で Object ID は 0 + 4 + 1 = 5
        0x80, 0x8C, 0x00, 0x04,
    ]);
    assert_eq!(
        decode_golden_stream(&bytes, 0x01),
        vec![
            DecodedFetchEntry::EndOfNonExistentRange {
                group_id: 5,
                object_id: 10
            },
            DecodedFetchEntry::EndOfUnknownRange {
                group_id: 5,
                object_id: 12
            },
            DecodedFetchEntry::EndOfTimedOutRange {
                group_id: 8,
                object_id: 0
            },
            DecodedFetchEntry::EndOfNonExistentRange {
                group_id: 8,
                object_id: 5
            },
        ],
        "連続する End of Range が直前の End of Range の Location を基準に解決されること"
    );
}

/// 昇順の End of Range の golden バイト列を組み立てる
///
/// Object と End of Range を交互に置き、Group をまたぐ End of Range の Group ID Delta には
/// Object と違って +1 が付かないこと (§11.4.2.1 (ID Decoding))、同一 Group の
/// End of Range は prior + delta + 1 になることを固定する。
fn ascending_golden() -> Vec<u8> {
    let mut bytes = Vec::new();
    // FETCH_HEADER (type 0x05 / Request ID 1)
    bytes.extend_from_slice(&[0x05, 0x01]);
    // Object: Group 1 / Subgroup 0 / Object 0 / Priority 128 / payload 1
    bytes.extend_from_slice(&[0x1F, 0x01, 0x00, 0x00, 0x80, 0x01, b'a']);
    // End of Non-Existent Range: Group 1 + 2 = 3 / Object は絶対値 2
    bytes.extend_from_slice(&[0x80, 0x8C, 0x02, 0x02]);
    // Object: Group 3 / Object 2 + 1 = 3 / Subgroup と Priority は直前の actual Object から継承
    bytes.extend_from_slice(&[0x01, 0x01, b'b']);
    // End of Unknown Range: Group 3 / Object 3 + 0 + 1 = 4
    bytes.extend_from_slice(&[0x81, 0x0C, 0x00, 0x00]);
    // End of Timed-Out Range: Group 3 + 2 = 5 / Object は絶対値 0
    bytes.extend_from_slice(&[0x82, 0x0C, 0x02, 0x00]);
    bytes
}

/// 昇順の golden バイト列をデコーダが絶対 Location に解決できること
#[test]
fn ascending_end_of_range_golden_bytes_decode() {
    assert_eq!(
        decode_golden_stream(&ascending_golden(), 0x01),
        vec![
            expected_object(1, 0, 1),
            DecodedFetchEntry::EndOfNonExistentRange {
                group_id: 3,
                object_id: 2
            },
            expected_object(3, 3, 1),
            DecodedFetchEntry::EndOfUnknownRange {
                group_id: 3,
                object_id: 4
            },
            DecodedFetchEntry::EndOfTimedOutRange {
                group_id: 5,
                object_id: 0
            },
        ],
        "昇順の End of Range と Object の交互配置が絶対 Location に解決されること"
    );
}

/// 昇順の golden バイト列をエンコーダが生成できること
///
/// エンコーダの公開 API は絶対 Location を受け取り、同じ golden バイト列を生成しなければならない。
/// Group ID Delta の +1 規則が Object と End of Range で異なること、End of Range を挟んでも
/// 直前の actual Object の Subgroup ID / Priority を保持することも併せて固定する。
#[test]
fn ascending_end_of_range_golden_bytes_encode() {
    let mut encoder = FetchStreamEncoder::new(1);
    let mut bytes = encoder.encode_header();
    let objects = [
        (1u64, 0u64, Some(b"a".as_slice())),
        (3, 3, Some(b"b".as_slice())),
    ];
    for (group_id, object_id, payload) in objects {
        encoder
            .encode_object(
                &FetchObjectInput {
                    group_id,
                    subgroup_id: 0,
                    object_id,
                    publisher_priority: 128,
                    has_properties: false,
                    is_datagram_origin: false,
                    payload_length: payload.map_or(0, |p| p.len() as u64),
                },
                None,
                &mut bytes,
            )
            .expect("golden バイト列の Object はエンコードできる");
        if let Some(payload) = payload {
            bytes.extend_from_slice(payload);
        }
        match (group_id, object_id) {
            (1, 0) => encoder
                .encode_end_of_non_existent_range(3, 2, &mut bytes)
                .expect("End of Non-Existent Range はエンコードできる"),
            (3, 3) => {
                encoder
                    .encode_end_of_unknown_range(3, 4, &mut bytes)
                    .expect("End of Unknown Range はエンコードできる");
                encoder
                    .encode_end_of_timed_out_range(5, 0, &mut bytes)
                    .expect("End of Timed-Out Range はエンコードできる");
            }
            _ => unreachable!("golden の Object は 2 件だけである"),
        }
    }
    assert_eq!(
        bytes,
        ascending_golden(),
        "昇順の End of Range が golden バイト列どおりにエンコードされること"
    );
}

/// 降順の End of Range の golden バイト列を組み立てる
///
/// draft-ietf-moq-transport-23 §11.4.2.1 (ID Decoding): 降順では Group ID を
/// 減算する。同一 Group の End of Range は prior + delta + 1 になる。
fn descending_golden() -> Vec<u8> {
    let mut bytes = Vec::new();
    // FETCH_HEADER (type 0x05 / Request ID 1)
    bytes.extend_from_slice(&[0x05, 0x01]);
    // Object: Group 10 / Subgroup 0 / Object 5 / Priority 128 / payload 0
    bytes.extend_from_slice(&[0x1F, 0x0A, 0x00, 0x05, 0x80, 0x00]);
    // End of Unknown Range: Group 10 - 3 = 7 / Object は絶対値 2
    bytes.extend_from_slice(&[0x81, 0x0C, 0x03, 0x02]);
    // Object: Group 7 / Object 2 + 1 = 3 / Subgroup と Priority は直前の actual Object から継承
    bytes.extend_from_slice(&[0x01, 0x00]);
    // End of Timed-Out Range: Group 7 / Object 3 + 2 + 1 = 6
    bytes.extend_from_slice(&[0x82, 0x0C, 0x00, 0x02]);
    // Object: Group 7 - (2 + 1) = 4 / Object は絶対値 0 / Priority は直前の actual Object と同値なので省略
    bytes.extend_from_slice(&[0x0F, 0x02, 0x00, 0x00, 0x00]);
    bytes
}

/// 降順の golden バイト列をデコーダが絶対 Location に解決できること
#[test]
fn descending_end_of_range_golden_bytes_decode() {
    assert_eq!(
        decode_golden_stream(&descending_golden(), 0x02),
        vec![
            expected_object(10, 5, 0),
            DecodedFetchEntry::EndOfUnknownRange {
                group_id: 7,
                object_id: 2
            },
            expected_object(7, 3, 0),
            DecodedFetchEntry::EndOfTimedOutRange {
                group_id: 7,
                object_id: 6
            },
            expected_object(4, 0, 0),
        ],
        "降順の End of Range が減算で解決されること"
    );
}

/// 降順の golden バイト列をエンコーダが生成できること
#[test]
fn descending_end_of_range_golden_bytes_encode() {
    let mut encoder =
        FetchStreamEncoder::new_with_group_order(1, 0x02).expect("Descending は有効な Group Order");
    let mut bytes = encoder.encode_header();
    encoder
        .encode_object(
            &FetchObjectInput {
                group_id: 10,
                subgroup_id: 0,
                object_id: 5,
                publisher_priority: 128,
                has_properties: false,
                is_datagram_origin: false,
                payload_length: 0,
            },
            None,
            &mut bytes,
        )
        .expect("golden バイト列の先頭 Object はエンコードできる");
    encoder
        .encode_end_of_unknown_range(7, 2, &mut bytes)
        .expect("End of Unknown Range はエンコードできる");
    encoder
        .encode_object(
            &FetchObjectInput {
                group_id: 7,
                subgroup_id: 0,
                object_id: 3,
                publisher_priority: 128,
                has_properties: false,
                is_datagram_origin: false,
                payload_length: 0,
            },
            None,
            &mut bytes,
        )
        .expect("End of Range の直後の Object はエンコードできる");
    encoder
        .encode_end_of_timed_out_range(7, 6, &mut bytes)
        .expect("End of Timed-Out Range はエンコードできる");
    encoder
        .encode_object(
            &FetchObjectInput {
                group_id: 4,
                subgroup_id: 0,
                object_id: 0,
                publisher_priority: 128,
                has_properties: false,
                is_datagram_origin: false,
                payload_length: 0,
            },
            None,
            &mut bytes,
        )
        .expect("Group をまたぐ Object はエンコードできる");
    assert_eq!(
        bytes,
        descending_golden(),
        "降順の End of Range が golden バイト列どおりにエンコードされること"
    );
}

/// 先頭の End of Range をエンコーダが絶対 ID で書き出すこと
///
/// draft-ietf-moq-transport-23 §11.4.2.1 (ID Decoding): FETCH_HEADER 直後の
/// End of Range は直前 Location が存在しないため、両フィールドが絶対 ID になる。
#[test]
fn encoder_writes_first_end_of_range_as_absolute_ids() {
    let mut encoder = FetchStreamEncoder::new(1);
    let mut bytes = encoder.encode_header();
    encoder
        .encode_end_of_non_existent_range(5, 10, &mut bytes)
        .expect("先頭の End of Non-Existent Range はエンコードできる");
    assert_eq!(
        bytes,
        vec![
            0x05, 0x01, // FETCH_HEADER
            0x80, 0x8C, 0x05,
            0x0A, // End of Non-Existent Range (絶対 Group 5 / 絶対 Object 10)
        ],
        "先頭の End of Range が絶対 ID で書き出されること"
    );
}

/// 昇順の Group ID の加算 overflow が DELTA_ENCODING_OVERFLOW になること
///
/// draft-ietf-moq-transport-23 §11.4.2.1 (ID Decoding): 解決結果が 2^64 - 1 を
/// 超える場合は DELTA_ENCODING_OVERFLOW (0x400) でセッションを閉じる。
#[test]
fn ascending_group_id_addition_overflow_is_delta_encoding_overflow() {
    let mut bytes = FETCH_HEADER_BYTES.to_vec();
    bytes.extend_from_slice(&[0x80, 0x8C]);
    // 最初の End of Range: Group ID = u64::MAX (絶対値) / Object ID = 0
    bytes.extend_from_slice(&[0xFF; 9]);
    bytes.push(0x00);
    // 2 番目の End of Range: Group ID Delta = 1 で u64::MAX + 1 が overflow する
    bytes.extend_from_slice(&[0x81, 0x0C, 0x01, 0x00]);

    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&bytes);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    let first = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert!(matches!(
        first,
        DecodedFetchEntry::EndOfNonExistentRange {
            group_id: u64::MAX,
            object_id: 0
        }
    ));
    assert!(
        matches!(
            decoder.try_decode_entry(),
            Err(MessageError::DeltaEncodingOverflow(_))
        ),
        "Group ID の加算 overflow は DELTA_ENCODING_OVERFLOW になること"
    );
}

/// 降順の Group ID の減算 underflow が DELTA_ENCODING_OVERFLOW になること
///
/// draft-ietf-moq-transport-23 §11.4.2.1 (ID Decoding): 解決結果が 0 未満になる場合は
/// DELTA_ENCODING_OVERFLOW (0x400) でセッションを閉じる。
#[test]
fn descending_group_id_subtraction_underflow_is_delta_encoding_overflow() {
    let mut bytes = FETCH_HEADER_BYTES.to_vec();
    bytes.extend_from_slice(&[
        0x80, 0x8C, 0x00, 0x00, // 最初の End of Range: 絶対 ID (0, 0)
        0x81, 0x0C, 0x01, 0x00, // Group ID Delta = 1 で 0 - 1 が underflow する
    ]);

    let mut decoder =
        FetchStreamDecoder::new_with_group_order(0x02).expect("Descending は有効な Group Order");
    decoder.push(&bytes);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    let first = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert!(matches!(
        first,
        DecodedFetchEntry::EndOfNonExistentRange {
            group_id: 0,
            object_id: 0
        }
    ));
    assert!(
        matches!(
            decoder.try_decode_entry(),
            Err(MessageError::DeltaEncodingOverflow(_))
        ),
        "Group ID の減算 underflow は DELTA_ENCODING_OVERFLOW になること"
    );
}

/// 同一 Group の Object ID の加算 overflow が DELTA_ENCODING_OVERFLOW になること
///
/// draft-ietf-moq-transport-23 §11.4.2.1 (ID Decoding): Group ID Delta が 0 のときは
/// Object ID が prior + delta + 1 になるため、prior が u64::MAX で delta 0 でも overflow する。
#[test]
fn same_group_object_id_addition_overflow_is_delta_encoding_overflow() {
    let mut bytes = FETCH_HEADER_BYTES.to_vec();
    bytes.extend_from_slice(&[0x80, 0x8C, 0x00]);
    // 最初の End of Range: Group ID = 0 (絶対値) / Object ID = u64::MAX (絶対値)
    bytes.extend_from_slice(&[0xFF; 9]);
    // 2 番目の End of Range: Group ID Delta = 0 / Object ID Delta = 0 で u64::MAX + 0 + 1 が overflow
    bytes.extend_from_slice(&[0x81, 0x0C, 0x00, 0x00]);

    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&bytes);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    let first = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert!(matches!(
        first,
        DecodedFetchEntry::EndOfNonExistentRange {
            group_id: 0,
            object_id: u64::MAX
        }
    ));
    assert!(
        matches!(
            decoder.try_decode_entry(),
            Err(MessageError::DeltaEncodingOverflow(_))
        ),
        "Object ID の加算 overflow は DELTA_ENCODING_OVERFLOW になること"
    );
}

/// End of Range が直前の actual Object の Subgroup ID / Priority を保持すること
///
/// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields): End of Range の後の
/// Object が prior 参照を使う場合、Prior Subgroup ID / Priority は End of Range より前の
/// 最後の actual Object の値になる。End of Range は Group ID / Object ID だけを更新する。
#[test]
fn end_of_range_preserves_prior_actual_object_fields() {
    let mut bytes = FETCH_HEADER_BYTES.to_vec();
    bytes.extend_from_slice(&[
        0x1F, // Object flags: Group ID + Object ID + Priority + Subgroup ID
        0x01, // Group ID Delta = 1 (絶対 Group ID 1)
        0x07, // Subgroup ID = 7
        0x00, // Object ID Delta = 0 (絶対 Object ID 0)
        0xC8, // Publisher Priority = 200
        0x00, // Object Payload Length = 0
        0x80, 0x8C, 0x02, 0x02, // End of Non-Existent Range (Group 3 / Object は絶対値 2)
        0x01, // Object flags: Subgroup ID は prior Object と同じ / Priority は省略 (prior Object)
        0x00, // Object Payload Length = 0
    ]);

    let entries = decode_golden_stream(&bytes, 0x01);
    assert_eq!(entries.len(), 3);
    let DecodedFetchEntry::Object(first) = &entries[0] else {
        panic!("1 件目は Object であること: {:?}", entries[0]);
    };
    assert_eq!(first.subgroup_id, 7);
    assert_eq!(first.publisher_priority, 200);

    let DecodedFetchEntry::Object(second) = &entries[2] else {
        panic!("3 件目は Object であること: {:?}", entries[2]);
    };
    assert_eq!(
        second.group_id, 3,
        "End of Range の Group ID が prior Location になること"
    );
    assert_eq!(
        second.object_id, 3,
        "End of Range の Object ID + 1 が Object ID になること"
    );
    assert_eq!(
        second.subgroup_id, 7,
        "End of Range は直前の actual Object の Subgroup ID を消さないこと"
    );
    assert_eq!(
        second.publisher_priority, 200,
        "End of Range は直前の actual Object の Priority を消さないこと"
    );
}

/// actual Object が未出現のまま prior Subgroup ID を参照すると MALFORMED_OBJECT になること
///
/// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields): prior Subgroup ID は
/// End of Range より前の最後の actual Object の値であり、先行する actual Object が
/// 無い場合は MALFORMED_OBJECT でセッションを閉じる。
#[test]
fn prior_subgroup_id_without_actual_object_is_malformed_object() {
    let mut bytes = FETCH_HEADER_BYTES.to_vec();
    bytes.extend_from_slice(&[
        0x80, 0x8C, 0x05, 0x0A, // 先頭の End of Non-Existent Range (絶対 ID 5, 10)
        0x01, // Object flags: Subgroup ID は prior Object と同じ / Group ID / Object ID / Priority は省略
        0x00, // Object Payload Length = 0
    ]);

    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&bytes);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert!(
        matches!(
            decoder.try_decode_entry(),
            Err(MessageError::MalformedObject(_))
        ),
        "actual Object 未出現の prior Subgroup ID 参照は MALFORMED_OBJECT になること"
    );
}

/// actual Object が未出現のまま prior Priority を参照すると MALFORMED_OBJECT になること
///
/// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields): prior Priority は
/// End of Range より前の最後の actual Object の値であり、先行する actual Object が
/// 無い場合は MALFORMED_OBJECT でセッションを閉じる。
#[test]
fn prior_priority_without_actual_object_is_malformed_object() {
    let mut bytes = FETCH_HEADER_BYTES.to_vec();
    bytes.extend_from_slice(&[
        0x80, 0x8C, 0x05, 0x0A, // 先頭の End of Non-Existent Range (絶対 ID 5, 10)
        0x00, // Object flags: Subgroup ID は 0 / Group ID / Object ID / Priority は省略
        0x00, // Object Payload Length = 0
    ]);

    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&bytes);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert!(
        matches!(
            decoder.try_decode_entry(),
            Err(MessageError::MalformedObject(_))
        ),
        "actual Object 未出現の prior Priority 参照は MALFORMED_OBJECT になること"
    );
}

/// エンコーダが Group Order と逆方向の End of Range を拒否すること
///
/// 昇順では Group ID を減らせず、降順では増やせない。表現できない入力は
/// `ProtocolViolation` にして、デコーダが DELTA_ENCODING_OVERFLOW になるワイヤを
/// 生成しない。
#[test]
fn encoder_rejects_end_of_range_group_id_in_the_wrong_direction() {
    let mut ascending = FetchStreamEncoder::new(1);
    let mut bytes = ascending.encode_header();
    ascending
        .encode_end_of_non_existent_range(5, 10, &mut bytes)
        .expect("先頭の End of Range はエンコードできる");
    assert!(
        matches!(
            ascending.encode_end_of_unknown_range(4, 10, &mut bytes),
            Err(MessageError::ProtocolViolation(_))
        ),
        "昇順で Group ID を減らす End of Range は拒否されること"
    );

    let mut descending =
        FetchStreamEncoder::new_with_group_order(1, 0x02).expect("Descending は有効な Group Order");
    let mut bytes = descending.encode_header();
    descending
        .encode_end_of_non_existent_range(5, 10, &mut bytes)
        .expect("先頭の End of Range はエンコードできる");
    assert!(
        matches!(
            descending.encode_end_of_unknown_range(6, 10, &mut bytes),
            Err(MessageError::ProtocolViolation(_))
        ),
        "降順で Group ID を増やす End of Range は拒否されること"
    );
}

/// エンコーダが同一 Group で直前の Location 以前の Object ID を拒否すること
///
/// draft-ietf-moq-transport-23 §11.4.2.1 (ID Decoding): Group ID Delta が 0 のときの
/// Object ID は prior + delta + 1 であり、prior と同じかそれ以前の Object ID は表現できない。
#[test]
fn encoder_rejects_end_of_range_object_id_not_after_prior_location() {
    let mut encoder = FetchStreamEncoder::new(1);
    let mut bytes = encoder.encode_header();
    encoder
        .encode_end_of_non_existent_range(5, 10, &mut bytes)
        .expect("先頭の End of Range はエンコードできる");

    // 同一 Group で同じ Object ID は prior + delta + 1 で表現できない
    assert!(
        matches!(
            encoder.encode_end_of_unknown_range(5, 10, &mut bytes),
            Err(MessageError::ProtocolViolation(_))
        ),
        "同一 Group で Object ID が増加しない End of Range は拒否されること"
    );
    let len_after_error = bytes.len();
    // prior より小さい Object ID も拒否され、失敗時にバッファへ書き込まない
    assert!(
        matches!(
            encoder.encode_end_of_unknown_range(5, 9, &mut bytes),
            Err(MessageError::ProtocolViolation(_))
        ),
        "同一 Group で Object ID が減少する End of Range は拒否されること"
    );
    assert_eq!(
        bytes.len(),
        len_after_error,
        "エラー時に End of Range のバイト列を書き込まないこと"
    );
}
