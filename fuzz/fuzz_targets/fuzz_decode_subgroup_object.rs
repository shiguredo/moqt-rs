#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // has_properties = false (Properties なしの Object)
    let _ = shiguredo_moqt::stream::subgroup::SubgroupObject::decode(data, false);
    // has_properties = true (Properties ありの Object)
    let _ = shiguredo_moqt::stream::subgroup::SubgroupObject::decode(data, true);
});
