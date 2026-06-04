#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = shiguredo_moqt::stream::subgroup::SubgroupHeader::decode(data);
});
