use shiguredo_moqt::track_properties::{
    PROP_MAX_CACHE_DURATION, TrackProperties, TrackProperty, TrackPropertyValue,
};

use pbt::common::{sample_track_properties, test_runner};

#[test]
fn roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let props = sample_track_properties(ctx);
        let mut buf = Vec::new();
        props
            .encode(&mut buf)
            .expect("正当なテスト入力の encode は成功する");
        let decoded = TrackProperties::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        // デコード後も内容が一致すること (件数・再 encode のみでは値の破壊を検出できない)
        assert_eq!(decoded, props);

        // decode 後のプロパティが prop_type の昇順でソートされていること
        let slice = decoded.as_slice();
        for w in slice.windows(2) {
            assert!(w[0].prop_type < w[1].prop_type);
        }
        Ok(())
    })?;
    Ok(())
}

/// MAX_CACHE_DURATION (0x04) を含む TrackProperties が往復し、
/// デコード後も同じ値を返すこと (draft-ietf-moq-transport-21 §10.3 (MAX CACHE DURATION))。
/// 値域制約は無いため full u64 域 (varint は 9 バイトまでで全域対応) を生成する。
#[test]
fn max_cache_duration_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let v = noprop::sample_u64(ctx);
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: PROP_MAX_CACHE_DURATION,
            value: TrackPropertyValue::VarInt(v),
        });
        let mut buf = Vec::new();
        props
            .encode(&mut buf)
            .expect("正当なテスト入力の encode は成功する");
        let decoded = TrackProperties::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(decoded.find_varint(PROP_MAX_CACHE_DURATION), Some(v));
        Ok(())
    })?;
    Ok(())
}
