//! `FetchStreamEncoder` のデルタ圧縮エンコード → `FetchStreamDecoder` デコードの往復 PBT
//!
//! エンコーダは前回オブジェクトとの差分 (Group ID / Object ID / Subgroup ID / Priority) を
//! 内部状態で判断する (draft-ietf-moq-transport-23 §11.4.1.1 (Flags))。昇順 (0x01) と
//! 降順 (0x02) の両方のオブジェクト列をエンコードしてデコードし直したとき、絶対値の
//! group_id / subgroup_id / object_id / publisher_priority / payload_length が
//! 保存されることを検証する。
//! Datagram 起源 (0x40) の Object は Subgroup ID を運ばず 0 に解決されるため、
//! subgroup_id の期待値は通常起源と Datagram 起源で分ける。
//! Properties 付きの Object も混ぜ、delta 圧縮と Properties の組み合わせで
//! 回帰が起きないこと、および Properties の生バイト列 (Properties Length varint 込み) が
//! そのまま復元されることを検証する。
//!
//! 高水準の往復だけではエンコーダとデコーダが同じ誤りを共有していても検出できないため、
//! エンコーダの出力を低水準 codec で読み直し、Object ID Delta の生値が仕様の規則に
//! 一致することを独立に検証する。

use pbt::common::{sample_varint, test_runner};
use shiguredo_moqt::object_properties::{ObjectProperties, ObjectProperty, ObjectPropertyValue};
use shiguredo_moqt::stream::decoder::{DecodedFetchEntry, DecodedFetchObject};
use shiguredo_moqt::stream::encoder::{FetchObjectInput, FetchStreamEncoder};
use shiguredo_moqt::stream::fetch::{FetchHeader, FetchPriorContext, FetchStreamEntry};
use shiguredo_moqt::track_properties::{
    PROP_OBJECT_DELIVERY_TIMEOUT, PROP_SUBGROUP_DELIVERY_TIMEOUT,
};

use crate::decoder::drive_fetch_with_group_order;

/// Subgroup ID のサンプル
///
/// 0 は Table 9 の 0x00 (Subgroup ID is zero) に対応する値であり、Datagram 起源の
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

/// エンコーダの制約 (同一 Group 内の Object ID は狭義増加、Group は `group_order` の向きに
/// 狭義単調、同一 Subgroup 内の Priority 不変) を満たす入力列と、各オブジェクトの
/// ペイロードを生成する
///
/// `group_order` は 0x01 (昇順) または 0x02 (降順)。
fn sample_objects(
    ctx: &mut noprop::TestCaseContext,
    group_order: u8,
) -> Vec<(FetchObjectInput, Vec<u8>, Vec<u8>)> {
    let count = noprop::sample_usize_in(ctx, 1..=4);
    // 昇順は加算で overflow しないよう小さい初期値から、降順は減算で underflow しないよう
    // 十分大きい初期値から始める (どちらも最大 4 Object、Group の増減は 1 回あたり 1001 以下)
    let mut group_id = if group_order == 0x02 {
        noprop::sample_u64_in(ctx, 5_000..=1_000_000)
    } else {
        noprop::sample_u64_in(ctx, 0..=1_000_000)
    };
    let mut subgroup_id = sample_subgroup_id(ctx);
    let mut object_id = noprop::sample_u64_in(ctx, 0..=1_000_000);
    let mut publisher_priority = noprop::sample_u8(ctx);
    let mut out = Vec::new();
    for index in 0..count {
        if index > 0 {
            if noprop::sample_bool(ctx) {
                // 新しい Group (group_order の向きに狭義単調): 各フィールドを再抽選する
                let delta = 1 + noprop::sample_u64_in(ctx, 0..=1000);
                if group_order == 0x02 {
                    group_id -= delta;
                } else {
                    group_id += delta;
                }
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
///
/// 昇順 (0x01) と降順 (0x02) の両方を検証する。降順では Group が変化するケースまで
/// 生成されないと Group デルタの解決が検証されないため、観測をゲートする。
#[test]
fn encoder_decoder_roundtrip() -> noprop::TestResult {
    let ascending_seen = std::cell::Cell::new(false);
    let descending_seen = std::cell::Cell::new(false);
    let descending_group_change_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let request_id = sample_varint(ctx);
        let group_order = if noprop::sample_bool(ctx) { 0x01 } else { 0x02 };
        let objects = sample_objects(ctx, group_order);
        if group_order == 0x02 {
            descending_seen.set(true);
            if objects
                .windows(2)
                .any(|pair| pair[0].0.group_id != pair[1].0.group_id)
            {
                descending_group_change_seen.set(true);
            }
        } else {
            ascending_seen.set(true);
        }

        let mut encoder = FetchStreamEncoder::new_with_group_order(request_id, group_order)
            .expect("0x01 / 0x02 は有効な Group Order");
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

        let decoded = drive_fetch_with_group_order(&[&bytes], group_order)
            .expect("エンコーダ出力はデコードできる");
        assert_eq!(decoded.len(), objects.len());

        for ((entry, payload), (input, expected_payload, properties_data)) in
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
            // Properties は Properties Length varint 込みの生バイト列として復元される
            // (`sample_properties` の doc を参照)。bit 0x20 が 0 なら `None`、
            // 1 なら (Length = 0 でも) `Some` になる。
            match (input.has_properties, object.properties_bytes.as_deref()) {
                (false, None) => {}
                (true, Some(bytes)) => assert_eq!(
                    bytes,
                    properties_data.as_slice(),
                    "Properties の生バイト列が保存されること"
                ),
                (false, Some(bytes)) => panic!(
                    "Properties 無しの Object が Properties を運んだ: \
                     group_id={}, object_id={}, properties_bytes={bytes:?}",
                    input.group_id, input.object_id
                ),
                (true, None) => panic!(
                    "Properties 付きの Object が Properties を落とした: \
                     group_id={}, object_id={}, expected={properties_data:?}",
                    input.group_id, input.object_id
                ),
            }
        }
        Ok(())
    })?;
    assert!(
        ascending_seen.get(),
        "昇順のケースが生成されなかった\n{runner}"
    );
    assert!(
        descending_seen.get(),
        "降順のケースが生成されなかった\n{runner}"
    );
    assert!(
        descending_group_change_seen.get(),
        "降順で Group が変化するケースが生成されなかった\n{runner}"
    );
    Ok(())
}

/// End of Range を混ぜた操作列のエンコード → デコード往復
///
/// draft-ietf-moq-transport-23 §11.4.2.1 (ID Decoding): End of Range は先頭 entry だけが
/// 絶対 ID で、それ以外は直前の Object または End of Range の Location からのデルタに
/// なる。Group ID Delta は昇順で加算、降順で減算し、Group ID Delta が 0 なら
/// Object ID は prior + delta + 1、Group が変わるなら絶対 Object ID になる。
/// エンコーダが絶対 Location をこの規則でデルタに変換し、デコーダが同じ規則で
/// 絶対 Location に戻せることを、連続する End of Range と Object の交互配置を含めて検証する。
///
/// 生成する Location は Group Order の向きに狭義単調で、同一 Group 内の Object ID は
/// 狭義増加にする。Subgroup ID と Publisher Priority は固定し、同じ Subgroup の
/// Priority 不変条件には触れない (その検証は既存の PBT が担う)。
#[test]
fn encoder_decoder_roundtrip_with_end_of_range() -> noprop::TestResult {
    let end_of_range_seen = std::cell::Cell::new(false);
    let first_end_of_range_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let request_id = sample_varint(ctx);
        let group_order = if noprop::sample_bool(ctx) { 0x01 } else { 0x02 };
        let mut encoder = FetchStreamEncoder::new_with_group_order(request_id, group_order)
            .expect("0x01 / 0x02 は有効な Group Order");
        let mut bytes = encoder.encode_header();
        let mut expected_entries = Vec::new();
        let mut expected_payloads = Vec::new();

        // 降順は減算で underflow しないよう十分大きい初期値から始める
        let mut group_id = if group_order == 0x02 {
            noprop::sample_u64_in(ctx, 5_000..=1_000_000)
        } else {
            noprop::sample_u64_in(ctx, 0..=1_000_000)
        };
        let mut object_id = noprop::sample_u64_in(ctx, 0..=1_000_000);
        let count = noprop::sample_usize_in(ctx, 1..=4);
        // 先頭 entry が End of Range になるケースを混ぜ、絶対 ID の経路を通す
        let first_is_end_of_range = noprop::sample_bool(ctx);

        for index in 0..count {
            let is_end_of_range =
                (index == 0 && first_is_end_of_range) || (index > 0 && noprop::sample_bool(ctx));
            if index > 0 {
                if noprop::sample_bool(ctx) {
                    // Group を Group Order の向きに動かす (最大 4 回 x 1001 で underflow しない)
                    let delta = 1 + noprop::sample_u64_in(ctx, 0..=1000);
                    if group_order == 0x02 {
                        group_id -= delta;
                    } else {
                        group_id += delta;
                    }
                    object_id = noprop::sample_u64_in(ctx, 0..=1_000_000);
                } else {
                    // 同一 Group 内で Object ID を狭義増加させる
                    object_id += 1 + noprop::sample_u64_in(ctx, 0..=1000);
                }
            }

            if is_end_of_range {
                let kind = noprop::sample_usize_in(ctx, 0..3);
                match kind {
                    0 => encoder
                        .encode_end_of_non_existent_range(group_id, object_id, &mut bytes)
                        .expect("絶対 Location はエンコードできる"),
                    1 => encoder
                        .encode_end_of_unknown_range(group_id, object_id, &mut bytes)
                        .expect("絶対 Location はエンコードできる"),
                    // kind は 0..3 のため 2 のみがここに到達する
                    _ => encoder
                        .encode_end_of_timed_out_range(group_id, object_id, &mut bytes)
                        .expect("絶対 Location はエンコードできる"),
                }
                expected_entries.push(match kind {
                    0 => DecodedFetchEntry::EndOfNonExistentRange {
                        group_id,
                        object_id,
                    },
                    1 => DecodedFetchEntry::EndOfUnknownRange {
                        group_id,
                        object_id,
                    },
                    _ => DecodedFetchEntry::EndOfTimedOutRange {
                        group_id,
                        object_id,
                    },
                });
                expected_payloads.push(Vec::new());
                end_of_range_seen.set(true);
                if index == 0 {
                    first_end_of_range_seen.set(true);
                }
            } else {
                let payload_length = noprop::sample_u64_in(ctx, 0..=16);
                let payload = noprop::sample_bytes_vec(ctx, payload_length as usize);
                encoder
                    .encode_object(
                        &FetchObjectInput {
                            group_id,
                            subgroup_id: 0,
                            object_id,
                            publisher_priority: 128,
                            has_properties: false,
                            is_datagram_origin: false,
                            payload_length,
                        },
                        None,
                        &mut bytes,
                    )
                    .expect("狭義単調な Location はエンコードできる");
                bytes.extend_from_slice(&payload);
                expected_entries.push(DecodedFetchEntry::Object(DecodedFetchObject {
                    group_id,
                    subgroup_id: 0,
                    object_id,
                    publisher_priority: 128,
                    is_datagram_origin: false,
                    payload_length,
                    properties_bytes: None,
                }));
                expected_payloads.push(payload);
            }
        }

        let decoded = drive_fetch_with_group_order(&[&bytes], group_order)
            .expect("エンコーダ出力はデコードできる");
        // End of Range を含むストリームも、任意の 1 分割で push した結果が一括 push と一致する
        let at = noprop::sample_usize_in(ctx, 0..=bytes.len());
        let split = drive_fetch_with_group_order(&[&bytes[..at], &bytes[at..]], group_order)
            .expect("分割したエンコーダ出力もデコードできる");
        assert_eq!(
            split, decoded,
            "End of Range の分割受信でデコード結果が変わらないこと"
        );
        let decoded_entries: Vec<DecodedFetchEntry> =
            decoded.iter().map(|(entry, _)| entry.clone()).collect();
        let decoded_payloads: Vec<Vec<u8>> =
            decoded.iter().map(|(_, payload)| payload.clone()).collect();
        assert_eq!(
            decoded_entries, expected_entries,
            "End of Range を含む絶対 Location が往復すること"
        );
        assert_eq!(decoded_payloads, expected_payloads);
        Ok(())
    })?;
    assert!(
        end_of_range_seen.get(),
        "End of Range を含むケースが生成されなかった\n{runner}"
    );
    assert!(
        first_end_of_range_seen.get(),
        "先頭が End of Range のケースが生成されなかった\n{runner}"
    );
    Ok(())
}

/// エンコーダが書き出す Object ID Delta の生値が仕様の規則に一致すること
///
/// draft-ietf-moq-transport-23 §11.4.1.1 (Flags):
/// - 最初の Object は Group ID Delta / Object ID Delta が絶対値
/// - Group ID Delta が present のときは Object ID Delta が新しい Group の絶対 Object ID
/// - Group ID Delta が absent のときは prior Object ID + delta + 2、absent なら prior + 1
///
/// 高水準デコーダを経由した往復はエンコーダとデコーダが同じ誤りを持っていても成立する。
/// 低水準 codec (`FetchStreamEntry::decode`) で wire の生値を読み、絶対 Location との対応を
/// 独立に検証する。End of Range の +1 規則 (§11.4.2.1 (ID Decoding)) と混同していないことも
/// ここで固定する (この生成列に End of Range は含まれない)。
#[test]
fn object_id_delta_matches_draft_23_rule() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let group_order = if noprop::sample_bool(ctx) { 0x01 } else { 0x02 };
        let objects = sample_objects(ctx, group_order);
        let mut encoder =
            FetchStreamEncoder::new_with_group_order(sample_varint(ctx), group_order)
                .expect("0x01 / 0x02 は有効な Group Order");
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

        let (_, header_len) = FetchHeader::decode(&bytes).expect("FETCH_HEADER をデコードできる");
        let mut pos = header_len;
        let mut prior: Option<(u64, u64)> = None;
        for (index, (input, payload, _)) in objects.iter().enumerate() {
            let prior_context = if index == 0 {
                FetchPriorContext::First
            } else {
                FetchPriorContext::HasPriorObject
            };
            let (entry, consumed) = FetchStreamEntry::decode(&bytes[pos..], prior_context)
                .expect("エンコーダ出力は低水準 codec でデコードできる");
            pos += consumed + payload.len();
            let FetchStreamEntry::Object(raw) = entry else {
                panic!("通常の Object エントリが返ること: {entry:?}");
            };
            match prior {
                // 最初の Object は両フィールドが絶対値である
                None => {
                    assert_eq!(
                        raw.group_id,
                        Some(input.group_id),
                        "最初の Object の Group ID Delta は絶対 Group ID であること"
                    );
                    assert_eq!(
                        raw.object_id,
                        Some(input.object_id),
                        "最初の Object の Object ID Delta は絶対 Object ID であること"
                    );
                }
                Some((prior_group, prior_object)) => {
                    if input.group_id == prior_group {
                        // 同一 Group は Group ID Delta absent、Object ID Delta は +2 された差
                        assert_eq!(
                            raw.group_id, None,
                            "同一 Group では Group ID Delta を省略すること"
                        );
                        if input.object_id == prior_object + 1 {
                            assert_eq!(
                                raw.object_id, None,
                                "prior + 1 は Object ID Delta を省略すること"
                            );
                        } else {
                            assert_eq!(
                                raw.object_id,
                                Some(input.object_id - prior_object - 2),
                                "同一 Group の Object ID Delta は実際の差から 2 を引いた値であること"
                            );
                        }
                    } else {
                        // Group 変更は Group ID Delta present、Object ID は絶対値
                        let expected_group_delta = if group_order == 0x02 {
                            prior_group - input.group_id - 1
                        } else {
                            input.group_id - prior_group - 1
                        };
                        assert_eq!(
                            raw.group_id,
                            Some(expected_group_delta),
                            "Group 変更時の Group ID Delta が Group Order の向きに一致すること"
                        );
                        assert_eq!(
                            raw.object_id,
                            Some(input.object_id),
                            "Group ID Delta が present のときは Object ID が絶対値であること"
                        );
                    }
                }
            }
            prior = Some((input.group_id, input.object_id));
        }
        assert_eq!(pos, bytes.len(), "全エントリとペイロードを読み切ること");
        Ok(())
    })?;
    Ok(())
}

/// End of Group / End of Track を混ぜた操作列のエンコード → デコード往復
///
/// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields): marker は直前の Object が
/// Group / Track の最終 Object であることを示し、prior Object ID を `2^64 - 1` にする。
/// そのため marker の後に Object を置く場合は新しい Group に移り、Group ID Delta と絶対
/// Object ID を持つ必要がある。エンコーダがその制約を満たすバイト列を書き、デコーダが
/// marker の前後の Group と Object ID を復元することを検証する。
///
/// 分割受信でも marker の位置と前後の entry が変わらないこと、および marker の直後に
/// 後続 ID フィールドが無いことを、低水準 codec の消費バイト数で独立に確認する。
#[test]
fn encoder_decoder_roundtrip_with_group_and_track_end() -> noprop::TestResult {
    let group_end_seen = std::cell::Cell::new(false);
    let track_end_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let request_id = sample_varint(ctx);
        let group_order = if noprop::sample_bool(ctx) { 0x01 } else { 0x02 };
        let mut encoder = FetchStreamEncoder::new_with_group_order(request_id, group_order)
            .expect("0x01 / 0x02 は有効な Group Order");
        let mut bytes = encoder.encode_header();
        let mut expected_entries = Vec::new();
        let mut expected_payloads = Vec::new();

        // 降順は減算で underflow しないよう十分大きい初期値から始める
        let mut group_id = if group_order == 0x02 {
            noprop::sample_u64_in(ctx, 5_000..=1_000_000)
        } else {
            noprop::sample_u64_in(ctx, 0..=1_000_000)
        };
        let mut object_id = noprop::sample_u64_in(ctx, 0..=1_000_000);
        let first_group_object_count = noprop::sample_usize_in(ctx, 1..=3);
        for index in 0..first_group_object_count {
            if index > 0 {
                object_id += 1 + noprop::sample_u64_in(ctx, 0..=1000);
            }
            let payload_length = noprop::sample_u64_in(ctx, 0..=16);
            let payload = noprop::sample_bytes_vec(ctx, payload_length as usize);
            encoder
                .encode_object(
                    &FetchObjectInput {
                        group_id,
                        subgroup_id: 0,
                        object_id,
                        publisher_priority: 128,
                        has_properties: false,
                        is_datagram_origin: false,
                        payload_length,
                    },
                    None,
                    &mut bytes,
                )
                .expect("狭義単調な Location はエンコードできる");
            bytes.extend_from_slice(&payload);
            expected_entries.push(DecodedFetchEntry::Object(DecodedFetchObject {
                group_id,
                subgroup_id: 0,
                object_id,
                publisher_priority: 128,
                is_datagram_origin: false,
                payload_length,
                properties_bytes: None,
            }));
            expected_payloads.push(payload);
        }

        // Group の終端を宣言し、次の Group の Object を書く
        encoder
            .encode_end_of_group(&mut bytes)
            .expect("直前の Object があるため End of Group を書ける");
        group_end_seen.set(true);
        expected_entries.push(DecodedFetchEntry::EndOfGroup { group_id });
        expected_payloads.push(Vec::new());

        let group_delta = 1 + noprop::sample_u64_in(ctx, 0..=1000);
        group_id = if group_order == 0x02 {
            group_id - group_delta
        } else {
            group_id + group_delta
        };
        object_id = noprop::sample_u64_in(ctx, 0..=1_000_000);
        let second_group_object_count = noprop::sample_usize_in(ctx, 1..=2);
        for index in 0..second_group_object_count {
            if index > 0 {
                object_id += 1 + noprop::sample_u64_in(ctx, 0..=1000);
            }
            let payload_length = noprop::sample_u64_in(ctx, 0..=16);
            let payload = noprop::sample_bytes_vec(ctx, payload_length as usize);
            encoder
                .encode_object(
                    &FetchObjectInput {
                        group_id,
                        subgroup_id: 0,
                        object_id,
                        publisher_priority: 128,
                        has_properties: false,
                        is_datagram_origin: false,
                        payload_length,
                    },
                    None,
                    &mut bytes,
                )
                .expect("End of Group の後の新しい Group の Object はエンコードできる");
            bytes.extend_from_slice(&payload);
            expected_entries.push(DecodedFetchEntry::Object(DecodedFetchObject {
                group_id,
                subgroup_id: 0,
                object_id,
                publisher_priority: 128,
                is_datagram_origin: false,
                payload_length,
                properties_bytes: None,
            }));
            expected_payloads.push(payload);
        }

        // 応答末尾の Object が Track の終端と分かる場合は End of Track を書く
        let write_track_end = noprop::sample_bool(ctx);
        if write_track_end {
            encoder
                .encode_end_of_track(&mut bytes)
                .expect("直前の Object があるため End of Track を書ける");
            track_end_seen.set(true);
            expected_entries.push(DecodedFetchEntry::EndOfTrack { group_id });
            expected_payloads.push(Vec::new());
        }

        let decoded = drive_fetch_with_group_order(&[&bytes], group_order)
            .expect("エンコーダ出力はデコードできる");
        // marker を含むストリームも、任意の 1 分割で push した結果が一括 push と一致する
        let at = noprop::sample_usize_in(ctx, 0..=bytes.len());
        let split = drive_fetch_with_group_order(&[&bytes[..at], &bytes[at..]], group_order)
            .expect("分割したエンコーダ出力もデコードできる");
        assert_eq!(
            split, decoded,
            "End of Group / End of Track の分割受信でデコード結果が変わらないこと"
        );
        let decoded_entries: Vec<DecodedFetchEntry> =
            decoded.iter().map(|(entry, _)| entry.clone()).collect();
        let decoded_payloads: Vec<Vec<u8>> =
            decoded.iter().map(|(_, payload)| payload.clone()).collect();
        assert_eq!(
            decoded_entries, expected_entries,
            "marker の前後の Group と Object ID が往復すること"
        );
        assert_eq!(decoded_payloads, expected_payloads);
        Ok(())
    })?;
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
