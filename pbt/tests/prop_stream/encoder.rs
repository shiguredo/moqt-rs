//! `FetchStreamEncoder` のデルタ圧縮エンコード → `FetchStreamDecoder` デコードの往復 PBT
//!
//! エンコーダは前回オブジェクトとの差分 (Group ID / Object ID / Subgroup ID / Priority) を
//! 内部状態で判断する (draft-ietf-moq-transport-21 §11.4.1.1 (Flags))。任意の昇順オブジェクト列を
//! エンコードしてデコードし直したとき、絶対値の group_id / subgroup_id / object_id /
//! publisher_priority / payload_length が保存されることを検証する。

use pbt::common::{sample_varint, test_runner};
use shiguredo_moqt::stream::decoder::DecodedFetchEntry;
use shiguredo_moqt::stream::encoder::{FetchObjectInput, FetchStreamEncoder};

use crate::decoder::drive_fetch;

/// エンコーダの制約 (同一 Group 内の Object ID は狭義増加、Group は昇順、同一 Subgroup 内の
/// Priority 不変) を満たす入力列と、各オブジェクトのペイロードを生成する
fn sample_objects(ctx: &mut noprop::TestCaseContext) -> Vec<(FetchObjectInput, Vec<u8>)> {
    let count = noprop::sample_usize_in(ctx, 1..=4);
    // 加算で overflow しないよう小さい初期値から始める
    let mut group_id = noprop::sample_u64_in(ctx, 0..=1_000_000);
    let mut subgroup_id = noprop::sample_u64_in(ctx, 0..=1_000_000);
    let mut object_id = noprop::sample_u64_in(ctx, 0..=1_000_000);
    let mut publisher_priority = noprop::sample_u8(ctx);
    let mut out = Vec::new();
    for index in 0..count {
        if index > 0 {
            if noprop::sample_bool(ctx) {
                // 新しい Group (昇順): 各フィールドを再抽選する
                group_id += 1 + noprop::sample_u64_in(ctx, 0..=1000);
                subgroup_id = noprop::sample_u64_in(ctx, 0..=1_000_000);
                object_id = noprop::sample_u64_in(ctx, 0..=1_000_000);
                publisher_priority = noprop::sample_u8(ctx);
            } else {
                // 同一 Group: Object ID を狭義増加させる
                object_id += 1 + noprop::sample_u64_in(ctx, 0..=1000);
            }
        }
        let payload_length = noprop::sample_u64_in(ctx, 0..=16);
        let payload = noprop::sample_bytes_vec(ctx, payload_length as usize);
        out.push((
            FetchObjectInput {
                group_id,
                subgroup_id,
                object_id,
                publisher_priority,
                has_properties: false,
                is_datagram_origin: false,
                payload_length,
            },
            payload,
        ));
    }
    out
}

/// エンコードしたオブジェクト列をデコードし直すと絶対値が保存される
#[test]
fn encoder_decoder_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let request_id = sample_varint(ctx);
        let objects = sample_objects(ctx);

        let mut encoder = FetchStreamEncoder::new(request_id);
        let mut bytes = encoder.encode_header();
        for (input, payload) in &objects {
            encoder
                .encode_object(input, None, &mut bytes)
                .expect("エンコーダの制約を満たす入力列はエンコードできる");
            bytes.extend_from_slice(payload);
        }

        let decoded = drive_fetch(&[&bytes]).expect("エンコーダ出力はデコードできる");
        assert_eq!(decoded.len(), objects.len());

        for ((entry, payload), (input, expected_payload)) in decoded.iter().zip(objects.iter()) {
            let DecodedFetchEntry::Object(object) = entry else {
                panic!("通常の Object エントリが返ること: {entry:?}");
            };
            assert_eq!(object.group_id, input.group_id);
            assert_eq!(object.subgroup_id, input.subgroup_id);
            assert_eq!(object.object_id, input.object_id);
            assert_eq!(object.publisher_priority, input.publisher_priority);
            assert_eq!(object.payload_length, input.payload_length);
            assert_eq!(payload, expected_payload);
        }
        Ok(())
    })?;
    Ok(())
}
