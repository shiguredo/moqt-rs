//! base64url (RFC 4648 §5) と標準 Base64 (§4) のエンコード / デコード
//!
//! CAT の compact 形式 (draft-ietf-moq-c4m-01 付録 A) と JWS compact (RFC 7515) は
//! パディング無しの base64url を使う。JWK (RFC 7517) の値はパディング無しが仕様で
//! あるが、パディング付きの入力も受理する。また、CAT トークンを URL に埋め込む
//! 場合は標準 Base64 (RFC 4648 §4) が使われる (§2 / §4) ため、トークン全体の
//! デコードでは両方を受理する。

use alloc::string::String;
use alloc::vec::Vec;

use base64ct::{Base64, Base64Unpadded, Base64Url, Base64UrlUnpadded, Encoding};

/// パディング無しの base64url でエンコードする
pub(crate) fn encode(bytes: &[u8]) -> String {
    Base64UrlUnpadded::encode_string(bytes)
}

/// base64url をデコードする
///
/// パディング無しを優先し、失敗した場合はパディング付きとして再試行する。JWK だけで
/// なく、JWS compact (RFC 7515 §2 はパディング無しを前提とする) と CAT の compact
/// 形式でもパディング付きを受理する。
pub(crate) fn decode(text: &str) -> Result<Vec<u8>, ()> {
    if let Ok(bytes) = Base64UrlUnpadded::decode_vec(text) {
        return Ok(bytes);
    }
    Base64Url::decode_vec(text).map_err(|_| ())
}

/// base64url または標準 Base64 (RFC 4648 §4) をデコードする
///
/// CAT トークンを URL に埋め込む場合は標準 Base64 が使われるため
/// (draft-ietf-moq-c4m-01 §2 / §4)、トークン全体のデコードでは両方を受理する。
/// base64url を優先し、パディングの有無も両方を受ける。
pub(crate) fn decode_base64_or_url(text: &str) -> Result<Vec<u8>, ()> {
    if let Ok(bytes) = Base64UrlUnpadded::decode_vec(text) {
        return Ok(bytes);
    }
    if let Ok(bytes) = Base64Url::decode_vec(text) {
        return Ok(bytes);
    }
    if let Ok(bytes) = Base64::decode_vec(text) {
        return Ok(bytes);
    }
    Base64Unpadded::decode_vec(text).map_err(|_| ())
}
