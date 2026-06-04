//! OBJECT_DATAGRAM (draft-ietf-moq-transport-21 §11.2.1 (Object Datagram)) のラウンドトリップ PBT
//!
//! `ObjectDatagram::encode` はヘッダのみを返し、ペイロードは含まない。`decode` は
//! datagram 全体 (ヘッダ + ペイロード) を受け取り、ヘッダ部分の消費バイト数を返す。
//! このため status オブジェクト (ペイロードなし) と通常オブジェクト (ペイロードあり) で
//! 検証方法を分ける。

use pbt::common::test_runner;
use shiguredo_moqt::stream::datagram::ObjectDatagram;

use crate::length_prefixed_properties;
use pbt::common::sample_varint;

/// OBJECT_DATAGRAM (status オブジェクト・ properties なし・ペイロードなし) のラウンドトリップ。
/// status と END_OF_GROUP は同時指定不可のため end_of_group = false に固定する。
/// status 値域は Normal(0x0) / EndOfGroup(0x3) / EndOfTrack(0x4) (draft-ietf-moq-transport-21 §11.1.2 (Object Status))。
#[test]
fn object_datagram_roundtrip_status_object() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let track_alias = sample_varint(ctx);
        let group_id = sample_varint(ctx);
        let object_id = sample_varint(ctx);
        let publisher_priority = if noprop::sample_bool(ctx) {
            Some(noprop::sample_u8(ctx))
        } else {
            None
        };
        let status = noprop::sample_choice(ctx, &[0u64, 3u64, 4u64]);
        let datagram = ObjectDatagram {
            track_alias,
            group_id,
            object_id,
            publisher_priority,
            properties_data: None,
            end_of_group: false,
            status: Some(status),
        };
        let encoded = datagram
            .encode()
            .expect("正当なテスト入力の encode は成功する");
        let (decoded, consumed) =
            ObjectDatagram::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        // status オブジェクトはペイロードを持たないため、全体が消費される。
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, datagram);
        Ok(())
    })?;
    Ok(())
}

/// OBJECT_DATAGRAM (Normal status オブジェクト・ properties あり・ペイロードなし) の
/// ラウンドトリップ。properties は Normal(0x0) status にのみ付けられる
/// (draft-ietf-moq-transport-21 §11.1.3 (Object Properties))。`properties_data` は
/// Properties Length varint を含む生バイト列で渡す (draft-ietf-moq-transport-21 §11.2.1 (Object Datagram))。
#[test]
fn object_datagram_roundtrip_normal_status_with_properties() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let track_alias = sample_varint(ctx);
        let group_id = sample_varint(ctx);
        let object_id = sample_varint(ctx);
        let publisher_priority = if noprop::sample_bool(ctx) {
            Some(noprop::sample_u8(ctx))
        } else {
            None
        };
        // Datagram の properties_data は Properties Length + Properties の生バイト列
        // (SubgroupObject / FetchStreamObject と同じ規約)
        let prop_len = noprop::sample_usize_in(ctx, 1..64);
        let prop_payload = noprop::sample_bytes_vec(ctx, prop_len);
        let prop_blob = length_prefixed_properties(&prop_payload);
        let datagram = ObjectDatagram {
            track_alias,
            group_id,
            object_id,
            publisher_priority,
            properties_data: Some(prop_blob),
            end_of_group: false,
            status: Some(0),
        };
        let encoded = datagram
            .encode()
            .expect("正当なテスト入力の encode は成功する");
        let (decoded, consumed) =
            ObjectDatagram::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, datagram);
        Ok(())
    })?;
    Ok(())
}

/// OBJECT_DATAGRAM (通常オブジェクト・ペイロードあり) のラウンドトリップ。
/// status = None のときペイロードが必須 (zero-length は Normal status の明示が必要,
/// draft-ietf-moq-transport-21 §11.1.2 (Object Status))。decode の消費バイト数はヘッダ長と一致し、残りがペイロード。
#[test]
fn object_datagram_roundtrip_with_payload() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let track_alias = sample_varint(ctx);
        let group_id = sample_varint(ctx);
        let object_id = sample_varint(ctx);
        let publisher_priority = if noprop::sample_bool(ctx) {
            Some(noprop::sample_u8(ctx))
        } else {
            None
        };
        let end_of_group = noprop::sample_bool(ctx);
        let payload_len = noprop::sample_usize_in(ctx, 1..64);
        let payload = noprop::sample_bytes_vec(ctx, payload_len);
        let datagram = ObjectDatagram {
            track_alias,
            group_id,
            object_id,
            publisher_priority,
            properties_data: None,
            end_of_group,
            status: None,
        };
        let mut buf = datagram
            .encode()
            .expect("正当なテスト入力の encode は成功する");
        let header_len = buf.len();
        buf.extend_from_slice(&payload);
        let (decoded, consumed) =
            ObjectDatagram::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        // decode はヘッダのみ消費し、残りはペイロードとして呼び出し側が扱う。
        assert_eq!(consumed, header_len);
        assert_eq!(&buf[consumed..], &payload[..]);
        assert_eq!(decoded, datagram);
        Ok(())
    })?;
    Ok(())
}

/// OBJECT_DATAGRAM (properties あり・ペイロードあり) のラウンドトリップ。
/// `properties_data` は Properties Length varint を含む生バイト列で渡す
/// (draft-ietf-moq-transport-21 §11.2.1 (Object Datagram))。
#[test]
fn object_datagram_roundtrip_with_properties() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let track_alias = sample_varint(ctx);
        let group_id = sample_varint(ctx);
        let object_id = sample_varint(ctx);
        let publisher_priority = if noprop::sample_bool(ctx) {
            Some(noprop::sample_u8(ctx))
        } else {
            None
        };
        let end_of_group = noprop::sample_bool(ctx);
        // Datagram の properties_data は Properties Length + Properties の生バイト列
        // (SubgroupObject / FetchStreamObject と同じ規約)
        let prop_len = noprop::sample_usize_in(ctx, 1..64);
        let prop_payload = noprop::sample_bytes_vec(ctx, prop_len);
        let prop_blob = length_prefixed_properties(&prop_payload);
        let payload_len = noprop::sample_usize_in(ctx, 1..64);
        let payload = noprop::sample_bytes_vec(ctx, payload_len);
        let datagram = ObjectDatagram {
            track_alias,
            group_id,
            object_id,
            publisher_priority,
            properties_data: Some(prop_blob),
            end_of_group,
            status: None,
        };
        let mut buf = datagram
            .encode()
            .expect("正当なテスト入力の encode は成功する");
        let header_len = buf.len();
        buf.extend_from_slice(&payload);
        let (decoded, consumed) =
            ObjectDatagram::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, header_len);
        assert_eq!(&buf[consumed..], &payload[..]);
        assert_eq!(decoded, datagram);
        Ok(())
    })?;
    Ok(())
}
