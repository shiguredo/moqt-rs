//! JWS compact 形式の JWT (RFC 7515 §7.1)
//!
//! JWT のヘッダ (JSON) をパースし、署名対象 (`base64url(header).base64url(payload)`) と
//! 署名を保持する。ペイロードの解釈は利用側 (DPoP proof など) が行う。

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use nojson::{RawJson, RawJsonValue};

use super::base64url;
use super::cose::Algorithm;
use super::crypto::{CoseCrypto, CoseKey, CryptoError};
use super::jwk::{Jwk, JwkError};

/// JWS compact のヘッダ (RFC 7515 §4)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JwsHeader {
    /// `alg` を COSE のアルゴリズムへ変換したもの
    pub algorithm: Algorithm,
    /// `typ`
    pub typ: Option<String>,
    /// `kid`
    pub key_id: Option<String>,
    /// `jwk` (DPoP proof が埋め込む公開鍵)
    pub jwk: Option<Jwk>,
}

impl JwsHeader {
    /// ヘッダの JSON をデコードする
    ///
    /// JOSE ヘッダのメンバー名が重複している場合はエラーを返す (RFC 7515 §4)。
    pub fn decode(text: &str) -> Result<Self, JwtError> {
        let json = RawJson::parse(text).map_err(json_error)?;
        let value = json.value();
        if let Some(name) = super::json::find_duplicate_member(value).map_err(json_error)? {
            return Err(JwtError::DuplicateMember(name));
        }
        let algorithm_name = required_string(value, "alg")?;
        let algorithm = Algorithm::from_jose_name(&algorithm_name)
            .ok_or_else(|| JwtError::UnsupportedAlgorithm(algorithm_name.clone()))?;
        // RFC 7515 §4.1.11: crit に挙げた拡張ヘッダを理解できない場合は JWS を無効とする。
        // 本実装は拡張ヘッダを 1 つも解釈しないため、crit を持つ JWS は拒否する
        if value
            .to_member("crit")
            .map_err(json_error)?
            .optional()
            .is_some()
        {
            return Err(JwtError::UnsupportedCriticalHeader);
        }
        let typ = optional_string(value, "typ")?;
        let key_id = optional_string(value, "kid")?;
        let jwk = match value.to_member("jwk").map_err(json_error)?.optional() {
            Some(member) => Some(Jwk::decode(member.extract().text())?),
            None => None,
        };
        Ok(Self {
            algorithm,
            typ,
            key_id,
            jwk,
        })
    }
}

/// JWS compact 形式の JWT (RFC 7515 §7.1)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JwsCompact {
    header: JwsHeader,
    payload: Vec<u8>,
    signature: Vec<u8>,
    signing_input: Vec<u8>,
}

impl JwsCompact {
    /// `header.payload.signature` の 3 分割形式をデコードする
    pub fn decode(input: &str) -> Result<Self, JwtError> {
        let parts: Vec<&str> = input.split('.').collect();
        if parts.len() != 3 {
            return Err(JwtError::InvalidTokenFormat);
        }
        let header_bytes = decode_segment(parts[0])?;
        let payload = decode_segment(parts[1])?;
        let signature = decode_segment(parts[2])?;
        let header_text =
            core::str::from_utf8(&header_bytes).map_err(|_| JwtError::InvalidTokenFormat)?;
        let header = JwsHeader::decode(header_text)?;
        // 署名対象は base64url のままの header と payload (RFC 7515 §7.1)
        let signing_input = format!("{}.{}", parts[0], parts[1]).into_bytes();
        Ok(Self {
            header,
            payload,
            signature,
            signing_input,
        })
    }

    /// ヘッダを返す
    pub fn header(&self) -> &JwsHeader {
        &self.header
    }

    /// ペイロードの生バイト列を返す
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// 署名を返す
    pub fn signature(&self) -> &[u8] {
        &self.signature
    }

    /// 署名対象のバイト列を返す
    pub fn signing_input(&self) -> &[u8] {
        &self.signing_input
    }

    /// 署名を検証する
    pub fn verify<C: CoseCrypto>(&self, crypto: &C, key: &CoseKey) -> Result<(), JwtError> {
        crypto
            .verify(
                self.header.algorithm,
                key,
                &self.signing_input,
                &self.signature,
            )
            .map_err(JwtError::Crypto)
    }
}

/// JWT のエラー
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JwtError {
    /// 3 分割の形式ではない
    InvalidTokenFormat,
    /// base64url のデコードに失敗した
    InvalidBase64,
    /// JSON のパースに失敗した
    Json(String),
    /// メンバー名が重複している (RFC 7515 §4)
    DuplicateMember(String),
    /// `crit` に理解できない拡張ヘッダがある (RFC 7515 §4.1.11)
    UnsupportedCriticalHeader,
    /// 対応していない `alg` である
    UnsupportedAlgorithm(String),
    /// 署名 / 検証のエラー
    Crypto(CryptoError),
    /// JWK のエラー
    Jwk(JwkError),
}

impl fmt::Display for JwtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTokenFormat => write!(f, "JWT must have 3 dot-separated parts"),
            Self::InvalidBase64 => write!(f, "invalid base64url in JWT"),
            Self::Json(message) => write!(f, "invalid JWT JSON: {message}"),
            Self::DuplicateMember(name) => write!(f, "duplicate JWT member: {name}"),
            Self::UnsupportedCriticalHeader => {
                write!(f, "crit lists an extension header that is not understood")
            }
            Self::UnsupportedAlgorithm(name) => write!(f, "unsupported JWT algorithm: {name}"),
            Self::Crypto(error) => write!(f, "crypto error: {error}"),
            Self::Jwk(error) => write!(f, "JWK error: {error}"),
        }
    }
}

impl core::error::Error for JwtError {}

impl From<JwkError> for JwtError {
    fn from(error: JwkError) -> Self {
        Self::Jwk(error)
    }
}

impl From<CryptoError> for JwtError {
    fn from(error: CryptoError) -> Self {
        Self::Crypto(error)
    }
}

fn json_error(error: nojson::JsonParseError) -> JwtError {
    JwtError::Json(error.to_string())
}

fn decode_segment(text: &str) -> Result<Vec<u8>, JwtError> {
    base64url::decode(text).map_err(|_| JwtError::InvalidBase64)
}

/// 必須の文字列メンバーを取り出す
fn required_string(value: RawJsonValue<'_, '_>, name: &'static str) -> Result<String, JwtError> {
    let text: String = value
        .to_member(name)
        .map_err(json_error)?
        .required()
        .map_err(json_error)?
        .try_into()
        .map_err(json_error)?;
    Ok(text)
}

/// 任意の文字列メンバーを取り出す
fn optional_string(
    value: RawJsonValue<'_, '_>,
    name: &'static str,
) -> Result<Option<String>, JwtError> {
    let Some(member) = value.to_member(name).map_err(json_error)?.optional() else {
        return Ok(None);
    };
    let text: String = member.try_into().map_err(json_error)?;
    Ok(Some(text))
}
