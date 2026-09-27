//! COSE (CBOR Object Signing and Encryption) の構造
//!
//! RFC 9052 の COSE_Sign1 (タグ 18) / COSE_Mac0 (タグ 17) と、RFC 8392 の CWT
//! (タグ 61) を扱う。アルゴリズムの識別子は RFC 9053 の COSE Algorithms レジストリ
//! に従う。
//!
//! 署名 / 検証の実行は [`crate::c4m::crypto`] の trait に分離しており、このモジュール
//! は構造のエンコード / デコードと署名対象バイト列の組み立てだけを行う (Sans I/O)。

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use super::cbor::{self, CborError, Value};

/// CWT の CBOR タグ (RFC 8392 §6)
pub const TAG_CWT: u64 = 61;

/// COSE_Mac0 の CBOR タグ (RFC 9052 §6.2)
pub const TAG_COSE_MAC0: u64 = 17;

/// COSE_Sign1 の CBOR タグ (RFC 9052 §4.2)
pub const TAG_COSE_SIGN1: u64 = 18;

/// ヘッダパラメータ `alg` (RFC 9052 §3.1)
pub const HEADER_ALGORITHM: i64 = 1;

/// ヘッダパラメータ `crit` (RFC 9052 §3.1)
pub const HEADER_CRITICAL: i64 = 2;

/// ヘッダパラメータ `content type` (RFC 9052 §3.1)
pub const HEADER_CONTENT_TYPE: i64 = 3;

/// ヘッダパラメータ `kid` (RFC 9052 §3.1)
pub const HEADER_KEY_ID: i64 = 4;

/// ヘッダパラメータ `typ` (RFC 9596 §2)
pub const HEADER_TYPE: i64 = 16;

/// ドラフトのテストベクタが HMAC-SHA256 に使うアルゴリズム ID
///
/// draft-ietf-moq-c4m-01 付録 A のベクタは `alg = -4` を HMAC-SHA256 として扱う。
/// IANA の COSE Algorithms レジストリでは -4 は A192KW であり、HMAC 256/256 は 5
/// である。検証では両方を HMAC-SHA256 として扱い、発行は 5 を使う
/// ([`crate::c4m::cat::CatTokenBuilder::build_compact`])。
pub const C4M_DRAFT_HMAC_SHA256_ALGORITHM_ID: i64 = -4;

/// COSE のアルゴリズム (RFC 9053 §2 / §3 のレジストリ値)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Algorithm {
    /// HMAC w/ SHA-256 (HMAC 256/256、値 5)
    HmacSha256,
    /// HMAC w/ SHA-384 (HMAC 384/384、値 6)
    HmacSha384,
    /// HMAC w/ SHA-512 (HMAC 512/512、値 7)
    HmacSha512,
    /// ECDSA w/ SHA-256 (ES256、値 -7)
    Es256,
    /// ECDSA w/ SHA-384 (ES384、値 -35)
    Es384,
    /// ECDSA w/ SHA-512 (ES512、値 -36)
    Es512,
    /// EdDSA (Ed25519、値 -8)
    EdDsa,
}

impl Algorithm {
    /// COSE アルゴリズムの識別子を返す
    pub const fn identifier(self) -> i64 {
        match self {
            Self::HmacSha256 => 5,
            Self::HmacSha384 => 6,
            Self::HmacSha512 => 7,
            Self::Es256 => -7,
            Self::Es384 => -35,
            Self::Es512 => -36,
            Self::EdDsa => -8,
        }
    }

    /// 識別子からアルゴリズムを返す
    ///
    /// C4M ドラフトの別名 (`-4` を HMAC-SHA256 とする) は含めない。別名を含めて
    /// 解釈する場合は [`Algorithm::from_identifier_with_c4m_draft_alias`] を使う。
    pub const fn from_identifier(identifier: i64) -> Option<Self> {
        match identifier {
            5 => Some(Self::HmacSha256),
            6 => Some(Self::HmacSha384),
            7 => Some(Self::HmacSha512),
            -7 => Some(Self::Es256),
            -35 => Some(Self::Es384),
            -36 => Some(Self::Es512),
            -8 => Some(Self::EdDsa),
            _ => None,
        }
    }

    /// 識別子からアルゴリズムを返す (C4M ドラフトの別名を含む)
    ///
    /// [`C4M_DRAFT_HMAC_SHA256_ALGORITHM_ID`] を HMAC-SHA256 として受理する。
    pub const fn from_identifier_with_c4m_draft_alias(identifier: i64) -> Option<Self> {
        if identifier == C4M_DRAFT_HMAC_SHA256_ALGORITHM_ID {
            return Some(Self::HmacSha256);
        }
        Self::from_identifier(identifier)
    }

    /// アルゴリズムの種別 (MAC か署名か) を返す
    pub const fn class(self) -> AlgorithmClass {
        match self {
            Self::HmacSha256 | Self::HmacSha384 | Self::HmacSha512 => AlgorithmClass::Mac,
            Self::Es256 | Self::Es384 | Self::Es512 | Self::EdDsa => AlgorithmClass::Signature,
        }
    }

    /// MAC アルゴリズムかどうかを返す
    pub const fn is_mac(self) -> bool {
        matches!(self.class(), AlgorithmClass::Mac)
    }

    /// JOSE の `alg` 名 (RFC 7518 §3.1 / RFC 8037 §3.1) からアルゴリズムを返す
    ///
    /// COSE と JOSE は同じ識別子体系を使うため、JWT の `alg` はこの対応で COSE の
    /// アルゴリズムへ変換できる。
    pub fn from_jose_name(name: &str) -> Option<Self> {
        match name {
            "ES256" => Some(Self::Es256),
            "ES384" => Some(Self::Es384),
            "ES512" => Some(Self::Es512),
            "EdDSA" => Some(Self::EdDsa),
            "HS256" => Some(Self::HmacSha256),
            "HS384" => Some(Self::HmacSha384),
            "HS512" => Some(Self::HmacSha512),
            _ => None,
        }
    }

    /// JOSE の `alg` 名を返す
    pub const fn jose_name(self) -> &'static str {
        match self {
            Self::Es256 => "ES256",
            Self::Es384 => "ES384",
            Self::Es512 => "ES512",
            Self::EdDsa => "EdDSA",
            Self::HmacSha256 => "HS256",
            Self::HmacSha384 => "HS384",
            Self::HmacSha512 => "HS512",
        }
    }
}

/// COSE 構造の種別
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlgorithmClass {
    /// COSE_Mac0 を使う MAC アルゴリズム (RFC 9053 §3)
    Mac,
    /// COSE_Sign1 を使う署名アルゴリズム (RFC 9053 §2)
    Signature,
}

impl AlgorithmClass {
    /// 署名対象バイト列を組み立てるコンテキスト文字列を返す
    ///
    /// RFC 9052 §4.4 の `Sig_structure` は `"Signature1"`、§6.3 の `MAC_structure` は
    /// `"MAC0"` を使う。
    pub const fn context(self) -> &'static str {
        match self {
            Self::Mac => "MAC0",
            Self::Signature => "Signature1",
        }
    }

    /// 構造の CBOR タグを返す
    pub const fn tag(self) -> u64 {
        match self {
            Self::Mac => TAG_COSE_MAC0,
            Self::Signature => TAG_COSE_SIGN1,
        }
    }
}

/// COSE ヘッダの `kid`
///
/// RFC 9052 §3.1 はバイト文字列とするが、CAT の実装にはテキスト文字列を使うものも
/// あるため両方を保持する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyId {
    /// バイト文字列の kid
    Bytes(Vec<u8>),
    /// テキスト文字列の kid
    Text(String),
}

impl KeyId {
    /// バイト列として返す
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Bytes(value) => value,
            Self::Text(value) => value.as_bytes(),
        }
    }
}

/// COSE の protected / unprotected ヘッダ
///
/// 解釈しないパラメータは [`Header::raw`] に保持し、再エンコード時に決定論的な順序で
/// 復元する。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Header {
    /// アルゴリズム (`alg`)。エイリアスを解決した結果
    pub algorithm: Option<Algorithm>,
    /// ヘッダに書かれていたアルゴリズム識別子の生値
    pub algorithm_identifier: Option<i64>,
    /// 鍵識別子 (`kid`)
    pub key_id: Option<KeyId>,
    /// 完全な COSE オブジェクトのコンテンツタイプ (`typ`、ラベル 16)
    pub typ: Option<Value>,
    /// ペイロードのコンテンツタイプ (`content type`、ラベル 3)
    pub content_type: Option<Value>,
    /// 必ず理解しなければならないヘッダパラメータ (`crit`)
    pub critical: Vec<Value>,
    /// 解釈しなかったヘッダパラメータ
    pub raw: Vec<(Value, Value)>,
}

/// ヘッダのバケット (RFC 9052 §3)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeaderBucket {
    /// 保護されるバケット。`crit` を置ける唯一のバケット
    Protected,
    /// 保護されないバケット
    Unprotected,
}

impl Header {
    /// protected ヘッダのマップからデコードする
    ///
    /// `crit` の各ラベルが同じ protected ヘッダに存在し、かつ理解できることを検証する
    /// (RFC 9052 §3.1 は「protected ヘッダに無いラベルを `crit` が指す場合は致命的
    /// エラー」と定める)。解釈できない `alg` もエラーにする。
    pub fn decode_protected(value: &Value) -> Result<Self, CoseError> {
        let entries = value
            .as_map()
            .ok_or(CoseError::UnexpectedType("header map"))?;
        Self::decode_entries(entries, HeaderBucket::Protected)
    }

    /// unprotected ヘッダのマップからデコードする
    ///
    /// RFC 9052 §3.1 は `crit` を protected ヘッダに置くことを MUST、RFC 9596 §2 は
    /// `typ` (ラベル 16) を unprotected ヘッダに置かないことを MUST とするため、
    /// どちらもエラーにする。
    pub fn decode_unprotected(value: &Value) -> Result<Self, CoseError> {
        let entries = value
            .as_map()
            .ok_or(CoseError::UnexpectedType("unprotected header map"))?;
        Self::decode_entries(entries, HeaderBucket::Unprotected)
    }

    fn decode_entries(entries: &[(Value, Value)], bucket: HeaderBucket) -> Result<Self, CoseError> {
        let mut header = Self::default();
        for (key, entry) in entries {
            match key.as_int() {
                Some(HEADER_ALGORITHM) => {
                    let identifier = entry.as_int().ok_or(CoseError::UnexpectedType("alg"))?;
                    let algorithm = Algorithm::from_identifier_with_c4m_draft_alias(identifier)
                        .ok_or(CoseError::UnsupportedAlgorithm(identifier))?;
                    header.algorithm = Some(algorithm);
                    header.algorithm_identifier = Some(identifier);
                }
                Some(HEADER_CRITICAL) => {
                    if bucket == HeaderBucket::Unprotected {
                        return Err(CoseError::UnprotectedCriticalHeader);
                    }
                    let labels = entry.as_array().ok_or(CoseError::UnexpectedType("crit"))?;
                    if labels.is_empty() {
                        return Err(CoseError::EmptyCriticalHeader);
                    }
                    header.critical = labels.to_vec();
                }
                Some(HEADER_KEY_ID) => {
                    header.key_id = Some(match entry {
                        Value::ByteString(bytes) => KeyId::Bytes(bytes.clone()),
                        Value::TextString(text) => KeyId::Text(text.clone()),
                        _ => return Err(CoseError::UnexpectedType("kid")),
                    });
                }
                Some(HEADER_TYPE) => {
                    if bucket == HeaderBucket::Unprotected {
                        return Err(CoseError::UnprotectedTypeHeader);
                    }
                    header.typ = Some(entry.clone());
                }
                Some(HEADER_CONTENT_TYPE) => header.content_type = Some(entry.clone()),
                _ => header.raw.push((key.clone(), entry.clone())),
            }
        }
        // `crit` のラベルは protected ヘッダに実在し、理解できる必要がある
        for label in &header.critical {
            let understood = matches!(
                label.as_int(),
                Some(
                    HEADER_ALGORITHM
                        | HEADER_CRITICAL
                        | HEADER_CONTENT_TYPE
                        | HEADER_KEY_ID
                        | HEADER_TYPE
                )
            );
            if !understood {
                // counter signature (ラベル 7) は RFC 9052 §3.1 が新実装の理解を求めるが、
                // 本実装は検証しないため fail-closed として拒否する
                return Err(CoseError::UnsupportedCriticalHeader);
            }
            if !entries.iter().any(|(key, _)| key == label) {
                return Err(CoseError::CriticalHeaderNotPresent);
            }
        }
        Ok(header)
    }

    /// ヘッダを CBOR のマップへエンコードする
    pub fn encode(&self) -> Result<Value, CoseError> {
        let mut entries = Vec::new();
        if let Some(algorithm) = self.algorithm {
            let identifier = self.algorithm_identifier.unwrap_or(algorithm.identifier());
            entries.push((Value::integer(HEADER_ALGORITHM), Value::integer(identifier)));
        }
        if let Some(key_id) = &self.key_id {
            let value = match key_id {
                KeyId::Bytes(bytes) => Value::ByteString(bytes.clone()),
                KeyId::Text(text) => Value::TextString(text.clone()),
            };
            entries.push((Value::integer(HEADER_KEY_ID), value));
        }
        if let Some(typ) = &self.typ {
            entries.push((Value::integer(HEADER_TYPE), typ.clone()));
        }
        if let Some(content_type) = &self.content_type {
            entries.push((Value::integer(HEADER_CONTENT_TYPE), content_type.clone()));
        }
        entries.extend(self.raw.iter().cloned());
        if !self.critical.is_empty() {
            // デコード側と同じ規則: crit のラベルは理解でき、保護されるマップに実在
            // すること (RFC 9052 §3.1)
            for label in &self.critical {
                let understood = matches!(
                    label.as_int(),
                    Some(
                        HEADER_ALGORITHM
                            | HEADER_CRITICAL
                            | HEADER_CONTENT_TYPE
                            | HEADER_KEY_ID
                            | HEADER_TYPE
                    )
                );
                if !understood {
                    return Err(CoseError::UnsupportedCriticalHeader);
                }
                // crit 自身 (ラベル 2) はこの直後に追加される
                if label.as_int() != Some(HEADER_CRITICAL)
                    && !entries.iter().any(|(key, _)| key == label)
                {
                    return Err(CoseError::CriticalHeaderNotPresent);
                }
            }
            entries.push((
                Value::integer(HEADER_CRITICAL),
                Value::Array(self.critical.clone()),
            ));
        }
        Ok(Value::Map(entries))
    }
}

/// COSE_Sign1 (RFC 9052 §4.2)
#[derive(Debug, Clone, PartialEq)]
pub struct CoseSign1 {
    /// protected ヘッダの CBOR バイト列 (bstr の中身)
    pub protected: Vec<u8>,
    /// unprotected ヘッダのマップ
    pub unprotected: Vec<(Value, Value)>,
    /// ペイロード。`None` は detached payload を表す
    pub payload: Option<Vec<u8>>,
    /// 署名
    pub signature: Vec<u8>,
    /// COSE タグ (18) が付いていたか
    pub cose_tagged: bool,
    /// CWT タグ (61) が付いていたか
    pub cwt_tagged: bool,
}

/// COSE_Mac0 (RFC 9052 §6.2)
#[derive(Debug, Clone, PartialEq)]
pub struct CoseMac0 {
    /// protected ヘッダの CBOR バイト列 (bstr の中身)
    pub protected: Vec<u8>,
    /// unprotected ヘッダのマップ
    pub unprotected: Vec<(Value, Value)>,
    /// ペイロード。`None` は detached payload を表す
    pub payload: Option<Vec<u8>>,
    /// MAC
    pub tag: Vec<u8>,
    /// COSE タグ (17) が付いていたか
    pub cose_tagged: bool,
    /// CWT タグ (61) が付いていたか
    pub cwt_tagged: bool,
}

/// COSE のメッセージ (COSE_Sign1 / COSE_Mac0)
#[derive(Debug, Clone, PartialEq)]
pub enum CoseMessage {
    /// COSE_Sign1
    Sign1(CoseSign1),
    /// COSE_Mac0
    Mac0(CoseMac0),
}

impl CoseMessage {
    /// CBOR のバイト列からデコードする
    ///
    /// CWT タグ (61) と COSE タグ (17 / 18) を許容する。タグが無い場合は protected
    /// ヘッダのアルゴリズム種別から COSE_Sign1 / COSE_Mac0 を判別する。
    pub fn decode(bytes: &[u8]) -> Result<Self, CoseError> {
        let value = cbor::decode(bytes)?;
        Self::decode_value(&value)
    }

    /// CBOR のデータ項目からデコードする
    pub fn decode_value(value: &Value) -> Result<Self, CoseError> {
        let mut cwt_tagged = false;
        let mut current = value;
        if let Value::Tag(TAG_CWT, inner) = current {
            // RFC 8392 §6: CWT タグは COSE のタグ付きオブジェクトにだけ前置できる
            if !matches!(
                inner.as_ref(),
                Value::Tag(TAG_COSE_SIGN1 | TAG_COSE_MAC0, _)
            ) {
                return Err(CoseError::InvalidStructure(
                    "CWT tag must prefix a COSE tagged message (RFC 8392 §6)",
                ));
            }
            cwt_tagged = true;
            current = inner;
        }
        match current {
            Value::Tag(TAG_COSE_SIGN1, inner) => {
                let parts = CoseArray::decode(inner)?;
                let header = parts.header()?;
                if header.algorithm.is_some_and(Algorithm::is_mac) {
                    return Err(CoseError::AlgorithmClassMismatch);
                }
                Ok(Self::Sign1(parts.into_sign1(true, cwt_tagged)))
            }
            Value::Tag(TAG_COSE_MAC0, inner) => {
                let parts = CoseArray::decode(inner)?;
                let header = parts.header()?;
                if header
                    .algorithm
                    .is_some_and(|algorithm| !algorithm.is_mac())
                {
                    return Err(CoseError::AlgorithmClassMismatch);
                }
                Ok(Self::Mac0(parts.into_mac0(true, cwt_tagged)))
            }
            Value::Array(_) => {
                let parts = CoseArray::decode(current)?;
                let header = parts.header()?;
                let algorithm = header.algorithm.ok_or(CoseError::MissingAlgorithm)?;
                if algorithm.is_mac() {
                    Ok(Self::Mac0(parts.into_mac0(false, cwt_tagged)))
                } else {
                    Ok(Self::Sign1(parts.into_sign1(false, cwt_tagged)))
                }
            }
            _ => Err(CoseError::UnexpectedType("COSE message")),
        }
    }

    /// protected / unprotected を統合したヘッダを返す
    ///
    /// protected ヘッダが空の場合は unprotected だけを返す。`alg` は protected に
    /// 必須で、同じラベルが両方のバケットにある場合は
    /// [`CoseError::DuplicateHeaderParameter`] を返す。
    pub fn header(&self) -> Result<Header, CoseError> {
        let (protected, unprotected) = match self {
            Self::Sign1(message) => (&message.protected, &message.unprotected),
            Self::Mac0(message) => (&message.protected, &message.unprotected),
        };
        decode_header(protected, unprotected)
    }

    /// ペイロードを返す
    pub fn payload(&self) -> Option<&[u8]> {
        match self {
            Self::Sign1(message) => message.payload.as_deref(),
            Self::Mac0(message) => message.payload.as_deref(),
        }
    }

    /// 署名または MAC を返す
    pub fn signature(&self) -> &[u8] {
        match self {
            Self::Sign1(message) => &message.signature,
            Self::Mac0(message) => &message.tag,
        }
    }

    /// 署名 / MAC の対象バイト列を組み立てる
    ///
    /// COSE_Sign1 は `Sig_structure`、COSE_Mac0 は `MAC_structure` を返す
    /// (RFC 9052 §4.4 / §6.3)。external_aad は空のバイト文字列とする。
    /// detached payload は扱わないためエラーを返す。
    pub fn signing_input(&self) -> Result<Vec<u8>, CoseError> {
        let (protected, payload, class) = match self {
            Self::Sign1(message) => (
                &message.protected,
                message
                    .payload
                    .as_deref()
                    .ok_or(CoseError::DetachedPayloadUnsupported)?,
                AlgorithmClass::Signature,
            ),
            Self::Mac0(message) => (
                &message.protected,
                message
                    .payload
                    .as_deref()
                    .ok_or(CoseError::DetachedPayloadUnsupported)?,
                AlgorithmClass::Mac,
            ),
        };
        let value = Value::Array(vec![
            Value::TextString(String::from(class.context())),
            Value::ByteString(protected.clone()),
            Value::ByteString(Vec::new()),
            Value::ByteString(payload.to_vec()),
        ]);
        Ok(cbor::encode(&value)?)
    }

    /// COSE メッセージをエンコードする
    ///
    /// タグの付与は「メッセージがデコード時に持っていたタグ」と
    /// [`CoseEncodingOptions`] の OR で決まる。例えばタグ無しでデコードした
    /// メッセージに [`CoseEncodingOptions::default`] を渡すと CWT タグ (61) と
    /// COSE タグ (17 / 18) が付与される。CWT タグを付ける場合は COSE タグも
    /// 必要になる (RFC 8392 §6)。
    pub fn encode(&self, options: &CoseEncodingOptions) -> Result<Vec<u8>, CoseError> {
        let tagged = match self {
            Self::Sign1(message) => message.cose_tagged || options.cose_tag,
            Self::Mac0(message) => message.cose_tagged || options.cose_tag,
        };
        let cwt = match self {
            Self::Sign1(message) => message.cwt_tagged || options.cwt_tag,
            Self::Mac0(message) => message.cwt_tagged || options.cwt_tag,
        };
        // RFC 8392 §6: CWT タグは COSE のタグ付きオブジェクトにだけ前置できる
        if cwt && !tagged {
            return Err(CoseError::InvalidStructure(
                "CWT tag requires the COSE tag (RFC 8392 §6)",
            ));
        }
        let array = match self {
            Self::Sign1(message) => message.array_value(),
            Self::Mac0(message) => message.array_value(),
        };
        let mut value = array;
        if tagged {
            let tag = match self {
                Self::Sign1(_) => AlgorithmClass::Signature.tag(),
                Self::Mac0(_) => AlgorithmClass::Mac.tag(),
            };
            value = Value::Tag(tag, Box::new(value));
        }
        if cwt {
            value = Value::Tag(TAG_CWT, Box::new(value));
        }
        Ok(cbor::encode(&value)?)
    }
}

/// COSE メッセージのエンコードオプション
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoseEncodingOptions {
    /// COSE タグ (17 / 18) を付与する
    pub cose_tag: bool,
    /// CWT タグ (61) を付与する
    pub cwt_tag: bool,
}

impl Default for CoseEncodingOptions {
    /// CWT タグと COSE タグの両方を付与する
    fn default() -> Self {
        Self {
            cose_tag: true,
            cwt_tag: true,
        }
    }
}

/// COSE の構造のエンコード / デコードエラー
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoseError {
    /// CBOR のデコードに失敗した
    Cbor(CborError),
    /// 期待した型と異なる
    UnexpectedType(&'static str),
    /// 構造が不正である
    InvalidStructure(&'static str),
    /// `alg` が無い
    MissingAlgorithm,
    /// 対応していないアルゴリズムである
    UnsupportedAlgorithm(i64),
    /// `crit` に理解できないヘッダパラメータがある
    UnsupportedCriticalHeader,
    /// `crit` のラベルが protected ヘッダに無い (RFC 9052 §3.1)
    CriticalHeaderNotPresent,
    /// `crit` の配列が空である (RFC 9052 §3.1)
    EmptyCriticalHeader,
    /// `crit` が unprotected ヘッダにある (RFC 9052 §3.1)
    UnprotectedCriticalHeader,
    /// `typ` が unprotected ヘッダにある (RFC 9596 §2)
    UnprotectedTypeHeader,
    /// `alg` が unprotected ヘッダにしかない (RFC 9052 §3.1)
    UnprotectedAlgorithm,
    /// 同じヘッダパラメータが protected と unprotected の両方にある
    DuplicateHeaderParameter(i64),
    /// タグとアルゴリズムの種別が一致しない
    AlgorithmClassMismatch,
    /// detached payload は扱わない
    DetachedPayloadUnsupported,
}

impl fmt::Display for CoseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cbor(error) => write!(f, "CBOR error: {error}"),
            Self::UnexpectedType(expected) => write!(f, "expected {expected}"),
            Self::InvalidStructure(reason) => write!(f, "invalid COSE structure: {reason}"),
            Self::MissingAlgorithm => write!(f, "COSE header has no alg parameter"),
            Self::UnsupportedAlgorithm(identifier) => {
                write!(f, "unsupported COSE algorithm: {identifier}")
            }
            Self::UnsupportedCriticalHeader => {
                write!(
                    f,
                    "crit lists a header parameter that is not understood (RFC 9052 §3.1)"
                )
            }
            Self::CriticalHeaderNotPresent => {
                write!(
                    f,
                    "crit lists a header parameter that is not in the protected header"
                )
            }
            Self::EmptyCriticalHeader => write!(f, "crit array must not be empty"),
            Self::UnprotectedCriticalHeader => {
                write!(f, "crit must be in the protected header (RFC 9052 §3.1)")
            }
            Self::UnprotectedTypeHeader => {
                write!(f, "typ must not be in the unprotected header (RFC 9596 §2)")
            }
            Self::UnprotectedAlgorithm => {
                write!(f, "alg must be in the protected header (RFC 9052 §3.1)")
            }
            Self::DuplicateHeaderParameter(label) => {
                write!(f, "header parameter {label} is present in both buckets")
            }
            Self::AlgorithmClassMismatch => {
                write!(f, "COSE tag and algorithm class do not match")
            }
            Self::DetachedPayloadUnsupported => write!(f, "detached payload is not supported"),
        }
    }
}

impl core::error::Error for CoseError {}

impl From<CborError> for CoseError {
    fn from(error: CborError) -> Self {
        Self::Cbor(error)
    }
}

/// protected ヘッダと unprotected ヘッダを統合してデコードする
///
/// 同じラベルが両方のバケットにある場合は `DuplicateHeaderParameter` で拒否する
/// (RFC 9052 §3 は「同じラベルが両方に現れないことを検証すること」を SHOULD とし、
/// 拒否しない場合は protected を優先する MUST を定める)。
fn decode_header(protected: &[u8], unprotected: &[(Value, Value)]) -> Result<Header, CoseError> {
    let protected_value = if protected.is_empty() {
        None
    } else {
        Some(cbor::decode(protected)?)
    };
    let mut header = match &protected_value {
        Some(value) => Header::decode_protected(value)?,
        None => Header::default(),
    };
    let empty: &[(Value, Value)] = &[];
    let protected_entries = protected_value
        .as_ref()
        .and_then(Value::as_map)
        .unwrap_or(empty);
    for (key, _) in unprotected {
        if protected_entries
            .iter()
            .any(|(protected_key, _)| protected_key == key)
        {
            if let Some(label) = key.as_int() {
                return Err(CoseError::DuplicateHeaderParameter(label));
            }
            return Err(CoseError::InvalidStructure(
                "the same header parameter label is present in both buckets",
            ));
        }
    }
    let unprotected_header = Header::decode_entries(unprotected, HeaderBucket::Unprotected)?;
    // `alg` は protected ヘッダで認証されなければならない (RFC 9052 §3.1)
    if unprotected_header.algorithm.is_some() {
        return Err(CoseError::UnprotectedAlgorithm);
    }
    header.key_id = header.key_id.or(unprotected_header.key_id);
    header.content_type = header.content_type.or(unprotected_header.content_type);
    header.raw.extend(unprotected_header.raw);
    Ok(header)
}

/// COSE_Sign1 / COSE_Mac0 の共通の配列構造
///
/// RFC 9052 §4.2 / §6.2 の `[protected, unprotected, payload, signature]` を表す。
struct CoseArray {
    protected: Vec<u8>,
    unprotected: Vec<(Value, Value)>,
    payload: Option<Vec<u8>>,
    signature: Vec<u8>,
}

impl CoseArray {
    fn decode(value: &Value) -> Result<Self, CoseError> {
        let items = value
            .as_array()
            .ok_or(CoseError::UnexpectedType("COSE array"))?;
        if items.len() != 4 {
            return Err(CoseError::InvalidStructure(
                "COSE array must have 4 elements",
            ));
        }
        let protected = items[0]
            .as_bytes()
            .ok_or(CoseError::UnexpectedType("protected header"))?
            .to_vec();
        let unprotected = items[1]
            .as_map()
            .ok_or(CoseError::UnexpectedType("unprotected header"))?
            .to_vec();
        let payload = match &items[2] {
            Value::ByteString(bytes) => Some(bytes.clone()),
            Value::Null => None,
            _ => return Err(CoseError::UnexpectedType("payload")),
        };
        let signature = items[3]
            .as_bytes()
            .ok_or(CoseError::UnexpectedType("signature"))?
            .to_vec();
        Ok(Self {
            protected,
            unprotected,
            payload,
            signature,
        })
    }

    fn header(&self) -> Result<Header, CoseError> {
        decode_header(&self.protected, &self.unprotected)
    }

    fn into_sign1(self, cose_tagged: bool, cwt_tagged: bool) -> CoseSign1 {
        CoseSign1 {
            protected: self.protected,
            unprotected: self.unprotected,
            payload: self.payload,
            signature: self.signature,
            cose_tagged,
            cwt_tagged,
        }
    }

    fn into_mac0(self, cose_tagged: bool, cwt_tagged: bool) -> CoseMac0 {
        CoseMac0 {
            protected: self.protected,
            unprotected: self.unprotected,
            payload: self.payload,
            tag: self.signature,
            cose_tagged,
            cwt_tagged,
        }
    }
}

/// COSE_Sign1 / COSE_Mac0 の配列を組み立てる
fn cose_array_value(
    protected: &[u8],
    unprotected: &[(Value, Value)],
    payload: &Option<Vec<u8>>,
    signature: &[u8],
) -> Value {
    Value::Array(vec![
        Value::ByteString(protected.to_vec()),
        Value::Map(unprotected.to_vec()),
        match payload {
            Some(payload) => Value::ByteString(payload.clone()),
            None => Value::Null,
        },
        Value::ByteString(signature.to_vec()),
    ])
}

impl CoseSign1 {
    fn array_value(&self) -> Value {
        cose_array_value(
            &self.protected,
            &self.unprotected,
            &self.payload,
            &self.signature,
        )
    }
}

impl CoseMac0 {
    fn array_value(&self) -> Value {
        cose_array_value(&self.protected, &self.unprotected, &self.payload, &self.tag)
    }
}
