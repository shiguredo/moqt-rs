#![no_main]

use libfuzzer_sys::fuzz_target;

// 任意バイトを COSE メッセージ (COSE_Sign1 / COSE_Mac0、CWT タグ付きを含む) としてデコードし、
// protected / unprotected を統合したヘッダの組み立てと、署名 / MAC 対象 (Sig_structure /
// MAC_structure) の組み立てが panic しないことを fuzz する
fuzz_target!(|data: &[u8]| {
    if let Ok(message) = shiguredo_moqt::c4m::cose::CoseMessage::decode(data) {
        // ヘッダは protected / unprotected の重複や detached payload をエラーにするため、
        // 成功 / 失敗のどちらの経路も通す
        let _ = message.header();
        let _ = message.payload();
        let _ = message.signature();
        let _ = message.signing_input();
    }
});
