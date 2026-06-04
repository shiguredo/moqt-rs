#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use shiguredo_moqt::msf::MsfTemplate;

/// MsfTemplate の全フィールドと解決する index
#[derive(Arbitrary, Debug)]
struct Input {
    start_media_time: u64,
    delta_media_time: u64,
    start_group_id: u64,
    start_object_id: u64,
    delta_group_id: u64,
    delta_object_id: u64,
    start_wallclock: u64,
    delta_wallclock: u64,
    n: u64,
}

// 任意の系列・delta・index で resolve_entry が overflow して panic しない
fuzz_target!(|input: Input| {
    let template = MsfTemplate {
        start_media_time: input.start_media_time,
        delta_media_time: input.delta_media_time,
        start_group_id: input.start_group_id,
        start_object_id: input.start_object_id,
        delta_group_id: input.delta_group_id,
        delta_object_id: input.delta_object_id,
        start_wallclock: input.start_wallclock,
        delta_wallclock: input.delta_wallclock,
    };
    let _ = template.resolve_entry(input.n);
});
