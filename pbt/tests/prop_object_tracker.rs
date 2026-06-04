//! `ObjectPropertyTracker` / `ObjectFieldTracker` の状態追跡 PBT
//!
//! - `ObjectPropertyTracker`: PRIOR_GROUP_ID_GAP が示す欠落 Group を再受信したら拒否する
//!   (draft-ietf-moq-transport-21 §10.8 (Prior Group ID Gap))
//! - `ObjectFieldTracker`: 同一 Object の再受信は全フィールド一致のときのみ成功する
//!   (draft-ietf-moq-transport-21 §12.1 (Malformed Tracks))

use pbt::common::test_runner;
use shiguredo_moqt::error::MessageError;
use shiguredo_moqt::object_properties::{
    ObjectFieldTracker, ObjectProperties, ObjectProperty, ObjectPropertyTracker,
    ObjectPropertyValue, PROP_PRIOR_GROUP_ID_GAP,
};

/// PRIOR_GROUP_ID_GAP が示す欠落 Group に属する Object は拒否され、欠落外は受理される
#[test]
fn prior_group_id_gap_rejects_group_in_gap() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let group_id = noprop::sample_u64_in(ctx, 1..=1_000_000);
        let gap = noprop::sample_u64_in(ctx, 1..=group_id);

        let mut props = ObjectProperties::new();
        props.push(ObjectProperty {
            prop_type: PROP_PRIOR_GROUP_ID_GAP,
            value: ObjectPropertyValue::VarInt(gap),
        });
        let mut bytes = Vec::new();
        props
            .encode(&mut bytes)
            .expect("正当なテスト入力の encode は成功する");

        let mut tracker = ObjectPropertyTracker::new();
        tracker
            .observe_object(group_id, 0, Some(&bytes))
            .expect("ギャップを申告する最初の Object は受理される");

        // 欠落 Group [group_id - gap, group_id - 1] の Object は拒否される
        let in_gap = noprop::sample_u64_in(ctx, group_id - gap..=group_id - 1);
        let err = tracker
            .observe_object(in_gap, 0, None)
            .expect_err("欠落 Group に属する Object は拒否される");
        assert!(
            matches!(err, MessageError::ProtocolViolation(_)),
            "PROTOCOL_VIOLATION で拒否されること: {err:?}"
        );

        // 欠落範囲外の未観測 Group は受理される
        if group_id - gap > 0 {
            let outside = noprop::sample_u64_in(ctx, 0..=group_id - gap - 1);
            tracker
                .observe_object(outside, 0, None)
                .expect("欠落範囲外の Group は受理される");
        }
        Ok(())
    })?;
    Ok(())
}

/// 同一 Object の再観測は全フィールド一致のときのみ成功する
#[test]
fn object_field_tracker_accepts_iff_fields_match() -> noprop::TestResult {
    let match_seen = std::cell::Cell::new(false);
    let mismatch_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let group_id = noprop::sample_u64_in(ctx, 0..=1_000_000);
        let object_id = noprop::sample_u64_in(ctx, 0..=1_000_000);

        let first = (
            noprop::sample_bool(ctx),
            if noprop::sample_bool(ctx) {
                Some(noprop::sample_u64_in(ctx, 0..=1_000_000))
            } else {
                None
            },
            noprop::sample_u8(ctx),
        );
        // 一致ケースを確実に観測するため、一定確率で first をそのまま使う
        let second = if noprop::sample_bool(ctx) {
            first
        } else {
            (
                noprop::sample_bool(ctx),
                if noprop::sample_bool(ctx) {
                    Some(noprop::sample_u64_in(ctx, 0..=1_000_000))
                } else {
                    None
                },
                noprop::sample_u8(ctx),
            )
        };

        let mut tracker = ObjectFieldTracker::new();
        tracker
            .observe_object_fields(group_id, object_id, first.0, first.1, first.2)
            .expect("初回の観測は成功する");

        let result =
            tracker.observe_object_fields(group_id, object_id, second.0, second.1, second.2);
        assert_eq!(
            result.is_ok(),
            second == first,
            "全フィールド一致のときのみ再観測が成功すること"
        );
        if second == first {
            match_seen.set(true);
        } else {
            mismatch_seen.set(true);
        }
        Ok(())
    })?;
    assert!(
        match_seen.get(),
        "全フィールド一致のケースが観測されなかった\n{runner}"
    );
    assert!(
        mismatch_seen.get(),
        "フィールド不一致のケースが観測されなかった\n{runner}"
    );
    Ok(())
}
