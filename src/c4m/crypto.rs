//! COSE / JWT の署名と検証を抽象化する trait
//!
//! 既定ビルドでは trait と鍵表現だけを提供し、暗号実装を一切リンクしない。
//! `aws-lc-rs` feature を有効にすると `aws_lc_rs::AwsLcRsCrypto` が使える。
//!
//! Sans I/O の方針に合わせて、このモジュールは乱数も時計も持たない。署名に必要な
//! 乱数は実装側 (aws-lc-rs) が内部で取得する。

use alloc::vec::Vec;
use core::fmt;

use super::cose::Algorithm;

/// 楕円曲線 (RFC 9053 §7 の COSE Elliptic Curves)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EcCurve {
    /// NIST P-256 (secp256r1、値 1)
    P256,
    /// NIST P-384 (secp384r1、値 2)
    P384,
    /// NIST P-521 (secp521r1、値 3)
    P521,
}

impl EcCurve {
    /// 座標と秘密鍵のバイト長を返す
    pub const fn coordinate_length(self) -> usize {
        match self {
            Self::P256 => 32,
            Self::P384 => 48,
            Self::P521 => 66,
        }
    }

    /// COSE の `crv` 値 (RFC 9053 §7) を返す
    pub const fn identifier(self) -> i64 {
        match self {
            Self::P256 => 1,
            Self::P384 => 2,
            Self::P521 => 3,
        }
    }
}

/// OKP (Octet Key Pair) の曲線 (RFC 9053 §7)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OkpCurve {
    /// Ed25519 (値 6)
    Ed25519,
}

impl OkpCurve {
    /// COSE の `crv` 値 (RFC 9053 §7) を返す
    pub const fn identifier(self) -> i64 {
        match self {
            Self::Ed25519 => 6,
        }
    }
}

/// COSE Key (RFC 9052 §7) と JWK (RFC 7517) を共通に扱う鍵表現
///
/// 秘密鍵は署名にだけ使い、検証では公開鍵の部分だけを参照する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoseKey {
    /// 対称鍵 (`kty` = Symmetric)
    Symmetric {
        /// 鍵のバイト列
        key: Vec<u8>,
    },
    /// EC2 鍵 (`kty` = EC2)
    Ec2 {
        /// 曲線
        curve: EcCurve,
        /// x 座標 (ビッグエンディアンの固定長)
        x: Vec<u8>,
        /// y 座標 (ビッグエンディアンの固定長)
        y: Vec<u8>,
        /// 秘密鍵 (スカラー)。署名時にだけ使う
        private_key: Option<Vec<u8>>,
    },
    /// OKP 鍵 (`kty` = OKP)
    Okp {
        /// 曲線
        curve: OkpCurve,
        /// 公開鍵
        public_key: Vec<u8>,
        /// 秘密鍵 (Ed25519 の種)。署名時にだけ使う
        private_key: Option<Vec<u8>>,
    },
}

impl CoseKey {
    /// 対称鍵を作る
    pub fn symmetric(key: impl Into<Vec<u8>>) -> Self {
        Self::Symmetric { key: key.into() }
    }

    /// 公開鍵だけの EC2 鍵を作る
    pub fn ec2(curve: EcCurve, x: impl Into<Vec<u8>>, y: impl Into<Vec<u8>>) -> Self {
        Self::Ec2 {
            curve,
            x: x.into(),
            y: y.into(),
            private_key: None,
        }
    }

    /// 秘密鍵付きの EC2 鍵を作る
    pub fn ec2_with_private_key(
        curve: EcCurve,
        x: impl Into<Vec<u8>>,
        y: impl Into<Vec<u8>>,
        private_key: impl Into<Vec<u8>>,
    ) -> Self {
        Self::Ec2 {
            curve,
            x: x.into(),
            y: y.into(),
            private_key: Some(private_key.into()),
        }
    }

    /// 公開鍵だけの Ed25519 鍵を作る
    pub fn ed25519(public_key: impl Into<Vec<u8>>) -> Self {
        Self::Okp {
            curve: OkpCurve::Ed25519,
            public_key: public_key.into(),
            private_key: None,
        }
    }

    /// 秘密鍵付きの Ed25519 鍵を作る
    pub fn ed25519_with_private_key(
        public_key: impl Into<Vec<u8>>,
        private_key: impl Into<Vec<u8>>,
    ) -> Self {
        Self::Okp {
            curve: OkpCurve::Ed25519,
            public_key: public_key.into(),
            private_key: Some(private_key.into()),
        }
    }

    /// 署名に使える秘密鍵を保持しているかどうかを返す
    pub fn has_private_key(&self) -> bool {
        match self {
            Self::Symmetric { .. } => true,
            Self::Ec2 { private_key, .. } => private_key.is_some(),
            Self::Okp { private_key, .. } => private_key.is_some(),
        }
    }
}

/// ハッシュアルゴリズム
///
/// JWK サムプリント (RFC 7638) など、署名以外でハッシュが必要な処理に使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DigestAlgorithm {
    /// SHA-256
    Sha256,
    /// SHA-384
    Sha384,
    /// SHA-512
    Sha512,
}

/// COSE / JWT の署名と検証を行う trait
///
/// 実装は `aws-lc-rs` feature を有効にした `aws_lc_rs::AwsLcRsCrypto` を参照。
pub trait CoseCrypto {
    /// メッセージに署名する
    ///
    /// 対称鍵の場合は MAC を返す。EC2 / OKP の場合は固定長形式 (COSE の署名形式)
    /// の署名を返す。
    fn sign(
        &self,
        algorithm: Algorithm,
        key: &CoseKey,
        message: &[u8],
    ) -> Result<Vec<u8>, CryptoError>;

    /// 署名または MAC を検証する
    fn verify(
        &self,
        algorithm: Algorithm,
        key: &CoseKey,
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), CryptoError>;

    /// メッセージのハッシュを計算する
    fn digest(&self, algorithm: DigestAlgorithm, message: &[u8]) -> Result<Vec<u8>, CryptoError>;
}

/// 鍵の種別から既定の署名アルゴリズムを返す
///
/// 対称鍵は HMAC-SHA256、EC2 は曲線に対応する ES256 / ES384 / ES512、OKP は
/// EdDSA を返す。
pub fn default_signing_algorithm(key: &CoseKey) -> Algorithm {
    match key {
        CoseKey::Symmetric { .. } => Algorithm::HmacSha256,
        CoseKey::Ec2 { curve, .. } => match curve {
            EcCurve::P256 => Algorithm::Es256,
            EcCurve::P384 => Algorithm::Es384,
            EcCurve::P521 => Algorithm::Es512,
        },
        CoseKey::Okp {
            curve: OkpCurve::Ed25519,
            ..
        } => Algorithm::EdDsa,
    }
}

/// 署名 / 検証のエラー
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CryptoError {
    /// 鍵の種別がアルゴリズムと一致しない
    UnsupportedKey,
    /// 秘密鍵が必要な操作で秘密鍵が無い
    MissingPrivateKey,
    /// 鍵の長さや値が不正である
    InvalidKey,
    /// 署名 / MAC の検証に失敗した
    SignatureVerificationFailed,
    /// 署名の生成に失敗した
    SigningFailed,
    /// 署名の長さがアルゴリズムと一致しない
    InvalidSignatureLength,
    /// ハッシュの計算に失敗した
    DigestFailed,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedKey => write!(f, "key type does not match the algorithm"),
            Self::MissingPrivateKey => write!(f, "private key is required for signing"),
            Self::InvalidKey => write!(f, "invalid key"),
            Self::SignatureVerificationFailed => write!(f, "signature verification failed"),
            Self::SigningFailed => write!(f, "signing failed"),
            Self::InvalidSignatureLength => write!(f, "invalid signature length"),
            Self::DigestFailed => write!(f, "digest computation failed"),
        }
    }
}

impl core::error::Error for CryptoError {}

#[cfg(feature = "aws-lc-rs")]
pub mod aws_lc_rs;
