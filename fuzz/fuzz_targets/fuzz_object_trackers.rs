#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use shiguredo_moqt::object_properties::{ObjectFieldTracker, ObjectPropertyTracker};

/// Object ID / Group ID を畳む空間の大きさ
///
/// 生の `u64` のままだと重複 Object が実質発生せず、重複検証の分岐に到達しない。
const FIELD_ID_SPACE: u64 = 8;

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
    /// IMMUTABLE_PROPERTIES (0x0B) の内側の生バイト列 (draft §10.7 (Immutable Properties))
    immutable_properties: Option<Vec<u8>>,
    /// payload の比較キー (可変長。Session は status と payload 長を符号化した 18 バイトを渡す)
    payload_key: Option<Vec<u8>>,
}

// 任意の Object 列で ObjectPropertyTracker / ObjectFieldTracker が panic しない
fuzz_target!(|observes: Vec<Observe>| {
    let mut properties = ObjectPropertyTracker::new();
    // 5 引数 API と内容込み API は別の tracker に観測させる。同じ tracker に順に観測させると
    // 内容込み API が常に 2 回目以降の観測になり、初回記録の分岐に到達しない。
    let mut fields = ObjectFieldTracker::new(true);
    let mut fields_with_content = ObjectFieldTracker::new(true);
    for observe in &observes {
        // 重複 Object の比較分岐に到達させるため、ID 空間を小さく畳む
        let group_id = observe.group_id % FIELD_ID_SPACE;
        let object_id = observe.object_id % FIELD_ID_SPACE;
        let _ = properties.observe_object(
            group_id,
            object_id,
            Some(observe.properties.as_slice()),
        );
        let _ = fields.observe_object_fields(
            group_id,
            object_id,
            observe.is_subgroup,
            observe.subgroup_id,
            observe.publisher_priority,
        );
        // 条件 6 (draft §12.1 (Malformed Tracks)) の内容込み比較も任意入力で panic しない
        let _ = fields_with_content.observe_object_fields_with_content(
            group_id,
            object_id,
            observe.is_subgroup,
            observe.subgroup_id,
            observe.publisher_priority,
            observe.immutable_properties.as_deref(),
            observe.payload_key.as_deref(),
        );
        // prune の対象 group も畳む。生の u64 だと「全削除」か「全保持」に落ち、
        // 一部だけを残す retain の分岐に到達しない。
        let prune_group = observe.prune_group % FIELD_ID_SPACE;
        properties.prune_past_groups(observe.ascending, prune_group);
        fields.prune_past_groups(observe.ascending, prune_group);
        fields_with_content.prune_past_groups(observe.ascending, prune_group);
    }
});
