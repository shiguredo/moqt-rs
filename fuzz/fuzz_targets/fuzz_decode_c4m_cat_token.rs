#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // compact 形式 / COSE 形式 / base64url を包んだ形式の自動判別を含めてデコードする
    let _ = shiguredo_moqt::c4m::cat::CatToken::decode(data);
});
