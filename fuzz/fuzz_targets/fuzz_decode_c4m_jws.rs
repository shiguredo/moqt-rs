#![no_main]

use libfuzzer_sys::fuzz_target;

// 任意バイトを JWS compact 形式の JWT (RFC 7515 §7.1) として解釈し、JOSE ヘッダの JSON パースと
// header.payload.signature の分割デコードが panic しないことを fuzz する
//
// 署名検証は行わない。fuzz クレートは aws-lc-rs feature を有効にしていないため、
// JwsCompact::verify は呼べない。
fuzz_target!(|data: &[u8]| {
    // JWT はテキスト形式のため、UTF-8 として解釈できるときだけ渡す
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    // JOSE ヘッダ単体のデコード
    let _ = shiguredo_moqt::c4m::jwt::JwsHeader::decode(text);
    // JWS compact (header.payload.signature) のデコード
    if let Ok(jws) = shiguredo_moqt::c4m::jwt::JwsCompact::decode(text) {
        // デコード結果のアクセサを一通り呼ぶ。ヘッダの各メンバーとペイロード / 署名 /
        // 署名対象の参照を取得しても panic しないことを確認する
        let _ = jws.header().algorithm;
        let _ = jws.header().typ.as_deref();
        let _ = jws.header().key_id.as_deref();
        let _ = jws.header().jwk.as_ref();
        let _ = jws.payload().len();
        let _ = jws.signature().len();
        let _ = jws.signing_input().len();
    }
});
