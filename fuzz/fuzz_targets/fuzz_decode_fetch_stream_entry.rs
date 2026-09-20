#![no_main]

use libfuzzer_sys::fuzz_target;

use shiguredo_moqt::stream::fetch::{FetchPriorContext, FetchStreamEntry};

fuzz_target!(|data: &[u8]| {
    // すべての prior 参照文脈でテストする
    let _ = FetchStreamEntry::decode(data, FetchPriorContext::First);
    let _ = FetchStreamEntry::decode(data, FetchPriorContext::NoPriorActualObject);
    let _ = FetchStreamEntry::decode(data, FetchPriorContext::HasPriorObject);
});
