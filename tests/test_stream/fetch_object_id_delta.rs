//! Fetch ストリームの Object ID Delta (draft-ietf-moq-transport-23 §11.4.1.1 (Flags)) の外部テスト
//!
//! draft 23 は同一 Group の Object ID Delta を「prior Object ID + delta + 2」と定める。
//! Group ID Delta が present のときは Object ID Delta 自体が新しい Group の絶対 Object ID に
//! なり、Object ID Delta が absent のときは prior Object ID + 1 になる。
//! End of Range の同一 Group の +1 (§11.4.2.1 (ID Decoding)) とは別規則である。
//!
//! バイト列は仕様の規則から手計算した golden として固定する。エンコーダが生成する
//! バイト列と、そのバイト列をデコーダが解決した絶対 Location の両方を検証する。
use super::*;

/// テスト用の FETCH_HEADER: type 0x05 + Request ID 1
const FETCH_HEADER_BYTES: [u8; 2] = [0x05, 0x01];

/// golden バイト列で使う Publisher Priority
const PRIORITY: u8 = 200;

/// golden バイト列を先頭から順にデコードし、Object のペイロードも読み出す
fn decode_golden_stream(bytes: &[u8], group_order: u8) -> Vec<DecodedFetchEntry> {
    let mut decoder = FetchStreamDecoder::new_with_group_order(group_order)
        .expect("0x01 / 0x02 は有効な Group Order");
    decoder.push(bytes);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
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
fn expected_object(
    group_id: u64,
    subgroup_id: u64,
    object_id: u64,
    payload_length: u64,
) -> DecodedFetchEntry {
    DecodedFetchEntry::Object(DecodedFetchObject {
        group_id,
        subgroup_id,
        object_id,
        publisher_priority: PRIORITY,
        is_datagram_origin: false,
        payload_length,
        properties_bytes: None,
    })
}

/// エンコーダへ渡す絶対 Location の Object を作る
fn object_input(group_id: u64, subgroup_id: u64, object_id: u64) -> FetchObjectInput {
    FetchObjectInput {
        group_id,
        subgroup_id,
        object_id,
        publisher_priority: PRIORITY,
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 0,
    }
}

/// prior Object ID 0 の次が 1 なら Object ID Delta を省略すること
///
/// draft-ietf-moq-transport-23 §11.4.1.1 (Flags): "If Object ID Delta is not present,
/// the Object ID is the prior Object's ID plus one, regardless of which group it belongs
/// to." そのため +1 の増加にフィールドを消費しない。flags 0x01 は Subgroup ID を prior と
/// 同じにするだけで、Object ID Delta は存在しない。
#[test]
fn prior_plus_one_omits_object_id_delta() {
    let mut golden = FETCH_HEADER_BYTES.to_vec();
    // Object: Group 0 / Subgroup 0 / Object 0 / Priority 200 / payload 0
    golden.extend_from_slice(&[0x1F, 0x00, 0x00, 0x00, 0xC8, 0x00]);
    // Object: 同一 Group / Subgroup は prior と同じ / Object 1 は delta 省略 / payload 0
    golden.extend_from_slice(&[0x01, 0x00]);
    assert_eq!(
        decode_golden_stream(&golden, 0x01),
        vec![expected_object(0, 0, 0, 0), expected_object(0, 0, 1, 0)],
        "prior 0 の次が 1 の Object は Object ID Delta の省略で解決されること"
    );

    let mut encoder = FetchStreamEncoder::new(1);
    let mut bytes = encoder.encode_header();
    encoder
        .encode_object(&object_input(0, 0, 0), None, &mut bytes)
        .expect("golden バイト列の先頭 Object はエンコードできる");
    encoder
        .encode_object(&object_input(0, 0, 1), None, &mut bytes)
        .expect("prior + 1 の Object はエンコードできる");
    assert_eq!(
        bytes, golden,
        "prior 0 から 1 への増加は Object ID Delta を省略してエンコードされること"
    );
}

/// prior Object ID 0 の次が 2 なら Object ID Delta 0 になること
///
/// draft-ietf-moq-transport-23 §11.4.1.1 (Flags): "When the Group ID Delta field is not
/// present, the Object ID is the prior Object's ID plus the Object ID Delta + 2 if present."
/// そのため差が 2 の増加は delta 0 になる。flags 0x05 は Object ID Delta present (0x04) と
/// Subgroup ID を prior と同じにする 0x01 の組み合わせである。
#[test]
fn prior_plus_two_encodes_object_id_delta_zero() {
    let mut golden = FETCH_HEADER_BYTES.to_vec();
    // Object: Group 0 / Subgroup 0 / Object 0 / Priority 200 / payload 0
    golden.extend_from_slice(&[0x1F, 0x00, 0x00, 0x00, 0xC8, 0x00]);
    // Object: 同一 Group / Object 2 は Object ID Delta 0 (+ 2) / payload 0
    golden.extend_from_slice(&[0x05, 0x00, 0x00]);
    assert_eq!(
        decode_golden_stream(&golden, 0x01),
        vec![expected_object(0, 0, 0, 0), expected_object(0, 0, 2, 0)],
        "prior 0 の次が 2 の Object は Object ID Delta 0 で解決されること"
    );

    let mut encoder = FetchStreamEncoder::new(1);
    let mut bytes = encoder.encode_header();
    encoder
        .encode_object(&object_input(0, 0, 0), None, &mut bytes)
        .expect("golden バイト列の先頭 Object はエンコードできる");
    encoder
        .encode_object(&object_input(0, 0, 2), None, &mut bytes)
        .expect("prior + 2 の Object はエンコードできる");
    assert_eq!(
        bytes, golden,
        "prior 0 から 2 への増加は Object ID Delta 0 でエンコードされること"
    );
}

/// 昇順ストリームの golden バイト列を組み立てる
///
/// 最初の Object の絶対 ID、連続する +1 の省略、+2 の delta 0、+3 の delta 1、
/// Group 変更時の絶対 Object ID、Subgroup 変更時の +2 規則、End of Range (同一 Group は +1)、
/// End of Range 後の Object (同一 Group は +2) を 1 本に並べる。
fn ascending_golden() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&FETCH_HEADER_BYTES);
    // Object: Group 10 / Subgroup 3 / Object 5 / Priority 200 / payload 0
    // 最初の Object なので Group ID Delta と Object ID Delta は絶対値
    bytes.extend_from_slice(&[0x1F, 0x0A, 0x03, 0x05, 0xC8, 0x00]);
    // Object: 同一 Group / Subgroup 3 は prior と同じ / Object 6 は prior + 1 の省略
    bytes.extend_from_slice(&[0x01, 0x00]);
    // Object: 同一 Group / Object 8 は prior 6 + delta 0 + 2
    bytes.extend_from_slice(&[0x05, 0x00, 0x00]);
    // Object: 同一 Group / Object 11 は prior 8 + delta 1 + 2
    bytes.extend_from_slice(&[0x05, 0x01, 0x00]);
    // Object: Group 12 は Group ID Delta 1 (10 + 1 + 1) / Group ID Delta が present なので
    // Object ID は絶対値 0。Group をまたぐため Subgroup ID 3 も明示する
    bytes.extend_from_slice(&[0x0F, 0x01, 0x03, 0x00, 0x00]);
    // Object: 同一 Group / Subgroup 7 を明示 / Object 2 は prior 0 + delta 0 + 2
    bytes.extend_from_slice(&[0x07, 0x07, 0x00, 0x00]);
    // Object: 同一 Group / Subgroup 7 は prior と同じ / Object 3 は prior + 1 の省略
    bytes.extend_from_slice(&[0x01, 0x00]);
    // End of Non-Existent Range: Group 12 は delta 0 / Object 10 は prior 3 + delta 6 + 1
    // (End of Range の同一 Group は +1 であり Object の +2 ではない)
    bytes.extend_from_slice(&[0x80, 0x8C, 0x00, 0x06]);
    // Object: 同一 Group / Subgroup 7 は直前の actual Object と同じなので prior 参照 /
    // Object 12 は End of Range の Object 10 + delta 0 + 2
    bytes.extend_from_slice(&[0x05, 0x00, 0x00]);
    // Object: 同一 Group / Object 13 は prior + 1 の省略
    bytes.extend_from_slice(&[0x01, 0x00]);
    bytes
}

/// 昇順ストリームの golden バイト列をデコーダが絶対 Location に解決できること
#[test]
fn ascending_object_id_delta_golden_bytes_decode() {
    assert_eq!(
        decode_golden_stream(&ascending_golden(), 0x01),
        vec![
            expected_object(10, 3, 5, 0),
            expected_object(10, 3, 6, 0),
            expected_object(10, 3, 8, 0),
            expected_object(10, 3, 11, 0),
            expected_object(12, 3, 0, 0),
            expected_object(12, 7, 2, 0),
            expected_object(12, 7, 3, 0),
            DecodedFetchEntry::EndOfNonExistentRange {
                group_id: 12,
                object_id: 10,
            },
            expected_object(12, 7, 12, 0),
            expected_object(12, 7, 13, 0),
        ],
        "最初の絶対 ID と +2 規則が位置に応じて解決されること"
    );
}

/// 昇順ストリームの golden バイト列をエンコーダが生成できること
///
/// エンコーダの公開 API は絶対 Location を受け取り、同じ golden バイト列を生成しなければ
/// ならない。End of Range を挟んでも直前の actual Object の Subgroup ID / Priority を
/// 保持することも併せて固定する。
#[test]
fn ascending_object_id_delta_golden_bytes_encode() {
    let mut encoder = FetchStreamEncoder::new(1);
    let mut bytes = encoder.encode_header();
    let objects = [
        (10u64, 3u64, 5u64),
        (10, 3, 6),
        (10, 3, 8),
        (10, 3, 11),
        (12, 3, 0),
        (12, 7, 2),
        (12, 7, 3),
    ];
    for (group_id, subgroup_id, object_id) in objects {
        encoder
            .encode_object(
                &object_input(group_id, subgroup_id, object_id),
                None,
                &mut bytes,
            )
            .expect("golden バイト列の Object はエンコードできる");
    }
    // 7 件目の後に End of Range を置き、その後の Object は End of Range の Location を基準にする
    encoder
        .encode_end_of_non_existent_range(12, 10, &mut bytes)
        .expect("End of Non-Existent Range はエンコードできる");
    for (group_id, subgroup_id, object_id) in [(12u64, 7u64, 12u64), (12, 7, 13)] {
        encoder
            .encode_object(
                &object_input(group_id, subgroup_id, object_id),
                None,
                &mut bytes,
            )
            .expect("End of Range の後の Object はエンコードできる");
    }
    assert_eq!(
        bytes,
        ascending_golden(),
        "昇順の Object ID Delta が golden バイト列どおりにエンコードされること"
    );
}

/// 降順ストリームの golden バイト列を組み立てる
///
/// 降順では Group ID Delta を prior Group ID から減算する。Group ID Delta が 0 でも
/// フィールドが present なら Group は 1 つ進むため、Object ID は絶対値になる。
/// これは End of Range の「Group ID Delta が非 0 のときだけ絶対 Object ID」という規則とは
/// 異なる (Object はフィールドの有無で決まる) ことを固定する。
fn descending_golden() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&FETCH_HEADER_BYTES);
    // Object: Group 10 / Subgroup 0 / Object 5 / Priority 200 / payload 0
    bytes.extend_from_slice(&[0x1F, 0x0A, 0x00, 0x05, 0xC8, 0x00]);
    // Object: 同一 Group / Object 6 は prior + 1 の省略
    bytes.extend_from_slice(&[0x01, 0x00]);
    // Object: 同一 Group / Object 8 は prior 6 + delta 0 + 2
    bytes.extend_from_slice(&[0x05, 0x00, 0x00]);
    // Object: Group 9 は Group ID Delta 0 (10 - (0 + 1)) / Object ID は絶対値 0。
    // Group をまたぐため Subgroup ID 0 も明示する
    bytes.extend_from_slice(&[0x0F, 0x00, 0x00, 0x00, 0x00]);
    // Object: 同一 Group / Object 2 は prior 0 + delta 0 + 2
    bytes.extend_from_slice(&[0x05, 0x00, 0x00]);
    bytes
}

/// 降順ストリームの golden バイト列をデコーダが解決できること
#[test]
fn descending_object_id_delta_golden_bytes_decode() {
    assert_eq!(
        decode_golden_stream(&descending_golden(), 0x02),
        vec![
            expected_object(10, 0, 5, 0),
            expected_object(10, 0, 6, 0),
            expected_object(10, 0, 8, 0),
            expected_object(9, 0, 0, 0),
            expected_object(9, 0, 2, 0),
        ],
        "降順でも +2 規則と Group ID Delta present の絶対 Object ID が解決されること"
    );
}

/// 降順ストリームの golden バイト列をエンコーダが生成できること
#[test]
fn descending_object_id_delta_golden_bytes_encode() {
    let mut encoder =
        FetchStreamEncoder::new_with_group_order(1, 0x02).expect("Descending は有効な Group Order");
    let mut bytes = encoder.encode_header();
    let objects = [
        (10u64, 0u64, 5u64),
        (10, 0, 6),
        (10, 0, 8),
        (9, 0, 0),
        (9, 0, 2),
    ];
    for (group_id, subgroup_id, object_id) in objects {
        encoder
            .encode_object(
                &object_input(group_id, subgroup_id, object_id),
                None,
                &mut bytes,
            )
            .expect("golden バイト列の Object はエンコードできる");
    }
    assert_eq!(
        bytes,
        descending_golden(),
        "降順の Object ID Delta が golden バイト列どおりにエンコードされること"
    );
}

/// End of Range の後の Object が End of Range の位置を基準に +2 規則で解決されること
///
/// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields): End of Range の後の Object が
/// prior 参照を使う場合、prior Group ID / Object ID は End of Range の解決済み Location である。
/// その Object の Object ID Delta は §11.4.1.1 (Flags) の +2 規則で解決し、End of Range の
/// +1 規則 (§11.4.2.1 (ID Decoding)) を流用しない。Subgroup 0 の Object 10 →
/// End of Range 20 → Object 31 は delta 9 (31 - 20 - 2)、Object 32 は +1 の省略になる。
#[test]
fn object_after_end_of_range_uses_plus_two_rule() {
    let mut golden = FETCH_HEADER_BYTES.to_vec();
    // Object: Group 5 / Subgroup 0 / Object 10 / Priority 200 / payload 0
    golden.extend_from_slice(&[0x1F, 0x05, 0x00, 0x0A, 0xC8, 0x00]);
    // End of Unknown Range: Group 5 (delta 0) / Object 20 (10 + 9 + 1)
    golden.extend_from_slice(&[0x81, 0x0C, 0x00, 0x09]);
    // Object: Object 31 (20 + 9 + 2) / Subgroup 0 は直前の actual Object と同じ
    golden.extend_from_slice(&[0x05, 0x09, 0x00]);
    // Object: Object 32 は prior + 1 の省略
    golden.extend_from_slice(&[0x01, 0x00]);
    assert_eq!(
        decode_golden_stream(&golden, 0x01),
        vec![
            expected_object(5, 0, 10, 0),
            DecodedFetchEntry::EndOfUnknownRange {
                group_id: 5,
                object_id: 20,
            },
            expected_object(5, 0, 31, 0),
            expected_object(5, 0, 32, 0),
        ],
        "End of Range の +1 と Object の +2 が混同されずに解決されること"
    );

    let mut encoder = FetchStreamEncoder::new(1);
    let mut bytes = encoder.encode_header();
    encoder
        .encode_object(&object_input(5, 0, 10), None, &mut bytes)
        .expect("golden バイト列の先頭 Object はエンコードできる");
    encoder
        .encode_end_of_unknown_range(5, 20, &mut bytes)
        .expect("End of Unknown Range はエンコードできる");
    encoder
        .encode_object(&object_input(5, 0, 31), None, &mut bytes)
        .expect("End of Range の後の Object はエンコードできる");
    encoder
        .encode_object(&object_input(5, 0, 32), None, &mut bytes)
        .expect("End of Range の後の +1 Object はエンコードできる");
    assert_eq!(
        bytes, golden,
        "End of Range の後の Object が +2 規則でエンコードされること"
    );
}

/// 同一 Group の +2 加算が u64::MAX に到達できること
///
/// draft-ietf-moq-transport-23 §11.4.1.1 (Flags): 解決結果が 2^64 - 1 を超える場合だけ
/// DELTA_ENCODING_OVERFLOW になる。u64::MAX - 2 に delta 0 を加えた u64::MAX は
/// 表現可能であり、境界値そのものは受理される。
#[test]
fn same_group_object_id_delta_reaches_u64_max() {
    let mut bytes = FETCH_HEADER_BYTES.to_vec();
    // 最初の Object の Object ID = u64::MAX - 2 (9 バイト varint の絶対値)
    bytes.extend_from_slice(&[0x1F, 0x00, 0x00]);
    bytes.extend_from_slice(&[0xFF; 8]);
    bytes.push(0xFD);
    bytes.extend_from_slice(&[0xC8, 0x00]);
    // 同一 Group の delta 0 で (u64::MAX - 2) + 0 + 2 = u64::MAX
    bytes.extend_from_slice(&[0x05, 0x00, 0x00]);

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
    assert!(
        matches!(
            first,
            DecodedFetchEntry::Object(ref object) if object.object_id == u64::MAX - 2
        ),
        "最初の Object の絶対 Object ID が解決されること: {first:?}"
    );
    let second = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(
        second,
        expected_object(0, 0, u64::MAX, 0),
        "u64::MAX ちょうどは加算 overflow にならないこと"
    );
    decoder
        .finish()
        .expect("エントリ境界で終端したストリームは finish で受容される");
}

/// 同一 Group の +2 加算の overflow が DELTA_ENCODING_OVERFLOW になること
///
/// draft-ietf-moq-transport-23 §11.4.1.1 (Flags): "If the computed Object ID would be
/// greater than 2^64-1, the Subscriber MUST close the Session with DELTA_ENCODING_OVERFLOW."
/// prior Object ID が u64::MAX のとき delta 0 でも +2 が overflow する。
#[test]
fn same_group_object_id_delta_plus_two_overflow_is_delta_encoding_overflow() {
    let mut bytes = FETCH_HEADER_BYTES.to_vec();
    // 最初の Object の Object ID = u64::MAX (9 バイト varint の絶対値)
    bytes.extend_from_slice(&[0x1F, 0x00, 0x00]);
    bytes.extend_from_slice(&[0xFF; 9]);
    bytes.extend_from_slice(&[0xC8, 0x00]);
    // 同一 Group の delta 0 で u64::MAX + 0 + 2 が overflow する
    bytes.extend_from_slice(&[0x05, 0x00, 0x00]);

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
    assert!(
        matches!(
            first,
            DecodedFetchEntry::Object(ref object) if object.object_id == u64::MAX
        ),
        "最初の Object の絶対 Object ID が解決されること: {first:?}"
    );
    assert!(
        matches!(
            decoder.try_decode_entry(),
            Err(MessageError::DeltaEncodingOverflow(_))
        ),
        "Object ID の +2 加算 overflow は DELTA_ENCODING_OVERFLOW になること"
    );
}
