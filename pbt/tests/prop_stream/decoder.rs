//! `SubgroupStreamDecoder` / `FetchStreamDecoder` のインクリメンタルデコード不変式の PBT
//!
//! `MessageDecoder` と同じく、QUIC / WebTransport のフレーム境界とオブジェクト境界は
//! 一致しない。受信バッファへの蓄積と分割デコードで結果が変わってはならない
//! (draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header) / §11.4.1 (Fetch Header))。

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

/// Fetch バイト列を chunks で与えて、デコード済みエントリとペイロードを列挙する
pub(crate) fn drive_fetch(
    chunks: &[&[u8]],
) -> Result<Vec<(DecodedFetchEntry, Vec<u8>)>, shiguredo_moqt::error::MessageError> {
    let mut decoder = FetchStreamDecoder::new();
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
                        | DecodedFetchEntry::EndOfTimedOutRange { .. } => 0,
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
