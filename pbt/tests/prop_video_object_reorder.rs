//! `VideoObjectReorderBuffer` の並べ替えの PBT
//!
//! object モード (draft-ietf-moq-msf-02 §4.1.1 (LOC to MOQT Stream mapping)) では、
//! 1 Group の Object が Object ごとに別の Subgroup で届く。draft-ietf-moq-transport-23
//! §2.2 (Subgroups) は順序を stream 内だけに保証するため、受信側には到着順の入れ替わりが
//! 起きる。Group 内の最初の Object ID を 0 として 1 ずつ増やす規則
//! (draft-ietf-moq-msf-02 §6.2 (Object Numbering)) を使い、Object ID の連続した範囲だけを
//! 配送する。
//!
//! 検証する性質は次の 2 つである。
//!
//! - 取り出した Object の Location は (Group ID, Object ID) の辞書順で厳密に増える
//!   (重複を配送せず、遅着した古い Object も配送しない)
//! - Group をまたいで昇順に届き、各 Group の Object がすべて揃っていれば、すべての Object を
//!   Object ID 順に取り出す
//!
//! あわせて、到着順の入れ替わり・古い Group の保留分の取り出し・重複の廃棄の分岐に
//! 到達したことを確かめる。

use std::cell::Cell;

use pbt::common::test_runner;
use shiguredo_moqt::video_object_reorder::VideoObjectReorderBuffer;

/// Group 数の境界 (空にはしない)
const GROUP_COUNT_BOUNDARIES: [usize; 3] = [1, 2, 4];

/// Group 内の Object 数の境界
const OBJECT_COUNT_BOUNDARIES: [usize; 3] = [1, 3, 8];

/// 1 ケースの受信列の長さの境界
const STEP_BOUNDARIES: [usize; 3] = [1, 8, 24];

/// 到着順の入れ替えの分岐に到達した回数
fn count_reordered(received: &[(u64, u64)], released: &[(u64, u64)], counter: &Cell<usize>) {
    // 受信順と配送順が異なる Object があれば、入れ替えを吸収したことになる
    for (index, location) in released.iter().enumerate() {
        if received.get(index) != Some(location) {
            counter.set(counter.get() + 1);
            break;
        }
    }
}

/// 到着順を任意に入れ替えてよい受信列を生成する
///
/// 各要素は `(group_id, object_id)` である。Group は昇順に現れるが、Group 内の Object は
/// 任意の順で届き、同じ Object が再度届くことがある。
fn sample_received(ctx: &mut noprop::TestCaseContext) -> Vec<(u64, u64)> {
    let group_count = noprop::sample_with_boundaries(
        ctx,
        &GROUP_COUNT_BOUNDARIES,
        noprop::Ratio::one_nth(4),
        |ctx| noprop::sample_usize_in(ctx, 1..=GROUP_COUNT_BOUNDARIES[2]),
    );
    let mut received = Vec::new();
    for group_index in 0..group_count {
        let group_id = 100 + group_index as u64;
        let object_count = noprop::sample_with_boundaries(
            ctx,
            &OBJECT_COUNT_BOUNDARIES,
            noprop::Ratio::one_nth(4),
            |ctx| noprop::sample_usize_in(ctx, 1..=OBJECT_COUNT_BOUNDARIES[2]),
        );
        let mut objects: Vec<u64> = (0..object_count as u64).collect();
        // Fisher-Yates で Group 内の到着順を入れ替える
        for i in (1..objects.len()).rev() {
            let j = noprop::sample_usize_in(ctx, 0..=i);
            objects.swap(i, j);
        }
        // 同じ Object の再送を足す
        if noprop::sample_ratio(ctx, noprop::Ratio::one_nth(4)) {
            let index = noprop::sample_usize_in(ctx, 0..objects.len());
            let at = noprop::sample_usize_in(ctx, 0..=objects.len());
            let duplicate = objects[index];
            objects.insert(at, duplicate);
        }
        received.extend(objects.into_iter().map(|object_id| (group_id, object_id)));
    }
    received
}

/// 取り出した Object の Location が (Group ID, Object ID) の辞書順で厳密に増えること
#[test]
fn released_locations_increase_strictly() -> noprop::TestResult {
    // ゲート: 到着順の入れ替えを吸収したケースと、複数の Group を配送したケースの両方が
    // 観測されること
    let reordered = Cell::new(0usize);
    let multi_group = Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let received = sample_received(ctx);
        let mut buffer = VideoObjectReorderBuffer::new();
        let mut released = Vec::new();
        for (group_id, object_id) in &received {
            released.extend(buffer.insert(*group_id, *object_id, ()));
        }
        let locations: Vec<(u64, u64)> = released
            .iter()
            .map(|object| (object.group_id, object.object_id))
            .collect();
        for pair in locations.windows(2) {
            assert!(
                pair[0] < pair[1],
                "取り出した Location は厳密に増えること: previous={:?} next={:?} received={received:?}",
                pair[0],
                pair[1],
            );
        }
        count_reordered(&received, &locations, &reordered);
        if locations
            .windows(2)
            .any(|pair| pair[0].0 != pair[1].0)
        {
            multi_group.set(multi_group.get() + 1);
        }
        Ok(())
    })?;
    assert!(
        reordered.get() > 0,
        "到着順の入れ替えを吸収したケースが観測されなかった\n{runner}"
    );
    assert!(
        multi_group.get() > 0,
        "複数の Group を配送したケースが観測されなかった\n{runner}"
    );
    Ok(())
}

/// 各 Group の Object がすべて揃っていれば、すべて Object ID 順に取り出されること
#[test]
fn complete_groups_are_released_in_object_id_order() -> noprop::TestResult {
    // ゲート: 到着順の入れ替えが起きたケースと、複数 Object を持つ Group が観測されること
    let reordered = Cell::new(0usize);
    let multi_object = Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let received = sample_received(ctx);
        let mut buffer = VideoObjectReorderBuffer::new();
        let mut released = Vec::new();
        for (group_id, object_id) in &received {
            released.extend(buffer.insert(*group_id, *object_id, ()));
        }
        let locations: Vec<(u64, u64)> = released
            .iter()
            .map(|object| (object.group_id, object.object_id))
            .collect();
        // 受信列が持つ (Group ID, Object ID) の集合と、配送した集合が一致すること
        let mut expected: Vec<(u64, u64)> = received.clone();
        expected.sort_unstable();
        expected.dedup();
        assert_eq!(
            locations, expected,
            "すべての Object を Object ID 順に取り出すこと: received={received:?}"
        );
        count_reordered(&received, &locations, &reordered);
        if received.iter().filter(|(group, _)| *group == 100).count() > 1 {
            multi_object.set(multi_object.get() + 1);
        }
        Ok(())
    })?;
    assert!(
        reordered.get() > 0,
        "到着順の入れ替えを吸収したケースが観測されなかった\n{runner}"
    );
    assert!(
        multi_object.get() > 0,
        "複数 Object を持つ Group が観測されなかった\n{runner}"
    );
    Ok(())
}

/// 古い Group の保留分が、新しい Group の到着で Object ID 順に取り出されること
#[test]
fn a_newer_group_flushes_the_pending_objects_of_the_older_group() -> noprop::TestResult {
    // ゲート: 先頭 (Object ID 0) が欠けたまま次の Group が届くケースを観測すること
    let flushed = Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let object_id = noprop::sample_u64_in(ctx, 1..=8);
        let mut buffer = VideoObjectReorderBuffer::new();
        assert!(
            buffer.insert(10, object_id, ()).is_empty(),
            "Group の先頭が届くまでは保留すること: object_id={object_id}"
        );
        let released = buffer.insert(11, 0, ());
        let locations: Vec<(u64, u64)> = released
            .iter()
            .map(|object| (object.group_id, object.object_id))
            .collect();
        assert_eq!(
            locations,
            vec![(10, object_id), (11, 0)],
            "古い Group の保留分を先に、Object ID 順に取り出すこと"
        );
        flushed.set(flushed.get() + 1);
        Ok(())
    })?;
    assert!(
        flushed.get() > 0,
        "Group の保留分の取り出しが観測されなかった\n{runner}"
    );
    Ok(())
}

/// 受信列の長さの境界を使い、長い列でも Object を取りこぼさないこと
#[test]
fn long_received_sequences_use_every_object() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(64, |ctx| {
        let step_count = noprop::sample_with_boundaries(
            ctx,
            &STEP_BOUNDARIES,
            noprop::Ratio::one_nth(4),
            |ctx| noprop::sample_usize_in(ctx, 1..=STEP_BOUNDARIES[2]),
        );
        // 1 Group の Object を Object ID の昇順とは限らない順で step_count 個受信する。
        // 各 Object ID は一意であり、配送した数が受信した数と一致することを確かめる
        let mut object_ids: Vec<u64> = (0..step_count as u64).collect();
        for i in (1..object_ids.len()).rev() {
            let j = noprop::sample_usize_in(ctx, 0..=i);
            object_ids.swap(i, j);
        }
        let mut buffer = VideoObjectReorderBuffer::new();
        let mut released = Vec::new();
        for object_id in &object_ids {
            released.extend(buffer.insert(0, *object_id, ()));
        }
        assert_eq!(
            released.len(),
            step_count,
            "受信した Object をすべて取り出すこと: object_ids={object_ids:?}"
        );
        Ok(())
    })?;
    Ok(())
}
