//! `ObjectPropertyTracker` / `ObjectFieldTracker` の状態追跡 PBT
//!
//! - `ObjectPropertyTracker`: PRIOR_GROUP_ID_GAP が示す欠落 Group を再受信したら拒否する
//!   (draft-ietf-moq-transport-21 §10.8 (Prior Group ID Gap))
//! - `ObjectFieldTracker`: 同一 Object の再受信は全フィールド一致のときのみ成功する
//!   (draft-ietf-moq-transport-21 §12.1 (Malformed Tracks))
//! - `ObjectFieldTracker::observe_object_fields_with_content`: immutables / payload_key は
//!   両方 `Some` のときだけ比較する (draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 6)

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
            matches!(err, MessageError::MalformedTrack(_)),
            "Malformed Track (§12.1) として拒否されること: {err:?}"
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

/// 内容込みの再観測は「全フィールド一致」かつ「比較可能な内容が一致」のときのみ成功する
///
/// draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 6: "The same Object is received
/// more than once with different Payload or other immutable properties."
/// `immutable_properties` と `payload_key` の比較は**両方** `Some` のときだけ行い、片方でも
/// `None` なら比較しない (見逃し側に倒す)。この規則を全入力の組み合わせで固定する。
#[test]
fn object_field_tracker_content_comparison_matches_expected() -> noprop::TestResult {
    // 内容の不一致 2 種 (immutables / payload_key) が実際に観測されたことを検証するゲート。
    // どちらかが一度も生成されなければ、規則を検証しないまま通ってしまう。
    let immutables_mismatch_seen = std::cell::Cell::new(false);
    let payload_key_mismatch_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let group_id = noprop::sample_u64_in(ctx, 0..=1_000_000);
        let object_id = noprop::sample_u64_in(ctx, 0..=1_000_000);
        let is_subgroup = noprop::sample_bool(ctx);
        let subgroup_id = if noprop::sample_bool(ctx) {
            Some(noprop::sample_u64_in(ctx, 0..=1_000_000))
        } else {
            None
        };
        let publisher_priority = noprop::sample_u8(ctx);

        // 内容は「未提供 (None)」と「1-8 バイトの値」をどちらも生成する
        let sample_content = |ctx: &mut noprop::TestCaseContext| {
            if noprop::sample_bool(ctx) {
                None
            } else {
                let len = noprop::sample_usize_in(ctx, 1..=8);
                Some(noprop::sample_bytes_vec(ctx, len))
            }
        };
        // 一致ケースを確実に観測するため、一定確率で 1 回目をそのまま使う
        let first_immutables = sample_content(ctx);
        let first_payload_key = sample_content(ctx);
        let (second_immutables, second_payload_key) = if noprop::sample_bool(ctx) {
            (first_immutables.clone(), first_payload_key.clone())
        } else {
            (sample_content(ctx), sample_content(ctx))
        };
        // 3 回目は 2 回目と独立に生成する。記録は初回受信時のまま更新しないため、
        // 3 回目の比較相手は常に 1 回目である
        let (third_immutables, third_payload_key) = (sample_content(ctx), sample_content(ctx));

        let mut tracker = ObjectFieldTracker::new();
        tracker
            .observe_object_fields_with_content(
                group_id,
                object_id,
                is_subgroup,
                subgroup_id,
                publisher_priority,
                first_immutables.as_deref(),
                first_payload_key.as_deref(),
            )
            .expect("初回の観測は成功する");

        let result = tracker.observe_object_fields_with_content(
            group_id,
            object_id,
            is_subgroup,
            subgroup_id,
            publisher_priority,
            second_immutables.as_deref(),
            second_payload_key.as_deref(),
        );

        // 両方 `Some` のときだけ比較し、値が異なれば不一致になる
        let content_matches =
            |first: &Option<Vec<u8>>, second: &Option<Vec<u8>>| match (first, second) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            };
        let immutables_matches = content_matches(&first_immutables, &second_immutables);
        let payload_key_matches = content_matches(&first_payload_key, &second_payload_key);
        assert_eq!(
            result.is_ok(),
            immutables_matches && payload_key_matches,
            "内容の比較は両方 Some のときだけ行われること: \
             immutables={first_immutables:?}/{second_immutables:?} \
             payload_key={first_payload_key:?}/{second_payload_key:?}"
        );
        if !immutables_matches {
            immutables_mismatch_seen.set(true);
        }
        if !payload_key_matches {
            payload_key_mismatch_seen.set(true);
        }
        match result {
            // immutables を先に比較するため、両方が不一致なら immutables の理由が返る
            Err(err) if !immutables_matches => assert_eq!(
                err.reason, "malformed track: duplicate Object with different immutable properties",
                "immutables の不一致理由が返ること"
            ),
            Err(err) => assert_eq!(
                err.reason, "malformed track: duplicate Object with different Payload",
                "payload_key の不一致理由が返ること"
            ),
            Ok(()) => {}
        }

        // 3 回目の観測。記録は初回のまま更新しないため、比較相手は常に 1 回目である
        let third = tracker.observe_object_fields_with_content(
            group_id,
            object_id,
            is_subgroup,
            subgroup_id,
            publisher_priority,
            third_immutables.as_deref(),
            third_payload_key.as_deref(),
        );
        let expected_third_ok = content_matches(&first_immutables, &third_immutables)
            && content_matches(&first_payload_key, &third_payload_key);
        assert_eq!(
            third.is_ok(),
            expected_third_ok,
            "記録は初回受信時のまま更新しないこと: \
             first_immutables={first_immutables:?} second_immutables={second_immutables:?} \
             third_immutables={third_immutables:?} first_payload_key={first_payload_key:?} \
             second_payload_key={second_payload_key:?} third_payload_key={third_payload_key:?}"
        );
        Ok(())
    })?;
    assert!(
        immutables_mismatch_seen.get(),
        "immutables 不一致のケースが観測されなかった\n{runner}"
    );
    assert!(
        payload_key_mismatch_seen.get(),
        "payload_key 不一致のケースが観測されなかった\n{runner}"
    );
    Ok(())
}
