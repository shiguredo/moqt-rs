#![no_main]

use libfuzzer_sys::fuzz_target;

use shiguredo_moqt::loc::LocProperties;

fuzz_target!(|data: &[u8]| {
    let _ = LocProperties::decode(data);
});
