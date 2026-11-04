//! COSE / JWT の署名と検証を抽象化する trait
//!
//! 既定ビルドでは trait と鍵表現だけを提供し、暗号実装を一切リンクしない。
//! `aws-lc-rs` feature を有効にすると `aws_lc_rs::AwsLcRsCrypto` が使える。
//!
//! Sans I/O の方針に合わせて、このモジュールは乱数も時計も持たない。署名に必要な
//! 乱数は実装側 (aws-lc-rs) が内部で取得する。
//!
//! COSE_Key (RFC 9052 §7) の CBOR マップと [`CoseKey`] の相互変換、および
//! RFC 9679 の COSE Key Thumbprint (SHA-256) の計算も扱う。

use alloc::vec::Vec;
use core::fmt;

use super::cbor::{self, Value};
use super::cose::{Algorithm, CoseError};

/// COSE_Key の key type (`kty`、ラベル 1、RFC 9052 §7.1)
pub const COSE_KEY_TYPE: i64 = 1;

/// COSE_Key の curve (`crv`、ラベル -1、RFC 9053 §7.1 / §7.2)
pub const COSE_KEY_CURVE: i64 = -1;

/// COSE_Key の x 座標 / 公開鍵 (`x`、ラベル -2、RFC 9053 §7.1 / §7.2)
pub const COSE_KEY_X: i64 = -2;

/// COSE_Key の y 座標 (`y`、ラベル -3、RFC 9053 §7.1.1)
pub const COSE_KEY_Y: i64 = -3;

/// COSE_Key の秘密鍵 (`d`、ラベル -4、RFC 9053 §7.1.1 / §7.2)
pub const COSE_KEY_PRIVATE: i64 = -4;

/// COSE_Key の鍵識別子 (`kid`、ラベル 2、RFC 9052 §7.1)
pub const COSE_KEY_KID: i64 = 2;

/// COSE_Key のアルゴリズム (`alg`、ラベル 3、RFC 9052 §7.1)
pub const COSE_KEY_ALG: i64 = 3;

/// COSE_Key の許可操作 (`key_ops`、ラベル 4、RFC 9052 §7.1)
pub const COSE_KEY_OPS: i64 = 4;

/// COSE_Key の Base IV (`Base IV`、ラベル 5、RFC 9052 §7.1)
pub const COSE_KEY_BASE_IV: i64 = 5;

/// COSE Key Types の OKP (RFC 9053 §7 Table 17)
pub const COSE_KEY_TYPE_OKP: i64 = 1;

/// COSE Key Types の EC2 (RFC 9053 §7 Table 17)
pub const COSE_KEY_TYPE_EC2: i64 = 2;

/// COSE Key Types の Symmetric (RFC 9053 §7 Table 17)
pub const COSE_KEY_TYPE_SYMMETRIC: i64 = 4;

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

    /// COSE の `crv` 値 (RFC 9053 §7) から曲線を返す
    ///
    /// 対応していない値は `None` を返す。
    pub const fn from_identifier(identifier: i64) -> Option<Self> {
        match identifier {
            1 => Some(Self::P256),
            2 => Some(Self::P384),
            3 => Some(Self::P521),
            _ => None,
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

    /// 公開鍵 / 秘密鍵のバイト長を返す
    ///
    /// Ed25519 は RFC 8032 §5.1.5 の 32 バイトである。
    pub const fn key_length(self) -> usize {
        match self {
            Self::Ed25519 => 32,
        }
    }

    /// COSE の `crv` 値 (RFC 9053 §7) から曲線を返す
    ///
    /// 対応していない値は `None` を返す。
    pub const fn from_identifier(identifier: i64) -> Option<Self> {
        match identifier {
            6 => Some(Self::Ed25519),
            _ => None,
        }
    }
}

/// COSE Key (RFC 9052 §7) と JWK (RFC 7517) を共通に扱う鍵表現
///
/// 秘密鍵は署名にだけ使い、検証では公開鍵の部分だけを参照する。
#[derive(Clone, PartialEq, Eq)]
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

    /// COSE_Key (RFC 9052 §7) の CBOR マップへ変換する
    ///
    /// DPoP proof の `kccs` に埋め込む公開鍵を作るための API である。EC2
    /// (`kty` = 2) と OKP (`kty` = 1、Ed25519) だけを扱い、対称鍵は
    /// [`CoseError::SymmetricKeyNotAllowed`] で拒否する。
    ///
    /// 秘密鍵パラメータ (`d`、ラベル -4) は常に出力しない。署名に使う鍵をそのまま
    /// 渡せるようにするためであり、proof に秘密鍵を埋め込まないための契約である。
    /// 座標 / 公開鍵の長さが曲線と一致しない場合は
    /// [`CoseError::InvalidKeyLength`] を返す。
    pub fn to_cose_key_map(&self) -> Result<Value, CoseError> {
        match self {
            Self::Symmetric { .. } => Err(CoseError::SymmetricKeyNotAllowed),
            Self::Ec2 { curve, x, y, .. } => {
                let length = curve.coordinate_length();
                if x.len() != length {
                    return Err(CoseError::InvalidKeyLength("x"));
                }
                if y.len() != length {
                    return Err(CoseError::InvalidKeyLength("y"));
                }
                Ok(Value::Map(alloc::vec![
                    (
                        Value::integer(COSE_KEY_TYPE),
                        Value::integer(COSE_KEY_TYPE_EC2),
                    ),
                    (
                        Value::integer(COSE_KEY_CURVE),
                        Value::integer(curve.identifier()),
                    ),
                    (Value::integer(COSE_KEY_X), Value::ByteString(x.clone())),
                    (Value::integer(COSE_KEY_Y), Value::ByteString(y.clone())),
                ]))
            }
            Self::Okp {
                curve, public_key, ..
            } => {
                if public_key.len() != curve.key_length() {
                    return Err(CoseError::InvalidKeyLength("x"));
                }
                Ok(Value::Map(alloc::vec![
                    (
                        Value::integer(COSE_KEY_TYPE),
                        Value::integer(COSE_KEY_TYPE_OKP),
                    ),
                    (
                        Value::integer(COSE_KEY_CURVE),
                        Value::integer(curve.identifier()),
                    ),
                    (
                        Value::integer(COSE_KEY_X),
                        Value::ByteString(public_key.clone()),
                    ),
                ]))
            }
        }
    }

    /// COSE_Key (RFC 9052 §7) の CBOR マップから公開鍵を取り出す
    ///
    /// EC2 (`kty` = 2、`crv` = 1 / 2 / 3) と OKP (`kty` = 1、`crv` = 6 = Ed25519)
    /// だけを扱う。DPoP proof の検証に使う公開鍵として次の値を検査する。
    ///
    /// - 対称鍵 (`kty` = 4) と秘密鍵パラメータ `d` (ラベル -4) は拒否する
    ///   ([`CoseError::SymmetricKeyNotAllowed`] / [`CoseError::PrivateKeyParameter`])
    /// - `alg` (ラベル 3) がある場合は `expected_algorithm` と一致しなければ拒否する
    ///   ([`CoseError::KeyAlgorithmMismatch`])
    /// - `key_ops` (ラベル 4) がある場合は `"verify"` を含まなければ拒否する
    ///   ([`CoseError::KeyOperationNotAllowed`])
    ///
    /// `kty` / `crv` / `alg` は RFC 9052 §7 の CDDL が `tstr / int` を許すため、レジストリ
    /// 名 (`"EC2"` / `"P-256"` / `"ES256"` など) と整数の両方を受け付ける。`key_ops` は
    /// `"verify"` と整数 2 (RFC 9052 Table 5 の verify) の両方を受理する。
    /// - それ以外のメンバー (`kid` / Base IV / 未知のラベル) は RFC 8747 §3.2 が
    ///   他のメンバーの同梱を許すため無視する
    ///
    /// EC2 の `y` を bool で表す圧縮点表現 (RFC 9053 §7.1.1) は、[`CoseCrypto`] の
    /// 実装が非圧縮形式だけを受け取るため扱わない
    /// ([`CoseError::UnexpectedType`])。
    ///
    /// 同じパラメータが重複するマップは [`CoseError::DuplicateKeyParameter`] で拒否
    /// する。CBOR のデコーダも重複キーを拒否する (RFC 8949 §5.6) が、`Value` を直接
    /// 組み立てた入力のためにここでも検査する。
    pub fn from_cose_key_map(
        value: &Value,
        expected_algorithm: Option<Algorithm>,
    ) -> Result<Self, CoseError> {
        let entries = value
            .as_map()
            .ok_or(CoseError::UnexpectedType("COSE_Key"))?;
        // ラベルの重複と秘密鍵パラメータの混入を先に検査する。重複はソートして
        // 隣接比較するため、攻撃者が大きな map を渡しても二次関数的な時間を使わない
        let mut labels = Vec::new();
        for (key, _) in entries {
            let label = key
                .as_int()
                .ok_or(CoseError::UnexpectedType("COSE_Key parameter label"))?;
            labels.push(label);
        }
        let mut duplicates = labels.clone();
        duplicates.sort();
        if let Some(pair) = duplicates.windows(2).find(|pair| pair[0] == pair[1]) {
            return Err(CoseError::DuplicateKeyParameter(pair[0]));
        }
        if labels.contains(&COSE_KEY_PRIVATE) {
            return Err(CoseError::PrivateKeyParameter(COSE_KEY_PRIVATE));
        }
        if let Some(algorithm) = take_optional_key_parameter(entries, COSE_KEY_ALG) {
            let key_algorithm = key_algorithm_value(algorithm)?;
            if let Some(expected) = expected_algorithm
                && key_algorithm != expected
            {
                return Err(CoseError::KeyAlgorithmMismatch {
                    key: key_algorithm,
                    expected,
                });
            }
        }
        if let Some(operations) = take_optional_key_parameter(entries, COSE_KEY_OPS) {
            let operations = operations
                .as_array()
                .ok_or(CoseError::UnexpectedType("key_ops"))?;
            let allows_verify = operations.iter().any(key_operation_allows_verify);
            if !allows_verify {
                return Err(CoseError::KeyOperationNotAllowed("verify"));
            }
        }
        let key_type = key_type_value(key_parameter(entries, COSE_KEY_TYPE, "kty")?)?;
        match key_type {
            COSE_KEY_TYPE_EC2 => {
                let curve = ec_curve_value(key_parameter(entries, COSE_KEY_CURVE, "crv")?)?;
                let x = key_byte_parameter(entries, COSE_KEY_X, "x")?;
                let y = key_byte_parameter(entries, COSE_KEY_Y, "y")?;
                if x.len() != curve.coordinate_length() {
                    return Err(CoseError::InvalidKeyLength("x"));
                }
                if y.len() != curve.coordinate_length() {
                    return Err(CoseError::InvalidKeyLength("y"));
                }
                Ok(Self::ec2(curve, x, y))
            }
            COSE_KEY_TYPE_OKP => {
                let curve = okp_curve_value(key_parameter(entries, COSE_KEY_CURVE, "crv")?)?;
                let public_key = key_byte_parameter(entries, COSE_KEY_X, "x")?;
                if public_key.len() != curve.key_length() {
                    return Err(CoseError::InvalidKeyLength("x"));
                }
                match curve {
                    OkpCurve::Ed25519 => Ok(Self::ed25519(public_key)),
                }
            }
            COSE_KEY_TYPE_SYMMETRIC => Err(CoseError::SymmetricKeyNotAllowed),
            other => Err(CoseError::UnsupportedKeyType(other)),
        }
    }

    /// RFC 9679 §3 の COSE Key Thumbprint を SHA-256 で計算する
    ///
    /// 必須パラメータだけを含む COSE_Key を決定論的 CBOR (RFC 8949 §4.2.1) に
    /// エンコードし、そのバイト列の SHA-256 を返す。必須パラメータは EC2 が
    /// OKP が `kty` / `crv` / `x` (RFC 9679 §4.1)、EC2 が `kty` / `crv` / `x` / `y`
    /// (RFC 9679 §4.2) である。
    ///
    /// `kid` / `alg` / `key_ops` などの任意パラメータと秘密鍵は、必須パラメータだけを
    /// 対象とするためハッシュ対象に含まれない (RFC 9679 §5.1)。EC2 の `x` / `y` は非圧縮点の座標として扱う
    /// (RFC 9679 §4.2)。そのため、秘密鍵付きの [`CoseKey`] を渡しても公開鍵の
    /// サムプリントを返す。
    ///
    /// 本実装が扱うのは DPoP proof の公開鍵 (EC2 / OKP) だけである。RFC 9679 自体は
    /// §4.4 で対称鍵のサムプリントも定義するが、proof の鍵束縛には公開鍵だけを使うため、
    /// 対称鍵は [`CryptoError::UnsupportedThumbprintKey`] で拒否する。座標 / 公開鍵の
    /// 長さが曲線と一致しない場合は [`CryptoError::InvalidKey`] を返す。
    pub fn cose_key_thumbprint_sha256<C: CoseCrypto>(
        &self,
        crypto: &C,
    ) -> Result<Vec<u8>, CryptoError> {
        let parameters = self.to_cose_key_map().map_err(|error| match error {
            CoseError::SymmetricKeyNotAllowed => CryptoError::UnsupportedThumbprintKey,
            _ => CryptoError::InvalidKey,
        })?;
        // 必須パラメータだけからなる浅いマップなので CBOR のエンコードは実質的に
        // 失敗しない。失敗した場合は鍵の表現が不正として扱う
        let encoded = cbor::encode(&parameters).map_err(|_| CryptoError::InvalidKey)?;
        crypto.digest(DigestAlgorithm::Sha256, &encoded)
    }
}

/// COSE_Key の必須パラメータを返す
fn key_parameter<'a>(
    entries: &'a [(Value, Value)],
    label: i64,
    name: &'static str,
) -> Result<&'a Value, CoseError> {
    entries
        .iter()
        .find(|(key, _)| key.as_int() == Some(label))
        .map(|(_, value)| value)
        .ok_or(CoseError::MissingKeyParameter(name))
}

/// COSE_Key の任意パラメータを返す
fn take_optional_key_parameter(entries: &[(Value, Value)], label: i64) -> Option<&Value> {
    entries
        .iter()
        .find(|(key, _)| key.as_int() == Some(label))
        .map(|(_, value)| value)
}

/// COSE_Key の `kty` を解釈する
///
/// RFC 9052 §7 の CDDL は `tstr / int` を許すため、レジストリ名と整数の両方を扱う。
fn key_type_value(value: &Value) -> Result<i64, CoseError> {
    if let Some(identifier) = value.as_int() {
        return Ok(identifier);
    }
    match value {
        Value::TextString(name) => match name.as_str() {
            "OKP" => Ok(COSE_KEY_TYPE_OKP),
            "EC2" => Ok(COSE_KEY_TYPE_EC2),
            "Symmetric" => Ok(COSE_KEY_TYPE_SYMMETRIC),
            _ => Err(CoseError::UnexpectedType("kty")),
        },
        _ => Err(CoseError::UnexpectedType("kty")),
    }
}

/// COSE_Key の `alg` を解釈する
///
/// RFC 9052 §7 の CDDL は `tstr / int` を許すため、レジストリ名と整数の両方を扱う。
fn key_algorithm_value(value: &Value) -> Result<Algorithm, CoseError> {
    if let Some(identifier) = value.as_int() {
        return Algorithm::from_identifier_with_c4m_draft_alias(identifier)
            .ok_or(CoseError::UnsupportedAlgorithm(identifier));
    }
    match value {
        Value::TextString(name) => match name.as_str() {
            "ES256" => Ok(Algorithm::Es256),
            "ES384" => Ok(Algorithm::Es384),
            "ES512" => Ok(Algorithm::Es512),
            "EdDSA" => Ok(Algorithm::EdDsa),
            "HMAC 256/256" => Ok(Algorithm::HmacSha256),
            "HMAC 384/384" => Ok(Algorithm::HmacSha384),
            "HMAC 512/512" => Ok(Algorithm::HmacSha512),
            _ => Err(CoseError::UnexpectedType("alg")),
        },
        _ => Err(CoseError::UnexpectedType("alg")),
    }
}

/// COSE_Key の `crv` を EC2 の曲線として解釈する
fn ec_curve_value(value: &Value) -> Result<EcCurve, CoseError> {
    if let Some(identifier) = value.as_int() {
        return EcCurve::from_identifier(identifier).ok_or(CoseError::UnsupportedCurve(identifier));
    }
    match value {
        Value::TextString(name) => match name.as_str() {
            "P-256" => Ok(EcCurve::P256),
            "P-384" => Ok(EcCurve::P384),
            "P-521" => Ok(EcCurve::P521),
            _ => Err(CoseError::UnexpectedType("crv")),
        },
        _ => Err(CoseError::UnexpectedType("crv")),
    }
}

/// COSE_Key の `crv` を OKP の曲線として解釈する
fn okp_curve_value(value: &Value) -> Result<OkpCurve, CoseError> {
    if let Some(identifier) = value.as_int() {
        return OkpCurve::from_identifier(identifier)
            .ok_or(CoseError::UnsupportedCurve(identifier));
    }
    match value {
        Value::TextString(name) => match name.as_str() {
            "Ed25519" => Ok(OkpCurve::Ed25519),
            _ => Err(CoseError::UnexpectedType("crv")),
        },
        _ => Err(CoseError::UnexpectedType("crv")),
    }
}

/// `key_ops` が署名検証を許可しているか
///
/// RFC 9052 §7 の CDDL は `[+ (tstr / int)]` を許すため、文字列 `"verify"` と
/// 整数 2 (RFC 9052 Table 5 の verify) の両方を扱う。
fn key_operation_allows_verify(operation: &Value) -> bool {
    match operation {
        Value::TextString(name) => name == "verify",
        _ => operation.as_int() == Some(2),
    }
}

/// COSE_Key のバイト列パラメータを返す
fn key_byte_parameter(
    entries: &[(Value, Value)],
    label: i64,
    name: &'static str,
) -> Result<Vec<u8>, CoseError> {
    key_parameter(entries, label, name)?
        .as_bytes()
        .map(<[u8]>::to_vec)
        .ok_or(CoseError::UnexpectedType(name))
}

/// 秘密鍵の内容をログへ出さないための [`Debug`] 実装
///
/// 対称鍵の値と秘密鍵 (スカラー / 種) は伏せ、長さだけを表示する。
impl fmt::Debug for CoseKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Symmetric { key } => f
                .debug_struct("Symmetric")
                .field("key_bytes", &key.len())
                .finish(),
            Self::Ec2 {
                curve,
                x,
                y,
                private_key,
            } => f
                .debug_struct("Ec2")
                .field("curve", curve)
                .field("x", x)
                .field("y", y)
                .field(
                    "private_key",
                    &private_key.as_ref().map(|private_key| private_key.len()),
                )
                .finish(),
            Self::Okp {
                curve,
                public_key,
                private_key,
            } => f
                .debug_struct("Okp")
                .field("curve", curve)
                .field("public_key", public_key)
                .field(
                    "private_key",
                    &private_key.as_ref().map(|private_key| private_key.len()),
                )
                .finish(),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// COSE Key Thumbprint の計算に対応していない鍵である (対称鍵)
    UnsupportedThumbprintKey,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedKey => write!(f, "key type does not match the algorithm"),
            Self::UnsupportedThumbprintKey => {
                write!(f, "COSE Key type is not supported for thumbprint")
            }
            Self::MissingPrivateKey => write!(f, "private key is required for signing"),
            Self::InvalidKey => write!(f, "invalid key"),
            Self::SignatureVerificationFailed => write!(f, "signature verification failed"),
            Self::SigningFailed => write!(f, "signing failed"),
            Self::InvalidSignatureLength => write!(f, "invalid signature length"),
        }
    }
}

impl core::error::Error for CryptoError {}

#[cfg(feature = "aws-lc-rs")]
pub mod aws_lc_rs;
