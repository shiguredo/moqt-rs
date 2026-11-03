//! End of Gone Range (0x40C) の codec と型付きデコーダの外部テスト
//!
//! Gone は「元の publisher が再配送できないが、存在するか否かは知らない」状態であり
//! (draft-ietf-moq-transport-23 §2.1.2 (Object States))、存在しないと確定した Non-Existent
//! (0x8C) とは区別する。Gone が Non-Existent と混同されると、別経路から後で届く Object を
//! §12.1 (Malformed Tracks) 条件 10 の「存在しないと記録した後の Object」として誤って
//! 拒否してしまう。
//!
//! バイト列は仕様の規則から手計算した golden として固定する。End of Range の 4 種は
//! フィールド構造が同じで Serialization Flags だけが異なるため (draft-ietf-moq-transport-23
//! §11.4.1 (Fetch Header) Table 8 / §11.4.2 (End of Range))、Gone も Group ID Delta /
//! Object ID Delta のデルタ規則と prior state の規則 (§11.4.2.1 (ID Decoding) /
//! §11.4.2.2 (Prior Object Fields)) を他の 3 種と共有する。
use super::*;

/// テスト用の FETCH_HEADER: type 0x05 + Request ID 1
const FETCH_HEADER_BYTES: [u8; 2] = [0x05, 0x01];

/// End of Gone Range の Serialization Flags (vi64 の 0x40C)
const END_OF_GONE_RANGE_FLAGS: [u8; 2] = [0x84, 0x0C];

/// End of Non-Existent Range の Serialization Flags (vi64 の 0x8C)
const END_OF_NON_EXISTENT_RANGE_FLAGS: [u8; 2] = [0x80, 0x8C];

/// 期待する Object エントリを作る
///
/// Subgroup ID 0 / Publisher Priority 128 / Datagram 起源でない / Properties なし /
/// ペイロード長 0 を固定する。ペイロードを持たないため、分割受信のテストでも
/// ペイロードの読み出し待ちが発生しない。
fn expected_object(group_id: u64, object_id: u64) -> DecodedFetchEntry {
    DecodedFetchEntry::Object(DecodedFetchObject {
        group_id,
        subgroup_id: 0,
        object_id,
        publisher_priority: 128,
        is_datagram_origin: false,
        payload_length: 0,
        properties_bytes: None,
    })
}

/// テスト用の Object 入力を作る
///
/// Subgroup ID 0 / Publisher Priority 128 / Properties なし / ペイロード長 0 を固定する。
fn object_input(group_id: u64, object_id: u64) -> FetchObjectInput {
    FetchObjectInput {
        group_id,
        subgroup_id: 0,
        object_id,
        publisher_priority: 128,
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 0,
    }
}

/// golden バイト列をチャンク列として push し、デコードした entry を順に返す
///
/// ペイロード長 0 の Object だけを扱うため、ペイロードの読み出しは行わない。
/// 各チャンクの境界でエントリが途中でも、残りが次のチャンクで揃えばデコードできる。
fn decode_stream_in_chunks(chunks: &[&[u8]]) -> Vec<DecodedFetchEntry> {
    let mut decoder = FetchStreamDecoder::new();
    let mut header_decoded = false;
    let mut entries = Vec::new();
    for chunk in chunks {
        decoder.push(chunk);
        if !header_decoded {
            match decoder
                .try_decode_header()
                .expect("FETCH_HEADER はデコードできる")
            {
                Some(header) => {
                    assert_eq!(header.request_id, 1, "FETCH_HEADER の Request ID");
                    header_decoded = true;
                }
                None => continue,
            }
        }
        while let Some(entry) = decoder
            .try_decode_entry()
            .expect("golden バイト列はデコードできる")
        {
            entries.push(entry);
        }
    }
    decoder
        .finish()
        .expect("エントリ境界で終端したストリームは finish で受容される");
    entries
}

/// golden バイト列を一括で push してデコードした entry を順に返す
fn decode_stream(bytes: &[u8]) -> Vec<DecodedFetchEntry> {
    decode_stream_in_chunks(&[bytes])
}

/// End of Gone Range が 0x40C の 1 個の vi64 と 2 個の delta で往復すること
///
/// 0x40C は 128 以上のため vi64 の 2 バイト形式 (`0x84 0x0C`) になる。
/// 4 種の End of Range はいずれも Group ID Delta と Object ID Delta を必ず持ち
/// (draft-ietf-moq-transport-23 §11.4.2 (End of Range))、prior 参照文脈によらず
/// 同じフィールド構造である (同 §11.4.2.1 (ID Decoding))。
#[test]
fn gone_range_entry_roundtrip_has_flags_and_two_deltas() {
    for context in [
        FetchPriorContext::First,
        FetchPriorContext::NoPriorActualObject,
        FetchPriorContext::HasPriorObject,
    ] {
        let entry = FetchStreamEntry::EndOfGoneRange {
            group_id_delta: 5,
            object_id_delta: 10,
        };
        let mut buf = Vec::new();
        entry
            .encode(None, context, &mut buf)
            .expect("End of Gone Range はエンコードできる");
        assert_eq!(
            buf,
            [0x84, 0x0C, 0x05, 0x0A],
            "0x40C の vi64 と 2 個の delta が書き出されること: {context:?}"
        );
        let (decoded, consumed) =
            FetchStreamEntry::decode(&buf, context).expect("End of Gone Range はデコードできる");
        assert_eq!(consumed, buf.len(), "後続フィールドを読まないこと");
        assert_eq!(
            decoded, entry,
            "End of Gone Range が往復すること: {context:?}"
        );
    }
}

/// 同じ delta でも Gone と Non-Existent が別の variant にデコードされること
///
/// Serialization Flags だけが異なる 2 つのバイト列をデコードし、アプリが取得できなかった
/// 理由を区別できることを固定する。Gone は存在を否定しない状態であり
/// (draft-ietf-moq-transport-23 §2.1.2 (Object States))、
/// [`DecodedFetchEntry::EndOfNonExistentRange`] と同一視してはならない。
#[test]
fn gone_range_is_distinct_from_non_existent_range() {
    let gone = FetchStreamEntry::EndOfGoneRange {
        group_id_delta: 3,
        object_id_delta: 4,
    };
    let mut buf = Vec::new();
    gone.encode(None, FetchPriorContext::First, &mut buf)
        .expect("End of Gone Range はエンコードできる");
    assert_eq!(
        buf[..2],
        END_OF_GONE_RANGE_FLAGS,
        "End of Gone Range の Serialization Flags が 0x40C であること"
    );
    let decoded = decode_stream(&[FETCH_HEADER_BYTES.to_vec(), buf].concat());
    assert_eq!(
        decoded,
        vec![DecodedFetchEntry::EndOfGoneRange {
            group_id: 3,
            object_id: 4
        }]
    );

    // 同じ delta を持つ Non-Existent は別の variant になる
    let mut non_existent = FETCH_HEADER_BYTES.to_vec();
    non_existent.extend_from_slice(&END_OF_NON_EXISTENT_RANGE_FLAGS);
    non_existent.extend_from_slice(&[0x03, 0x04]);
    let decoded = decode_stream(&non_existent);
    assert_eq!(
        decoded,
        vec![DecodedFetchEntry::EndOfNonExistentRange {
            group_id: 3,
            object_id: 4
        }],
        "同じ Location でも Non-Existent は別の variant になること"
    );
    assert_ne!(
        DecodedFetchEntry::EndOfGoneRange {
            group_id: 3,
            object_id: 4
        },
        DecodedFetchEntry::EndOfNonExistentRange {
            group_id: 3,
            object_id: 4
        },
        "Gone と Non-Existent はアプリから区別できること"
    );
}

/// FETCH_HEADER 直後の End of Gone Range が絶対 ID、Object の直後がデルタになること
///
/// draft-ietf-moq-transport-23 §11.4.2.1 (ID Decoding): 最初の entry の
/// Group ID Delta / Object ID Delta は絶対 ID で、それ以外は直前の Object または
/// End of Range の Location からのデルタである。
#[test]
fn first_gone_range_is_absolute_and_after_object_is_delta() {
    // FETCH_HEADER 直後: Group 3 / Object 4 の絶対 ID
    let mut first = FETCH_HEADER_BYTES.to_vec();
    first.extend_from_slice(&END_OF_GONE_RANGE_FLAGS);
    first.extend_from_slice(&[0x03, 0x04]);
    assert_eq!(
        decode_stream(&first),
        vec![DecodedFetchEntry::EndOfGoneRange {
            group_id: 3,
            object_id: 4
        }],
        "FETCH_HEADER 直後の End of Gone Range は絶対 ID として解決されること"
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
    ]);
    after_object.extend_from_slice(&END_OF_GONE_RANGE_FLAGS);
    after_object.extend_from_slice(&[0x03, 0x04]);
    assert_eq!(
        decode_stream(&after_object),
        vec![
            expected_object(1, 0),
            DecodedFetchEntry::EndOfGoneRange {
                group_id: 4,
                object_id: 4
            },
        ],
        "Object の直後の End of Gone Range は直前 Location のデルタとして解決されること"
    );
}

/// End of Gone Range が直前の actual Object の Subgroup ID / Priority を保持すること
///
/// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields): End of Range の後の
/// Object が prior 参照を使う場合、Prior Subgroup ID / Priority は End of Range より前の
/// 最後の actual Object の値になる。End of Range は Group ID / Object ID だけを更新する。
/// Gone も同じ規則に従うことを、Gone の直後の Object が Subgroup ID と Priority を
/// 省略して解決できることで固定する。
#[test]
fn gone_range_preserves_prior_actual_object_fields() {
    let mut bytes = FETCH_HEADER_BYTES.to_vec();
    bytes.extend_from_slice(&[
        0x1F, // Object flags: Group ID + Object ID + Priority + Subgroup ID
        0x01, // Group ID Delta = 1 (絶対 Group ID 1)
        0x07, // Subgroup ID = 7
        0x00, // Object ID Delta = 0 (絶対 Object ID 0)
        0xC8, // Publisher Priority = 200
        0x00, // Object Payload Length = 0
    ]);
    bytes.extend_from_slice(&END_OF_GONE_RANGE_FLAGS);
    bytes.extend_from_slice(&[
        0x02, // Group ID Delta = 2 (prior Group 1 + 2 = 3)
        0x02, // Object ID Delta = 2 (Group が変わるので新しい Group の絶対 Object ID)
    ]);
    bytes.extend_from_slice(&[
        0x01, // Object flags: Subgroup ID は prior Object と同じ / Priority は省略 (prior Object)
        0x00, // Object Payload Length = 0
    ]);

    let entries = decode_stream(&bytes);
    assert_eq!(entries.len(), 3);
    let DecodedFetchEntry::EndOfGoneRange {
        group_id,
        object_id,
    } = &entries[1]
    else {
        panic!("2 件目は End of Gone Range であること: {:?}", entries[1]);
    };
    assert_eq!((*group_id, *object_id), (3, 2), "Gone の終端 Location");

    let DecodedFetchEntry::Object(second) = &entries[2] else {
        panic!("3 件目は Object であること: {:?}", entries[2]);
    };
    assert_eq!(
        second.group_id, 3,
        "End of Gone Range の Group ID が prior Location になること"
    );
    assert_eq!(
        second.object_id, 3,
        "End of Gone Range の Object ID + 1 が Object ID になること"
    );
    assert_eq!(
        second.subgroup_id, 7,
        "End of Gone Range は直前の actual Object の Subgroup ID を消さないこと"
    );
    assert_eq!(
        second.publisher_priority, 200,
        "End of Gone Range は直前の actual Object の Priority を消さないこと"
    );
}

/// End of Gone Range を含むストリームが任意の分割受信で同じ結果になること
///
/// draft-ietf-moq-transport-23 §11.4.2.1 (ID Decoding) のデルタ解決は stream 内の
/// 直前状態に依存するため、チャンク境界が entry の途中でも状態遷移が変わらないことを
/// 1 バイトずつの push で固定する。
#[test]
fn gone_range_split_reception_matches_whole_push() {
    let mut bytes = FETCH_HEADER_BYTES.to_vec();
    // Object (Group 2 / Object 0)
    bytes.extend_from_slice(&[0x1F, 0x02, 0x00, 0x00, 0x80, 0x00]);
    // End of Gone Range (Group 4 / 絶対 Object 7)
    bytes.extend_from_slice(&END_OF_GONE_RANGE_FLAGS);
    bytes.extend_from_slice(&[0x02, 0x07]);
    // Object (Group 4 / Object 8 = Gone の Object ID + 1)
    bytes.extend_from_slice(&[0x01, 0x00]);
    // End of Gone Range (同一 Group 4 で Object 8 + 2 + 1 = 11)
    bytes.extend_from_slice(&END_OF_GONE_RANGE_FLAGS);
    bytes.extend_from_slice(&[0x00, 0x02]);
    // Object (Group 4 / Object 12 = Gone の Object ID + 1)
    bytes.extend_from_slice(&[0x01, 0x00]);

    let expected = vec![
        expected_object(2, 0),
        DecodedFetchEntry::EndOfGoneRange {
            group_id: 4,
            object_id: 7,
        },
        expected_object(4, 8),
        DecodedFetchEntry::EndOfGoneRange {
            group_id: 4,
            object_id: 11,
        },
        expected_object(4, 12),
    ];
    assert_eq!(decode_stream(&bytes), expected, "一括 push の結果");

    let one_byte_chunks: Vec<&[u8]> = bytes.chunks(1).collect();
    assert_eq!(
        decode_stream_in_chunks(&one_byte_chunks),
        expected,
        "1 バイトずつ push しても結果が変わらないこと"
    );
    let two_byte_chunks: Vec<&[u8]> = bytes.chunks(2).collect();
    assert_eq!(
        decode_stream_in_chunks(&two_byte_chunks),
        expected,
        "2 バイトずつ push しても結果が変わらないこと"
    );
}

/// End of Gone Range と通常 Object を混在させてエンコード・デコードできること
///
/// エンコーダは絶対 Location を受け取り、Gone も Object と同じデルタ規則で書き出す
/// (draft-ietf-moq-transport-23 §11.4.2.1 (ID Decoding))。連続する Gone と Object の
/// 交互配置が往復することを固定する。
#[test]
fn gone_range_mixed_with_objects_roundtrips() {
    let mut encoder = FetchStreamEncoder::new(1);
    let mut stream = encoder.encode_header();
    encoder
        .encode_object(&object_input(1, 0), None, &mut stream)
        .expect("Object はエンコードできる");
    encoder
        .encode_end_of_gone_range(3, 5, &mut stream)
        .expect("End of Gone Range はエンコードできる");
    encoder
        .encode_object(&object_input(3, 6), None, &mut stream)
        .expect("Object はエンコードできる");
    encoder
        .encode_end_of_gone_range(5, 0, &mut stream)
        .expect("End of Gone Range はエンコードできる");
    encoder
        .encode_object(&object_input(5, 1), None, &mut stream)
        .expect("Object はエンコードできる");

    let expected = vec![
        expected_object(1, 0),
        DecodedFetchEntry::EndOfGoneRange {
            group_id: 3,
            object_id: 5,
        },
        expected_object(3, 6),
        DecodedFetchEntry::EndOfGoneRange {
            group_id: 5,
            object_id: 0,
        },
        expected_object(5, 1),
    ];
    assert_eq!(decode_stream(&stream), expected, "Gone と Object の混在");

    let one_byte_chunks: Vec<&[u8]> = stream.chunks(1).collect();
    assert_eq!(
        decode_stream_in_chunks(&one_byte_chunks),
        expected,
        "混在ストリームも分割受信で結果が変わらないこと"
    );
}

/// End of Gone Range の後に続く Object を不存在の矛盾として拒否しないこと
///
/// draft-ietf-moq-transport-23 §2.1.2 (Object States): Gone は「元の publisher が
/// 再配送できないが、存在するか否かは知らない」状態であり、存在しないと確定した
/// Non-Existent とは異なる。"Because another source can still hold a Gone Object, receiving
/// an Object that was recorded as Gone or Unknown is not an error." のため、Gone の範囲を
/// 記録しても、その範囲やその周辺の Location の Object を後から受け取ることを
/// §12.1 (Malformed Tracks) 条件 10 の「存在しないと記録した後の Object」として拒否しては
/// ならない。
///
/// このデコーダは FETCH ストリームごとの Location 解決だけを行い、不存在の範囲を記録しない。
/// そのため Gone の直後でも Object を通常どおり受理する。ここでは同一 Group の続き
/// (Gone の終端 + 1) と、Gone の範囲より後ろの新しい Group の 2 通りを固定する。
#[test]
fn object_after_gone_range_is_not_rejected() {
    // End of Gone Range (Group 2 / Object 5) の後、同一 Group の続きを置く
    let mut bytes = FETCH_HEADER_BYTES.to_vec();
    bytes.extend_from_slice(&END_OF_GONE_RANGE_FLAGS);
    bytes.extend_from_slice(&[0x02, 0x05]);
    // Object flags 0x13: Subgroup ID (0x03) と Priority (0x10) を明示する。
    // actual Object が未出現のため prior 参照は使えない (§11.4.2.2 (Prior Object Fields))。
    bytes.extend_from_slice(&[
        0x13, // flags: Priority + Subgroup ID 明示 (Group ID / Object ID は prior 参照)
        0x00, // Subgroup ID = 0
        0x80, // Publisher Priority = 128
        0x00, // Object Payload Length = 0
    ]);
    assert_eq!(
        decode_stream(&bytes),
        vec![
            DecodedFetchEntry::EndOfGoneRange {
                group_id: 2,
                object_id: 5,
            },
            expected_object(2, 6),
        ],
        "Gone の直後に同一 Group の Object が来ても受理されること"
    );

    // End of Gone Range (Group 2 / Object 5) の後、新しい Group の Object を置く
    let mut next_group = FETCH_HEADER_BYTES.to_vec();
    next_group.extend_from_slice(&END_OF_GONE_RANGE_FLAGS);
    next_group.extend_from_slice(&[0x02, 0x05]);
    next_group.extend_from_slice(&[
        0x1F, // flags: Group ID + Object ID + Priority + Subgroup ID
        0x00, // Group ID Delta = 0 (prior Group 2 + 0 + 1 = 3)
        0x00, // Subgroup ID = 0
        0x00, // Object ID Delta = 0 (Group が変わるので新しい Group の絶対 Object ID)
        0x80, // Publisher Priority = 128
        0x00, // Object Payload Length = 0
    ]);
    assert_eq!(
        decode_stream(&next_group),
        vec![
            DecodedFetchEntry::EndOfGoneRange {
                group_id: 2,
                object_id: 5,
            },
            expected_object(3, 0),
        ],
        "Gone の後に別 Group の Object が来ても受理されること"
    );
}

/// 降順の End of Gone Range が Group ID の減算で解決されること
///
/// draft-ietf-moq-transport-23 §11.4.2.1 (ID Decoding): Descending Group Order では
/// Group ID を prior Group ID から減算する。Gone も同じ規則に従う。
#[test]
fn descending_gone_range_resolves_with_subtraction() {
    let mut bytes = FETCH_HEADER_BYTES.to_vec();
    // Object (Group 10 / Object 5)
    bytes.extend_from_slice(&[0x1F, 0x0A, 0x00, 0x05, 0x80, 0x00]);
    // End of Gone Range: Group 10 - 3 = 7 / Object は新しい Group の絶対値 2
    bytes.extend_from_slice(&END_OF_GONE_RANGE_FLAGS);
    bytes.extend_from_slice(&[0x03, 0x02]);
    // Object (Group 7 / Object 2 + 1 = 3)
    bytes.extend_from_slice(&[0x01, 0x00]);

    let mut decoder =
        FetchStreamDecoder::new_with_group_order(0x02).expect("0x02 は有効な Group Order");
    decoder.push(&bytes);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("FETCH_HEADER があること");
    let mut entries = Vec::new();
    while let Some(entry) = decoder
        .try_decode_entry()
        .expect("golden バイト列はデコードできる")
    {
        entries.push(entry);
    }
    assert_eq!(
        entries,
        vec![
            expected_object(10, 5),
            DecodedFetchEntry::EndOfGoneRange {
                group_id: 7,
                object_id: 2,
            },
            expected_object(7, 3),
        ],
        "降順の Gone が減算で解決されること"
    );
    decoder
        .finish()
        .expect("entry 境界で終端したストリームは finish で受容される");
}

/// End of Gone Range をエンコードしたバイト列が golden と一致すること
///
/// エンコーダの公開 API は絶対 Location を受け取り、Gone でも同じデルタ規則で
/// 書き出さなければならない (draft-ietf-moq-transport-23 §11.4.2.1 (ID Decoding))。
#[test]
fn encoder_writes_gone_range_golden_bytes() {
    let mut encoder = FetchStreamEncoder::new(1);
    let mut bytes = encoder.encode_header();
    encoder
        .encode_end_of_gone_range(3, 4, &mut bytes)
        .expect("先頭の End of Gone Range はエンコードできる");
    encoder
        .encode_end_of_gone_range(3, 8, &mut bytes)
        .expect("同一 Group の続きの End of Gone Range はエンコードできる");
    encoder
        .encode_end_of_gone_range(6, 0, &mut bytes)
        .expect("Group をまたぐ End of Gone Range はエンコードできる");
    assert_eq!(
        bytes,
        [
            0x05, 0x01, // FETCH_HEADER
            0x84, 0x0C, 0x03, 0x04, // 先頭: 絶対 Group 3 / 絶対 Object 4
            0x84, 0x0C, 0x00, 0x03, // 同一 Group: 4 + 3 + 1 = 8
            0x84, 0x0C, 0x03, 0x00, // Group 3 + 3 = 6 / 新しい Group の絶対 Object 0
        ],
        "End of Gone Range が golden バイト列どおりにエンコードされること"
    );
    assert_eq!(
        decode_stream(&bytes),
        vec![
            DecodedFetchEntry::EndOfGoneRange {
                group_id: 3,
                object_id: 4,
            },
            DecodedFetchEntry::EndOfGoneRange {
                group_id: 3,
                object_id: 8,
            },
            DecodedFetchEntry::EndOfGoneRange {
                group_id: 6,
                object_id: 0,
            },
        ],
        "エンコーダ出力が同じ Location に解決されること"
    );
}

/// End of Track の後の End of Gone Range をエンコーダが拒否すること
///
/// draft-ietf-moq-transport-23 §12.1 (Malformed Tracks) 条件 5: Track の最終 Object より
/// 後ろの Location を指す entry は malformed track になる。End of Gone Range も
/// End of Range の一種であるため、書き出し前に拒否して malformed なワイヤを生成しない
/// (同 §11.4.2 (End of Range))。
#[test]
fn encoder_rejects_gone_range_after_end_of_track() {
    let mut encoder = FetchStreamEncoder::new(1);
    let mut bytes = encoder.encode_header();
    encoder
        .encode_object(&object_input(1, 0), None, &mut bytes)
        .expect("Object はエンコードできる");
    encoder
        .encode_end_of_track(&mut bytes)
        .expect("End of Track はエンコードできる");
    let len_after_track = bytes.len();
    assert!(
        matches!(
            encoder.encode_end_of_gone_range(2, 0, &mut bytes),
            Err(MessageError::ProtocolViolation(_))
        ),
        "End of Track の後の End of Gone Range は拒否されること"
    );
    assert_eq!(
        bytes.len(),
        len_after_track,
        "拒否した場合はバイト列を書き込まないこと"
    );
}
