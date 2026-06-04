#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use shiguredo_moqt::subgroup_tracker::SubgroupTracker;

/// SubgroupTracker へ与える 1 操作
#[derive(Arbitrary, Debug)]
enum Op {
    Open {
        track_alias: u64,
        group_id: u64,
        subgroup_id: u64,
    },
    MarkFin {
        track_alias: u64,
        group_id: u64,
        subgroup_id: u64,
        last_object_id: Option<u64>,
    },
    MarkReset {
        track_alias: u64,
        group_id: u64,
        subgroup_id: u64,
        reliable_size: Option<u64>,
    },
    MarkStopSending {
        track_alias: u64,
        group_id: u64,
        subgroup_id: u64,
    },
    RecordPriority {
        track_alias: u64,
        group_id: u64,
        subgroup_id: u64,
        publisher_priority: u8,
    },
    CheckObjectAfterFin {
        track_alias: u64,
        group_id: u64,
        subgroup_id: u64,
        object_id: u64,
    },
    Get {
        track_alias: u64,
        group_id: u64,
        subgroup_id: u64,
    },
    RemoveTrackAlias {
        track_alias: u64,
    },
}

fuzz_target!(|ops: Vec<Op>| {
    let mut tracker = SubgroupTracker::new();
    for op in ops {
        match op {
            Op::Open {
                track_alias,
                group_id,
                subgroup_id,
            } => {
                let _ = tracker.open(track_alias, group_id, subgroup_id);
            }
            Op::MarkFin {
                track_alias,
                group_id,
                subgroup_id,
                last_object_id,
            } => {
                let _ = tracker.mark_fin(track_alias, group_id, subgroup_id, last_object_id);
            }
            Op::MarkReset {
                track_alias,
                group_id,
                subgroup_id,
                reliable_size,
            } => {
                tracker.mark_reset(track_alias, group_id, subgroup_id, reliable_size);
            }
            Op::MarkStopSending {
                track_alias,
                group_id,
                subgroup_id,
            } => {
                tracker.mark_stop_sending(track_alias, group_id, subgroup_id);
            }
            Op::RecordPriority {
                track_alias,
                group_id,
                subgroup_id,
                publisher_priority,
            } => {
                let _ =
                    tracker.record_priority(track_alias, group_id, subgroup_id, publisher_priority);
            }
            Op::CheckObjectAfterFin {
                track_alias,
                group_id,
                subgroup_id,
                object_id,
            } => {
                let _ =
                    tracker.check_object_after_fin(track_alias, group_id, subgroup_id, object_id);
            }
            Op::Get {
                track_alias,
                group_id,
                subgroup_id,
            } => {
                let _ = tracker.get(track_alias, group_id, subgroup_id);
            }
            Op::RemoveTrackAlias { track_alias } => {
                tracker.remove_track_alias(track_alias);
            }
        }
    }
});
