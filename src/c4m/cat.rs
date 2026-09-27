//! CAT (Common Access Token) のクレームとトークン
//!
//! CTA-5007-B の CAT を CWT (RFC 8392) のクレームとして扱い、draft-ietf-moq-c4m-01
//! の `moqt` / `moqt-reval` を加えたトークンの発行と検証を提供する。
//!
//! 直列化は 2 種類を扱う。
//!
//! - compact: `base64url(protected).base64url(claims).base64url(signature)`。
//!   draft-ietf-moq-c4m-01 付録 A のテストベクタの形式で、署名対象は ASCII の
//!   `protected.claims` である
//! - COSE: CWT タグ 61 + COSE_Sign1 (タグ 18) / COSE_Mac0 (タグ 17)。RFC 8392 /
//!   RFC 9052 の形式で、署名対象は `Sig_structure` / `MAC_structure` である
//!
//! CTA-5007-B 本体は有償仕様のため、クレームキーは IANA の CWT Claims レジストリと
//! IANA の CWT Confirmation Methods レジストリ、および draft-ietf-moq-c4m-01 の
//! テストベクタに従う。

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use super::cbor::{self, CborError, Value};
use super::cose::{
    Algorithm, CoseEncodingOptions, CoseError, CoseMac0, CoseMessage, CoseSign1, HEADER_ALGORITHM,
    HEADER_KEY_ID, HEADER_TYPE, Header, KeyId,
};
use super::crypto::{CoseCrypto, CoseKey, CryptoError};
use super::{C4mError, CLAIM_MOQT, CLAIM_MOQT_REVAL, CatDpop, MoqtAction, MoqtClaim, number_value};

/// CWT の `iss` (RFC 8392 §3.1.1)
pub const CLAIM_ISSUER: i64 = 1;

/// CWT の `sub` (RFC 8392 §3.1.2)
pub const CLAIM_SUBJECT: i64 = 2;

/// CWT の `aud` (RFC 8392 §3.1.3)
pub const CLAIM_AUDIENCE: i64 = 3;

/// CWT の `exp` (RFC 8392 §3.1.4)
pub const CLAIM_EXPIRATION: i64 = 4;

/// CWT の `nbf` (RFC 8392 §3.1.5)
pub const CLAIM_NOT_BEFORE: i64 = 5;

/// CWT の `iat` (RFC 8392 §3.1.6)
pub const CLAIM_ISSUED_AT: i64 = 6;

/// CWT の `cti` (RFC 8392 §3.1.7)
pub const CLAIM_CWT_ID: i64 = 7;

/// CWT の `cnf` (RFC 8747)
pub const CLAIM_CONFIRMATION: i64 = 8;

/// CAT の `catreplay` (IANA CWT Claims レジストリ、CTA-5007)
pub const CLAIM_CAT_REPLAY: i64 = 308;

/// CAT の `catpor` (IANA CWT Claims レジストリ、CTA-5007)
pub const CLAIM_CAT_PROBABILITY_OF_REJECTION: i64 = 309;

/// CAT の `catv` (IANA CWT Claims レジストリ、CTA-5007)
pub const CLAIM_CAT_VERSION: i64 = 310;

/// CAT の `catnip` (IANA CWT Claims レジストリ、CTA-5007)
pub const CLAIM_CAT_NETWORK_IP: i64 = 311;

/// CAT の `catu` (IANA CWT Claims レジストリ、CTA-5007)
pub const CLAIM_CAT_URI: i64 = 312;

/// CAT の `catm` (IANA CWT Claims レジストリ、CTA-5007)
pub const CLAIM_CAT_METHOD: i64 = 313;

/// CAT の `catalpn` (IANA CWT Claims レジストリ、CTA-5007)
pub const CLAIM_CAT_ALPN: i64 = 314;

/// CAT の `cath` (IANA CWT Claims レジストリ、CTA-5007)
pub const CLAIM_CAT_HEADER: i64 = 315;

/// CAT の `catgeoiso3166` (IANA CWT Claims レジストリ、CTA-5007)
pub const CLAIM_CAT_GEO_ISO3166: i64 = 316;

/// CAT の `catgeocoord` (IANA CWT Claims レジストリ、CTA-5007)
pub const CLAIM_CAT_GEO_COORD: i64 = 317;

/// CAT の `catgeoalt` (IANA CWT Claims レジストリ、CTA-5007)
pub const CLAIM_CAT_GEO_ALT: i64 = 318;

/// CAT の `cattpk` (IANA CWT Claims レジストリ、CTA-5007)
pub const CLAIM_CAT_TLS_PUBLIC_KEY: i64 = 319;

/// CAT の `catifdata` (IANA CWT Claims レジストリ、CTA-5007)
pub const CLAIM_CAT_IF_DATA: i64 = 320;

/// CAT の `catdpop` (IANA CWT Claims レジストリ、CTA-5007)
pub const CLAIM_CAT_DPOP: i64 = 321;

/// CAT の `catif` (IANA CWT Claims レジストリ、CTA-5007)
pub const CLAIM_CAT_IF: i64 = 322;

/// CAT の `catr` (IANA CWT Claims レジストリ、CTA-5007)
pub const CLAIM_CAT_RENEWAL: i64 = 323;

/// `cnf` の `jkt` (JWK サムプリント) の confirmation key
///
/// IANA の CWT Confirmation Methods レジストリに CTA が登録した値 (323)。
pub const CONFIRMATION_JWK_THUMBPRINT: i64 = 323;

/// draft-ietf-moq-c4m-01 のベクタが `jkt` に使う confirmation key
///
/// 付録 A.4 のベクタは 3 を使うが、IANA の CWT Confirmation Methods レジストリでは
/// 3 は `kid` (RFC 8747 §3.4) である。検証では両方を受け、発行は
/// [`CONFIRMATION_JWK_THUMBPRINT`] を既定とする。
pub const CONFIRMATION_C4M_DRAFT_JWK_THUMBPRINT: i64 = 3;

/// MOQT の Auth Token Type (CAT) (draft-ietf-moq-c4m-01 §7.1)
pub const MOQT_AUTH_TOKEN_TYPE_CAT: u64 = 0x01;

/// CAT のトークン直列化
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenFormat {
    /// draft-ietf-moq-c4m-01 付録 A の compact 形式
    Compact,
    /// COSE_Sign1 (タグ 18) の COSE 形式
    CoseSign1,
    /// COSE_Mac0 (タグ 17) の COSE 形式
    CoseMac0,
}

/// `cnf` (confirmation) クレーム (RFC 8747 / CTA-5007-B)
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Confirmation {
    /// IANA 登録の `jkt` (confirmation key 323) の値
    pub jwk_thumbprint: Option<Vec<u8>>,
    /// draft-ietf-moq-c4m-01 のベクタが使う `jkt` (confirmation key 3) の値
    ///
    /// IANA のレジストリでは 3 は `kid` であり、値の意味が確定していないため
    /// [`Confirmation::raw`] とは分けて保持する。
    pub c4m_draft_jwk_thumbprint: Option<Vec<u8>>,
    /// 解釈しなかった confirmation の値
    pub raw: Vec<(Value, Value)>,
}

impl Confirmation {
    /// `cnf` をデコードする
    pub fn decode(value: &Value) -> Result<Self, CatError> {
        let entries = value.as_map().ok_or(CatError::UnexpectedType("cnf"))?;
        let mut confirmation = Self::default();
        for (key, entry) in entries {
            match key.as_int() {
                Some(CONFIRMATION_JWK_THUMBPRINT) => {
                    confirmation.jwk_thumbprint = Some(
                        entry
                            .as_bytes()
                            .ok_or(CatError::UnexpectedType("jkt"))?
                            .to_vec(),
                    );
                }
                Some(CONFIRMATION_C4M_DRAFT_JWK_THUMBPRINT) => {
                    confirmation.c4m_draft_jwk_thumbprint = Some(
                        entry
                            .as_bytes()
                            .ok_or(CatError::UnexpectedType("jkt"))?
                            .to_vec(),
                    );
                }
                _ => confirmation.raw.push((key.clone(), entry.clone())),
            }
        }
        Ok(confirmation)
    }

    /// `cnf` をエンコードする
    pub fn encode(&self) -> Value {
        let mut entries = Vec::new();
        if let Some(jkt) = &self.jwk_thumbprint {
            entries.push((
                Value::integer(CONFIRMATION_JWK_THUMBPRINT),
                Value::ByteString(jkt.clone()),
            ));
        }
        if let Some(jkt) = &self.c4m_draft_jwk_thumbprint {
            entries.push((
                Value::integer(CONFIRMATION_C4M_DRAFT_JWK_THUMBPRINT),
                Value::ByteString(jkt.clone()),
            ));
        }
        entries.extend(self.raw.iter().cloned());
        Value::Map(entries)
    }

    /// JWK サムプリントを返す
    ///
    /// IANA 登録の 323 を優先し、無ければドラフトのベクタが使う 3 を返す。
    pub fn jkt(&self) -> Option<&[u8]> {
        self.jwk_thumbprint
            .as_deref()
            .or(self.c4m_draft_jwk_thumbprint.as_deref())
    }
}

/// CAT のクレームセット (CWT のクレーム + CAT / C4M のクレーム)
///
/// 型付きで解釈しないクレームは [`CatClaims::raw`] にそのまま保持する。CAT の
/// クレームは IANA の登録と draft-ietf-moq-c4m-01 のベクタで値型が一致しないもの
/// (catv / catu など) があるため、意味論を定める C4M のクレームだけを型付きにする。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CatClaims {
    /// `iss`
    pub issuer: Option<String>,
    /// `sub`
    pub subject: Option<String>,
    /// `aud`。単一のテキストと配列の両方を受ける
    pub audience: Vec<String>,
    /// `exp` (UNIX 秒)
    pub expiration: Option<f64>,
    /// `nbf` (UNIX 秒)
    pub not_before: Option<f64>,
    /// `iat` (UNIX 秒)
    pub issued_at: Option<f64>,
    /// `cti`。バイト文字列とテキスト文字列の両方を受ける
    pub cwt_id: Option<Vec<u8>>,
    /// `cnf`
    pub confirmation: Option<Confirmation>,
    /// `moqt` (draft-ietf-moq-c4m-01 §2.1)
    pub moqt: Option<MoqtClaim>,
    /// `moqt-reval` (draft-ietf-moq-c4m-01 §2.2) の再検証間隔 (秒)
    ///
    /// §2.2 は「再検証できない受信者は `moqt-reval` 付きトークンを拒否する MUST」
    /// 「再検証間隔が自身の能力を下回る場合も拒否する MUST」を定める。本ライブラリは
    /// Sans-I/O のため再検証の実行は行わず、この値の解釈と拒否の判断は利用側が行う。
    pub moqt_reval: Option<f64>,
    /// `catdpop` (draft-ietf-moq-c4m-01 §3.1.1)
    pub catdpop: Option<CatDpop>,
    /// 型付きで解釈しなかったクレーム
    pub raw: Vec<(Value, Value)>,
}

/// 型付きフィールドを持つ claim key かどうかを返す
fn is_typed_claim_key(key: i64) -> bool {
    matches!(
        key,
        CLAIM_ISSUER
            | CLAIM_SUBJECT
            | CLAIM_AUDIENCE
            | CLAIM_EXPIRATION
            | CLAIM_NOT_BEFORE
            | CLAIM_ISSUED_AT
            | CLAIM_CWT_ID
            | CLAIM_CONFIRMATION
            | CLAIM_MOQT
            | CLAIM_MOQT_REVAL
            | CLAIM_CAT_DPOP
    )
}

/// 数値クレームを有限値として CBOR のデータ項目へエンコードする
fn finite_number_value(number: f64, name: &'static str) -> Result<Value, CatError> {
    if !number.is_finite() {
        return Err(CatError::NonFiniteNumber(name));
    }
    Ok(number_value(number))
}

/// 数値クレームを有限の `f64` として取り出す
///
/// NaN / 無限大は期限判定を素通りさせるため、デコードの時点で拒否する。
fn finite_claim_number(entry: &Value, name: &'static str) -> Result<f64, CatError> {
    let number = entry.as_number().ok_or(CatError::UnexpectedType(name))?;
    if !number.is_finite() {
        return Err(CatError::NonFiniteNumber(name));
    }
    Ok(number)
}

impl CatClaims {
    /// クレームセットをデコードする
    pub fn decode(value: &Value) -> Result<Self, CatError> {
        let entries = value.as_map().ok_or(CatError::UnexpectedType("claims"))?;
        let mut claims = Self::default();
        for (key, entry) in entries {
            // CWT の claim key は整数またはテキスト文字列 (RFC 8392 §3)。
            // それ以外の型は意味を解釈できないため拒否する
            let Some(key_int) = key.as_int() else {
                if matches!(key, Value::TextString(_)) {
                    claims.raw.push((key.clone(), entry.clone()));
                    continue;
                }
                return Err(CatError::UnexpectedType("claim key"));
            };
            match key_int {
                CLAIM_ISSUER => {
                    claims.issuer = Some(
                        entry
                            .as_text()
                            .ok_or(CatError::UnexpectedType("iss"))?
                            .into(),
                    );
                }
                CLAIM_SUBJECT => {
                    claims.subject = Some(
                        entry
                            .as_text()
                            .ok_or(CatError::UnexpectedType("sub"))?
                            .into(),
                    );
                }
                CLAIM_AUDIENCE => match entry {
                    Value::TextString(audience) => claims.audience.push(audience.clone()),
                    Value::Array(audiences) => {
                        for audience in audiences {
                            claims.audience.push(
                                audience
                                    .as_text()
                                    .ok_or(CatError::UnexpectedType("aud"))?
                                    .into(),
                            );
                        }
                    }
                    _ => return Err(CatError::UnexpectedType("aud")),
                },
                CLAIM_EXPIRATION => {
                    claims.expiration = Some(finite_claim_number(entry, "exp")?);
                }
                CLAIM_NOT_BEFORE => {
                    claims.not_before = Some(finite_claim_number(entry, "nbf")?);
                }
                CLAIM_ISSUED_AT => {
                    claims.issued_at = Some(finite_claim_number(entry, "iat")?);
                }
                CLAIM_CWT_ID => {
                    claims.cwt_id = Some(match entry {
                        Value::ByteString(id) => id.clone(),
                        // 付録 A.2 / A.3 のベクタはテキスト文字列を使う
                        Value::TextString(id) => id.as_bytes().to_vec(),
                        _ => return Err(CatError::UnexpectedType("cti")),
                    });
                }
                CLAIM_CONFIRMATION => {
                    claims.confirmation = Some(Confirmation::decode(entry)?);
                }
                CLAIM_MOQT => {
                    claims.moqt = Some(MoqtClaim::decode(entry)?);
                }
                CLAIM_MOQT_REVAL => {
                    claims.moqt_reval = Some(finite_claim_number(entry, "moqt-reval")?);
                }
                CLAIM_CAT_DPOP => {
                    claims.catdpop = Some(CatDpop::decode(entry)?);
                }
                _ => claims.raw.push((key.clone(), entry.clone())),
            }
        }
        Ok(claims)
    }

    /// クレームセットをエンコードする
    ///
    /// 型付きフィールドを持つ claim key を [`CatClaims::raw`] に置いた場合は、型付き
    /// フィールドが未設定でもエラーを返す。非有限値の数値クレームもエラーを返す。
    ///
    /// デコードしたクレームを再エンコードすると表現が正規化される (`aud` の単一
    /// テキストは配列になり、`cti` のテキストはバイト文字列になり、整数値の浮動
    /// 小数点数は整数になる)。
    pub fn encode(&self) -> Result<Value, CatError> {
        let mut entries = Vec::new();
        if let Some(issuer) = &self.issuer {
            entries.push((
                Value::integer(CLAIM_ISSUER),
                Value::TextString(issuer.clone()),
            ));
        }
        if let Some(subject) = &self.subject {
            entries.push((
                Value::integer(CLAIM_SUBJECT),
                Value::TextString(subject.clone()),
            ));
        }
        if !self.audience.is_empty() {
            entries.push((
                Value::integer(CLAIM_AUDIENCE),
                Value::Array(
                    self.audience
                        .iter()
                        .map(|audience| Value::TextString(audience.clone()))
                        .collect(),
                ),
            ));
        }
        if let Some(expiration) = self.expiration {
            entries.push((
                Value::integer(CLAIM_EXPIRATION),
                finite_number_value(expiration, "exp")?,
            ));
        }
        if let Some(not_before) = self.not_before {
            entries.push((
                Value::integer(CLAIM_NOT_BEFORE),
                finite_number_value(not_before, "nbf")?,
            ));
        }
        if let Some(issued_at) = self.issued_at {
            entries.push((
                Value::integer(CLAIM_ISSUED_AT),
                finite_number_value(issued_at, "iat")?,
            ));
        }
        if let Some(cwt_id) = &self.cwt_id {
            entries.push((
                Value::integer(CLAIM_CWT_ID),
                Value::ByteString(cwt_id.clone()),
            ));
        }
        if let Some(confirmation) = &self.confirmation {
            entries.push((Value::integer(CLAIM_CONFIRMATION), confirmation.encode()));
        }
        if let Some(moqt) = &self.moqt {
            entries.push((Value::integer(CLAIM_MOQT), moqt.encode()?));
        }
        if let Some(moqt_reval) = self.moqt_reval {
            entries.push((
                Value::integer(CLAIM_MOQT_REVAL),
                finite_number_value(moqt_reval, "moqt-reval")?,
            ));
        }
        if let Some(catdpop) = &self.catdpop {
            entries.push((Value::integer(CLAIM_CAT_DPOP), catdpop.encode()?));
        }
        for (key, value) in &self.raw {
            // 型付きフィールドを持つ claim key を raw に置くと、デコード時に型付き
            // フィールドと raw のどちらが使われるかが曖昧になる。値の型も検証できない
            // ため、設定の有無にかかわらず拒否する
            if let Some(key) = key.as_int()
                && is_typed_claim_key(key)
            {
                return Err(CatError::DuplicateClaim(key));
            }
            entries.push((key.clone(), value.clone()));
        }
        Ok(Value::Map(entries))
    }

    /// 整数キーのクレームを取り出す
    ///
    /// 型付きフィールドとして解釈しなかったクレーム、および整数キーの未知のクレーム
    /// を対象とする。
    pub fn get(&self, key: i64) -> Option<&Value> {
        self.raw.iter().find_map(|(entry_key, value)| {
            if entry_key.as_int() == Some(key) {
                Some(value)
            } else {
                None
            }
        })
    }

    /// `moqt` クレームによりアクションが認可されるかどうかを返す
    ///
    /// `moqt` クレームが無い場合は常に `false` を返す (§2 の「明示的に許可された
    /// アクション以外はブロックする」)。
    ///
    /// 評価するのは `moqt` クレームだけである。`catu` / `catnip` / `cath` などの
    /// CAT 固有クレームは保持するだけで評価しないため、必要に応じて [`CatClaims::get`]
    /// で取り出して呼び出し側が検証すること。
    pub fn authorize(&self, action: MoqtAction, namespace: &[&[u8]], track_name: &[u8]) -> bool {
        self.moqt
            .as_ref()
            .is_some_and(|moqt| moqt.authorize(action, namespace, track_name))
    }

    /// 時刻と期待値に対するクレームの検証を行う
    ///
    /// 署名の検証は [`CatToken::verify`] が行う。ここでは `exp` / `nbf` / `iss` /
    /// `aud` を検証し、加えて現在時刻 / 許容ずれの有限性と、手組みで入り得る非有限の
    /// 数値クレームを拒否する。
    pub fn validate(
        &self,
        options: &ClaimValidationOptions<'_>,
    ) -> Result<(), ClaimValidationError> {
        if !options.reference_time_seconds.is_finite()
            || !options.clock_tolerance_seconds.is_finite()
            || options.clock_tolerance_seconds < 0.0
        {
            return Err(ClaimValidationError::InvalidReferenceTime);
        }
        // フィールドは公開のため、デコード以外の経路で非有限値が入り得る
        for (number, name) in [
            (self.expiration, "exp"),
            (self.not_before, "nbf"),
            (self.issued_at, "iat"),
            (self.moqt_reval, "moqt-reval"),
        ] {
            if number.is_some_and(|number| !number.is_finite()) {
                return Err(ClaimValidationError::NonFiniteClaim(name));
            }
        }
        if let Some(catdpop) = &self.catdpop
            && catdpop
                .window_seconds
                .is_some_and(|window| !window.is_finite())
        {
            return Err(ClaimValidationError::NonFiniteClaim("catdpop window"));
        }
        if let Some(expiration) = self.expiration
            && options.reference_time_seconds > expiration + options.clock_tolerance_seconds
        {
            return Err(ClaimValidationError::Expired);
        }
        if let Some(not_before) = self.not_before
            && options.reference_time_seconds + options.clock_tolerance_seconds < not_before
        {
            return Err(ClaimValidationError::NotYetValid);
        }
        if !options.expected_issuers.is_empty() {
            let matches = self
                .issuer
                .as_deref()
                .is_some_and(|issuer| options.expected_issuers.contains(&issuer));
            if !matches {
                return Err(ClaimValidationError::IssuerMismatch);
            }
        }
        if !options.expected_audiences.is_empty() {
            let matches = self
                .audience
                .iter()
                .any(|audience| options.expected_audiences.contains(&audience.as_str()));
            if !matches {
                return Err(ClaimValidationError::AudienceMismatch);
            }
        }
        Ok(())
    }
}

/// クレームの検証オプション
#[derive(Debug, Clone, Copy)]
pub struct ClaimValidationOptions<'a> {
    /// 検証に使う現在時刻 (UNIX 秒)
    pub reference_time_seconds: f64,
    /// `exp` / `nbf` に許容するずれ (秒)
    pub clock_tolerance_seconds: f64,
    /// 期待する `iss` の一覧。空の場合は検証しない
    pub expected_issuers: &'a [&'a str],
    /// 期待する `aud` の一覧。空の場合は検証しない
    pub expected_audiences: &'a [&'a str],
}

impl Default for ClaimValidationOptions<'_> {
    fn default() -> Self {
        Self {
            reference_time_seconds: 0.0,
            clock_tolerance_seconds: 0.0,
            expected_issuers: &[],
            expected_audiences: &[],
        }
    }
}

/// クレームの検証エラー
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimValidationError {
    /// `exp` を過ぎている
    Expired,
    /// `nbf` より前である
    NotYetValid,
    /// `iss` が期待する発行者と一致しない
    IssuerMismatch,
    /// `aud` が期待する宛先と一致しない
    AudienceMismatch,
    /// 検証に渡された現在時刻または許容ずれが有限でない / 負である
    InvalidReferenceTime,
    /// クレームの数値が有限でない (NaN / 無限大)
    NonFiniteClaim(&'static str),
}

impl fmt::Display for ClaimValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Expired => write!(f, "token is expired"),
            Self::NotYetValid => write!(f, "token is not yet valid"),
            Self::IssuerMismatch => write!(f, "token issuer does not match"),
            Self::AudienceMismatch => write!(f, "token audience does not match"),
            Self::InvalidReferenceTime => write!(
                f,
                "reference time must be finite and clock tolerance must be finite and non-negative"
            ),
            Self::NonFiniteClaim(name) => write!(f, "{name} must be finite"),
        }
    }
}

impl core::error::Error for ClaimValidationError {}

/// 署名検証のオプション
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VerifyOptions {
    /// トークンの `alg` に期待するアルゴリズム
    pub expected_algorithm: Option<Algorithm>,
}

/// CAT のトークン
///
/// 生トークン (bearer クレデンシャル) と署名は [`Debug`] では長さだけを表示する。
#[derive(Clone, PartialEq)]
pub struct CatToken {
    format: TokenFormat,
    /// `decode` に渡された生バイト (DPoP の `ath` に使う)
    raw: Vec<u8>,
    protected: Vec<u8>,
    unprotected: Vec<(Value, Value)>,
    payload: Vec<u8>,
    signature: Vec<u8>,
    signing_input: Vec<u8>,
    header: Header,
    claims: CatClaims,
}

impl core::fmt::Debug for CatToken {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CatToken")
            .field("format", &self.format)
            .field("header", &self.header)
            .field("claims", &self.claims)
            .field("raw_bytes", &self.raw.len())
            .field("payload_bytes", &self.payload.len())
            .field("signature_bytes", &self.signature.len())
            .finish()
    }
}

impl CatToken {
    /// トークンをデコードする
    ///
    /// `.` で区切られた 3 分割の compact 形式、COSE 形式の CBOR、COSE 形式を
    /// base64url で包んだテキストの順に判別する。
    pub fn decode(input: &[u8]) -> Result<Self, CatError> {
        let mut token = if let Ok(text) = core::str::from_utf8(input)
            && text.matches('.').count() == 2
        {
            Self::decode_compact(text)?
        } else {
            match Self::decode_cose(input) {
                Ok(token) => token,
                Err(error) => {
                    if let Ok(text) = core::str::from_utf8(input)
                        && let Ok(bytes) = super::base64url::decode(text.trim())
                    {
                        Self::decode_cose(&bytes)?
                    } else {
                        return Err(error);
                    }
                }
            }
        };
        // `ath` は呼び出し側が decode に渡した表現をハッシュする
        token.raw = input.to_vec();
        Ok(token)
    }

    /// compact 形式 (`base64url(protected).base64url(claims).base64url(signature)`) をデコードする
    pub fn decode_compact(text: &str) -> Result<Self, CatError> {
        let parts: Vec<&str> = text.split('.').collect();
        if parts.len() != 3 {
            return Err(CatError::InvalidTokenFormat);
        }
        let protected = base64url_decode(parts[0])?;
        let payload = base64url_decode(parts[1])?;
        let signature = base64url_decode(parts[2])?;
        let header = Header::decode_protected(&cbor::decode(&protected)?)?;
        if header.algorithm.is_none() {
            return Err(CatError::MissingAlgorithm);
        }
        let claims = CatClaims::decode(&cbor::decode(&payload)?)?;
        // 署名対象は base64url のままの protected と claims (付録 A のベクタ)
        let signing_input = format!("{}.{}", parts[0], parts[1]).into_bytes();
        Ok(Self {
            format: TokenFormat::Compact,
            raw: text.as_bytes().to_vec(),
            protected,
            unprotected: Vec::new(),
            payload,
            signature,
            signing_input,
            header,
            claims,
        })
    }

    /// COSE 形式 (CBOR) をデコードする
    pub fn decode_cose(bytes: &[u8]) -> Result<Self, CatError> {
        let message = CoseMessage::decode(bytes)?;
        let header = message.header()?;
        if header.algorithm.is_none() {
            return Err(CatError::MissingAlgorithm);
        }
        let payload = message.payload().ok_or(CatError::DetachedPayload)?.to_vec();
        let claims = CatClaims::decode(&cbor::decode(&payload)?)?;
        let signing_input = message.signing_input()?;
        let (format, protected, unprotected, signature) = match message {
            CoseMessage::Sign1(message) => (
                TokenFormat::CoseSign1,
                message.protected,
                message.unprotected,
                message.signature,
            ),
            CoseMessage::Mac0(message) => (
                TokenFormat::CoseMac0,
                message.protected,
                message.unprotected,
                message.tag,
            ),
        };
        Ok(Self {
            format,
            raw: bytes.to_vec(),
            protected,
            unprotected,
            payload,
            signature,
            signing_input,
            header,
            claims,
        })
    }

    /// MOQT の Auth Token Type と Token Value からデコードする
    ///
    /// draft-ietf-moq-c4m-01 §7.1 の Token Type が 0x01 (CAT) 以外の場合はエラーを
    /// 返す。
    pub fn decode_moqt_auth_token(token_type: u64, value: &[u8]) -> Result<Self, CatError> {
        if token_type != MOQT_AUTH_TOKEN_TYPE_CAT {
            return Err(CatError::InvalidAuthTokenType(token_type));
        }
        Self::decode(value)
    }

    /// 直列化の形式を返す
    pub fn format(&self) -> TokenFormat {
        self.format
    }

    /// `decode` に渡された生バイトを返す
    ///
    /// compact 形式では ASCII のトークン文字列、COSE 形式では CBOR のバイト列、
    /// base64url で包んだ入力を渡した場合はそのテキストである。DPoP の `ath` は
    /// このバイト列をハッシュする (`DpopProof::verify_against_cat_token`)。
    pub fn raw_token(&self) -> &[u8] {
        &self.raw
    }

    /// protected / unprotected を統合したヘッダを返す
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// クレームを返す
    pub fn claims(&self) -> &CatClaims {
        &self.claims
    }

    /// 署名対象のバイト列を返す
    pub fn signing_input(&self) -> &[u8] {
        &self.signing_input
    }

    /// 署名または MAC を返す
    pub fn signature(&self) -> &[u8] {
        &self.signature
    }

    /// protected ヘッダの CBOR バイト列を返す
    pub fn protected_header(&self) -> &[u8] {
        &self.protected
    }

    /// クレームセットの CBOR バイト列を返す
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// COSE 形式の場合の unprotected ヘッダを返す
    pub fn unprotected_header(&self) -> &[(Value, Value)] {
        &self.unprotected
    }

    /// トークンの署名 / MAC を検証する
    pub fn verify<C: CoseCrypto>(&self, crypto: &C, key: &CoseKey) -> Result<(), CatError> {
        self.verify_with(crypto, key, &VerifyOptions::default())
    }

    /// トークンの署名 / MAC を検証する
    ///
    /// `options.expected_algorithm` が指定され、トークンの `alg` と一致しない場合は
    /// [`CatError::AlgorithmMismatch`] を返す。
    pub fn verify_with<C: CoseCrypto>(
        &self,
        crypto: &C,
        key: &CoseKey,
        options: &VerifyOptions,
    ) -> Result<(), CatError> {
        let algorithm = self.header.algorithm.ok_or(CatError::MissingAlgorithm)?;
        if let Some(expected) = options.expected_algorithm
            && expected != algorithm
        {
            return Err(CatError::AlgorithmMismatch {
                token: algorithm,
                expected,
            });
        }
        crypto.verify(algorithm, key, &self.signing_input, &self.signature)?;
        Ok(())
    }
}

/// CAT のトークンを作るビルダー
///
/// 発行 (署名) には [`CoseCrypto`] の実装と秘密鍵が必要である。
#[derive(Debug, Clone, Default)]
pub struct CatTokenBuilder {
    /// 発行するクレームセット
    pub claims: CatClaims,
    /// protected ヘッダに置く鍵識別子 (`kid`)
    pub key_id: Option<KeyId>,
    /// COSE ヘッダの `typ`。未指定の場合は `"CAT"` を使う
    pub typ: Option<String>,
    /// 署名アルゴリズム。未指定の場合は鍵の種別から決める
    pub algorithm: Option<Algorithm>,
}

impl CatTokenBuilder {
    /// 空のビルダーを作る
    pub fn new() -> Self {
        Self::default()
    }

    /// `iss` を設定する
    pub fn issuer(mut self, issuer: impl Into<String>) -> Self {
        self.claims.issuer = Some(issuer.into());
        self
    }

    /// `sub` を設定する
    pub fn subject(mut self, subject: impl Into<String>) -> Self {
        self.claims.subject = Some(subject.into());
        self
    }

    /// `aud` を追加する
    pub fn audience(mut self, audience: impl Into<String>) -> Self {
        self.claims.audience.push(audience.into());
        self
    }

    /// `exp` (UNIX 秒) を設定する
    pub fn expiration(mut self, expiration: f64) -> Self {
        self.claims.expiration = Some(expiration);
        self
    }

    /// `nbf` (UNIX 秒) を設定する
    pub fn not_before(mut self, not_before: f64) -> Self {
        self.claims.not_before = Some(not_before);
        self
    }

    /// `iat` (UNIX 秒) を設定する
    pub fn issued_at(mut self, issued_at: f64) -> Self {
        self.claims.issued_at = Some(issued_at);
        self
    }

    /// `cti` をバイト文字列として設定する
    pub fn cwt_id(mut self, cwt_id: impl Into<Vec<u8>>) -> Self {
        self.claims.cwt_id = Some(cwt_id.into());
        self
    }

    /// `moqt` クレームを設定する
    pub fn moqt(mut self, moqt: MoqtClaim) -> Self {
        self.claims.moqt = Some(moqt);
        self
    }

    /// `moqt-reval` (再検証間隔、秒) を設定する
    pub fn moqt_reval(mut self, seconds: f64) -> Self {
        self.claims.moqt_reval = Some(seconds);
        self
    }

    /// `cnf` の `jkt` を IANA 登録の confirmation key 323 で設定する
    pub fn jwk_thumbprint(mut self, thumbprint: impl Into<Vec<u8>>) -> Self {
        let confirmation = self
            .claims
            .confirmation
            .get_or_insert_with(Default::default);
        confirmation.jwk_thumbprint = Some(thumbprint.into());
        self
    }

    /// `cnf` の `jkt` を draft-ietf-moq-c4m-01 のベクタが使う key 3 で設定する
    pub fn c4m_draft_jwk_thumbprint(mut self, thumbprint: impl Into<Vec<u8>>) -> Self {
        let confirmation = self
            .claims
            .confirmation
            .get_or_insert_with(Default::default);
        confirmation.c4m_draft_jwk_thumbprint = Some(thumbprint.into());
        self
    }

    /// `catdpop` を設定する
    pub fn catdpop(mut self, window_seconds: f64, honor_jti: bool) -> Self {
        self.claims.catdpop = Some(CatDpop::new(window_seconds, honor_jti));
        self
    }

    /// 任意のクレームを追加する
    ///
    /// 型付きフィールドを持つ claim key (`iss` / `moqt` / `catdpop` など) には専用の
    /// 設定メソッドを使うこと。ここに型付きキーを渡した場合は、型付きフィールドの
    /// 設定有無にかかわらずエンコード時に [`CatError::DuplicateClaim`] を返す。
    pub fn claim(mut self, key: i64, value: Value) -> Self {
        self.claims.raw.push((Value::integer(key), value));
        self
    }

    /// `kid` を設定する
    pub fn key_id(mut self, key_id: KeyId) -> Self {
        self.key_id = Some(key_id);
        self
    }

    /// `typ` を設定する
    pub fn typ(mut self, typ: impl Into<String>) -> Self {
        self.typ = Some(typ.into());
        self
    }

    /// 署名アルゴリズムを設定する
    pub fn algorithm(mut self, algorithm: Algorithm) -> Self {
        self.algorithm = Some(algorithm);
        self
    }

    /// compact 形式 (draft-ietf-moq-c4m-01 付録 A) のトークンを発行する
    ///
    /// HMAC-SHA256 のアルゴリズム識別子は RFC 9053 の HMAC 256/256 (5) を使う。
    /// ドラフト付録 A のベクタは -4 を使うが、IANA の COSE Algorithms レジストリでは
    /// -4 は A192KW であり発行には使わない (検証は
    /// [`super::cose::C4M_DRAFT_HMAC_SHA256_ALGORITHM_ID`] も HMAC-SHA256 として受理する)。
    pub fn build_compact<C: CoseCrypto>(
        &self,
        crypto: &C,
        key: &CoseKey,
    ) -> Result<String, CatError> {
        let algorithm = self.signing_algorithm(key)?;
        let protected = self.encode_protected_header(algorithm.identifier())?;
        let payload = cbor::encode(&self.claims.encode()?)?;
        let protected_text = base64url_encode(&protected);
        let payload_text = base64url_encode(&payload);
        let signing_input = format!("{protected_text}.{payload_text}");
        let signature = crypto.sign(algorithm, key, signing_input.as_bytes())?;
        Ok(format!("{signing_input}.{}", base64url_encode(&signature)))
    }

    /// COSE 形式 (CWT + COSE_Sign1 / COSE_Mac0) のトークンを発行する
    ///
    /// CWT タグ (61) と COSE タグ (17 / 18) を付与する。
    pub fn build_cose<C: CoseCrypto>(
        &self,
        crypto: &C,
        key: &CoseKey,
    ) -> Result<Vec<u8>, CatError> {
        self.build_cose_with(crypto, key, &CoseEncodingOptions::default())
    }

    /// タグの付与を指定して COSE 形式のトークンを発行する
    pub fn build_cose_with<C: CoseCrypto>(
        &self,
        crypto: &C,
        key: &CoseKey,
        options: &CoseEncodingOptions,
    ) -> Result<Vec<u8>, CatError> {
        let algorithm = self.signing_algorithm(key)?;
        let protected = self.encode_protected_header(algorithm.identifier())?;
        let payload = cbor::encode(&self.claims.encode()?)?;
        let message = if algorithm.is_mac() {
            CoseMessage::Mac0(CoseMac0 {
                protected,
                unprotected: Vec::new(),
                payload: Some(payload),
                tag: Vec::new(),
                cose_tagged: options.cose_tag,
                cwt_tagged: options.cwt_tag,
            })
        } else {
            CoseMessage::Sign1(CoseSign1 {
                protected,
                unprotected: Vec::new(),
                payload: Some(payload),
                signature: Vec::new(),
                cose_tagged: options.cose_tag,
                cwt_tagged: options.cwt_tag,
            })
        };
        let signing_input = message.signing_input()?;
        let signature = crypto.sign(algorithm, key, &signing_input)?;
        let message = match message {
            CoseMessage::Sign1(mut message) => {
                message.signature = signature;
                CoseMessage::Sign1(message)
            }
            CoseMessage::Mac0(mut message) => {
                message.tag = signature;
                CoseMessage::Mac0(message)
            }
        };
        Ok(message.encode(options)?)
    }

    /// 鍵の種別から署名アルゴリズムを決める
    fn signing_algorithm(&self, key: &CoseKey) -> Result<Algorithm, CatError> {
        if let Some(algorithm) = self.algorithm {
            return Ok(algorithm);
        }
        Ok(super::crypto::default_signing_algorithm(key))
    }

    /// protected ヘッダをエンコードする
    fn encode_protected_header(&self, algorithm_identifier: i64) -> Result<Vec<u8>, CatError> {
        let mut entries = vec![
            (
                Value::integer(HEADER_ALGORITHM),
                Value::integer(algorithm_identifier),
            ),
            (
                Value::integer(HEADER_TYPE),
                Value::TextString(self.typ.clone().unwrap_or_else(|| String::from("CAT"))),
            ),
        ];
        if let Some(key_id) = &self.key_id {
            let value = match key_id {
                KeyId::Bytes(bytes) => Value::ByteString(bytes.clone()),
                KeyId::Text(text) => Value::TextString(text.clone()),
            };
            entries.push((Value::integer(HEADER_KEY_ID), value));
        }
        Ok(cbor::encode(&Value::Map(entries))?)
    }
}

/// CAT のエラー
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatError {
    /// CBOR のエラー
    Cbor(CborError),
    /// COSE のエラー
    Cose(CoseError),
    /// C4M のクレームのエラー
    C4m(C4mError),
    /// 署名 / 検証のエラー
    Crypto(CryptoError),
    /// base64url のデコードに失敗した
    InvalidBase64,
    /// トークンの形式を判別できない
    InvalidTokenFormat,
    /// 期待した型と異なる
    UnexpectedType(&'static str),
    /// 同じ claim key が型付きフィールドと raw の両方にある
    DuplicateClaim(i64),
    /// COSE ヘッダに `alg` が無い
    MissingAlgorithm,
    /// トークンの `alg` が期待するアルゴリズムと一致しない
    AlgorithmMismatch {
        /// トークンのアルゴリズム
        token: Algorithm,
        /// 期待したアルゴリズム
        expected: Algorithm,
    },
    /// MOQT の Auth Token Type が CAT (0x01) 以外である
    InvalidAuthTokenType(u64),
    /// detached payload は扱わない
    DetachedPayload,
    /// 数値クレームが有限でない (NaN / 無限大)
    NonFiniteNumber(&'static str),
}

impl fmt::Display for CatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cbor(error) => write!(f, "CBOR error: {error}"),
            Self::Cose(error) => write!(f, "COSE error: {error}"),
            Self::C4m(error) => write!(f, "C4M claim error: {error}"),
            Self::Crypto(error) => write!(f, "crypto error: {error}"),
            Self::InvalidBase64 => write!(f, "invalid base64url encoding"),
            Self::InvalidTokenFormat => write!(f, "invalid CAT token format"),
            Self::UnexpectedType(expected) => write!(f, "expected {expected}"),
            Self::DuplicateClaim(key) => write!(f, "duplicate claim key: {key}"),
            Self::MissingAlgorithm => write!(f, "COSE header has no alg parameter"),
            Self::AlgorithmMismatch { token, expected } => write!(
                f,
                "token algorithm {:?} does not match expected {:?}",
                token, expected
            ),
            Self::InvalidAuthTokenType(token_type) => {
                write!(f, "unsupported MOQT auth token type: {token_type:#x}")
            }
            Self::DetachedPayload => write!(f, "detached payload is not supported"),
            Self::NonFiniteNumber(name) => write!(f, "{name} must be finite"),
        }
    }
}

impl core::error::Error for CatError {}

impl From<CborError> for CatError {
    fn from(error: CborError) -> Self {
        Self::Cbor(error)
    }
}

impl From<CoseError> for CatError {
    fn from(error: CoseError) -> Self {
        Self::Cose(error)
    }
}

impl From<C4mError> for CatError {
    fn from(error: C4mError) -> Self {
        Self::C4m(error)
    }
}

impl From<CryptoError> for CatError {
    fn from(error: CryptoError) -> Self {
        Self::Crypto(error)
    }
}

/// パディング無しの base64url でエンコードする (RFC 4648 §5)
fn base64url_encode(bytes: &[u8]) -> String {
    super::base64url::encode(bytes)
}

/// パディング無しの base64url をデコードする (RFC 4648 §5)
fn base64url_decode(text: &str) -> Result<Vec<u8>, CatError> {
    super::base64url::decode(text).map_err(|_| CatError::InvalidBase64)
}
