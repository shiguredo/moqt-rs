//! `SubgroupStreamDecoder` / `FetchStreamDecoder` のインクリメンタルデコード不変式の PBT
//!
//! `MessageDecoder` と同じく、QUIC / WebTransport のフレーム境界とオブジェクト境界は
//! 一致しない。受信バッファへの蓄積と分割デコードで結果が変わってはならない
//! (draft-ietf-moq-transport-22 §11.3.1 (Subgroup Header) / §11.4.1 (Fetch Header))。

use pbt::common::{sample_varint, test_runner};
use shiguredo_moqt::stream::decoder::{
    DecodedFetchEntry, DecodedSubgroupObject, FetchStreamDecoder, SubgroupStreamDecoder,
};
use shiguredo_moqt::stream::fetch::{
    FetchHeader, FetchPriorContext, FetchStreamEntry, FetchStreamObject, FetchSubgroupIdMode,
};
use shiguredo_moqt::stream::subgroup::{SubgroupHeader, SubgroupIdMode, SubgroupObject};

use crate::length_prefixed_properties;

/// Subgroup のワイヤバイト列を valid-by-construction で生成する
fn sample_subgroup_stream(ctx: &mut noprop::TestCaseContext) -> Vec<u8> {
    let has_properties = noprop::sample_bool(ctx);
    let subgroup_id = match noprop::sample_usize_in(ctx, 0..2) {
        0 => SubgroupIdMode::Zero,
        _ => SubgroupIdMode::Explicit(sample_varint(ctx)),
    };
    let header = SubgroupHeader {
        track_alias: sample_varint(ctx),
        group_id: sample_varint(ctx),
        subgroup_id,
        publisher_priority: Some(noprop::sample_u8(ctx)),
        has_properties,
        end_of_group: false,
        first_object: false,
    };
    let mut bytes = header.encode();

    let object_count = noprop::sample_usize_in(ctx, 1..=4);
    for _ in 0..object_count {
        // object_id_delta は小さい値に抑えて累積 overflow を避ける
        let object_id_delta = noprop::sample_u64_in(ctx, 0..=1000);
        // properties は Normal status 以外には付けられないため、properties ありのときは
        // ペイロードあり (status = None) に固定する
        let (payload_length, status) = if has_properties || noprop::sample_bool(ctx) {
            (noprop::sample_u64_in(ctx, 1..=16), None)
        } else {
            (0, Some(noprop::sample_choice(ctx, &[0u64, 3, 4])))
        };
        let properties_data = if has_properties {
            Some(length_prefixed_properties(&noprop::sample_bytes_vec(
                ctx, 0,
            )))
        } else {
            None
        };
        let object = SubgroupObject {
            object_id_delta,
            payload_length,
            status,
        };
        object
            .encode(has_properties, properties_data.as_deref(), &mut bytes)
            .expect("valid-by-construction な SubgroupObject はエンコードできる");
        let payload = noprop::sample_bytes_vec(ctx, payload_length as usize);
        bytes.extend_from_slice(&payload);
    }
    bytes
}

/// Subgroup バイト列を chunks で与えて、デコード済みオブジェクトとペイロードを列挙する
fn drive_subgroup(
    chunks: &[&[u8]],
) -> Result<Vec<(DecodedSubgroupObject, Vec<u8>)>, shiguredo_moqt::error::MessageError> {
    let mut decoder = SubgroupStreamDecoder::new();
    let mut header_decoded = false;
    let mut pending: Option<DecodedSubgroupObject> = None;
    let mut out = Vec::new();
    for chunk in chunks {
        decoder.push(chunk);
        if !header_decoded {
            match decoder.try_decode_header()? {
                Some(_) => header_decoded = true,
                None => continue,
            }
        }
        loop {
            if let Some(object) = pending.take() {
                match decoder.try_read_payload() {
                    Some(payload) => out.push((object, payload)),
                    None => {
                        pending = Some(object);
                        break;
                    }
                }
            }
            match decoder.try_decode_object()? {
                Some(object) => {
                    if object.payload_length > 0 {
                        match decoder.try_read_payload() {
                            Some(payload) => out.push((object, payload)),
                            None => {
                                pending = Some(object);
                                break;
                            }
                        }
                    } else {
                        out.push((object, Vec::new()));
                    }
                }
                None => break,
            }
        }
    }
    Ok(out)
}

/// 任意の 1 分割で push しても Subgroup のデコード結果が一括 push と一致する
#[test]
fn subgroup_decoder_split_invariance() -> noprop::TestResult {
    let decoded_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let bytes = sample_subgroup_stream(ctx);
        let at = noprop::sample_usize_in(ctx, 0..=bytes.len());
        let whole = drive_subgroup(&[&bytes]);
        let split = drive_subgroup(&[&bytes[..at], &bytes[at..]]);
        assert_eq!(whole, split, "分割してもデコード結果が一致すること");
        if whole.as_ref().is_ok_and(|objects| !objects.is_empty()) {
            decoded_seen.set(true);
        }
        Ok(())
    })?;
    assert!(
        decoded_seen.get(),
        "オブジェクトを 1 つ以上デコードするケースが観測されなかった\n{runner}"
    );
    Ok(())
}

/// Fetch のワイヤバイト列を valid-by-construction で生成する
fn sample_fetch_stream(ctx: &mut noprop::TestCaseContext) -> Vec<u8> {
    let mut bytes = FetchHeader {
        request_id: sample_varint(ctx),
    }
    .encode();

    let object_count = noprop::sample_usize_in(ctx, 1..=4);
    for index in 0..object_count {
        let prior_context = if index == 0 {
            FetchPriorContext::First
        } else {
            FetchPriorContext::HasPriorObject
        };
        // First 文脈は prior-relative な subgroup mode を使えない
        let subgroup_id = if index == 0 {
            match noprop::sample_usize_in(ctx, 0..2) {
                0 => FetchSubgroupIdMode::Zero,
                _ => FetchSubgroupIdMode::Explicit(sample_varint(ctx)),
            }
        } else {
            match noprop::sample_usize_in(ctx, 0..4) {
                0 => FetchSubgroupIdMode::Zero,
                1 => FetchSubgroupIdMode::PreviousSame,
                2 => FetchSubgroupIdMode::PreviousPlusOne,
                _ => FetchSubgroupIdMode::Explicit(sample_varint(ctx)),
            }
        };
        let object = FetchStreamObject {
            group_id: if index == 0 || noprop::sample_bool(ctx) {
                Some(sample_varint(ctx))
            } else {
                None
            },
            subgroup_id,
            object_id: if index == 0 || noprop::sample_bool(ctx) {
                Some(sample_varint(ctx))
            } else {
                None
            },
            publisher_priority: if index == 0 || noprop::sample_bool(ctx) {
                Some(noprop::sample_u8(ctx))
            } else {
                None
            },
            has_properties: false,
            is_datagram_origin: false,
            payload_length: noprop::sample_u64_in(ctx, 0..=16),
        };
        let payload_length = object.payload_length;
        FetchStreamEntry::Object(object)
            .encode(None, prior_context, &mut bytes)
            .expect("valid-by-construction な FetchStreamObject はエンコードできる");
        let payload = noprop::sample_bytes_vec(ctx, payload_length as usize);
        bytes.extend_from_slice(&payload);
    }
    bytes
}

/// Fetch バイト列を chunks で与えて、デコード済みエントリとペイロードを列挙する (昇順)
fn drive_fetch(
    chunks: &[&[u8]],
) -> Result<Vec<(DecodedFetchEntry, Vec<u8>)>, shiguredo_moqt::error::MessageError> {
    drive_fetch_with_group_order(chunks, 0x01)
}

/// Group Order を指定して Fetch バイト列をデコードする
///
/// `0x01` は昇順、`0x02` は降順 (draft-ietf-moq-transport-22 §9.20.8 (GROUP ORDER Parameter))。
/// エンコーダと同じ Group Order を渡さないとデルタ解決が一致しない。
pub(crate) fn drive_fetch_with_group_order(
    chunks: &[&[u8]],
    group_order: u8,
) -> Result<Vec<(DecodedFetchEntry, Vec<u8>)>, shiguredo_moqt::error::MessageError> {
    let mut decoder = FetchStreamDecoder::new_with_group_order(group_order)
        .expect("0x01 / 0x02 は有効な Group Order");
    let mut header_decoded = false;
    let mut pending: Option<DecodedFetchEntry> = None;
    let mut out = Vec::new();
    for chunk in chunks {
        decoder.push(chunk);
        if !header_decoded {
            match decoder.try_decode_header()? {
                Some(_) => header_decoded = true,
                None => continue,
            }
        }
        loop {
            if let Some(entry) = pending.take() {
                match decoder.try_read_payload() {
                    Some(payload) => out.push((entry, payload)),
                    None => {
                        pending = Some(entry);
                        break;
                    }
                }
            }
            match decoder.try_decode_entry()? {
                Some(entry) => {
                    let payload_length = match &entry {
                        DecodedFetchEntry::Object(object) => object.payload_length,
                        DecodedFetchEntry::EndOfNonExistentRange { .. }
                        | DecodedFetchEntry::EndOfUnknownRange { .. }
                        | DecodedFetchEntry::EndOfTimedOutRange { .. }
                        | DecodedFetchEntry::EndOfGoneRange { .. }
                        | DecodedFetchEntry::EndOfGroup { .. }
                        | DecodedFetchEntry::EndOfTrack { .. } => 0,
                    };
                    if payload_length > 0 {
                        match decoder.try_read_payload() {
                            Some(payload) => out.push((entry, payload)),
                            None => {
                                pending = Some(entry);
                                break;
                            }
                        }
                    } else {
                        out.push((entry, Vec::new()));
                    }
                }
                None => break,
            }
        }
    }
    Ok(out)
}

/// 任意の 1 分割で push しても Fetch のデコード結果が一括 push と一致する
#[test]
fn fetch_decoder_split_invariance() -> noprop::TestResult {
    let decoded_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let bytes = sample_fetch_stream(ctx);
        let at = noprop::sample_usize_in(ctx, 0..=bytes.len());
        let whole = drive_fetch(&[&bytes]);
        let split = drive_fetch(&[&bytes[..at], &bytes[at..]]);
        assert_eq!(whole, split, "分割してもデコード結果が一致すること");
        if whole.as_ref().is_ok_and(|entries| !entries.is_empty()) {
            decoded_seen.set(true);
        }
        Ok(())
    })?;
    assert!(
        decoded_seen.get(),
        "オブジェクトを 1 つ以上デコードするケースが観測されなかった\n{runner}"
    );
    Ok(())
}

/// marker (End of Group / End of Track) を含む Fetch のワイヤバイト列を valid-by-construction で
/// 生成する
///
/// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields): marker は直前の Object が
/// Group / Track の最終 Object であることを示し、ID フィールドを持たない。marker の後に
/// Object を置く場合は新しい Group に移り、Group ID Delta と絶対 Object ID を明示する
/// (§11.4.1.1 (Flags))。marker の後に End of Track を置いた場合はそこで生成を終える。
fn sample_fetch_stream_with_markers(ctx: &mut noprop::TestCaseContext) -> Vec<u8> {
    let mut bytes = FetchHeader {
        request_id: sample_varint(ctx),
    }
    .encode();
    let object_count = noprop::sample_usize_in(ctx, 1..=4);
    let mut group_id = noprop::sample_u64_in(ctx, 0..=1_000_000);
    let mut object_id = noprop::sample_u64_in(ctx, 0..=1_000_000);
    let mut prior_group_id = group_id;
    for index in 0..object_count {
        // 最初の Object は Group ID / Object ID が絶対値、以降は直前 Group からのデルタになる
        let group_id_field = if index == 0 {
            group_id
        } else {
            group_id - prior_group_id - 1
        };
        let payload_length = noprop::sample_u64_in(ctx, 0..=16);
        let object = FetchStreamObject {
            group_id: Some(group_id_field),
            subgroup_id: FetchSubgroupIdMode::Zero,
            object_id: Some(object_id),
            publisher_priority: Some(128),
            has_properties: false,
            is_datagram_origin: false,
            payload_length,
        };
        let prior_context = if index == 0 {
            FetchPriorContext::First
        } else {
            FetchPriorContext::HasPriorObject
        };
        FetchStreamEntry::Object(object)
            .encode(None, prior_context, &mut bytes)
            .expect("valid-by-construction な FetchStreamObject はエンコードできる");
        bytes.extend_from_slice(&noprop::sample_bytes_vec(ctx, payload_length as usize));
        prior_group_id = group_id;

        if index + 1 >= object_count {
            // 最後の Object の後にも marker を置ける
            match noprop::sample_usize_in(ctx, 0..3) {
                0 => FetchStreamEntry::EndOfGroup
                    .encode(None, FetchPriorContext::HasPriorObject, &mut bytes)
                    .expect("marker はエンコードできる"),
                1 => {
                    FetchStreamEntry::EndOfTrack
                        .encode(None, FetchPriorContext::HasPriorObject, &mut bytes)
                        .expect("marker はエンコードできる");
                    return bytes;
                }
                _ => {}
            }
            break;
        }

        match noprop::sample_usize_in(ctx, 0..3) {
            0 => FetchStreamEntry::EndOfGroup
                .encode(None, FetchPriorContext::HasPriorObject, &mut bytes)
                .expect("marker はエンコードできる"),
            1 => {
                FetchStreamEntry::EndOfTrack
                    .encode(None, FetchPriorContext::HasPriorObject, &mut bytes)
                    .expect("marker はエンコードできる");
                return bytes;
            }
            _ => {}
        }

        // 次の Object は新しい Group に置く (marker の有無によらず Group ID Delta が present)
        group_id += 1 + noprop::sample_u64_in(ctx, 0..=1000);
        object_id = noprop::sample_u64_in(ctx, 0..=1_000_000);
    }
    bytes
}

/// marker を含む Fetch バイト列でも、任意の 1 分割で push した結果が一括 push と一致する
///
/// marker は後続 ID フィールドを持たないため、vi64 の途中で分割されるとデータ不足になり、
/// 2 バイト目が届いた時点で確定する。分割位置によって entry の解釈が変わらないことを固定する。
#[test]
fn fetch_decoder_split_invariance_with_markers() -> noprop::TestResult {
    let decoded_seen = std::cell::Cell::new(false);
    let group_end_seen = std::cell::Cell::new(false);
    let track_end_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let bytes = sample_fetch_stream_with_markers(ctx);
        let at = noprop::sample_usize_in(ctx, 0..=bytes.len());
        let whole = drive_fetch(&[&bytes]);
        let split = drive_fetch(&[&bytes[..at], &bytes[at..]]);
        assert_eq!(whole, split, "分割してもデコード結果が一致すること");
        if let Ok(entries) = &whole {
            for (entry, _) in entries {
                match entry {
                    DecodedFetchEntry::EndOfGroup { .. } => group_end_seen.set(true),
                    DecodedFetchEntry::EndOfTrack { .. } => track_end_seen.set(true),
                    _ => {}
                }
            }
            if !entries.is_empty() {
                decoded_seen.set(true);
            }
        }
        Ok(())
    })?;
    assert!(
        decoded_seen.get(),
        "エントリを 1 つ以上デコードするケースが観測されなかった\n{runner}"
    );
    assert!(
        group_end_seen.get(),
        "End of Group を含むケースが生成されなかった\n{runner}"
    );
    assert!(
        track_end_seen.get(),
        "End of Track を含むケースが生成されなかった\n{runner}"
    );
    Ok(())
}
