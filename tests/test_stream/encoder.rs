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

/// 先頭 Object が Datagram 起源でもエンコードでき、Subgroup ID は 0 に解決される
///
/// draft-ietf-moq-transport-21 §11.4.1.1 (Flags): bit 0x40 が立つ Datagram 起源では
/// 下位 2 bit を無視するため、Subgroup ID は wire に載らず 0 に解決される。
#[test]
fn test_first_object_datagram_origin() {
    let entries = encode_and_decode(
        1,
        &[(
            FetchObjectInput {
                group_id: 10,
                subgroup_id: 7,
                object_id: 0,
                publisher_priority: 128,
                has_properties: false,
                is_datagram_origin: true,
                payload_length: 3,
            },
            b"abc",
        )],
    );

    assert_eq!(entries.len(), 1);
    match &entries[0] {
        DecodedFetchEntry::Object(obj) => {
            assert_eq!(obj.group_id, 10);
            assert_eq!(
                obj.subgroup_id, 0,
                "Datagram 起源の Subgroup ID は 0 に解決されること"
            );
            assert_eq!(obj.object_id, 0);
            assert_eq!(obj.publisher_priority, 128);
            assert!(obj.is_datagram_origin);
        }
        _ => panic!("Object が期待された"),
    }
}

/// Datagram 起源 Object の後に同一 subgroup_id の通常起源 Object が続いても往復が壊れない
///
/// Datagram 起源 Object の Subgroup ID は wire に載らずデコーダでは 0 に解決されるため、
/// 後続の通常起源 Object が同じ subgroup_id を宣言しても解決値が混ざらないことを確認する。
#[test]
fn test_datagram_origin_then_same_subgroup_id() {
    let entries = encode_and_decode(
        1,
        &[
            (
                FetchObjectInput {
                    group_id: 10,
                    subgroup_id: 7,
                    object_id: 0,
                    publisher_priority: 128,
                    has_properties: false,
                    is_datagram_origin: true,
                    payload_length: 1,
                },
                b"a",
            ),
            (
                FetchObjectInput {
                    group_id: 10,
                    subgroup_id: 7,
                    object_id: 1,
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
    let DecodedFetchEntry::Object(first) = &entries[0] else {
        panic!("Object が期待された");
    };
    assert_eq!(first.subgroup_id, 0);
    assert!(first.is_datagram_origin);
    let DecodedFetchEntry::Object(second) = &entries[1] else {
        panic!("Object が期待された");
    };
    assert_eq!(
        second.subgroup_id, 7,
        "通常起源の後続 Object は明示 subgroup_id 7 に解決されること"
    );
    assert!(!second.is_datagram_origin);
}

/// Datagram 起源の直後に subgroup_id = 0 の通常起源 Object が続く場合は
/// PreviousSame (prior 参照) ではなく Subgroup ID zero (0x00) を選ぶ
///
/// draft-ietf-moq-transport-21 §11.4.1.1 (Flags): Datagram 起源の Object には Subgroup ID が
/// 無いため、Table 8 の "prior Object's Subgroup ID" を参照させず、Zero または Explicit を選ぶ。
/// ラウンドトリップでは直前の Datagram 起源 Object の解決済み Subgroup ID (0) に
/// Zero / Explicit(0) / PreviousSame のいずれも一致してしまいモードを区別できないため、
/// 2 件目の flags バイトの検証が回帰検出の主眼となる。
#[test]
fn test_datagram_origin_then_normal_subgroup_zero_uses_zero_mode() {
    let mut encoder = FetchStreamEncoder::new(1);
    let mut stream = encoder.encode_header();
    // 1 件目: Datagram 起源 (subgroup_id は wire に載らない)
    encoder
        .encode_object(
            &FetchObjectInput {
                group_id: 10,
                subgroup_id: 7,
                object_id: 0,
                publisher_priority: 128,
                has_properties: false,
                is_datagram_origin: true,
                payload_length: 1,
            },
            None,
            &mut stream,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    stream.extend_from_slice(b"a");
    // 2 件目: 通常起源で subgroup_id = 0 (prior 参照を避ける)
    encoder
        .encode_object(
            &FetchObjectInput {
                group_id: 10,
                subgroup_id: 0,
                object_id: 1,
                publisher_priority: 128,
                has_properties: false,
                is_datagram_origin: false,
                payload_length: 1,
            },
            None,
            &mut stream,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    stream.extend_from_slice(b"b");

    assert_eq!(
        stream,
        vec![
            0x05, 0x01, // FetchHeader: type = 0x05, request_id = 1
            0x5C, 0x0A, 0x00, 0x80, 0x01, // 1 件目: Datagram + Group + Object + Priority
            0x61, // payload "a"
            0x00, 0x01, // 2 件目: flags 0x00 (Subgroup ID zero) + payload_length 1
            0x62, // payload "b"
        ],
        "Datagram 起源の直後は PreviousSame ではなく Zero を選ぶこと"
    );
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

/// draft-ietf-moq-transport-21 §11.4.1 (Fetch Header) Table 7:
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

/// Descending テスト用の FetchObjectInput を作る
///
/// `subgroup_id = 0` / `publisher_priority = 128` / Properties 無し / Datagram 起源でないことを
/// 固定する。
fn descending_input(group_id: u64, object_id: u64, payload_length: u64) -> FetchObjectInput {
    FetchObjectInput {
        group_id,
        subgroup_id: 0,
        object_id,
        publisher_priority: 128,
        has_properties: false,
        is_datagram_origin: false,
        payload_length,
    }
}

/// Descending ストリームをデコードして (Group ID, Object ID) を列挙するヘルパー
fn decode_descending_locations(stream: &[u8], count: usize) -> Vec<(u64, u64)> {
    let mut decoder =
        FetchStreamDecoder::new_with_group_order(0x02).expect("Descending は有効な Group Order");
    decoder.push(stream);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");

    let mut locations = Vec::new();
    for _ in 0..count {
        let entry = decoder
            .try_decode_entry()
            .expect("テストフィクスチャの前提条件を満たす")
            .expect("テストフィクスチャに期待される内部値が入っている");
        match entry {
            DecodedFetchEntry::Object(object) => {
                locations.push((object.group_id, object.object_id));
                // ペイロード長 0 の Object は読み出すペイロードが無い
                if object.payload_length > 0 {
                    drain_fetch_payload(&mut decoder, object.payload_length);
                }
            }
            other => panic!("Object が期待された: {other:?}"),
        }
    }
    decoder
        .finish()
        .expect("エントリ境界で終端したストリームは finish で受容される");
    locations
}

/// Descending (0x02) で複数 Group をスキップしても絶対値が保存されること
///
/// draft-ietf-moq-transport-22 §3.2.2 (Gaps in a Fetch Stream) は Descending Group Order で
/// Start と End Location が異なる Group にある場合のギャップの期待順序を定める。
/// デコーダは絶対 Location を返すだけでギャップの意味解釈 (非存在 / 不明の区別や
/// 末尾ギャップの FIN 判定) はアプリ責務であるため、ここではスキップした Group でも
/// 絶対値が保存されることを固定する。
#[test]
fn test_descending_group_order_skips_multiple_groups() {
    let mut encoder =
        FetchStreamEncoder::new_with_group_order(1, 0x02).expect("Descending は有効な Group Order");
    let mut stream = encoder.encode_header();

    // 10 → 3 → 1 と大きくスキップし、Group 内では Object ID を増やす
    let objects = [(10u64, 0u64), (10, 1), (3, 0), (3, 5), (1, 0)];
    for (index, (group_id, object_id)) in objects.iter().enumerate() {
        // ペイロードを持つ Object を 1 つ混ぜ、デコード側のペイロード読み出しも通す
        let payload_length = if index == 2 { 1 } else { 0 };
        encoder
            .encode_object(
                &descending_input(*group_id, *object_id, payload_length),
                None,
                &mut stream,
            )
            .expect("Descending 順のオブジェクトは encode できる");
        if payload_length > 0 {
            stream.push(b'x');
        }
    }

    let locations = decode_descending_locations(&stream, objects.len());
    assert_eq!(
        locations,
        vec![(10, 0), (10, 1), (3, 0), (3, 5), (1, 0)],
        "Group をスキップしても絶対値が保存されること"
    );
}

/// Descending (0x02) で Group が変わるとき Object ID が絶対値で符号化されること
///
/// draft-ietf-moq-transport-22 §11.4.1.1 (Flags): Group ID Delta が present なら Object ID は
/// Object ID Delta の値 (absent なら prior + 1)、not present なら prior Object ID + Object ID Delta
/// (absent なら prior + 1) になる。Group 変化で Object ID が大きく減る入力 (5001 → 1) を使い、
/// デルタ解釈では復元できない値であることを固定する。
#[test]
fn test_descending_group_change_uses_absolute_object_id() {
    let mut encoder =
        FetchStreamEncoder::new_with_group_order(1, 0x02).expect("Descending は有効な Group Order");
    let mut stream = encoder.encode_header();

    let objects = [(10u64, 5_000u64), (10, 5_001), (9, 1), (9, 2)];
    for (group_id, object_id) in objects {
        encoder
            .encode_object(&descending_input(group_id, object_id, 0), None, &mut stream)
            .expect("Descending 順のオブジェクトは encode できる");
    }

    let locations = decode_descending_locations(&stream, objects.len());
    assert_eq!(
        locations,
        vec![(10, 5_000), (10, 5_001), (9, 1), (9, 2)],
        "Group 変化時は Object ID が絶対値として復元されること"
    );
}

/// Descending (0x02) で End of Range (0x8C / 0x10C / 0x20C) と Object を混在できること
///
/// draft-ietf-moq-transport-22 §11.4.1.2 (End of Range): End of Range の Group ID /
/// Object ID は絶対値で、以後の prior 参照文脈はその値を使う。3 種すべての End of Range で
/// Group を直前 Object の Group と変えることで、エンコーダが End of Range の値を prior として
/// デルタを計算していることを検証する。エントリ境界で終端したストリームを `finish` が
/// 受容することも合わせて確認する。
#[test]
fn test_descending_end_of_range_mixed_with_objects() {
    let mut encoder =
        FetchStreamEncoder::new_with_group_order(1, 0x02).expect("Descending は有効な Group Order");
    let mut stream = encoder.encode_header();

    // 10/2 Object → 9/5 End of Non-Existent Range → 5/0 Object → 5/3 Object
    // → 4/9 End of Unknown Range → 2/0 Object → 1/11 End of Timed-Out Range → 1/12 Object
    encoder
        .encode_object(&descending_input(10, 2, 1), None, &mut stream)
        .expect("テストフィクスチャの前提条件を満たす");
    stream.extend_from_slice(b"a");
    encoder
        .encode_end_of_non_existent_range(9, 5, &mut stream)
        .expect("テストフィクスチャの前提条件を満たす");
    encoder
        .encode_object(&descending_input(5, 0, 1), None, &mut stream)
        .expect("テストフィクスチャの前提条件を満たす");
    stream.extend_from_slice(b"b");
    encoder
        .encode_object(&descending_input(5, 3, 1), None, &mut stream)
        .expect("テストフィクスチャの前提条件を満たす");
    stream.extend_from_slice(b"c");
    encoder
        .encode_end_of_unknown_range(4, 9, &mut stream)
        .expect("テストフィクスチャの前提条件を満たす");
    encoder
        .encode_object(&descending_input(2, 0, 1), None, &mut stream)
        .expect("テストフィクスチャの前提条件を満たす");
    stream.extend_from_slice(b"d");
    encoder
        .encode_end_of_timed_out_range(1, 11, &mut stream)
        .expect("テストフィクスチャの前提条件を満たす");
    encoder
        .encode_object(&descending_input(1, 12, 1), None, &mut stream)
        .expect("テストフィクスチャの前提条件を満たす");
    stream.extend_from_slice(b"e");

    let mut decoder =
        FetchStreamDecoder::new_with_group_order(0x02).expect("Descending は有効な Group Order");
    decoder.push(&stream);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");

    let mut entries = Vec::new();
    while let Some(entry) = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
    {
        match &entry {
            DecodedFetchEntry::Object(_) => drain_fetch_payload(&mut decoder, 1),
            DecodedFetchEntry::EndOfNonExistentRange { .. }
            | DecodedFetchEntry::EndOfUnknownRange { .. }
            | DecodedFetchEntry::EndOfTimedOutRange { .. } => {}
        }
        entries.push(entry);
    }
    assert_eq!(
        entries,
        vec![
            decoded_fetch_object(10, 2),
            DecodedFetchEntry::EndOfNonExistentRange {
                group_id: 9,
                object_id: 5
            },
            decoded_fetch_object(5, 0),
            decoded_fetch_object(5, 3),
            DecodedFetchEntry::EndOfUnknownRange {
                group_id: 4,
                object_id: 9
            },
            decoded_fetch_object(2, 0),
            DecodedFetchEntry::EndOfTimedOutRange {
                group_id: 1,
                object_id: 11
            },
            decoded_fetch_object(1, 12),
        ],
        "End of Range を混在させても絶対位置が解決されること"
    );
    decoder
        .finish()
        .expect("エントリ境界で終端したストリームは finish で受容される");
}

/// draft-ietf-moq-transport-21 §11.4.1 (Fetch Header) Table 7:
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

// ============================================================================
// Publisher Priority の Subgroup 単位検証 (draft-ietf-moq-transport-21 §12.1)
// ============================================================================
//
// §12.1 (Malformed Tracks) 条件 1:
// "An Object with a particular Subgroup ID is received, but its Publisher Priority
//  is different from that of the previous Object with the same Subgroup ID."
// encoder も decoder と同じ粒度 ((group_id, subgroup_id) ごとの最後の Priority) で
// 検証し、自身の decoder が拒否するワイヤを生成しないことを検証する。

/// 検証用の FetchObjectInput を作る
fn priority_input(
    group_id: u64,
    subgroup_id: u64,
    object_id: u64,
    publisher_priority: u8,
    is_datagram_origin: bool,
) -> FetchObjectInput {
    FetchObjectInput {
        group_id,
        subgroup_id,
        object_id,
        publisher_priority,
        has_properties: false,
        is_datagram_origin,
        payload_length: 1,
    }
}

/// 同じ Subgroup の Priority 変更は、間に別 Subgroup が挟まっても拒否される
#[test]
fn encoder_rejects_priority_change_after_other_subgroup() {
    let mut encoder = FetchStreamEncoder::new(0);
    let mut stream = encoder.encode_header();

    encoder
        .encode_object(&priority_input(0, 0, 0, 128, false), None, &mut stream)
        .expect("テストフィクスチャの前提条件を満たす");
    encoder
        .encode_object(&priority_input(0, 1, 1, 64, false), None, &mut stream)
        .expect("別 Subgroup は異なる Priority でも送れる");

    let err = encoder
        .encode_object(&priority_input(0, 0, 2, 32, false), None, &mut stream)
        .expect_err("同一 Subgroup の Priority 変更は拒否される");
    assert!(matches!(err, MessageError::ProtocolViolation(_)));
}

/// 同じ Subgroup の Priority 変更は、間に datagram 起源 Object が挟まっても拒否される
#[test]
fn encoder_rejects_priority_change_after_datagram_origin_object() {
    let mut encoder = FetchStreamEncoder::new(0);
    let mut stream = encoder.encode_header();

    encoder
        .encode_object(&priority_input(0, 0, 0, 128, false), None, &mut stream)
        .expect("テストフィクスチャの前提条件を満たす");
    encoder
        .encode_object(&priority_input(0, 0, 1, 64, true), None, &mut stream)
        .expect("datagram 起源 Object は Subgroup の Priority 記録に影響しない");

    let err = encoder
        .encode_object(&priority_input(0, 0, 2, 32, false), None, &mut stream)
        .expect_err("同一 Subgroup の Priority 変更は拒否される");
    assert!(matches!(err, MessageError::ProtocolViolation(_)));
}

/// 正常系: 同一 Subgroup で同一 Priority の連続、Subgroup 変更直後の異なる Priority、
/// datagram 起源 Object のみの列は拒否されない
#[test]
fn encoder_accepts_valid_priority_sequences() {
    let mut encoder = FetchStreamEncoder::new(0);
    let mut stream = encoder.encode_header();

    // 同一 Subgroup で同一 Priority
    encoder
        .encode_object(&priority_input(0, 0, 0, 128, false), None, &mut stream)
        .expect("テストフィクスチャの前提条件を満たす");
    encoder
        .encode_object(&priority_input(0, 0, 1, 128, false), None, &mut stream)
        .expect("同一 Subgroup の同一 Priority は送れる");

    // Subgroup 変更直後は異なる Priority でも送れる
    encoder
        .encode_object(&priority_input(0, 1, 2, 64, false), None, &mut stream)
        .expect("Subgroup 変更直後の Priority 変更は送れる");

    // datagram 起源 Object のみの列 (Subgroup ID を持たないため検証対象外)
    let mut datagram_encoder = FetchStreamEncoder::new(0);
    let mut datagram_stream = datagram_encoder.encode_header();
    datagram_encoder
        .encode_object(
            &priority_input(0, 0, 0, 128, true),
            None,
            &mut datagram_stream,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    datagram_encoder
        .encode_object(
            &priority_input(0, 0, 1, 64, true),
            None,
            &mut datagram_stream,
        )
        .expect("datagram 起源 Object は Priority が異なっても送れる");
}
