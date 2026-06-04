//! SUBGROUP_HEADER / SUBGROUP_OBJECT (draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header)) のラウンドトリップ PBT

use pbt::common::test_runner;
use shiguredo_moqt::{
    stream::subgroup::SubgroupHeader, stream::subgroup::SubgroupIdMode,
    stream::subgroup::SubgroupObject,
};

use crate::length_prefixed_properties;
use pbt::common::sample_varint;

/// SubgroupIdMode の生成 (Zero / FirstObjectId / Explicit(任意 varint))
fn sample_subgroup_id_mode(ctx: &mut noprop::TestCaseContext) -> SubgroupIdMode {
    match noprop::sample_weighted_index(ctx, &[1, 1, 1]) {
        0 => SubgroupIdMode::Zero,
        1 => SubgroupIdMode::FirstObjectId,
        _ => SubgroupIdMode::Explicit(sample_varint(ctx)),
    }
}

/// SubgroupHeader の生成。全フィールドを任意に生成する。
fn sample_subgroup_header(ctx: &mut noprop::TestCaseContext) -> SubgroupHeader {
    SubgroupHeader {
        track_alias: sample_varint(ctx),
        group_id: sample_varint(ctx),
        subgroup_id: sample_subgroup_id_mode(ctx),
        publisher_priority: if noprop::sample_bool(ctx) {
            Some(noprop::sample_u8(ctx))
        } else {
            None
        }, // publisher_priority (None = DEFAULT_PRIORITY)
        has_properties: noprop::sample_bool(ctx),
        end_of_group: noprop::sample_bool(ctx),
        first_object: noprop::sample_bool(ctx),
    }
}

/// properties 付き SUBGROUP_OBJECT で有効な (payload_length, status) の組を生成する。
///
/// properties は Normal(0x0) 以外の status には付けられない (draft-ietf-moq-transport-21 §11.1.3 (Object Properties)) ため、
/// 「ペイロードあり (status = None)」または「zero-length Normal status (payload_length = 0,
/// status = Some(0x0))」の 2 通りのみが有効。
fn sample_subgroup_payload_status_with_properties(
    ctx: &mut noprop::TestCaseContext,
) -> (u64, Option<u64>) {
    if noprop::sample_bool(ctx) {
        // ペイロードあり: payload_length > 0、status は None
        (sample_varint(ctx).max(1), None)
    } else {
        // zero-length Normal status (draft-ietf-moq-transport-21 §11.1.2 (Object Status))
        (0u64, Some(0u64))
    }
}

/// SUBGROUP_HEADER の encode → decode ラウンドトリップ。
#[test]
fn subgroup_header_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let header = sample_subgroup_header(ctx);
        let encoded = header.encode();
        let (decoded, consumed) =
            SubgroupHeader::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        // ヘッダ全体が消費される (余剰バイトなし)。
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, header);
        Ok(())
    })?;
    Ok(())
}

/// SUBGROUP_OBJECT (ペイロードあり・ properties なし) のラウンドトリップ。
/// payload_length > 0 のとき status は None でなければならない (draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header))。
#[test]
fn subgroup_object_roundtrip_with_payload() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let object_id_delta = sample_varint(ctx);
        let payload_length = sample_varint(ctx).max(1);
        let obj = SubgroupObject {
            object_id_delta,
            payload_length,
            status: None,
        };
        let mut buf = Vec::new();
        obj.encode(false, None, &mut buf)
            .expect("正当なテスト入力の encode は成功する");
        let (decoded, props, consumed) =
            SubgroupObject::decode(&buf, false).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, buf.len());
        assert_eq!(props, None);
        assert_eq!(decoded, obj);
        Ok(())
    })?;
    Ok(())
}

/// SUBGROUP_OBJECT (status オブジェクト・ properties なし) のラウンドトリップ。
/// payload_length == 0 のとき status は必須。値域は Normal(0x0) / EndOfGroup(0x3) /
/// EndOfTrack(0x4) のみ (draft-ietf-moq-transport-21 §11.1.2 (Object Status))。
#[test]
fn subgroup_object_roundtrip_status_object() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let object_id_delta = sample_varint(ctx);
        let status = noprop::sample_choice(ctx, &[0u64, 3u64, 4u64]);
        let obj = SubgroupObject {
            object_id_delta,
            payload_length: 0,
            status: Some(status),
        };
        let mut buf = Vec::new();
        obj.encode(false, None, &mut buf)
            .expect("正当なテスト入力の encode は成功する");
        let (decoded, props, consumed) =
            SubgroupObject::decode(&buf, false).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, buf.len());
        assert_eq!(props, None);
        assert_eq!(decoded, obj);
        Ok(())
    })?;
    Ok(())
}

/// SUBGROUP_OBJECT (properties あり) のラウンドトリップ。
/// has_properties = true のとき Properties Length + データの生バイト列を渡す。
/// 有効なのは「ペイロードあり (status = None)」と「zero-length Normal status
/// (status = Some(0x0))」の 2 通り (draft-ietf-moq-transport-21 §11.1.3 (Object Properties): Normal 以外には付けられない)。
#[test]
fn subgroup_object_roundtrip_with_properties() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let object_id_delta = sample_varint(ctx);
        let (payload_length, status) = sample_subgroup_payload_status_with_properties(ctx);
        let prop_len = noprop::sample_usize_in(ctx, 0..64);
        let prop_payload = noprop::sample_bytes_vec(ctx, prop_len);
        let properties_data = length_prefixed_properties(&prop_payload);
        let obj = SubgroupObject {
            object_id_delta,
            payload_length,
            status,
        };
        let mut buf = Vec::new();
        obj.encode(true, Some(&properties_data), &mut buf)
            .expect("テストフィクスチャの前提条件を満たす");
        let (decoded, props, consumed) =
            SubgroupObject::decode(&buf, true).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, buf.len());
        // Properties 生バイト列 (Length + データ) もそのまま往復する。
        assert_eq!(props, Some(properties_data));
        assert_eq!(decoded, obj);
        Ok(())
    })?;
    Ok(())
}
