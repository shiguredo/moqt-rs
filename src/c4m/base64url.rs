//! base64url (RFC 4648 §5) のエンコード / デコード
//!
//! CAT の compact 形式 (draft-ietf-moq-c4m-01 付録 A) と JWS compact (RFC 7515) は
//! パディング無しの base64url を使う。JWK (RFC 7517) の値はパディング無しが仕様で
//! あるが、パディング付きの入力も受理する。

use alloc::string::String;
use alloc::vec::Vec;

use base64ct::{Base64Url, Base64UrlUnpadded, Encoding};

/// パディング無しの base64url でエンコードする
pub(crate) fn encode(bytes: &[u8]) -> String {
    Base64UrlUnpadded::encode_string(bytes)
}

/// base64url をデコードする
///
/// パディング無しを優先し、失敗した場合はパディング付きとして再試行する。
pub(crate) fn decode(text: &str) -> Result<Vec<u8>, ()> {
    if let Ok(bytes) = Base64UrlUnpadded::decode_vec(text) {
        return Ok(bytes);
    }
    Base64Url::decode_vec(text).map_err(|_| ())
}
