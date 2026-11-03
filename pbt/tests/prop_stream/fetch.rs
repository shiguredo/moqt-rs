//! FETCH_HEADER / FETCH_STREAM_OBJECT (draft-ietf-moq-transport-22 §11.4.1 (Fetch Header)) のラウンドトリップ PBT
//!
//! `FetchStreamObject` / `FetchStreamEntry` の encode/decode は prior 参照文脈
//! (`FetchPriorContext`) を要求し、文脈によって有効なフィールドが変わる。単一エントリの
//! ラウンドトリップは、その文脈で妥当なエントリを生成すれば成立する。3 つの文脈
//! (First / NoPriorActualObject / HasPriorObject) それぞれを検証する。

use pbt::common::test_runner;
use shiguredo_moqt::stream::fetch::{
    FetchHeader, FetchPriorContext, FetchStreamEntry, FetchStreamObject, FetchSubgroupIdMode,
};

use crate::length_prefixed_properties;
use pbt::common::sample_varint;

/// FetchSubgroupIdMode の生成 (Zero / PreviousSame / PreviousPlusOne / Explicit)
fn sample_fetch_subgroup_id_mode(ctx: &mut noprop::TestCaseContext) -> FetchSubgroupIdMode {
    match noprop::sample_weighted_index(ctx, &[1, 1, 1, 1]) {
        0 => FetchSubgroupIdMode::Zero,
        1 => FetchSubgroupIdMode::PreviousSame,
        2 => FetchSubgroupIdMode::PreviousPlusOne,
        _ => FetchSubgroupIdMode::Explicit(sample_varint(ctx)),
    }
}

/// prior 参照を持たない (First / NoPriorActualObject) 文脈で許される subgroup mode の生成。
/// prior-relative な PreviousSame / PreviousPlusOne は禁止 (draft-ietf-moq-transport-23 §11.4.1.1 (Flags) / §11.4.2.2 (Prior Object Fields))。
fn sample_fetch_non_prior_subgroup_id_mode(
    ctx: &mut noprop::TestCaseContext,
) -> FetchSubgroupIdMode {
    match noprop::sample_weighted_index(ctx, &[1, 1]) {
        0 => FetchSubgroupIdMode::Zero,
        _ => FetchSubgroupIdMode::Explicit(sample_varint(ctx)),
    }
}

/// has_properties に応じた properties 生バイト列 (Length 込み) を生成する。
/// データ本体は 0..64 バイト。None のとき properties なし。
fn sample_fetch_properties(ctx: &mut noprop::TestCaseContext) -> Option<Vec<u8>> {
    if noprop::sample_bool(ctx) {
        let len = noprop::sample_usize_in(ctx, 0..64);
        Some(length_prefixed_properties(&noprop::sample_bytes_vec(
            ctx, len,
        )))
    } else {
        None
    }
}

/// HasPriorObject 文脈で妥当な `FetchStreamObject` と、それに対応する properties 生バイト列を
/// 生成する。HasPriorObject では prior 参照に制約がないため全フィールドを任意に取れるが、
/// Datagram 起源のオブジェクトは Subgroup ID を持てない制約だけ満たす (draft-ietf-moq-transport-23 §11.4.1.1 (Flags))。
fn sample_fetch_object_has_prior(
    ctx: &mut noprop::TestCaseContext,
) -> (FetchStreamObject, Option<Vec<u8>>) {
    let group_id = if noprop::sample_bool(ctx) {
        Some(sample_varint(ctx))
    } else {
        None
    }; // group_id (None = 前のオブジェクトを継承)
    let subgroup_id = sample_fetch_subgroup_id_mode(ctx);
    let object_id = if noprop::sample_bool(ctx) {
        Some(sample_varint(ctx))
    } else {
        None
    }; // object_id (None = 前 + 1)
    let publisher_priority = if noprop::sample_bool(ctx) {
        Some(noprop::sample_u8(ctx))
    } else {
        None
    }; // publisher_priority (None = 前を継承)
    let is_datagram_origin = noprop::sample_bool(ctx);
    let payload_length = sample_varint(ctx);
    let properties_data = sample_fetch_properties(ctx);

    // Datagram 起源のオブジェクトは Subgroup ID を持てない (Zero 固定)。
    let subgroup_id = if is_datagram_origin {
        FetchSubgroupIdMode::Zero
    } else {
        subgroup_id
    };
    let obj = FetchStreamObject {
        group_id,
        subgroup_id,
        object_id,
        publisher_priority,
        has_properties: properties_data.is_some(),
        is_datagram_origin,
        payload_length,
    };
    (obj, properties_data)
}

/// NoPriorActualObject 文脈で妥当な `FetchStreamObject` と properties 生バイト列を生成する。
/// この文脈では Publisher Priority が明示必須で prior-relative subgroup mode は禁止だが、
/// Group ID / Object ID は prior 参照 (None) が許される (draft-ietf-moq-transport-23 §11.4.2 (End of Range))。
/// First 文脈との差分 (Group/Object を None にできる点) を検証する。
fn sample_fetch_object_no_prior_actual(
    ctx: &mut noprop::TestCaseContext,
) -> (FetchStreamObject, Option<Vec<u8>>) {
    let group_id = if noprop::sample_bool(ctx) {
        Some(sample_varint(ctx))
    } else {
        None
    }; // group_id (None = prior 参照 = 許容)
    let subgroup_id = sample_fetch_non_prior_subgroup_id_mode(ctx); // prior-relative 禁止
    let object_id = if noprop::sample_bool(ctx) {
        Some(sample_varint(ctx))
    } else {
        None
    }; // object_id (None = prior + 1 = 許容)
    let publisher_priority = noprop::sample_u8(ctx); // 明示必須
    let is_datagram_origin = noprop::sample_bool(ctx);
    let payload_length = sample_varint(ctx);
    let properties_data = sample_fetch_properties(ctx);

    // Datagram 起源のオブジェクトは Subgroup ID を持てない (Zero 固定)。
    let subgroup_id = if is_datagram_origin {
        FetchSubgroupIdMode::Zero
    } else {
        subgroup_id
    };
    let obj = FetchStreamObject {
        group_id,
        subgroup_id,
        object_id,
        publisher_priority: Some(publisher_priority),
        has_properties: properties_data.is_some(),
        is_datagram_origin,
        payload_length,
    };
    (obj, properties_data)
}

/// FETCH_HEADER の encode → decode ラウンドトリップ。
#[test]
fn fetch_header_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let request_id = sample_varint(ctx);
        let header = FetchHeader { request_id };
        let encoded = header.encode();
        let (decoded, consumed) =
            FetchHeader::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, header);
        Ok(())
    })?;
    Ok(())
}

/// HasPriorObject 文脈での FETCH_STREAM_OBJECT ラウンドトリップ。
/// この文脈では prior 参照に制約がないため、全フィールドの組み合わせを検証できる。
#[test]
fn fetch_stream_object_roundtrip_has_prior() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let (obj, properties_data) = sample_fetch_object_has_prior(ctx);
        let mut buf = Vec::new();
        obj.encode(
            properties_data.as_deref(),
            FetchPriorContext::HasPriorObject,
            &mut buf,
        )
        .expect("テストフィクスチャの前提条件を満たす");
        let (entry, consumed) = FetchStreamEntry::decode(&buf, FetchPriorContext::HasPriorObject)
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, buf.len());
        assert_eq!(entry, FetchStreamEntry::Object(obj));
        Ok(())
    })?;
    Ok(())
}

/// NoPriorActualObject 文脈での FETCH_STREAM_OBJECT ラウンドトリップ。
/// Group ID / Object ID は prior 参照 (None) が許されるが、Publisher Priority は必須で
/// prior-relative subgroup mode は禁止される (draft-ietf-moq-transport-23 §11.4.2 (End of Range))。
#[test]
fn fetch_stream_object_roundtrip_no_prior_actual() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let (obj, properties_data) = sample_fetch_object_no_prior_actual(ctx);
        let mut buf = Vec::new();
        obj.encode(
            properties_data.as_deref(),
            FetchPriorContext::NoPriorActualObject,
            &mut buf,
        )
        .expect("テストフィクスチャの前提条件を満たす");
        let (entry, consumed) =
            FetchStreamEntry::decode(&buf, FetchPriorContext::NoPriorActualObject)
                .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, buf.len());
        assert_eq!(entry, FetchStreamEntry::Object(obj));
        Ok(())
    })?;
    Ok(())
}

/// First 文脈での FETCH_STREAM_OBJECT ラウンドトリップ。
/// First では group_id / object_id / publisher_priority が明示必須で、prior-relative な
/// subgroup mode (PreviousSame / PreviousPlusOne) は禁止される (draft-ietf-moq-transport-23 §11.4.1.1 (Flags))。
#[test]
fn fetch_stream_object_roundtrip_first() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let group_id = sample_varint(ctx);
        let object_id = sample_varint(ctx);
        let publisher_priority = noprop::sample_u8(ctx);
        let subgroup_id = sample_fetch_non_prior_subgroup_id_mode(ctx);
        let payload_length = sample_varint(ctx);
        let obj = FetchStreamObject {
            group_id: Some(group_id),
            subgroup_id,
            object_id: Some(object_id),
            publisher_priority: Some(publisher_priority),
            has_properties: false,
            is_datagram_origin: false,
            payload_length,
        };
        let mut buf = Vec::new();
        obj.encode(None, FetchPriorContext::First, &mut buf)
            .expect("正当なテスト入力の encode は成功する");
        let (entry, consumed) = FetchStreamEntry::decode(&buf, FetchPriorContext::First)
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, buf.len());
        assert_eq!(entry, FetchStreamEntry::Object(obj));
        Ok(())
    })?;
    Ok(())
}

/// End of Range エントリ (EndOfNonExistentRange / EndOfUnknownRange / EndOfTimedOutRange) のラウンドトリップ。
/// これらは特殊な Serialization Flags で識別され、低水準 codec は Group ID Delta /
/// Object ID Delta を生値のまま扱う (draft-ietf-moq-transport-23 §11.4.2 (End of Range) /
/// §11.4.1 Table 8)。生値の意味は prior 参照文脈で変わる (最初の entry は絶対 ID、
/// それ以外は直前 Location からのデルタ) が、低水準 codec の往復はその解釈に依存しない
/// ことを複数の文脈で確認する (§11.4.2.1 (ID Decoding))。
#[test]
fn fetch_stream_entry_end_of_range_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let kind = noprop::sample_usize_in(ctx, 0..3);
        let group_id = sample_varint(ctx);
        let object_id = sample_varint(ctx);
        let ctx_idx = noprop::sample_usize_in(ctx, 0..3);
        let entry = match kind {
            0 => FetchStreamEntry::EndOfNonExistentRange {
                group_id_delta: group_id,
                object_id_delta: object_id,
            },
            1 => FetchStreamEntry::EndOfUnknownRange {
                group_id_delta: group_id,
                object_id_delta: object_id,
            },
            // kind は 0..3 のため 2 のみがここに到達する
            _ => FetchStreamEntry::EndOfTimedOutRange {
                group_id_delta: group_id,
                object_id_delta: object_id,
            },
        };
        let ctx = match ctx_idx {
            0 => FetchPriorContext::First,
            1 => FetchPriorContext::NoPriorActualObject,
            _ => FetchPriorContext::HasPriorObject,
        };
        let mut buf = Vec::new();
        entry
            .encode(None, ctx, &mut buf)
            .expect("正当なテスト入力の encode は成功する");
        let (decoded, consumed) =
            FetchStreamEntry::decode(&buf, ctx).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, buf.len());
        assert_eq!(decoded, entry);
        Ok(())
    })?;
    Ok(())
}

/// prior actual Object が無い文脈で prior 参照を持つ Object は MALFORMED_OBJECT で拒否される
///
/// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields): prior Subgroup ID / Priority の
/// 参照は先行する実 Object がない文脈 (First / NoPriorActualObject) では禁止。
/// HasPriorObject 文脈で正当にエンコードしたバイト列を prior なし文脈でデコードし、
/// MALFORMED_OBJECT で拒否されることを検証する。End of Range だけが先行する文脈でも
/// prior Subgroup ID / Priority が存在しないものとして扱われることの回帰防御になる。
#[test]
fn prior_reference_object_rejected_without_prior_actual_object() -> noprop::TestResult {
    use shiguredo_moqt::error::MessageError;
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        // prior 参照の 4 通り (PreviousSame / PreviousPlusOne × Priority 有無)
        let subgroup_id = match noprop::sample_usize_in(ctx, 0..2) {
            0 => FetchSubgroupIdMode::PreviousSame,
            _ => FetchSubgroupIdMode::PreviousPlusOne,
        };
        let publisher_priority = if noprop::sample_bool(ctx) {
            Some(noprop::sample_u8(ctx))
        } else {
            None
        };
        let obj = FetchStreamObject {
            group_id: Some(sample_varint(ctx)),
            subgroup_id,
            object_id: Some(sample_varint(ctx)),
            publisher_priority,
            has_properties: false,
            is_datagram_origin: false,
            payload_length: sample_varint(ctx),
        };
        let entry = FetchStreamEntry::Object(obj);
        let mut buf = Vec::new();
        entry
            .encode(None, FetchPriorContext::HasPriorObject, &mut buf)
            .expect("prior あり文脈の encode は成功する");
        for context in [
            FetchPriorContext::First,
            FetchPriorContext::NoPriorActualObject,
        ] {
            let err = FetchStreamEntry::decode(&buf, context)
                .expect_err("prior なし文脈の prior 参照は拒否されること");
            assert!(
                matches!(err, MessageError::MalformedObject(_)),
                "MALFORMED_OBJECT で拒否されること: {err:?}"
            );
        }
        Ok(())
    })?;
    Ok(())
}

/// End of Group / End of Track (0x80 / 0x81) のラウンドトリップ
///
/// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields):
/// "When Serialization Flags is 0x80 (End of Group) or 0x81 (End of Track), no other fields
/// are present." marker は prior Object を参照するフィールドを持たないため、prior 文脈に
/// よらず同じ 1 個の vi64 になり、消費バイト数も 2 バイト (0x80 / 0x81 の vi64) で一定である。
#[test]
fn fetch_stream_marker_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let entry = if noprop::sample_bool(ctx) {
            FetchStreamEntry::EndOfGroup
        } else {
            FetchStreamEntry::EndOfTrack
        };
        for context in [
            FetchPriorContext::First,
            FetchPriorContext::NoPriorActualObject,
            FetchPriorContext::HasPriorObject,
        ] {
            let mut buf = Vec::new();
            entry
                .encode(None, context, &mut buf)
                .expect("marker は prior 文脈によらずエンコードできる");
            assert_eq!(
                buf.len(),
                2,
                "marker は 1 個の vi64 (2 バイト) だけを書き出すこと: {entry:?}"
            );
            let (decoded, consumed) = FetchStreamEntry::decode(&buf, context)
                .expect("marker は prior 文脈によらずデコードできる");
            assert_eq!(
                consumed,
                buf.len(),
                "marker は後続 ID フィールドを読まないこと: {entry:?}"
            );
            assert_eq!(decoded, entry, "marker が往復すること: {context:?}");
        }
        Ok(())
    })?;
    Ok(())
}

/// marker に properties_data を渡すと prior 文脈によらず拒否されること
///
/// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields):
/// "no other fields are present"。
#[test]
fn fetch_stream_marker_rejects_properties() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(64, |ctx| {
        let entry = if noprop::sample_bool(ctx) {
            FetchStreamEntry::EndOfGroup
        } else {
            FetchStreamEntry::EndOfTrack
        };
        let props = length_prefixed_properties(&noprop::sample_bytes_vec(ctx, 0));
        let mut buf = Vec::new();
        let err = entry
            .encode(Some(&props), FetchPriorContext::HasPriorObject, &mut buf)
            .expect_err("marker に properties_data は渡せない");
        assert!(
            matches!(
                err,
                shiguredo_moqt::error::MessageError::ProtocolViolation(_)
            ),
            "ProtocolViolation で拒否されること: {err:?}"
        );
        assert!(buf.is_empty(), "拒否した場合は書き込みを行わないこと");
        Ok(())
    })?;
    Ok(())
}
