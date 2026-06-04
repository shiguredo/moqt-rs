#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = shiguredo_moqt::stream::datagram::ObjectDatagram::decode(data);
});
