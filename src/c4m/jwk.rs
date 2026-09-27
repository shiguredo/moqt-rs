//! JWK (JSON Web Key, RFC 7517) と JWK サムプリント (RFC 7638)
//!
//! DPoP proof (draft-nandakumar-moq-generic-dpop-proof) の JWT ヘッダに埋め込まれる
//! 公開鍵を扱う。JWK サムプリントは RFC 7638 §3.2 の正規化 JSON (必須メンバーを
//! 辞書順に並べ、空白を入れない) に対する SHA-256 である。

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use nojson::{DisplayJson, JsonFormatter, RawJson, RawJsonValue};

use super::base64url;
use super::crypto::{CoseCrypto, CoseKey, CryptoError, DigestAlgorithm, EcCurve};

/// JWK (RFC 7517 §4)
///
/// `x` / `y` / `n` / `e` は base64url (パディング無し) の文字列として保持する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Jwk {
    /// EC 公開鍵 (`kty` = "EC")
    Ec {
        /// 曲線名 (`crv`)。"P-256" / "P-384" / "P-521"
        curve: String,
        /// x 座標 (base64url)
        x: String,
        /// y 座標 (base64url)
        y: String,
    },
    /// OKP 公開鍵 (`kty` = "OKP")
    Okp {
        /// 曲線名 (`crv`)。"Ed25519"
        curve: String,
        /// 公開鍵 (base64url)
        x: String,
    },
    /// RSA 公開鍵 (`kty` = "RSA")
    Rsa {
        /// モジュラス (base64url)
        n: String,
        /// 公開指数 (base64url)
        e: String,
    },
}

impl Jwk {
    /// JWK の JSON をデコードする
    pub fn decode(text: &str) -> Result<Self, JwkError> {
        let json = RawJson::parse(text).map_err(json_error)?;
        let value = json.value();
        let key_type = required_string(value, "kty")?;
        match key_type.as_str() {
            "EC" => Ok(Self::Ec {
                curve: required_string(value, "crv")?,
                x: required_string(value, "x")?,
                y: required_string(value, "y")?,
            }),
            "OKP" => Ok(Self::Okp {
                curve: required_string(value, "crv")?,
                x: required_string(value, "x")?,
            }),
            "RSA" => Ok(Self::Rsa {
                n: required_string(value, "n")?,
                e: required_string(value, "e")?,
            }),
            other => Err(JwkError::UnsupportedKeyType(String::from(other))),
        }
    }

    /// 公開鍵を [`CoseKey`] へ変換する
    ///
    /// RSA は [`CoseKey`] が表現を持たないためエラーを返す。
    pub fn to_cose_key(&self) -> Result<CoseKey, JwkError> {
        match self {
            Self::Ec { curve, x, y } => {
                let curve = match curve.as_str() {
                    "P-256" => EcCurve::P256,
                    "P-384" => EcCurve::P384,
                    "P-521" => EcCurve::P521,
                    other => return Err(JwkError::UnsupportedCurve(String::from(other))),
                };
                Ok(CoseKey::ec2(
                    curve,
                    decode_base64url(x)?,
                    decode_base64url(y)?,
                ))
            }
            Self::Okp { curve, x } => {
                if curve != "Ed25519" {
                    return Err(JwkError::UnsupportedCurve(curve.clone()));
                }
                Ok(CoseKey::ed25519(decode_base64url(x)?))
            }
            Self::Rsa { .. } => Err(JwkError::UnsupportedOperation),
        }
    }

    /// JWK が指定した鍵と同じ公開鍵を表すかどうかを返す
    ///
    /// DPoP proof を発行するときに、埋め込む JWK と署名鍵の食い違いを検出するために
    /// 使う。RSA は常にエラーを返す。
    pub fn matches_public_key(&self, key: &CoseKey) -> Result<bool, JwkError> {
        let public = self.to_cose_key()?;
        Ok(match (&public, key) {
            (
                CoseKey::Ec2 { curve, x, y, .. },
                CoseKey::Ec2 {
                    curve: key_curve,
                    x: key_x,
                    y: key_y,
                    ..
                },
            ) => curve == key_curve && x == key_x && y == key_y,
            (
                CoseKey::Okp { public_key, .. },
                CoseKey::Okp {
                    public_key: key_public_key,
                    ..
                },
            ) => public_key == key_public_key,
            _ => false,
        })
    }

    /// RFC 7638 §3.2 の正規化 JSON を返す
    ///
    /// 必須メンバーを辞書順に並べ、空白を入れない。base64url の値はパディング無しに
    /// 正規化する。
    pub fn canonical_json(&self) -> Result<String, JwkError> {
        match self {
            Self::Ec { curve, x, y } => {
                let x = normalize_base64url(x)?;
                let y = normalize_base64url(y)?;
                Ok(format!(
                    "{}",
                    nojson::object(|f| {
                        f.member("crv", curve.as_str())?;
                        f.member("kty", "EC")?;
                        f.member("x", x.as_str())?;
                        f.member("y", y.as_str())
                    })
                ))
            }
            Self::Okp { curve, x } => {
                let x = normalize_base64url(x)?;
                Ok(format!(
                    "{}",
                    nojson::object(|f| {
                        f.member("crv", curve.as_str())?;
                        f.member("kty", "OKP")?;
                        f.member("x", x.as_str())
                    })
                ))
            }
            Self::Rsa { n, e } => {
                let n = normalize_base64url(n)?;
                let e = normalize_base64url(e)?;
                Ok(format!(
                    "{}",
                    nojson::object(|f| {
                        f.member("e", e.as_str())?;
                        f.member("kty", "RSA")?;
                        f.member("n", n.as_str())
                    })
                ))
            }
        }
    }

    /// JWK サムプリント (RFC 7638) の SHA-256 を計算する
    pub fn thumbprint_sha256<C: CoseCrypto>(&self, crypto: &C) -> Result<Vec<u8>, JwkError> {
        let canonical = self.canonical_json()?;
        crypto
            .digest(DigestAlgorithm::Sha256, canonical.as_bytes())
            .map_err(JwkError::Crypto)
    }
}

impl DisplayJson for Jwk {
    fn fmt(&self, f: &mut JsonFormatter<'_, '_>) -> fmt::Result {
        match self {
            Self::Ec { curve, x, y } => f.object(|f| {
                f.member("kty", "EC")?;
                f.member("crv", curve.as_str())?;
                f.member("x", x.as_str())?;
                f.member("y", y.as_str())
            }),
            Self::Okp { curve, x } => f.object(|f| {
                f.member("kty", "OKP")?;
                f.member("crv", curve.as_str())?;
                f.member("x", x.as_str())
            }),
            Self::Rsa { n, e } => f.object(|f| {
                f.member("kty", "RSA")?;
                f.member("n", n.as_str())?;
                f.member("e", e.as_str())
            }),
        }
    }
}

/// JWK の操作エラー
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JwkError {
    /// JSON のパースに失敗した
    Json(String),
    /// 必須メンバーが無い
    MissingMember(&'static str),
    /// メンバーの値が不正である
    InvalidValue(&'static str),
    /// 対応していない `kty` である
    UnsupportedKeyType(String),
    /// 対応していない `crv` である
    UnsupportedCurve(String),
    /// この鍵では実行できない操作である (RSA の COSE 変換など)
    UnsupportedOperation,
    /// base64url のデコードに失敗した
    InvalidBase64,
    /// ハッシュの計算に失敗した
    Crypto(CryptoError),
}

impl fmt::Display for JwkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(message) => write!(f, "invalid JWK JSON: {message}"),
            Self::MissingMember(name) => write!(f, "JWK member is missing: {name}"),
            Self::InvalidValue(name) => write!(f, "invalid JWK member: {name}"),
            Self::UnsupportedKeyType(key_type) => {
                write!(f, "unsupported JWK key type: {key_type}")
            }
            Self::UnsupportedCurve(curve) => write!(f, "unsupported JWK curve: {curve}"),
            Self::UnsupportedOperation => write!(f, "operation is not supported for this JWK"),
            Self::InvalidBase64 => write!(f, "invalid base64url in JWK"),
            Self::Crypto(error) => write!(f, "crypto error: {error}"),
        }
    }
}

impl core::error::Error for JwkError {}

impl From<CryptoError> for JwkError {
    fn from(error: CryptoError) -> Self {
        Self::Crypto(error)
    }
}

fn json_error(error: nojson::JsonParseError) -> JwkError {
    JwkError::Json(error.to_string())
}

/// 必須の文字列メンバーを取り出す
fn required_string(value: RawJsonValue<'_, '_>, name: &'static str) -> Result<String, JwkError> {
    let text: String = value
        .to_member(name)
        .map_err(json_error)?
        .required()
        .map_err(json_error)?
        .try_into()
        .map_err(json_error)?;
    Ok(text)
}

/// base64url をデコードする
fn decode_base64url(text: &str) -> Result<Vec<u8>, JwkError> {
    base64url::decode(text).map_err(|_| JwkError::InvalidBase64)
}

/// base64url をパディング無しに正規化する
fn normalize_base64url(text: &str) -> Result<String, JwkError> {
    Ok(base64url::encode(&decode_base64url(text)?))
}
