#![no_main]

use libfuzzer_sys::fuzz_target;

use shiguredo_moqt::object_properties::ObjectProperties;

fuzz_target!(|data: &[u8]| {
    let _ = ObjectProperties::decode(data);
});
