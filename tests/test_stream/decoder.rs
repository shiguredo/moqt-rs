//! SubgroupStreamDecoder / FetchStreamDecoder の外部テスト
//!
//! エンコード済みバイト列をデコーダでインクリメンタルにデコードして検証する。
use super::*;
use shiguredo_moqt::object_properties::{
    ObjectProperties, ObjectProperty, ObjectPropertyValue, PROP_PRIOR_OBJECT_ID_GAP,
};

/// テスト用: SubgroupHeader + SubgroupObject 列をエンコードする
/// ペイロードも含めてバッファに書き込む（デコーダはペイロードに触れないが、
/// consume_payload 後にバッファから drain する必要があるためテスト側で処理する）
/// (object_id_delta, payload_length, status, payload)
type SubgroupObjectTuple<'a> = (u64, u64, Option<u64>, Option<&'a [u8]>);

fn encode_subgroup_stream(header: &SubgroupHeader, objects: &[SubgroupObjectTuple<'_>]) -> Vec<u8> {
    let mut buf = header.encode();
    for (delta, payload_length, status, payload) in objects {
        let obj = SubgroupObject {
            object_id_delta: *delta,
            payload_length: *payload_length,
            status: *status,
        };
        obj.encode(header.has_properties, None, &mut buf)
            .expect("正当なテスト入力の encode は成功する");
        if let Some(p) = payload {
            buf.extend_from_slice(p);
        }
    }
    buf
}

/// デコーダのバッファからペイロードを読み出して長さを検証するヘルパー
fn drain_payload(decoder: &mut SubgroupStreamDecoder, expected_length: u64) {
    let payload = decoder.try_read_payload().expect("payload が取れる");
    assert_eq!(
        payload.len() as u64,
        expected_length,
        "読み出したペイロード長が期待値と一致すること"
    );
}

#[test]
fn test_basic_decode() {
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 10,
        subgroup_id: SubgroupIdMode::Zero,
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    // object_id: 0 (delta=0), 1 (delta=0), 2 (delta=0)
    let data = encode_subgroup_stream(
        &header,
        &[
            (0, 4, None, Some(b"aaaa")),
            (0, 3, None, Some(b"bbb")),
            (0, 2, None, Some(b"cc")),
        ],
    );

    let mut decoder = SubgroupStreamDecoder::new();
    decoder.push(&data);

    // ヘッダーをデコードする
    let decoded_header = decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(decoded_header, header);

    // オブジェクト 0
    let obj = decoder
        .try_decode_object()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(obj.object_id, 0);
    assert_eq!(obj.payload_length, 4);
    drain_payload(&mut decoder, 4);

    // オブジェクト 1
    let obj = decoder
        .try_decode_object()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(obj.object_id, 1);
    assert_eq!(obj.payload_length, 3);
    drain_payload(&mut decoder, 3);

    // オブジェクト 2
    let obj = decoder
        .try_decode_object()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(obj.object_id, 2);
    assert_eq!(obj.payload_length, 2);
    drain_payload(&mut decoder, 2);
}

#[test]
fn test_incremental_push() {
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(5),
        publisher_priority: Some(64),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    let data = encode_subgroup_stream(&header, &[(0, 2, None, Some(b"ab"))]);

    let mut decoder = SubgroupStreamDecoder::new();

    // 1 バイトずつ push してヘッダーデコードを試みる
    for &byte in &data[..3] {
        assert!(
            decoder
                .try_decode_header()
                .expect("テストフィクスチャの前提条件を満たす")
                .is_none()
        );
        decoder.push(&[byte]);
    }
    // 残りを push する
    decoder.push(&data[3..]);
    let decoded_header = decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(decoded_header, header);

    let obj = decoder
        .try_decode_object()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(obj.object_id, 0);
    assert_eq!(obj.payload_length, 2);
}

#[test]
fn test_object_id_delta_resolution() {
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Zero,
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    // object_id: 5 (delta=5), 6 (delta=0), 10 (delta=3)
    let data = encode_subgroup_stream(
        &header,
        &[
            (5, 1, None, Some(b"a")),
            (0, 1, None, Some(b"b")),
            (3, 1, None, Some(b"c")),
        ],
    );

    let mut decoder = SubgroupStreamDecoder::new();
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");

    let obj = decoder
        .try_decode_object()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(obj.object_id, 5);
    drain_payload(&mut decoder, 1);

    let obj = decoder
        .try_decode_object()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(obj.object_id, 6);
    drain_payload(&mut decoder, 1);

    let obj = decoder
        .try_decode_object()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(obj.object_id, 10);
    drain_payload(&mut decoder, 1);
}

#[test]
fn test_first_object_id_mode() {
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 0,
        subgroup_id: SubgroupIdMode::FirstObjectId,
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    // 最初のオブジェクト: object_id = 3 → subgroup_id = 3
    let data = encode_subgroup_stream(
        &header,
        &[(3, 1, None, Some(b"x")), (0, 1, None, Some(b"y"))],
    );

    let mut decoder = SubgroupStreamDecoder::new();
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");

    // 未解決
    assert_eq!(decoder.resolved_subgroup_id(), None);

    let obj = decoder
        .try_decode_object()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(obj.object_id, 3);
    drain_payload(&mut decoder, 1);

    // 最初のオブジェクトで subgroup_id が確定する
    assert_eq!(decoder.resolved_subgroup_id(), Some(3));

    let obj = decoder
        .try_decode_object()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(obj.object_id, 4);
    drain_payload(&mut decoder, 1);
}

#[test]
fn test_status_object_no_payload_consume() {
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Zero,
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    // object_id: 0 (通常), 1 (EndOfGroup status)
    let data = encode_subgroup_stream(
        &header,
        &[(0, 2, None, Some(b"ab")), (0, 0, Some(0x3), None)],
    );

    let mut decoder = SubgroupStreamDecoder::new();
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");

    let obj = decoder
        .try_decode_object()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(obj.object_id, 0);
    assert_eq!(obj.payload_length, 2);
    drain_payload(&mut decoder, 2);

    // status オブジェクトは consume_payload 不要で次に進める
    let obj = decoder
        .try_decode_object()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(obj.object_id, 1);
    assert_eq!(obj.payload_length, 0);
    assert_eq!(obj.status, Some(0x3));
}

#[test]
fn test_empty_buffer_returns_none() {
    let mut decoder = SubgroupStreamDecoder::new();
    assert!(
        decoder
            .try_decode_header()
            .expect("テストフィクスチャの前提条件を満たす")
            .is_none()
    );
}

#[test]
fn test_double_header_decode_error() {
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Zero,
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    let data = header.encode();
    let mut decoder = SubgroupStreamDecoder::new();
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");

    // 2 回目のヘッダーデコードはエラー
    assert!(matches!(
        decoder.try_decode_header(),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn test_object_before_header_error() {
    let mut decoder = SubgroupStreamDecoder::new();
    // ヘッダーなしでオブジェクトをデコードしようとするとエラー
    assert!(matches!(
        decoder.try_decode_object(),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn test_resolved_subgroup_id_explicit() {
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(42),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    let data = header.encode();
    let mut decoder = SubgroupStreamDecoder::new();
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(decoder.resolved_subgroup_id(), Some(42));
}

#[test]
fn test_resolved_subgroup_id_zero() {
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Zero,
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    let data = header.encode();
    let mut decoder = SubgroupStreamDecoder::new();
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(decoder.resolved_subgroup_id(), Some(0));
}

#[test]
fn test_subgroup_finish_without_header_errors() {
    let decoder = SubgroupStreamDecoder::new();
    assert_eq!(decoder.finish(), Err(MessageError::UnexpectedEof));
}

/// ヘッダーのみ + FIN（オブジェクト 0 個）の空 Subgroup で finish が Ok を返す
///
/// draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams) 冒頭の
/// "If a sender has delivered all objects in a Subgroup to the QUIC stream, except any
/// Objects with Locations smaller than the subscription's Start Location, it MUST close
/// the stream with a FIN." により、配達すべきオブジェクトが 0 個の Subgroup では
/// ヘッダーのみ送って FIN で閉じるのが MUST に沿うフローになる。
/// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
#[test]
fn test_subgroup_finish_after_header_without_object_ok() {
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Zero,
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    let mut decoder = SubgroupStreamDecoder::new();
    decoder.push(&header.encode());
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(decoder.finish(), Ok(()));
}

/// ヘッダー + 不完全なオブジェクトヘッダー（バッファ非空）で finish が Err を返す
///
/// 空 Subgroup の受理は「バッファが空であること」で判定するため、partial object
/// header は従来どおり UnexpectedEof として検出される。
#[test]
fn test_subgroup_finish_with_partial_object_header_errors() {
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Zero,
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    let data = encode_subgroup_stream(&header, &[(0, 2, None, Some(b"ab"))]);
    // ヘッダー全体 + オブジェクトヘッダーの先頭 1 バイトのみ（オブジェクトヘッダーとして不完全）を送る
    let header_len = header.encode().len();
    let mut decoder = SubgroupStreamDecoder::new();
    decoder.push(&data[..header_len + 1]);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    // partial object header はデコード不能でデータ不足として扱われる
    assert!(
        decoder
            .try_decode_object()
            .expect("テストフィクスチャの前提条件を満たす")
            .is_none(),
        "partial object header はデータ不足として扱われること"
    );
    assert_eq!(decoder.finish(), Err(MessageError::UnexpectedEof));
}

/// partial SUBGROUP_HEADER（ヘッダーの途中で切れたデータ）で finish が Err を返す
///
/// 状態は `AwaitingHeader` のまま残るため、`finish` は Err を返す。
/// 先頭 1 バイトは Type varint であり、track_alias 以降が欠落しているため
/// ヘッダーデコードはデータ不足 (`Ok(None)`) になる。
#[test]
fn test_subgroup_finish_with_partial_header_errors() {
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Zero,
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    let data = header.encode();
    let mut decoder = SubgroupStreamDecoder::new();
    decoder.push(&data[..1]);
    assert!(
        decoder
            .try_decode_header()
            .expect("テストフィクスチャの前提条件を満たす")
            .is_none(),
        "partial header はデータ不足として扱われること"
    );
    assert_eq!(decoder.finish(), Err(MessageError::UnexpectedEof));
}

/// `SubgroupIdMode::FirstObjectId` の空 Subgroup で finish が Ok を返し、
/// subgroup_id が未解決 (`None`) のままである
///
/// draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header) の SUBGROUP_ID_MODE 0b01 は
/// "the Subgroup ID is the Object ID of the first Object transmitted in this Subgroup"
/// と定義しており、オブジェクト 0 個では Subgroup ID が仕様上未定義になる。
/// 受信側 session は `subgroup_id` が `None` の場合に tracker 更新をスキップして
/// 終端を受理するため、decoder も正規の完了として受理する。
#[test]
fn test_subgroup_finish_first_object_id_empty_ok() {
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 0,
        subgroup_id: SubgroupIdMode::FirstObjectId,
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    let mut decoder = SubgroupStreamDecoder::new();
    decoder.push(&header.encode());
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(decoder.resolved_subgroup_id(), None);
    assert_eq!(decoder.finish(), Ok(()));
}

#[test]
fn test_subgroup_finish_with_partial_payload_errors() {
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 10,
        subgroup_id: SubgroupIdMode::Zero,
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    let mut data = encode_subgroup_stream(&header, &[(0, 2, None, Some(b"ab"))]);
    data.pop();

    let mut decoder = SubgroupStreamDecoder::new();
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    let obj = decoder
        .try_decode_object()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(obj.payload_length, 2);
    assert_eq!(decoder.finish(), Err(MessageError::UnexpectedEof));
}

#[test]
fn test_subgroup_finish_at_object_boundary_ok() {
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 10,
        subgroup_id: SubgroupIdMode::Zero,
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    let data = encode_subgroup_stream(&header, &[(0, 2, None, Some(b"ab"))]);

    let mut decoder = SubgroupStreamDecoder::new();
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    let obj = decoder
        .try_decode_object()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    drain_payload(&mut decoder, obj.payload_length);
    assert_eq!(decoder.finish(), Ok(()));
}

/// payload 長 0 の status オブジェクトで終端した後（`ConsumingPayload` を経由しない）
/// finish が Ok を返す
///
/// `try_decode_object` は payload 0 のオブジェクトでは状態を `AwaitingObject` のまま
/// 進めるため、FIN 時点でバッファが空なら完結として受理される。
#[test]
fn test_subgroup_finish_after_status_object_ok() {
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 10,
        subgroup_id: SubgroupIdMode::Zero,
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    let data = encode_subgroup_stream(&header, &[(0, 0, Some(0x3), None)]);

    let mut decoder = SubgroupStreamDecoder::new();
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    let obj = decoder
        .try_decode_object()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(obj.payload_length, 0);
    assert_eq!(decoder.finish(), Ok(()));
}

#[test]
fn test_subgroup_rejects_object_id_delta_overflow() {
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Zero,
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    // 最初のオブジェクト: object_id = u64::MAX - 1 (delta = u64::MAX - 1)
    // 2 番目: delta = u64::MAX → prev + delta + 1 でオーバーフロー
    let data = encode_subgroup_stream(
        &header,
        &[
            (u64::MAX - 1, 1, None, Some(b"a")),
            (u64::MAX, 1, None, Some(b"b")),
        ],
    );

    let mut decoder = SubgroupStreamDecoder::new();
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");

    let obj = decoder
        .try_decode_object()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(obj.object_id, u64::MAX - 1);
    drain_payload(&mut decoder, 1);

    // 2 番目のオブジェクトでオーバーフローが検出される
    assert!(matches!(
        decoder.try_decode_object(),
        Err(MessageError::ProtocolViolation(_))
    ));
}

// ─── FetchStreamDecoder テスト ──────────────────────────────

/// FetchStreamDecoder のバッファからペイロードを読み出して長さを検証するヘルパー
fn drain_fetch_payload(decoder: &mut FetchStreamDecoder, expected_length: u64) {
    let payload = decoder.try_read_payload().expect("payload が取れる");
    assert_eq!(
        payload.len() as u64,
        expected_length,
        "読み出したペイロード長が期待値と一致すること"
    );
}

/// (entry, properties_data, payload)
type FetchEntryTuple<'a> = (FetchStreamEntry, Option<&'a [u8]>, Option<&'a [u8]>);

/// テスト用: FetchHeader + FetchStreamEntry 列をエンコードする
fn encode_fetch_stream(header: &FetchHeader, entries: &[FetchEntryTuple<'_>]) -> Vec<u8> {
    let mut buf = header.encode();
    let mut prior = FetchPriorContext::First;
    for (entry, props, payload) in entries {
        entry
            .encode(*props, prior, &mut buf)
            .expect("正当なテスト入力の encode は成功する");
        if let Some(p) = payload {
            buf.extend_from_slice(p);
        }
        // prior_context を遷移する
        match entry {
            FetchStreamEntry::Object(_) => {
                prior = FetchPriorContext::HasPriorObject;
            }
            FetchStreamEntry::EndOfNonExistentRange { .. }
            | FetchStreamEntry::EndOfUnknownRange { .. }
            | FetchStreamEntry::EndOfTimedOutRange { .. } => {
                if matches!(prior, FetchPriorContext::First) {
                    prior = FetchPriorContext::NoPriorActualObject;
                }
            }
        }
    }
    buf
}

#[test]
fn test_fetch_basic_decode() {
    let header = FetchHeader { request_id: 42 };
    let obj1 = FetchStreamEntry::Object(FetchStreamObject {
        group_id: Some(10),
        subgroup_id: FetchSubgroupIdMode::Explicit(0),
        object_id: Some(0),
        publisher_priority: Some(128),
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 3,
    });
    let obj2 = FetchStreamEntry::Object(FetchStreamObject {
        group_id: None,                                 // prior (10) を継承
        subgroup_id: FetchSubgroupIdMode::PreviousSame, // prior (0) を継承
        object_id: None,                                // prior + 1 = 1
        publisher_priority: None,                       // prior (128) を継承
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 2,
    });

    let data = encode_fetch_stream(
        &header,
        &[(obj1, None, Some(b"abc")), (obj2, None, Some(b"de"))],
    );

    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&data);

    // ヘッダー
    let h = decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(h.request_id, 42);

    // エントリ 1: 全フィールド明示
    let entry = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    match entry {
        DecodedFetchEntry::Object(obj) => {
            assert_eq!(obj.group_id, 10);
            assert_eq!(obj.subgroup_id, 0);
            assert_eq!(obj.object_id, 0);
            assert_eq!(obj.publisher_priority, 128);
            assert_eq!(obj.payload_length, 3);
        }
        _ => panic!("Object が期待された"),
    }
    drain_fetch_payload(&mut decoder, 3);

    // エントリ 2: デルタ圧縮
    let entry = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    match entry {
        DecodedFetchEntry::Object(obj) => {
            assert_eq!(obj.group_id, 10); // 継承
            assert_eq!(obj.subgroup_id, 0); // PreviousSame
            assert_eq!(obj.object_id, 1); // prior + 1
            assert_eq!(obj.publisher_priority, 128); // 継承
            assert_eq!(obj.payload_length, 2);
        }
        _ => panic!("Object が期待された"),
    }
    drain_fetch_payload(&mut decoder, 2);
}

#[test]
fn test_fetch_end_of_range_transitions() {
    let header = FetchHeader { request_id: 1 };
    // EndOfNonExistentRange → Object の順
    let end_entry = FetchStreamEntry::EndOfNonExistentRange {
        group_id: 5,
        object_id: 10,
    };
    // End of Range 後の Object は NoPriorActualObject 文脈
    // group_id / object_id は prior から参照可能だが、subgroup_id / priority は明示必須
    // draft-ietf-moq-transport-21 §11.4.1.1 (Flags): Object ID Delta absent → prior + 1
    // prior (End of Range) の object_id=10 のとき解決値は 11
    let obj = FetchStreamEntry::Object(FetchStreamObject {
        group_id: None,                                // prior (End of Range の 5) を継承
        subgroup_id: FetchSubgroupIdMode::Explicit(0), // 明示必須
        object_id: None,                               // absent → prior + 1 = 11
        publisher_priority: Some(64),                  // 明示必須
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 1,
    });

    let data = encode_fetch_stream(&header, &[(end_entry, None, None), (obj, None, Some(b"x"))]);

    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");

    // EndOfNonExistentRange
    let entry = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    match entry {
        DecodedFetchEntry::EndOfNonExistentRange {
            group_id,
            object_id,
        } => {
            assert_eq!(group_id, 5);
            assert_eq!(object_id, 10);
        }
        _ => panic!("EndOfNonExistentRange が期待された"),
    }

    // Object (NoPriorActualObject 文脈)
    let entry = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    match entry {
        DecodedFetchEntry::Object(obj) => {
            assert_eq!(obj.group_id, 5); // End of Range の値を継承
            assert_eq!(obj.subgroup_id, 0);
            assert_eq!(obj.object_id, 11);
            assert_eq!(obj.publisher_priority, 64);
        }
        _ => panic!("Object が期待された"),
    }
}

#[test]
fn test_fetch_subgroup_previous_plus_one() {
    let header = FetchHeader { request_id: 1 };
    let obj1 = FetchStreamEntry::Object(FetchStreamObject {
        group_id: Some(0),
        subgroup_id: FetchSubgroupIdMode::Explicit(3),
        object_id: Some(0),
        publisher_priority: Some(128),
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 1,
    });
    let obj2 = FetchStreamEntry::Object(FetchStreamObject {
        group_id: None,
        subgroup_id: FetchSubgroupIdMode::PreviousPlusOne, // 3 + 1 = 4
        object_id: None,                                   // 0 + 1 = 1
        publisher_priority: None,
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 1,
    });

    let data = encode_fetch_stream(
        &header,
        &[(obj1, None, Some(b"a")), (obj2, None, Some(b"b"))],
    );

    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");

    let entry = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    match entry {
        DecodedFetchEntry::Object(obj) => {
            assert_eq!(obj.subgroup_id, 3);
        }
        _ => panic!("Object が期待された"),
    }
    drain_fetch_payload(&mut decoder, 1);

    let entry = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    match entry {
        DecodedFetchEntry::Object(obj) => {
            assert_eq!(obj.subgroup_id, 4); // PreviousPlusOne
            assert_eq!(obj.object_id, 1); // prior + 1
        }
        _ => panic!("Object が期待された"),
    }
}

#[test]
fn test_fetch_empty_buffer_returns_none() {
    let mut decoder = FetchStreamDecoder::new();
    assert!(
        decoder
            .try_decode_header()
            .expect("テストフィクスチャの前提条件を満たす")
            .is_none()
    );
}

#[test]
fn test_fetch_double_header_error() {
    let header = FetchHeader { request_id: 1 };
    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&header.encode());
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");

    assert!(matches!(
        decoder.try_decode_header(),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn test_fetch_entry_before_header_error() {
    let mut decoder = FetchStreamDecoder::new();
    assert!(matches!(
        decoder.try_decode_entry(),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn test_fetch_zero_payload_no_consume() {
    let header = FetchHeader { request_id: 1 };
    let obj = FetchStreamEntry::Object(FetchStreamObject {
        group_id: Some(0),
        subgroup_id: FetchSubgroupIdMode::Zero,
        object_id: Some(0),
        publisher_priority: Some(128),
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 0,
    });

    let data = encode_fetch_stream(&header, &[(obj, None, None)]);

    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");

    let entry = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    match entry {
        DecodedFetchEntry::Object(obj) => {
            assert_eq!(obj.payload_length, 0);
        }
        _ => panic!("Object が期待された"),
    }
    // payload_length == 0 なので consume_payload 不要、次のエントリに進める
}

#[test]
fn test_fetch_rejects_object_id_delta_overflow() {
    let header = FetchHeader { request_id: 1 };
    let first = FetchStreamEntry::Object(FetchStreamObject {
        group_id: Some(10),
        subgroup_id: FetchSubgroupIdMode::Explicit(0),
        object_id: Some(2),
        publisher_priority: Some(128),
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 1,
    });
    // draft-ietf-moq-transport-21 §11.4.1.1 (Flags): 同 Group の object_id はデルタ (prev_object + delta + 1)
    // delta = u64::MAX でオーバーフローを起こさせる
    let second = FetchStreamEntry::Object(FetchStreamObject {
        group_id: None, // prior を継承 → 同 Group (10)
        subgroup_id: FetchSubgroupIdMode::Explicit(0),
        object_id: Some(u64::MAX), // デルタオーバーフロー
        publisher_priority: Some(128),
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 1,
    });

    let data = encode_fetch_stream(
        &header,
        &[(first, None, Some(b"a")), (second, None, Some(b"b"))],
    );

    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    let first = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert!(matches!(first, DecodedFetchEntry::Object(_)));
    drain_fetch_payload(&mut decoder, 1);

    assert!(matches!(
        decoder.try_decode_entry(),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn test_fetch_rejects_group_id_delta_overflow_in_ascending_mode() {
    let header = FetchHeader { request_id: 1 };
    let first = FetchStreamEntry::Object(FetchStreamObject {
        group_id: Some(10),
        subgroup_id: FetchSubgroupIdMode::Explicit(0),
        object_id: Some(0),
        publisher_priority: Some(128),
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 1,
    });
    // draft-ietf-moq-transport-21 §11.4.1.1 (Flags): 非初回 Object の group_id はデルタ (prev_group + delta + 1)
    // delta = u64::MAX でオーバーフローを起こさせる
    let second = FetchStreamEntry::Object(FetchStreamObject {
        group_id: Some(u64::MAX), // デルタオーバーフロー
        subgroup_id: FetchSubgroupIdMode::Explicit(0),
        object_id: Some(0), // Group 変更時は絶対値
        publisher_priority: Some(128),
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 1,
    });

    let data = encode_fetch_stream(
        &header,
        &[(first, None, Some(b"a")), (second, None, Some(b"b"))],
    );

    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    let first = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert!(matches!(first, DecodedFetchEntry::Object(_)));
    drain_fetch_payload(&mut decoder, 1);

    assert!(matches!(
        decoder.try_decode_entry(),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn test_fetch_rejects_subgroup_id_previous_plus_one_overflow() {
    let header = FetchHeader { request_id: 1 };
    let first = FetchStreamEntry::Object(FetchStreamObject {
        group_id: Some(0),
        subgroup_id: FetchSubgroupIdMode::Explicit(u64::MAX),
        object_id: Some(0),
        publisher_priority: Some(128),
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 1,
    });
    // PreviousPlusOne: prior.subgroup_id (u64::MAX) + 1 でオーバーフロー
    let second = FetchStreamEntry::Object(FetchStreamObject {
        group_id: None,
        subgroup_id: FetchSubgroupIdMode::PreviousPlusOne,
        object_id: None,
        publisher_priority: None,
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 1,
    });

    let data = encode_fetch_stream(
        &header,
        &[(first, None, Some(b"a")), (second, None, Some(b"b"))],
    );

    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    let first = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert!(matches!(first, DecodedFetchEntry::Object(_)));
    drain_fetch_payload(&mut decoder, 1);

    assert!(matches!(
        decoder.try_decode_entry(),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn test_fetch_rejects_ascending_group_in_descending_mode() {
    let header = FetchHeader { request_id: 1 };
    let first = FetchStreamEntry::Object(FetchStreamObject {
        group_id: Some(10),
        subgroup_id: FetchSubgroupIdMode::Explicit(0),
        object_id: Some(0),
        publisher_priority: Some(128),
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 1,
    });
    let second = FetchStreamEntry::Object(FetchStreamObject {
        group_id: Some(11),
        subgroup_id: FetchSubgroupIdMode::Explicit(0),
        object_id: Some(0),
        publisher_priority: Some(128),
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 1,
    });

    let data = encode_fetch_stream(
        &header,
        &[(first, None, Some(b"a")), (second, None, Some(b"b"))],
    );

    let mut decoder = FetchStreamDecoder::new_with_group_order(0x02)
        .expect("テストフィクスチャの前提条件を満たす");
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    let first = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert!(matches!(first, DecodedFetchEntry::Object(_)));
    drain_fetch_payload(&mut decoder, 1);

    assert!(matches!(
        decoder.try_decode_entry(),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn test_fetch_rejects_priority_change_within_subgroup() {
    let header = FetchHeader { request_id: 1 };
    let first = FetchStreamEntry::Object(FetchStreamObject {
        group_id: Some(10),
        subgroup_id: FetchSubgroupIdMode::Explicit(7),
        object_id: Some(0),
        publisher_priority: Some(128),
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 1,
    });
    let second = FetchStreamEntry::Object(FetchStreamObject {
        group_id: None,
        subgroup_id: FetchSubgroupIdMode::PreviousSame,
        object_id: None,
        publisher_priority: Some(64),
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 1,
    });

    let data = encode_fetch_stream(
        &header,
        &[(first, None, Some(b"a")), (second, None, Some(b"b"))],
    );

    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    let first = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert!(matches!(first, DecodedFetchEntry::Object(_)));
    drain_fetch_payload(&mut decoder, 1);

    assert!(matches!(
        decoder.try_decode_entry(),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn test_fetch_rejects_malformed_object_properties() {
    let header = FetchHeader { request_id: 1 };
    let obj = FetchStreamEntry::Object(FetchStreamObject {
        group_id: Some(10),
        subgroup_id: FetchSubgroupIdMode::Explicit(0),
        object_id: Some(0),
        publisher_priority: Some(128),
        has_properties: true,
        is_datagram_origin: false,
        payload_length: 1,
    });
    // Properties Length = 1, しかし中身が value を持たないので malformed
    let malformed_properties = [0x01, 0x00];
    let data = encode_fetch_stream(&header, &[(obj, Some(&malformed_properties), Some(b"a"))]);

    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");

    assert!(matches!(
        decoder.try_decode_entry(),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn test_fetch_accepts_valid_prior_object_gap_properties() {
    let header = FetchHeader { request_id: 1 };
    let first = FetchStreamEntry::Object(FetchStreamObject {
        group_id: Some(10),
        subgroup_id: FetchSubgroupIdMode::Explicit(0),
        object_id: Some(2),
        publisher_priority: Some(128),
        has_properties: true,
        is_datagram_origin: false,
        payload_length: 1,
    });
    let second = FetchStreamEntry::Object(FetchStreamObject {
        group_id: None,
        subgroup_id: FetchSubgroupIdMode::PreviousSame,
        object_id: Some(5),
        publisher_priority: None,
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 1,
    });

    let mut props = ObjectProperties::new();
    props.push(ObjectProperty {
        prop_type: PROP_PRIOR_OBJECT_ID_GAP,
        value: ObjectPropertyValue::VarInt(2),
    });
    let mut encoded_props = Vec::new();
    props
        .encode(&mut encoded_props)
        .expect("正当なテスト入力の encode は成功する");

    let data = encode_fetch_stream(
        &header,
        &[
            (first, Some(&encoded_props), Some(b"a")),
            (second, None, Some(b"b")),
        ],
    );

    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    let first = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert!(matches!(first, DecodedFetchEntry::Object(_)));
    drain_fetch_payload(&mut decoder, 1);
    let second = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert!(matches!(second, DecodedFetchEntry::Object(_)));
}

#[test]
fn test_fetch_rejects_object_beyond_known_final_subgroup_object() {
    let header = FetchHeader { request_id: 1 };
    let obj = FetchStreamEntry::Object(FetchStreamObject {
        group_id: Some(10),
        subgroup_id: FetchSubgroupIdMode::Explicit(0),
        object_id: Some(1),
        publisher_priority: Some(128),
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 1,
    });
    let data = encode_fetch_stream(&header, &[(obj, None, Some(b"a"))]);

    let mut decoder = FetchStreamDecoder::new();
    decoder
        .set_subgroup_final_object(10, 0, 0)
        .expect("テストフィクスチャの前提条件を満たす");
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");

    assert!(matches!(
        decoder.try_decode_entry(),
        Err(MessageError::ProtocolViolation(_))
    ));
}

#[test]
fn test_fetch_finish_without_header_errors() {
    let decoder = FetchStreamDecoder::new();
    assert_eq!(decoder.finish(), Err(MessageError::UnexpectedEof));
}

/// ヘッダーのみ + FIN（エントリ 0 個）は正規の空 FETCH 応答として成功を返す
///
/// draft-ietf-moq-transport-21 §9.11 (FETCH): "If no Objects exist in the
/// requested range, the publisher opens the unidirectional stream, sends the FETCH_HEADER
/// (see Section 11.4.4) and closes the stream with a FIN."
#[test]
fn test_fetch_finish_after_header_without_entry_ok() {
    let header = FetchHeader { request_id: 1 };
    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&header.encode());
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert_eq!(decoder.finish(), Ok(()));
}

/// ヘッダー + 不完全なエントリヘッダー（バッファ非空）で finish が Err を返す
#[test]
fn test_fetch_finish_with_partial_entry_header_errors() {
    let header = FetchHeader { request_id: 1 };
    let obj = FetchStreamEntry::Object(FetchStreamObject {
        group_id: Some(10),
        subgroup_id: FetchSubgroupIdMode::Explicit(0),
        object_id: Some(0),
        publisher_priority: Some(128),
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 1,
    });
    // エントリヘッダーの途中で切れたデータを作る（末尾の payload_length varint を欠落させる）
    let mut entry_bytes = Vec::new();
    obj.encode(None, FetchPriorContext::First, &mut entry_bytes)
        .expect("正当なテスト入力の encode は成功する");
    entry_bytes.pop();

    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&header.encode());
    decoder.push(&entry_bytes);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert!(
        decoder
            .try_decode_entry()
            .expect("テストフィクスチャの前提条件を満たす")
            .is_none(),
        "partial entry header はデータ不足として扱われること"
    );
    assert_eq!(decoder.finish(), Err(MessageError::UnexpectedEof));
}

/// partial FETCH_HEADER（ヘッダーの途中で切れたデータ）で finish が Err を返す
///
/// `request_id` の varint が 1 バイトで表現されることを前提に、末尾 1 バイトを除いて
/// ヘッダーを不完全にする。`try_decode_header` はデータ不足として `None` を返し、
/// 状態は `AwaitingHeader` のまま残るため、`finish` は Err を返す。
#[test]
fn test_fetch_finish_with_partial_header_errors() {
    let header = FetchHeader { request_id: 1 };
    let mut header_bytes = header.encode();
    header_bytes.pop();

    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&header_bytes);
    assert!(
        decoder
            .try_decode_header()
            .expect("テストフィクスチャの前提条件を満たす")
            .is_none(),
        "partial FETCH_HEADER はデータ不足として扱われること"
    );
    assert_eq!(decoder.finish(), Err(MessageError::UnexpectedEof));
}

#[test]
fn test_fetch_finish_with_partial_payload_errors() {
    let header = FetchHeader { request_id: 1 };
    let obj = FetchStreamEntry::Object(FetchStreamObject {
        group_id: Some(10),
        subgroup_id: FetchSubgroupIdMode::Explicit(0),
        object_id: Some(0),
        publisher_priority: Some(128),
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 2,
    });
    let mut data = encode_fetch_stream(&header, &[(obj, None, Some(b"ab"))]);
    data.pop();

    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    let entry = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    match entry {
        DecodedFetchEntry::Object(obj) => assert_eq!(obj.payload_length, 2),
        _ => panic!("Object が期待された"),
    }
    assert_eq!(decoder.finish(), Err(MessageError::UnexpectedEof));
}

#[test]
fn test_fetch_finish_at_entry_boundary_ok() {
    let header = FetchHeader { request_id: 1 };
    let obj = FetchStreamEntry::Object(FetchStreamObject {
        group_id: Some(10),
        subgroup_id: FetchSubgroupIdMode::Explicit(0),
        object_id: Some(0),
        publisher_priority: Some(128),
        has_properties: false,
        is_datagram_origin: false,
        payload_length: 2,
    });
    let data = encode_fetch_stream(&header, &[(obj, None, Some(b"ab"))]);

    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    let entry = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    match entry {
        DecodedFetchEntry::Object(obj) => drain_fetch_payload(&mut decoder, obj.payload_length),
        _ => panic!("Object が期待された"),
    }
    assert_eq!(decoder.finish(), Ok(()));
}

/// End of Range エントリで終端した後の finish が Ok を返す
///
/// End of Range エントリは `FetchPriorContext` を `NoPriorActualObject` に遷移させる点で
/// Object エントリと分岐が異なるため、finish の条件変更が波及しないことを固定する。
#[test]
fn test_fetch_finish_after_end_of_range_ok() {
    let header = FetchHeader { request_id: 1 };
    let entry = FetchStreamEntry::EndOfNonExistentRange {
        group_id: 10,
        object_id: 5,
    };
    let data = encode_fetch_stream(&header, &[(entry, None, None)]);

    let mut decoder = FetchStreamDecoder::new();
    decoder.push(&data);
    decoder
        .try_decode_header()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    let decoded = decoder
        .try_decode_entry()
        .expect("テストフィクスチャの前提条件を満たす")
        .expect("テストフィクスチャに期待される内部値が入っている");
    assert!(matches!(
        decoded,
        DecodedFetchEntry::EndOfNonExistentRange {
            group_id: 10,
            object_id: 5
        }
    ));
    assert_eq!(decoder.finish(), Ok(()));
}
