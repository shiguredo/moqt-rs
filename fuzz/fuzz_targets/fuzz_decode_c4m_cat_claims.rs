#![no_main]

use libfuzzer_sys::fuzz_target;

// 任意バイトを CBOR としてデコードし、その Value を CAT の各クレーム型として解釈して
// 型付きエンコードと CBOR 再エンコードまで panic しないことを fuzz する
//
// デコードで得たクレームの再エンコードは表現を正規化する (`aud` の単一テキストは配列に
// なり、`cti` のテキストはバイト文字列になる) ため、往復一致の assert はしない。
fuzz_target!(|data: &[u8]| {
    let Ok(value) = shiguredo_moqt::c4m::cbor::decode(data) else {
        return;
    };
    // クレームセット全体 (`cnf` / `moqt` / `catdpop` などの型付きクレームを含む)
    if let Ok(claims) = shiguredo_moqt::c4m::cat::CatClaims::decode(&value)
        && let Ok(encoded) = claims.encode()
    {
        let _ = shiguredo_moqt::c4m::cbor::encode(&encoded);
    }
    // 個別のクレーム型。同じ Value をそれぞれの型として解釈させる
    if let Ok(confirmation) = shiguredo_moqt::c4m::cat::Confirmation::decode(&value) {
        let _ = shiguredo_moqt::c4m::cbor::encode(&confirmation.encode());
    }
    if let Ok(moqt) = shiguredo_moqt::c4m::MoqtClaim::decode(&value)
        && let Ok(encoded) = moqt.encode()
    {
        let _ = shiguredo_moqt::c4m::cbor::encode(&encoded);
    }
    if let Ok(scope) = shiguredo_moqt::c4m::MoqtScope::decode(&value)
        && let Ok(encoded) = scope.encode()
    {
        let _ = shiguredo_moqt::c4m::cbor::encode(&encoded);
    }
    if let Ok(catdpop) = shiguredo_moqt::c4m::CatDpop::decode(&value)
        && let Ok(encoded) = catdpop.encode()
    {
        let _ = shiguredo_moqt::c4m::cbor::encode(&encoded);
    }
});
