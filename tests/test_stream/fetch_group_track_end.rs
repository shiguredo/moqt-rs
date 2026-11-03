//! Fetch ストリームの End of Group (0x80) / End of Track (0x81) と prior Object ID の sentinel
//! (draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields) / §12.1 (Malformed Tracks)) の外部テスト
//!
//! marker は ID フィールドを持たないため、golden バイト列で「後続フィールドを読まないこと」を
//! 固定する。prior Object ID が `2^64 - 1` になることの帰結 (同じ Group の次の Object が
//! DELTA_ENCODING_OVERFLOW になること、次の Group の絶対 Object ID は復元できること) も
//! デコード結果とエラーの両方で検証する。
use super::*;

/// テスト用の FETCH_HEADER: type 0x05 + Request ID 1
const FETCH_HEADER_BYTES: [u8; 2] = [0x05, 0x01];

/// End of Group の Serialization Flags (vi64 の 0x80)
const END_OF_GROUP_BYTES: [u8; 2] = [0x80, 0x80];

/// End of Track の Serialization Flags (vi64 の 0x81)
const END_OF_TRACK_BYTES: [u8; 2] = [0x80, 0x81];

/// 期待する Object エントリを作る
///
/// Subgroup ID 0 / Publisher Priority 128 / Datagram 起源でない / Properties なし /
/// ペイロード長 0 を固定する。Payload Length 0 は Object Status を持たない空 Object であり、
/// marker との位置関係だけを検証するための最小表現である。
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

/// golden バイト列を先頭から順にデコードする
///
/// ペイロード長 0 の Object と marker だけを扱うため、ペイロードの読み出しは行わない。
fn decode_golden_stream(
    bytes: &[u8],
    group_order: u8,
) -> Result<Vec<DecodedFetchEntry>, MessageError> {
    let mut decoder = FetchStreamDecoder::new_with_group_order(group_order)
        .expect("0x01 / 0x02 は有効な Group Order");
    decoder.push(bytes);
    let header = decoder
        .try_decode_header()
        .expect("FETCH_HEADER をデコードできる")
        .expect("FETCH_HEADER がある");
    assert_eq!(header.request_id, 1, "FETCH_HEADER の Request ID");
    let mut entries = Vec::new();
    while let Some(entry) = decoder.try_decode_entry()? {
        entries.push(entry);
    }
    decoder
        .finish()
        .expect("エントリ境界で終端したストリームは finish で受容される");
    Ok(entries)
}

/// 1 バイトずつ push した結果が一括 push と一致すること
fn decode_one_byte_at_a_time(
    bytes: &[u8],
    group_order: u8,
) -> Result<Vec<DecodedFetchEntry>, MessageError> {
    let mut decoder = FetchStreamDecoder::new_with_group_order(group_order)
        .expect("0x01 / 0x02 は有効な Group Order");
    let mut entries = Vec::new();
    for (index, byte) in bytes.iter().enumerate() {
        decoder.push(&[*byte]);
        if index < FETCH_HEADER_BYTES.len() - 1 {
            // FETCH_HEADER が揃うまでは entry を読めない
            continue;
        }
        if index == FETCH_HEADER_BYTES.len() - 1 {
            decoder
                .try_decode_header()
                .expect("FETCH_HEADER をデコードできる")
                .expect("FETCH_HEADER がある");
        }
        while let Some(entry) = decoder.try_decode_entry()? {
            entries.push(entry);
        }
    }
    Ok(entries)
}

/// End of Group が直前の Object の Group を終端し、次の Group の Object が復元されること
///
/// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields): marker は ID フィールドを
/// 持たず、直前の Object が Group の最終 Object になる。次の Object は Group ID Delta が
/// present になるため Object ID は絶対値として解決される
/// (§11.4.1.1 (Flags))。marker の 2 バイトが後続 ID フィールドとして消費されないことも
/// バイト列の並びで固定する。
#[test]
fn end_of_group_ends_the_group_and_restores_the_next_group() {
    // Object (5, 3) → End of Group → Object (6, 7)
    // 2 個目の Object は Group ID Delta 0 (5 + 0 + 1 = 6)、Object ID Delta 7 (絶対値)
    let mut bytes = FETCH_HEADER_BYTES.to_vec();
    bytes.extend_from_slice(&[
        // Object: flags 0x1F (Subgroup ID present + Object ID + Group ID + Priority)
        0x1F, 0x05, // Group ID Delta 5 (最初の Object なので絶対値)
        0x00, // Subgroup ID 0
        0x03, // Object ID Delta 3 (最初の Object なので絶対値)
        0x80, // Publisher Priority 128
        0x00, // Object Payload Length 0
    ]);
    bytes.extend_from_slice(&END_OF_GROUP_BYTES);
    bytes.extend_from_slice(&[
        // Object: flags 0x1D (Subgroup ID previous same + Object ID + Group ID + Priority)
        0x1D, 0x00, // Group ID Delta 0 → 直前の Group 5 + 0 + 1 = 6
        0x07, // Object ID Delta 7 (Group ID Delta が present なので絶対値)
        0x80, // Publisher Priority 128
        0x00, // Object Payload Length 0
    ]);

    let entries = decode_golden_stream(&bytes, 0x01).expect("golden バイト列はデコードできる");
    assert_eq!(
        entries,
        vec![
            expected_object(5, 3),
            DecodedFetchEntry::EndOfGroup { group_id: 5 },
            expected_object(6, 7),
        ],
        "End of Group の前後の Group と Object ID が解決されること"
    );

    // 1 バイトずつ push しても同じ結果になること (marker の varint が分割されても読める)
    assert_eq!(
        decode_one_byte_at_a_time(&bytes, 0x01).expect("分割受信でもデコードできる"),
        entries,
        "End of Group を 1 バイトずつ受信しても結果が変わらないこと"
    );
}

/// End of Track が直前の Object の Group と Track を終端すること
///
/// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields):
/// "End of Track also indicates End of Group." marker の後に FIN で終わるストリームを
/// `finish` が受容することも合わせて確認する。
#[test]
fn end_of_track_ends_the_group_and_the_track() {
    let mut bytes = FETCH_HEADER_BYTES.to_vec();
    bytes.extend_from_slice(&[
        0x1F, 0x02, // Group ID 2 (絶対値)
        0x00, // Subgroup ID 0
        0x01, // Object ID 1 (絶対値)
        0x80, // Publisher Priority 128
        0x00, // Object Payload Length 0
    ]);
    bytes.extend_from_slice(&END_OF_TRACK_BYTES);

    let entries = decode_golden_stream(&bytes, 0x01).expect("golden バイト列はデコードできる");
    assert_eq!(
        entries,
        vec![
            expected_object(2, 1),
            DecodedFetchEntry::EndOfTrack { group_id: 2 },
        ],
        "End of Track が直前の Object の Group を伴って通知されること"
    );

    assert_eq!(
        decode_one_byte_at_a_time(&bytes, 0x01).expect("分割受信でもデコードできる"),
        entries,
        "End of Track を 1 バイトずつ受信しても結果が変わらないこと"
    );
}

/// End of Track の後の Object と End of Range が MalformedTrack になること
///
/// draft-ietf-moq-transport-23 §12.1 (Malformed Tracks) 条件 5:
/// "An Object is received whose Group and Object ID are larger than the final Object in
/// the Track. The final Object in a Track is the Object with Status END_OF_TRACK."
/// Track を終端した後の entry は、たとえ新しい Group の絶対 Location でも Track の最終
/// Object より後ろになる。セッションを閉じる DELTA_ENCODING_OVERFLOW ではなく、
/// 購読 / fetch の cancel を求める MalformedTrack で通知する。
#[test]
fn entry_after_end_of_track_is_malformed_track() {
    let mut base = FETCH_HEADER_BYTES.to_vec();
    base.extend_from_slice(&[
        0x1F, 0x02, 0x00, 0x01, 0x80, 0x00, // Object (2, 1)
    ]);
    base.extend_from_slice(&END_OF_TRACK_BYTES);

    // 新しい Group (3) の Object を End of Track の後に置く
    let mut after_object = base.clone();
    after_object.extend_from_slice(&[0x1F, 0x00, 0x00, 0x00, 0x80, 0x00]);
    let err = decode_golden_stream(&after_object, 0x01)
        .expect_err("End of Track の後の Object は拒否されること");
    assert!(
        matches!(err, MessageError::MalformedTrack(_)),
        "malformed track として拒否されること: {err:?}"
    );

    // End of Range も Track の最終 Object より後ろの Location を指すため拒否する
    let mut after_range = base.clone();
    after_range.extend_from_slice(&[0x80, 0x8C, 0x00, 0x00]);
    let err = decode_golden_stream(&after_range, 0x01)
        .expect_err("End of Track の後の End of Range は拒否されること");
    assert!(
        matches!(err, MessageError::MalformedTrack(_)),
        "malformed track として拒否されること: {err:?}"
    );

    // marker の重複は直前状態を変えないため受理する
    let mut duplicate = base.clone();
    duplicate.extend_from_slice(&END_OF_GROUP_BYTES);
    let entries = decode_golden_stream(&duplicate, 0x01)
        .expect("End of Track の後の End of Group は受理されること");
    assert_eq!(
        entries,
        vec![
            expected_object(2, 1),
            DecodedFetchEntry::EndOfTrack { group_id: 2 },
            DecodedFetchEntry::EndOfGroup { group_id: 2 },
        ],
        "marker の重複が直前の Group を引き継ぐこと"
    );
}

/// End of Group の後の同じ Group の Object が DELTA_ENCODING_OVERFLOW になること
///
/// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields):
/// "The prior Object ID becomes 2^64-1." 同じ Group の次の Object ID は prior Object ID を
/// 基準に解決されるため (同 §11.4.1.1 (Flags))、`2^64 - 1` に何を足しても u64 を超える。
/// 同 §12.2 (Session Termination Codes) の
/// "When more than one code applies, an endpoint uses DELTA_ENCODING_OVERFLOW or
/// FIELD_LENGTH_EXCEEDED in preference to the other codes." により、malformed track ではなく
/// DELTA_ENCODING_OVERFLOW (0x400) で通知する。
#[test]
fn object_in_the_ended_group_is_delta_encoding_overflow() {
    let mut base = FETCH_HEADER_BYTES.to_vec();
    base.extend_from_slice(&[0x1F, 0x05, 0x00, 0x03, 0x80, 0x00]); // Object (5, 3)
    base.extend_from_slice(&END_OF_GROUP_BYTES);

    // Object ID Delta を明示する形: prior (2^64-1) + delta 0 + 2 が u64 を超える
    let mut explicit_delta = base.clone();
    explicit_delta.extend_from_slice(&[0x04, 0x00, 0x00]);
    let err = decode_golden_stream(&explicit_delta, 0x01)
        .expect_err("終端した Group の Object はデルタ解決できないこと");
    assert!(
        matches!(err, MessageError::DeltaEncodingOverflow(_)),
        "DELTA_ENCODING_OVERFLOW になること: {err:?}"
    );

    // Object ID Delta を省略する形: prior (2^64-1) + 1 が u64 を超える
    let mut omitted_delta = base;
    omitted_delta.extend_from_slice(&[0x00, 0x00]);
    let err = decode_golden_stream(&omitted_delta, 0x01)
        .expect_err("終端した Group の Object は prior + 1 でも解決できないこと");
    assert!(
        matches!(err, MessageError::DeltaEncodingOverflow(_)),
        "DELTA_ENCODING_OVERFLOW になること: {err:?}"
    );
}

/// FETCH_HEADER 直後の marker が MALFORMED_OBJECT になること
///
/// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields) の marker は
/// "The prior Object is the last Object in its Group, or in the Track, respectively." と
/// 定めるため、直前の Object も End of Range も無い marker は存在しない prior Object を
/// 参照する形になる。同 §12.2 (Session Termination Codes) の MALFORMED_OBJECT (0x201) は
/// "a flag references a prior Object that does not exist" を表す。
#[test]
fn marker_before_any_object_is_malformed_object() {
    for marker in [END_OF_GROUP_BYTES, END_OF_TRACK_BYTES] {
        let mut bytes = FETCH_HEADER_BYTES.to_vec();
        bytes.extend_from_slice(&marker);
        let err = decode_golden_stream(&bytes, 0x01)
            .expect_err("prior Object の無い marker は拒否されること");
        assert!(
            matches!(err, MessageError::MalformedObject(_)),
            "MALFORMED_OBJECT になること: {err:?}"
        );
    }
}

/// End of Range の後の End of Group が、End of Range の解決済み Group を引き継ぐこと
///
/// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields): End of Range の後の
/// prior Group ID / Object ID は "The decoded values from the End of Range indicator." で
/// あり、後続の marker もその Group ID を引き継ぐ。actual Object が未出現でも
/// End of Range の Location が prior になるため、marker は受理される。
#[test]
fn end_of_group_after_end_of_range_keeps_the_decoded_group() {
    let mut bytes = FETCH_HEADER_BYTES.to_vec();
    bytes.extend_from_slice(&[
        0x80, 0x8C, // End of Non-Existent Range (0x8C)
        0x04, // Group ID Delta 4 (最初の entry なので絶対 Group ID)
        0x09, // Object ID Delta 9 (最初の entry なので絶対 Object ID)
    ]);
    bytes.extend_from_slice(&END_OF_GROUP_BYTES);
    bytes.extend_from_slice(&[
        // actual Object が未出現なので Subgroup ID / Priority は明示する
        0x1F, 0x00, // Group ID Delta 0 → 4 + 0 + 1 = 5
        0x00, // Subgroup ID 0
        0x02, // Object ID Delta 2 (絶対値)
        0x80, // Publisher Priority 128
        0x00, // Object Payload Length 0
    ]);

    let entries = decode_golden_stream(&bytes, 0x01).expect("golden バイト列はデコードできる");
    assert_eq!(
        entries,
        vec![
            DecodedFetchEntry::EndOfNonExistentRange {
                group_id: 4,
                object_id: 9
            },
            DecodedFetchEntry::EndOfGroup { group_id: 4 },
            expected_object(5, 2),
        ],
        "End of Range の Location を Group の終端として引き継ぐこと"
    );
}

/// 降順の Group Order でも End of Group の次の Group が復元されること
///
/// draft-ietf-moq-transport-23 §11.4.1.1 (Flags): 降順では Group ID が
/// prior Group ID から (Group ID Delta + 1) を引いた値になる。marker は Group ID を
/// 変えないため、次の Object の Group は 10 - (0 + 1) = 9 になる。
#[test]
fn end_of_group_restores_the_next_group_in_descending_order() {
    let mut bytes = FETCH_HEADER_BYTES.to_vec();
    bytes.extend_from_slice(&[0x1F, 0x0A, 0x00, 0x03, 0x80, 0x00]); // Object (10, 3)
    bytes.extend_from_slice(&END_OF_GROUP_BYTES);
    bytes.extend_from_slice(&[0x1D, 0x00, 0x07, 0x80, 0x00]); // Object (9, 7)

    let entries = decode_golden_stream(&bytes, 0x02).expect("golden バイト列はデコードできる");
    assert_eq!(
        entries,
        vec![
            expected_object(10, 3),
            DecodedFetchEntry::EndOfGroup { group_id: 10 },
            expected_object(9, 7),
        ],
        "降順でも End of Group の次の Group が復元されること"
    );
}

/// エンコーダが End of Group / End of Track を書き出し、デコーダが同じ位置で読むこと
///
/// marker は「既知の終端」を送出するための API である。応答末尾の Object が Group の終端と
/// 分かる場合は End of Group、Track の終端と分かる場合は End of Track を書く
/// (draft-ietf-moq-transport-23 §3.2.2 (Gaps in a Fetch Stream) / §11.4.2.2 (Prior Object
/// Fields))。低水準 codec で marker のバイト数を独立に確認し、後続フィールドを書いていない
/// ことを固定する。
#[test]
fn encoder_writes_end_of_group_and_end_of_track() {
    let mut encoder = FetchStreamEncoder::new(1);
    let mut bytes = encoder.encode_header();
    encoder
        .encode_object(&object_input(5, 3), None, &mut bytes)
        .expect("Object (5, 3) はエンコードできる");
    encoder
        .encode_end_of_group(&mut bytes)
        .expect("直前の Object があるため End of Group を書ける");
    let end_of_group_at = bytes.len() - END_OF_GROUP_BYTES.len();
    assert_eq!(
        &bytes[end_of_group_at..],
        &END_OF_GROUP_BYTES,
        "End of Group は 1 個の vi64 だけを書くこと"
    );
    encoder
        .encode_object(&object_input(6, 7), None, &mut bytes)
        .expect("Group 6 の Object はエンコードできる");
    encoder
        .encode_end_of_track(&mut bytes)
        .expect("直前の Object があるため End of Track を書ける");
    assert_eq!(
        &bytes[bytes.len() - END_OF_TRACK_BYTES.len()..],
        &END_OF_TRACK_BYTES,
        "End of Track は 1 個の vi64 だけを書くこと"
    );

    // 低水準 codec で marker の直後から次の entry を読み、後続フィールドが無いことを確認する
    let mut pos = FETCH_HEADER_BYTES.len();
    let (object, consumed) = FetchStreamEntry::decode(&bytes[pos..], FetchPriorContext::First)
        .expect("1 個目の Object を低水準 codec で読める");
    pos += consumed;
    let (marker, consumed) =
        FetchStreamEntry::decode(&bytes[pos..], FetchPriorContext::HasPriorObject)
            .expect("End of Group を低水準 codec で読める");
    assert_eq!(marker, FetchStreamEntry::EndOfGroup);
    assert_eq!(
        consumed,
        END_OF_GROUP_BYTES.len(),
        "End of Group が余分なバイトを消費しないこと"
    );
    pos += consumed;
    let (_, consumed) = FetchStreamEntry::decode(&bytes[pos..], FetchPriorContext::HasPriorObject)
        .expect("2 個目の Object を低水準 codec で読める");
    pos += consumed;
    let (marker, consumed) =
        FetchStreamEntry::decode(&bytes[pos..], FetchPriorContext::HasPriorObject)
            .expect("End of Track を低水準 codec で読める");
    assert_eq!(marker, FetchStreamEntry::EndOfTrack);
    assert_eq!(consumed, END_OF_TRACK_BYTES.len());
    assert_eq!(
        pos + consumed,
        bytes.len(),
        "marker の後ろに余分なバイトが無いこと"
    );
    assert!(matches!(
        object,
        FetchStreamEntry::Object(FetchStreamObject {
            group_id: Some(5),
            object_id: Some(3),
            ..
        })
    ));

    // 高水準デコーダでも同じ entry 列になること
    assert_eq!(
        decode_golden_stream(&bytes, 0x01).expect("エンコーダの出力はデコードできる"),
        vec![
            expected_object(5, 3),
            DecodedFetchEntry::EndOfGroup { group_id: 5 },
            expected_object(6, 7),
            DecodedFetchEntry::EndOfTrack { group_id: 6 },
        ],
        "エンコーダとデコーダで marker の位置が一致すること"
    );
}

/// 直前の Object が無い状態で marker を書けないこと
///
/// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields) の marker は
/// "The prior Object is the last Object in its Group, or in the Track, respectively." であり、
/// 直前の Object も End of Range も無い状態では対象が存在しない。
#[test]
fn encoder_rejects_markers_without_a_prior_object() {
    let mut encoder = FetchStreamEncoder::new(1);
    let mut buf = Vec::new();
    assert!(
        matches!(
            encoder.encode_end_of_group(&mut buf),
            Err(MessageError::ProtocolViolation(_))
        ),
        "直前の Object が無い End of Group は拒否されること"
    );
    assert!(
        matches!(
            encoder.encode_end_of_track(&mut buf),
            Err(MessageError::ProtocolViolation(_))
        ),
        "直前の Object が無い End of Track は拒否されること"
    );
    assert!(buf.is_empty(), "拒否した場合は書き込みを行わないこと");
}

/// End of Track の後に Object と End of Range を書けないこと
///
/// draft-ietf-moq-transport-23 §12.1 (Malformed Tracks) 条件 5: Track の最終 Object より
/// 後ろの Location を持つ Object は malformed track になる。エンコーダは自分で malformed な
/// ストリームを作らないよう、書き出し前に拒否する。
#[test]
fn encoder_rejects_entries_after_end_of_track() {
    let mut encoder = FetchStreamEncoder::new(1);
    let mut bytes = encoder.encode_header();
    encoder
        .encode_object(&object_input(2, 1), None, &mut bytes)
        .expect("Object (2, 1) はエンコードできる");
    encoder
        .encode_end_of_track(&mut bytes)
        .expect("End of Track を書ける");
    let bytes_after_end_of_track = bytes.clone();

    let mut buf = Vec::new();
    assert!(
        matches!(
            encoder.encode_object(&object_input(3, 0), None, &mut buf),
            Err(MessageError::ProtocolViolation(_))
        ),
        "End of Track の後の Object は拒否されること"
    );
    assert!(
        matches!(
            encoder.encode_end_of_non_existent_range(3, 0, &mut buf),
            Err(MessageError::ProtocolViolation(_))
        ),
        "End of Track の後の End of Range は拒否されること"
    );
    assert!(buf.is_empty(), "拒否した場合は書き込みを行わないこと");
    assert_eq!(
        bytes, bytes_after_end_of_track,
        "拒否した場合はストリームが変化しないこと"
    );

    // marker の重複は受理する (End of Track は End of Group も意味する)
    encoder
        .encode_end_of_group(&mut bytes)
        .expect("End of Track の後の End of Group は受理されること");
    assert_eq!(
        &bytes[bytes.len() - END_OF_GROUP_BYTES.len()..],
        &END_OF_GROUP_BYTES
    );
}

/// End of Group の後に同じ Group の Object を書けないこと
///
/// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields): prior Object ID が
/// `2^64 - 1` になるため、同じ Group の次の Object ID は表現できない。デルタ計算の
/// underflow ではなく、書き出し前に拒否する。
#[test]
fn encoder_rejects_object_in_the_ended_group() {
    let mut encoder = FetchStreamEncoder::new(1);
    let mut bytes = encoder.encode_header();
    encoder
        .encode_object(&object_input(5, 3), None, &mut bytes)
        .expect("Object (5, 3) はエンコードできる");
    encoder
        .encode_end_of_group(&mut bytes)
        .expect("End of Group を書ける");

    let mut buf = Vec::new();
    assert!(
        matches!(
            encoder.encode_object(&object_input(5, 4), None, &mut buf),
            Err(MessageError::ProtocolViolation(_))
        ),
        "終端した Group の Object は拒否されること"
    );
    assert!(buf.is_empty(), "拒否した場合は書き込みを行わないこと");

    // 次の Group の Object は書ける
    encoder
        .encode_object(&object_input(6, 0), None, &mut bytes)
        .expect("次の Group の Object はエンコードできる");
    assert_eq!(
        decode_golden_stream(&bytes, 0x01).expect("エンコーダの出力はデコードできる"),
        vec![
            expected_object(5, 3),
            DecodedFetchEntry::EndOfGroup { group_id: 5 },
            expected_object(6, 0),
        ],
        "End of Group の後に次の Group の Object が続くこと"
    );
}

/// End of Range だけが先行する状態でも marker を書けること
///
/// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields): End of Range の後の
/// prior Group ID / Object ID は "The decoded values from the End of Range indicator." で
/// あるため、actual Object が無くても marker はその Location を引き継ぐ。
#[test]
fn encoder_writes_marker_after_end_of_range() {
    let mut encoder = FetchStreamEncoder::new(1);
    let mut bytes = encoder.encode_header();
    encoder
        .encode_end_of_non_existent_range(4, 9, &mut bytes)
        .expect("End of Non-Existent Range を書ける");
    encoder
        .encode_end_of_group(&mut bytes)
        .expect("End of Range の後の End of Group を書ける");
    assert_eq!(
        decode_golden_stream(&bytes, 0x01).expect("エンコーダの出力はデコードできる"),
        vec![
            DecodedFetchEntry::EndOfNonExistentRange {
                group_id: 4,
                object_id: 9
            },
            DecodedFetchEntry::EndOfGroup { group_id: 4 },
        ],
        "End of Range の Group を End of Group が引き継ぐこと"
    );
}
