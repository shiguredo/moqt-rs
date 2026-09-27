//! aws-lc-rs を使う署名 / 検証の実装
//!
//! feature `aws-lc-rs` を有効にした場合だけコンパイルされる。aws-lc-rs は no_std
//! ターゲットでビルドできないため、既定ビルドではこの実装はリンクされない。

use alloc::vec::Vec;

use ::aws_lc_rs::digest;
use ::aws_lc_rs::hmac;
use ::aws_lc_rs::rand::SystemRandom;
use ::aws_lc_rs::signature::{
    ECDSA_P256_SHA256_FIXED, ECDSA_P256_SHA256_FIXED_SIGNING, ECDSA_P384_SHA384_FIXED,
    ECDSA_P384_SHA384_FIXED_SIGNING, ECDSA_P521_SHA512_FIXED, ECDSA_P521_SHA512_FIXED_SIGNING,
    ED25519, EcdsaKeyPair, EcdsaSigningAlgorithm, EcdsaVerificationAlgorithm, Ed25519KeyPair,
    UnparsedPublicKey,
};

use super::{CoseCrypto, CoseKey, CryptoError, DigestAlgorithm, EcCurve, OkpCurve};
use crate::c4m::cose::Algorithm;

/// aws-lc-rs を使う [`CoseCrypto`] 実装
#[derive(Debug, Clone, Copy, Default)]
pub struct AwsLcRsCrypto;

impl AwsLcRsCrypto {
    /// 実装を作る
    pub fn new() -> Self {
        Self
    }
}

impl CoseCrypto for AwsLcRsCrypto {
    fn sign(
        &self,
        algorithm: Algorithm,
        key: &CoseKey,
        message: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        match algorithm {
            Algorithm::HmacSha256 | Algorithm::HmacSha384 | Algorithm::HmacSha512 => {
                let hmac_algorithm =
                    hmac_algorithm(algorithm).ok_or(CryptoError::UnsupportedKey)?;
                hmac_sign(hmac_algorithm, key, message)
            }
            Algorithm::Es256 | Algorithm::Es384 | Algorithm::Es512 => {
                let (signing_algorithm, curve) =
                    ecdsa_signing_algorithm(algorithm).ok_or(CryptoError::UnsupportedKey)?;
                ecdsa_sign(signing_algorithm, curve, key, message)
            }
            Algorithm::EdDsa => ed25519_sign(key, message),
        }
    }

    fn verify(
        &self,
        algorithm: Algorithm,
        key: &CoseKey,
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), CryptoError> {
        match algorithm {
            Algorithm::HmacSha256 | Algorithm::HmacSha384 | Algorithm::HmacSha512 => {
                let hmac_algorithm =
                    hmac_algorithm(algorithm).ok_or(CryptoError::UnsupportedKey)?;
                hmac_verify(hmac_algorithm, key, message, signature)
            }
            Algorithm::Es256 | Algorithm::Es384 | Algorithm::Es512 => {
                let (verification_algorithm, curve) =
                    ecdsa_verification_algorithm(algorithm).ok_or(CryptoError::UnsupportedKey)?;
                ecdsa_verify(verification_algorithm, curve, key, message, signature)
            }
            Algorithm::EdDsa => ed25519_verify(key, message, signature),
        }
    }

    fn digest(&self, algorithm: DigestAlgorithm, message: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let algorithm = match algorithm {
            DigestAlgorithm::Sha256 => &digest::SHA256,
            DigestAlgorithm::Sha384 => &digest::SHA384,
            DigestAlgorithm::Sha512 => &digest::SHA512,
        };
        Ok(digest::digest(algorithm, message).as_ref().to_vec())
    }
}

/// アルゴリズムに対応する HMAC の定義を返す
fn hmac_algorithm(algorithm: Algorithm) -> Option<&'static hmac::Algorithm> {
    match algorithm {
        Algorithm::HmacSha256 => Some(&hmac::HMAC_SHA256),
        Algorithm::HmacSha384 => Some(&hmac::HMAC_SHA384),
        Algorithm::HmacSha512 => Some(&hmac::HMAC_SHA512),
        _ => None,
    }
}

/// アルゴリズムに対応する ECDSA の署名定義と曲線を返す
fn ecdsa_signing_algorithm(
    algorithm: Algorithm,
) -> Option<(&'static EcdsaSigningAlgorithm, EcCurve)> {
    match algorithm {
        Algorithm::Es256 => Some((&ECDSA_P256_SHA256_FIXED_SIGNING, EcCurve::P256)),
        Algorithm::Es384 => Some((&ECDSA_P384_SHA384_FIXED_SIGNING, EcCurve::P384)),
        Algorithm::Es512 => Some((&ECDSA_P521_SHA512_FIXED_SIGNING, EcCurve::P521)),
        _ => None,
    }
}

/// アルゴリズムに対応する ECDSA の検証定義と曲線を返す
fn ecdsa_verification_algorithm(
    algorithm: Algorithm,
) -> Option<(&'static EcdsaVerificationAlgorithm, EcCurve)> {
    match algorithm {
        Algorithm::Es256 => Some((&ECDSA_P256_SHA256_FIXED, EcCurve::P256)),
        Algorithm::Es384 => Some((&ECDSA_P384_SHA384_FIXED, EcCurve::P384)),
        Algorithm::Es512 => Some((&ECDSA_P521_SHA512_FIXED, EcCurve::P521)),
        _ => None,
    }
}

/// 対称鍵を取り出す
fn symmetric_key(key: &CoseKey) -> Result<&[u8], CryptoError> {
    match key {
        CoseKey::Symmetric { key } => {
            if key.is_empty() {
                return Err(CryptoError::InvalidKey);
            }
            Ok(key)
        }
        _ => Err(CryptoError::UnsupportedKey),
    }
}

/// EC2 鍵から SEC1 の非圧縮形式の公開鍵を作る
fn ec2_public_key(curve: EcCurve, key: &CoseKey) -> Result<Vec<u8>, CryptoError> {
    let CoseKey::Ec2 {
        curve: key_curve,
        x,
        y,
        ..
    } = key
    else {
        return Err(CryptoError::UnsupportedKey);
    };
    if *key_curve != curve {
        return Err(CryptoError::UnsupportedKey);
    }
    let length = curve.coordinate_length();
    if x.len() != length || y.len() != length {
        return Err(CryptoError::InvalidKey);
    }
    let mut public_key = Vec::new();
    // SEC 1 の非圧縮形式 (0x04 || x || y)
    public_key.push(0x04);
    public_key.extend_from_slice(x);
    public_key.extend_from_slice(y);
    Ok(public_key)
}

/// EC2 鍵から秘密鍵 (スカラー) を取り出す
fn ec2_private_key(curve: EcCurve, key: &CoseKey) -> Result<&[u8], CryptoError> {
    let CoseKey::Ec2 { private_key, .. } = key else {
        return Err(CryptoError::UnsupportedKey);
    };
    let private_key = private_key
        .as_deref()
        .ok_or(CryptoError::MissingPrivateKey)?;
    if private_key.len() != curve.coordinate_length() {
        return Err(CryptoError::InvalidKey);
    }
    Ok(private_key)
}

fn hmac_sign(
    algorithm: &'static hmac::Algorithm,
    key: &CoseKey,
    message: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let key = symmetric_key(key)?;
    Ok(hmac::sign(&hmac::Key::new(*algorithm, key), message)
        .as_ref()
        .to_vec())
}

fn hmac_verify(
    algorithm: &'static hmac::Algorithm,
    key: &CoseKey,
    message: &[u8],
    signature: &[u8],
) -> Result<(), CryptoError> {
    let key = symmetric_key(key)?;
    hmac::verify(&hmac::Key::new(*algorithm, key), message, signature)
        .map_err(|_| CryptoError::SignatureVerificationFailed)
}

fn ecdsa_sign(
    algorithm: &'static EcdsaSigningAlgorithm,
    curve: EcCurve,
    key: &CoseKey,
    message: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let public_key = ec2_public_key(curve, key)?;
    let private_key = ec2_private_key(curve, key)?;
    let key_pair =
        EcdsaKeyPair::from_private_key_and_public_key(algorithm, private_key, &public_key)
            .map_err(|_| CryptoError::InvalidKey)?;
    let rng = SystemRandom::new();
    let signature = key_pair
        .sign(&rng, message)
        .map_err(|_| CryptoError::SigningFailed)?;
    Ok(signature.as_ref().to_vec())
}

fn ecdsa_verify(
    algorithm: &'static EcdsaVerificationAlgorithm,
    curve: EcCurve,
    key: &CoseKey,
    message: &[u8],
    signature: &[u8],
) -> Result<(), CryptoError> {
    let public_key = ec2_public_key(curve, key)?;
    if signature.len() != curve.coordinate_length() * 2 {
        return Err(CryptoError::InvalidSignatureLength);
    }
    UnparsedPublicKey::new(algorithm, public_key)
        .verify(message, signature)
        .map_err(|_| CryptoError::SignatureVerificationFailed)
}

/// Ed25519 の鍵 (公開鍵と秘密鍵の種) を取り出す
fn ed25519_key_pair(key: &CoseKey) -> Result<(&[u8], Option<&[u8]>), CryptoError> {
    let CoseKey::Okp {
        curve: OkpCurve::Ed25519,
        public_key,
        private_key,
    } = key
    else {
        return Err(CryptoError::UnsupportedKey);
    };
    if public_key.len() != 32 {
        return Err(CryptoError::InvalidKey);
    }
    Ok((public_key, private_key.as_deref()))
}

fn ed25519_sign(key: &CoseKey, message: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let (public_key, private_key) = ed25519_key_pair(key)?;
    let private_key = private_key.ok_or(CryptoError::MissingPrivateKey)?;
    if private_key.len() != 32 {
        return Err(CryptoError::InvalidKey);
    }
    let key_pair = Ed25519KeyPair::from_seed_and_public_key(private_key, public_key)
        .map_err(|_| CryptoError::InvalidKey)?;
    Ok(key_pair.sign(message).as_ref().to_vec())
}

fn ed25519_verify(key: &CoseKey, message: &[u8], signature: &[u8]) -> Result<(), CryptoError> {
    let (public_key, _) = ed25519_key_pair(key)?;
    if signature.len() != 64 {
        return Err(CryptoError::InvalidSignatureLength);
    }
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(message, signature)
        .map_err(|_| CryptoError::SignatureVerificationFailed)
}
