//! `FetchStreamEncoder` のデルタ圧縮エンコード → `FetchStreamDecoder` デコードの往復 PBT
//!
//! エンコーダは前回オブジェクトとの差分 (Group ID / Object ID / Subgroup ID / Priority) を
//! 内部状態で判断する (draft-ietf-moq-transport-21 §11.4.1.1 (Flags))。任意の昇順オブジェクト列を
//! エンコードしてデコードし直したとき、絶対値の group_id / subgroup_id / object_id /
//! publisher_priority / payload_length が保存されることを検証する。
//! Datagram 起源 (0x40) の Object は Subgroup ID を運ばず 0 に解決されるため、
//! subgroup_id の期待値は通常起源と Datagram 起源で分ける。
//! Properties 付きの Object も混ぜ、delta 圧縮と Properties の組み合わせで
//! 回帰が起きないことを検証する。

use pbt::common::{sample_varint, test_runner};
use shiguredo_moqt::object_properties::{ObjectProperties, ObjectProperty, ObjectPropertyValue};
use shiguredo_moqt::stream::decoder::DecodedFetchEntry;
use shiguredo_moqt::stream::encoder::{FetchObjectInput, FetchStreamEncoder};
use shiguredo_moqt::track_properties::{
    PROP_OBJECT_DELIVERY_TIMEOUT, PROP_SUBGROUP_DELIVERY_TIMEOUT,
};

use crate::decoder::drive_fetch;

/// Subgroup ID のサンプル
///
/// 0 は Table 8 の 0x00 (Subgroup ID is zero) に対応する値であり、Datagram 起源の
/// prior Object の直後に Zero モードを選ぶ分岐を踏むため 1/2 の確率で混ぜる。
fn sample_subgroup_id(ctx: &mut noprop::TestCaseContext) -> u64 {
    if noprop::sample_bool(ctx) {
        0
    } else {
        noprop::sample_u64_in(ctx, 0..=1_000_000)
    }
}

/// Object Properties のサンプル (`has_properties` と Properties 生バイト列) を生成する
///
/// `ObjectProperties::encode` の出力を渡すため、Properties Length varint を含む。
/// トラッカーの意味論的検証に抵触しない型だけを使う。PRIOR_GROUP_ID_GAP (0x3C) と
/// PRIOR_OBJECT_ID_GAP (0x3E) は受信済み object 列との整合が必要なため使わない。
/// 0x02 / 0x06 は object 単体で完結する。
/// 全プロパティを落とした場合は Properties Length = 0 のブロックになり、これは合法である
/// (空スライス `Some(&[])` は Properties Length varint を含まない契約違反入力であり、
/// 正常系サンプラーでは生成しない)。
fn sample_properties(ctx: &mut noprop::TestCaseContext) -> (bool, Vec<u8>) {
    if !noprop::sample_bool(ctx) {
        return (false, Vec::new());
    }
    let mut props = ObjectProperties::new();
    if noprop::sample_bool(ctx) {
        props.push(ObjectProperty {
            prop_type: PROP_OBJECT_DELIVERY_TIMEOUT,
            value: ObjectPropertyValue::VarInt(noprop::sample_u64_in(ctx, 0..=100_000)),
        });
    }
    if noprop::sample_bool(ctx) {
        props.push(ObjectProperty {
            prop_type: PROP_SUBGROUP_DELIVERY_TIMEOUT,
            value: ObjectPropertyValue::VarInt(noprop::sample_u64_in(ctx, 0..=100_000)),
        });
    }
    let mut buf = Vec::new();
    props
        .encode(&mut buf)
        .expect("サンプルする Properties はエンコードできる");
    (true, buf)
}

/// エンコーダの制約 (同一 Group 内の Object ID は狭義増加、Group は昇順、同一 Subgroup 内の
/// Priority 不変) を満たす入力列と、各オブジェクトのペイロードを生成する
fn sample_objects(ctx: &mut noprop::TestCaseContext) -> Vec<(FetchObjectInput, Vec<u8>, Vec<u8>)> {
    let count = noprop::sample_usize_in(ctx, 1..=4);
    // 加算で overflow しないよう小さい初期値から始める
    let mut group_id = noprop::sample_u64_in(ctx, 0..=1_000_000);
    let mut subgroup_id = sample_subgroup_id(ctx);
    let mut object_id = noprop::sample_u64_in(ctx, 0..=1_000_000);
    let mut publisher_priority = noprop::sample_u8(ctx);
    let mut out = Vec::new();
    for index in 0..count {
        if index > 0 {
            if noprop::sample_bool(ctx) {
                // 新しい Group (昇順): 各フィールドを再抽選する
                group_id += 1 + noprop::sample_u64_in(ctx, 0..=1000);
                subgroup_id = sample_subgroup_id(ctx);
                object_id = noprop::sample_u64_in(ctx, 0..=1_000_000);
                publisher_priority = noprop::sample_u8(ctx);
            } else {
                // 同一 Group: Object ID を狭義増加させる
                object_id += 1 + noprop::sample_u64_in(ctx, 0..=1000);
            }
        }
        let payload_length = noprop::sample_u64_in(ctx, 0..=16);
        let payload = noprop::sample_bytes_vec(ctx, payload_length as usize);
        // Datagram 起源 (0x40) を混在させる。Datagram 起源では Subgroup ID は wire に載らない
        let is_datagram_origin = noprop::sample_bool(ctx);
        // Properties 付きの Object を混在させる
        let (has_properties, properties_data) = sample_properties(ctx);
        out.push((
            FetchObjectInput {
                group_id,
                subgroup_id,
                object_id,
                publisher_priority,
                has_properties,
                is_datagram_origin,
                payload_length,
            },
            payload,
            properties_data,
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
        for (input, payload, properties_data) in &objects {
            let properties = if input.has_properties {
                Some(properties_data.as_slice())
            } else {
                None
            };
            encoder
                .encode_object(input, properties, &mut bytes)
                .expect("エンコーダの制約を満たす入力列はエンコードできる");
            bytes.extend_from_slice(payload);
        }

        let decoded = drive_fetch(&[&bytes]).expect("エンコーダ出力はデコードできる");
        assert_eq!(decoded.len(), objects.len());

        for ((entry, payload), (input, expected_payload, _properties_data)) in
            decoded.iter().zip(objects.iter())
        {
            let DecodedFetchEntry::Object(object) = entry else {
                panic!("通常の Object エントリが返ること: {entry:?}");
            };
            assert_eq!(object.group_id, input.group_id);
            // Datagram 起源は Subgroup ID を運ばず 0 に解決される (draft §11.4.1.1)
            let expected_subgroup_id = if input.is_datagram_origin {
                0
            } else {
                input.subgroup_id
            };
            assert_eq!(object.subgroup_id, expected_subgroup_id);
            assert_eq!(object.is_datagram_origin, input.is_datagram_origin);
            assert_eq!(object.object_id, input.object_id);
            assert_eq!(object.publisher_priority, input.publisher_priority);
            assert_eq!(object.payload_length, input.payload_length);
            assert_eq!(payload, expected_payload);
            // `DecodedFetchEntry::Object` は Properties 生バイトを公開しないため、
            // Properties の内容そのものは復元検証しない (デコード時に
            // ObjectProperties::decode を通ることだけが検証される)。
        }
        Ok(())
    })?;
    Ok(())
}
