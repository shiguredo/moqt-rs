#![no_main]

use libfuzzer_sys::fuzz_target;

// 任意バイト列の data_raw に対する encode のクラッシュ耐性を検証する
// (正常系の正しさは単体テスト・PBT が担い、ここでは panic しないことだけ見る)
fuzz_target!(|data: &[u8]| {
    use shiguredo_moqt::msf::{MsfEventIndex, MsfEventTimeline, MsfEventTimelineEntry};
    let timeline = MsfEventTimeline(vec![MsfEventTimelineEntry {
        index: MsfEventIndex::WallclockMs(0),
        data_raw: data.to_vec(),
    }]);
    let _ = timeline.encode();
});
