#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use shiguredo_moqt::object_properties::{ObjectFieldTracker, ObjectPropertyTracker};

/// 1 オブジェクト分の観測
#[derive(Arbitrary, Debug)]
struct Observe {
    group_id: u64,
    object_id: u64,
    properties: Vec<u8>,
    is_subgroup: bool,
    subgroup_id: Option<u64>,
    publisher_priority: u8,
    ascending: bool,
    prune_group: u64,
}

// 任意の Object 列で ObjectPropertyTracker / ObjectFieldTracker が panic しない
fuzz_target!(|observes: Vec<Observe>| {
    let mut properties = ObjectPropertyTracker::new();
    let mut fields = ObjectFieldTracker::new();
    for observe in &observes {
        let _ = properties.observe_object(
            observe.group_id,
            observe.object_id,
            Some(observe.properties.as_slice()),
        );
        let _ = fields.observe_object_fields(
            observe.group_id,
            observe.object_id,
            observe.is_subgroup,
            observe.subgroup_id,
            observe.publisher_priority,
        );
        properties.prune_past_groups(observe.ascending, observe.prune_group);
        fields.prune_past_groups(observe.ascending, observe.prune_group);
    }
});
