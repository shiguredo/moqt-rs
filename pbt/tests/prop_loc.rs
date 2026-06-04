use pbt::common::test_runner;
use shiguredo_moqt::loc::{
    LocProperties, LocProperty, LocPropertyValue, PROP_AUDIO_LEVEL, PROP_TIMESCALE, PROP_TIMESTAMP,
    PROP_VIDEO_CONFIG, PROP_VIDEO_FRAME_MARKING,
};

/// 空を含む LOC プロパティコレクションを生成する
///
/// draft-ietf-moq-transport-21 §11.1.3 (Object Properties) / §11.3.1 (Subgroup Header) に従い、空 (Properties Length = 0) も合法。
/// draft-ietf-moq-loc-04 の既知プロパティを組み合わせる。同一 prop_id の重複は含まない。
fn sample_loc_properties(ctx: &mut noprop::TestCaseContext) -> LocProperties {
    let has_vfm = noprop::sample_bool(ctx);
    let has_ts = noprop::sample_bool(ctx);
    let has_tsc = noprop::sample_bool(ctx);
    let has_al = noprop::sample_bool(ctx);
    let has_vc = noprop::sample_bool(ctx);
    let vfm_len = noprop::sample_usize_in(ctx, 1..=4);
    let vfm_bytes = noprop::sample_bytes_vec(ctx, vfm_len); // video_frame_marking: 長さ 1-4 bytes
    let ts_val = noprop::sample_u64_in(ctx, 0..=u64::MAX); // timestamp: vi64 1-9 bytes (u64 全域)
    let tsc_val = noprop::sample_u64_in(ctx, 0..=u64::MAX); // timescale: 制限なし
    let al_val = noprop::sample_u8(ctx) as u64; // audio_level: 下位 8 bit
    let vc_len = noprop::sample_usize_in(ctx, 0..=100);
    let vc_bytes = noprop::sample_bytes_vec(ctx, vc_len); // video_config の値

    // encode は prop_id 昇順にソートするため、ラウンドトリップが恒等になるよう昇順で push する
    // 昇順: 0x08 (Timescale), 0x09 (VFM), 0x0C (AudioLevel), 0x0D (VideoConfig), 0x10 (Timestamp)
    let mut props = LocProperties::new();
    if has_tsc {
        props.push(LocProperty {
            prop_id: PROP_TIMESCALE,
            value: LocPropertyValue::VarInt(tsc_val),
        });
    }
    if has_vfm {
        props.push(LocProperty {
            prop_id: PROP_VIDEO_FRAME_MARKING,
            value: LocPropertyValue::Bytes(vfm_bytes),
        });
    }
    if has_al {
        props.push(LocProperty {
            prop_id: PROP_AUDIO_LEVEL,
            value: LocPropertyValue::VarInt(al_val),
        });
    }
    if has_vc {
        props.push(LocProperty {
            prop_id: PROP_VIDEO_CONFIG,
            value: LocPropertyValue::Bytes(vc_bytes),
        });
    }
    if has_ts {
        props.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: LocPropertyValue::VarInt(ts_val),
        });
    }
    props
}

/// 空を含む encode → decode のラウンドトリップ検証
///
/// draft-ietf-moq-transport-21 §11.1.3 (Object Properties):
/// "Objects with no properties set Properties Length to 0"
#[test]
fn roundtrip_including_empty() -> noprop::TestResult {
    // 空 (プロパティ 0 個) と非空の両方が観測されたかを数える
    let empty_seen = std::cell::Cell::new(false);
    let non_empty_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let props = sample_loc_properties(ctx);
        if props.is_empty() {
            empty_seen.set(true);
        } else {
            non_empty_seen.set(true);
        }
        let encoded = props
            .encode()
            .expect("正当なテスト入力の encode は成功する");
        let (decoded, consumed) =
            LocProperties::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, props);
        Ok(())
    })?;
    assert!(
        empty_seen.get(),
        "空の LOC プロパティのケースが生成されなかった\n{runner}"
    );
    assert!(
        non_empty_seen.get(),
        "非空の LOC プロパティのケースが生成されなかった\n{runner}"
    );
    Ok(())
}

// 「非空なら encode 後 2 バイト以上」は length prefix + 最低 1 バイトの形式から自明で、
// roundtrip が成立していれば含意されるため、独立した property としては持たない。

/// アクセサの一貫性: デコード後のアクセサはエンコード前と同じ値を返す
///
/// (has_ts, has_tsc) は (true, false) / (false, true) / (true, true) の 3 状態から
/// valid-by-construction で選び、assume (reject) なしに has_ts || has_tsc を保証する。
#[test]
fn accessor_consistency() -> noprop::TestResult {
    // timestamp / timescale の Some と None の両ブランチの観測を数える
    let ts_some_seen = std::cell::Cell::new(false);
    let ts_none_seen = std::cell::Cell::new(false);
    let tsc_some_seen = std::cell::Cell::new(false);
    let tsc_none_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let (has_ts, has_tsc) = match noprop::sample_weighted_index(ctx, &[1, 1, 1]) {
            0 => (true, false),
            1 => (false, true),
            _ => (true, true),
        };
        let ts_val = noprop::sample_u64_in(ctx, 0..=u64::MAX);
        let tsc_val = noprop::sample_u64_in(ctx, 0..=u64::MAX);

        let mut props = LocProperties::new();
        if has_ts {
            props.push(LocProperty {
                prop_id: PROP_TIMESTAMP,
                value: LocPropertyValue::VarInt(ts_val),
            });
        }
        if has_tsc {
            props.push(LocProperty {
                prop_id: PROP_TIMESCALE,
                value: LocPropertyValue::VarInt(tsc_val),
            });
        }

        let encoded = props
            .encode()
            .expect("正当なテスト入力の encode は成功する");
        let (decoded, _) =
            LocProperties::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");

        if has_ts {
            assert_eq!(decoded.timestamp(), Some(ts_val));
            ts_some_seen.set(true);
        } else {
            assert_eq!(decoded.timestamp(), None);
            ts_none_seen.set(true);
        }
        if has_tsc {
            assert_eq!(decoded.timescale(), Some(tsc_val));
            tsc_some_seen.set(true);
        } else {
            assert_eq!(decoded.timescale(), None);
            tsc_none_seen.set(true);
        }
        Ok(())
    })?;
    assert!(
        ts_some_seen.get(),
        "timestamp が Some を返すケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        ts_none_seen.get(),
        "timestamp が None を返すケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        tsc_some_seen.get(),
        "timescale が Some を返すケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        tsc_none_seen.get(),
        "timescale が None を返すケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}
