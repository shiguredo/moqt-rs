//! CAT (Common Access Token) のクレームとトークン
//!
//! CTA-5007-B の CAT を CWT (RFC 8392) のクレームとして扱い、draft-ietf-moq-c4m-02 の
//! `moqt` / `moqt-reval` クレームなどを加えたトークンの発行と検証を提供する。
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
//!
//! bearer トークンの接続束縛 (draft-ietf-moq-c4m-02 §3 / §7.2) は、受信者が所有する
//! [`BearerReplayCache`] と [`CatToken::verify_bearer`] で扱う。Sans I/O を保つため、
//! 接続識別子と時刻はすべて呼び出し側から渡す。

use alloc::collections::BTreeMap;
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

/// `cnf` の `COSE_Key` (確認用の公開鍵) の confirmation key
///
/// RFC 8747 §3.1 Table 1 / §3.2 の値 (1)。draft-ietf-moq-c4m-02 §4.2 の DPoP proof は
/// `kccs` の `cnf` にこの confirmation method でクライアントの公開鍵を入れる。
pub const CONFIRMATION_COSE_KEY: i64 = 1;

/// draft-ietf-moq-c4m-01 のベクタが `jkt` に使う confirmation key
///
/// 付録 A.4 のベクタは 3 を使うが、IANA の CWT Confirmation Methods レジストリでは
/// 3 は `kid` (RFC 8747 §3.4) である。検証では両方を受け、発行は
/// [`CONFIRMATION_JWK_THUMBPRINT`] を既定とする。
pub const CONFIRMATION_C4M_DRAFT_JWK_THUMBPRINT: i64 = 3;

/// MOQT の Auth Token Type (CAT) (draft-ietf-moq-c4m-01 §7.1)
pub const MOQT_AUTH_TOKEN_TYPE_CAT: u64 = 0x01;

/// CAT の COSE ヘッダの `typ` の値
///
/// draft-ietf-moq-c4m-01 付録 A のテストベクタが protected ヘッダに置く値。
/// [`VerifyOptions::expected_type`] に渡して検証できる。
pub const CAT_CONTENT_TYPE: &str = "CAT";

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

/// CAT の `catreplay` クレームの値 (CTA-5007-B)
///
/// draft-ietf-moq-c4m-02 §3 は、MOQT では同じトークンを SETUP と後続のアクション
/// (SUBSCRIBE / PUBLISH / FETCH など) で同じ接続上に提示できるため、値 1 を
/// 「単一接続」と解釈する。そのため本ライブラリでも [`CatReplay::SingleConnection`]
/// を「1 つの接続での再利用だけを許す」値として扱う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CatReplay {
    /// 0: リプレイ保護を行わない (クレームが無い場合の既定)
    NotProtected,
    /// 1: 単一接続での再利用だけを許す (draft-ietf-moq-c4m-02 §3)
    SingleConnection,
    /// 2: リプレイの意味論は実装定義 (draft-ietf-moq-c4m-02 §3)
    ImplementationDefined,
}

impl CatReplay {
    /// `catreplay` の整数値を返す
    pub const fn key(self) -> i64 {
        match self {
            Self::NotProtected => 0,
            Self::SingleConnection => 1,
            Self::ImplementationDefined => 2,
        }
    }

    /// 整数値から [`CatReplay`] を返す
    ///
    /// 0 / 1 / 2 以外の整数は解釈できないため `None` を返す。
    pub const fn from_key(key: i64) -> Option<Self> {
        match key {
            0 => Some(Self::NotProtected),
            1 => Some(Self::SingleConnection),
            2 => Some(Self::ImplementationDefined),
            _ => None,
        }
    }
}

/// CAT のクレームセット (CWT のクレーム + CAT / C4M のクレーム)
///
/// 型付きで解釈しないクレームは [`CatClaims::raw`] にそのまま保持する。CAT の
/// クレームは IANA の登録と draft-ietf-moq-c4m-01 のベクタで値型が一致しないもの
/// (catv / catu など) があるため、意味論を定める C4M のクレームだけを型付きにする。
///
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
    /// `moqt` (draft-ietf-moq-c4m-02 §2.1)
    ///
    /// キー 0 以外のキーを含む claim は [`MoqtClaim::decode`] が拒否する (§2.1.1)。
    pub moqt: Option<MoqtClaim>,
    /// `moqt-reval` (draft-ietf-moq-c4m-02 §2.2) の再検証間隔 (秒)
    ///
    /// §2.2 は「再検証できない受信者は `moqt-reval` 付きトークンを拒否する MUST」
    /// 「再検証間隔が自身の能力を下回る場合も拒否する MUST」を定める。本ライブラリは
    /// Sans-I/O のため再検証の実行は行わず、この値の解釈と拒否の判断は利用側が行う。
    pub moqt_reval: Option<f64>,
    /// `catdpop` (draft-ietf-moq-c4m-01 §3.1.1)
    pub catdpop: Option<CatDpop>,
    /// `catreplay` (draft-ietf-moq-c4m-02 §3)
    ///
    /// 0 / 1 / 2 だけを型付きで解釈し、それ以外の値や整数でない値は
    /// [`CatClaims::decode`] が拒否する。値 1 の接続束縛の検証は
    /// [`CatToken::verify_bearer`] が行う。
    pub cat_replay: Option<CatReplay>,
    /// 型付きで解釈しなかったクレーム
    ///
    /// 旧 draft-01 のベクタが使う `moqt` / `moqt-reval` の label (327 / 328) は
    /// 暫定 label の alias にせず、ここに保持する。
    pub raw: Vec<(Value, Value)>,
}

/// 型付きフィールドを持つ claim key かどうかを返す
///
/// `moqt` / `moqt-reval` は暫定 label ([`CLAIM_MOQT`] / [`CLAIM_MOQT_REVAL`]) を
/// 使い、旧 draft-01 のベクタの 327 / 328 は alias にしない。`catreplay` は
/// 0 / 1 / 2 を型付きで解釈する。
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
            | CLAIM_CAT_REPLAY
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
    ///
    /// `moqt` / `moqt-reval` は draft-ietf-moq-c4m-02 §A.5 の暫定 label
    /// (-65537 / -65538) だけを型付きで解釈する。旧 draft-01 のベクタの 327 / 328 は
    /// 未知のクレームとして raw に保持し、型付きフィールドへは写さない。
    /// `catreplay` は整数の 0 / 1 / 2 だけを型付きで解釈し、整数でない値とそれ以外の
    /// 整数はエラーにする。
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
                CLAIM_CAT_REPLAY => {
                    // IANA の CWT Claims レジストリと CTA-5007-B では catreplay の値型は
                    // unsigned integer である。負値・浮動小数点数・テキストは意味を
                    // 解釈できないため拒否する
                    let value = entry
                        .as_unsigned()
                        .ok_or(CatError::UnexpectedType("catreplay"))?;
                    claims.cat_replay = Some(
                        u8::try_from(value)
                            .ok()
                            .and_then(|value| CatReplay::from_key(i64::from(value)))
                            .ok_or(CatError::InvalidCatReplayValue(value))?,
                    );
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
        if let Some(cat_replay) = self.cat_replay {
            entries.push((
                Value::integer(CLAIM_CAT_REPLAY),
                Value::integer(cat_replay.key()),
            ));
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
    ///
    /// この関数はクレームだけを見る stateless な検証であり、`catreplay` が要求する
    /// 接続への束縛も、`catreplay` 1 の cti / exp 必須条件も検証しない。つまり単独では、
    /// `catreplay` 1 のトークンが別の接続で使われても検出できず、cti / exp を欠いた
    /// トークンも受理され得る。接続束縛まで含めて bearer トークンを検証する場合は
    /// [`CatToken::verify_bearer`] を使うこと。
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

/// bearer トークンの cti と接続の束縛のエラー
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BearerReplayError {
    /// 許容 leeway が有限でない、または負である
    InvalidLeeway,
    /// 保持期限が有限でない (NaN / 無限大)
    NonFiniteRetention,
    /// 参照時刻が有限でない (NaN / 無限大)
    NonFiniteReferenceTime,
    /// 同じ cti が別の接続に束縛されている
    ConnectionMismatch,
    /// cti が空である
    EmptyCti,
    /// 接続識別子が空である
    EmptyConnectionId,
}

impl fmt::Display for BearerReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLeeway => write!(f, "leeway must be finite and non-negative"),
            Self::NonFiniteRetention => write!(f, "retention deadline must be finite"),
            Self::NonFiniteReferenceTime => write!(f, "reference time must be finite"),
            Self::ConnectionMismatch => {
                write!(f, "bearer token is already bound to another connection")
            }
            Self::EmptyCti => write!(f, "cti must not be empty"),
            Self::EmptyConnectionId => write!(f, "connection identifier must not be empty"),
        }
    }
}

impl core::error::Error for BearerReplayError {}

/// cti に束縛した接続と保持期限
#[derive(Debug, Clone, PartialEq)]
struct BearerReplayBinding {
    /// アプリケーションが発行した opaque な接続識別子
    connection_id: Vec<u8>,
    /// この時刻を超えるまで記録を保持する (`exp` + leeway)
    retain_until: f64,
}

/// bearer トークンの cti を接続に束縛するリプレイキャッシュ
///
/// draft-ietf-moq-c4m-02 §3 の `catreplay` 1 を「単一接続」の意味で扱うための、
/// 受信者側の状態である。§7.2 のとおり、この束縛はトークンを受け付けたリレーが
/// 所有する。
///
/// - 1 つのキャッシュは受信者が所有し、その受信者が受理するトークン群と全接続で
///   共有する。接続ごとに作り直してはならない。作り直すと、記録が接続とともに
///   消えて別の接続での再利用を検出できなくなる
/// - 状態は `cti` から (接続識別子, 保持期限) への写像である。同じ cti が同じ接続で
///   提示された場合は再利用として受理し、別の接続で提示された場合は拒否する
/// - 接続識別子はアプリケーションが発行する opaque な値である。記録が残っている間は
///   同じ値を使い回さないこと。接続が終了しても記録を削除しないため、終了した接続の
///   識別子を新しい接続に再利用すると、その新しい接続が拒否される
/// - 接続が終了しても記録を削除しない。参照時刻が保持期限 (`exp` + leeway) を
///   超えたときにだけ記録を解放する。§3 は「接続が終了した後も、トークンが受理され
///   得る間 (許容される期限の leeway を含む) はリプレイ状態を保持する MUST」を
///   定めている
/// - 未失効の記録を追い出して別接続での再利用を許すことはしない。記録数の上限は
///   持たないため、肥大化を防ぐ必要がある場合は token の発行者が発行する cti の
///   長さや受理するトークン数を制限すること
/// - 許容 leeway は構築時に固定し、[`CatToken::verify_bearer`] の `exp` 検証と保持期限の
///   両方に同じ値を使う
///
/// ネットワーク越しの共有やリレー・クラスタ間の状態共有は行わない。§7.2 が推奨する
/// 複数リレー間での束縛の共有はアプリケーションの責務であり、この型は Sans I/O の
/// まま 1 つの受信者が持つ状態だけを扱う。プロセスをまたぐ永続化も行わないため、
/// 再起動すると保持期限前の束縛も失われる。
///
/// [`BearerReplayCache::check_and_bind`] が成功するたびに全記録を走査するため、記録数に
/// 対して O(n) のコストがかかる。
///
/// 参照時刻は単調増加を前提とする。期限切れの記録は参照時刻が保持期限を超えた時点で
/// 解放されるため、時計が巻き戻ると解放済みの `cti` が再び受理され得る。
///
/// 束縛は `cti` だけをキーにし、発行者 (`iss`) を含めない。複数の発行者のトークンを
/// 同時に受理する場合、`cti` が発行者をまたいで一意であることを前提とする。
#[derive(Debug)]
pub struct BearerReplayCache {
    /// 通常の期限検証と保持期限に使う許容 leeway (秒)
    leeway_seconds: f64,
    /// cti → (接続識別子, 保持期限)
    bindings: BTreeMap<Vec<u8>, BearerReplayBinding>,
}

impl BearerReplayCache {
    /// 許容 leeway (秒) を固定してキャッシュを作る
    ///
    /// leeway は有限で 0 以上でなければならない。不正な場合は
    /// [`BearerReplayError::InvalidLeeway`] を返す。
    pub fn new(leeway_seconds: f64) -> Result<Self, BearerReplayError> {
        if !leeway_seconds.is_finite() || leeway_seconds < 0.0 {
            return Err(BearerReplayError::InvalidLeeway);
        }
        Ok(Self {
            leeway_seconds,
            bindings: BTreeMap::new(),
        })
    }

    /// 構築時に固定した許容 leeway (秒) を返す
    pub fn leeway_seconds(&self) -> f64 {
        self.leeway_seconds
    }

    /// cti を接続に束縛できるか確認し、束縛できれば記録する
    ///
    /// `retain_until` はこの cti の記録を保持する期限 (`exp` + leeway)、
    /// `reference_time_seconds` は検証に使う現在時刻 (UNIX 秒) である。
    ///
    /// この呼び出しが成功する経路でだけ、参照時刻が保持期限を超えた記録を解放する。
    /// 参照時刻が保持期限と等しい間は記録を保持するため、期限の境界では別接続からの
    /// 利用を拒否し続ける。未失効の記録は参照されなくても解放しない。
    ///
    /// `retain_until` は `reference_time_seconds` 以降であることを前提とする。期限を
    /// 既に過ぎた値を渡した場合は、その束縛をそのまま登録して `Ok` を返す
    /// (`verify_bearer` は期限検証を先に行うため、この状態にはならない)。
    ///
    /// 同じ cti が同じ接続に束縛済みの場合は再利用として `Ok` を返す。このとき
    /// `retain_until` が既存の保持期限より後なら、トークンが受理され得る期間に合わせて
    /// 保持期限を延ばす。同じ cti が別の接続に束縛済みの場合は
    /// [`BearerReplayError::ConnectionMismatch`] を返し、記録を変更しない。記録が無い
    /// 場合は新しい束縛を登録する。
    ///
    /// `retain_until` と `reference_time_seconds` が有限でない場合はエラーを返し、
    /// 記録を変更しない。
    pub fn check_and_bind(
        &mut self,
        cti: &[u8],
        connection_id: &[u8],
        retain_until: f64,
        reference_time_seconds: f64,
    ) -> Result<(), BearerReplayError> {
        // 空の cti や接続識別子を受け入れると、全接続が同じ束縛に一致して
        // リプレイ保護が無効になるため、契約違反として拒否する
        if cti.is_empty() {
            return Err(BearerReplayError::EmptyCti);
        }
        if connection_id.is_empty() {
            return Err(BearerReplayError::EmptyConnectionId);
        }
        if !reference_time_seconds.is_finite() {
            return Err(BearerReplayError::NonFiniteReferenceTime);
        }
        if !retain_until.is_finite() {
            return Err(BearerReplayError::NonFiniteRetention);
        }
        // 未失効の束縛が別の接続にある場合は、他の記録も含めて何も変更せずに拒否する
        if let Some(binding) = self.bindings.get(cti)
            && binding.connection_id != connection_id
            && reference_time_seconds <= binding.retain_until
        {
            return Err(BearerReplayError::ConnectionMismatch);
        }
        // ここから先は束縛が成功する経路である。参照時刻が保持期限を超えた記録だけを
        // 解放する (等しい間は保持する)
        self.bindings
            .retain(|_, binding| reference_time_seconds <= binding.retain_until);
        match self.bindings.get_mut(cti) {
            // 期限切れの記録は上の retain で消えているため、残っているのは同じ接続の
            // 記録だけである
            Some(binding) => {
                if retain_until > binding.retain_until {
                    binding.retain_until = retain_until;
                }
                Ok(())
            }
            None => {
                self.bindings.insert(
                    cti.to_vec(),
                    BearerReplayBinding {
                        connection_id: connection_id.to_vec(),
                        retain_until,
                    },
                );
                Ok(())
            }
        }
    }

    /// 記録数を返す
    ///
    /// 期限切れの記録は次の [`BearerReplayCache::check_and_bind`] の成功経路で解放
    /// されるまで残るため、この数には期限切れの記録も含まれ得る。
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    /// 記録が空かどうかを返す
    ///
    /// 期限切れの記録は次の [`BearerReplayCache::check_and_bind`] の成功経路で解放
    /// されるまで残るため、期限切れの記録だけが残っている場合も `false` を返す。
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

/// 署名検証のオプション
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VerifyOptions<'a> {
    /// トークンの `alg` に期待するアルゴリズム
    pub expected_algorithm: Option<Algorithm>,
    /// トークンの `typ` に期待する値
    ///
    /// CAT では [`CAT_CONTENT_TYPE`] (`"CAT"`) を指定する。未指定の場合は `typ` を
    /// 検証しない。`typ` がテキスト文字列でない場合と一致しない場合は
    /// [`CatError::TypeMismatch`] を返す。
    pub expected_type: Option<&'a str>,
}

/// bearer トークンの検証オプション
///
/// [`CatToken::verify_bearer`] に渡す入力である。署名検証の設定と、要求された
/// アクション・名前空間・トラック、接続識別子をまとめる。
#[derive(Debug, Clone, Copy)]
pub struct BearerVerificationOptions<'a> {
    /// 検証に使う現在時刻 (UNIX 秒)
    ///
    /// `exp` / `nbf` の検証と、保持期限を過ぎた束縛の解放の両方に使う。
    pub reference_time_seconds: f64,
    /// 期待する `iss` の一覧。空の場合は検証しない
    pub expected_issuers: &'a [&'a str],
    /// 期待する `aud` の一覧。空の場合は検証しない
    pub expected_audiences: &'a [&'a str],
    /// 署名検証のオプション
    pub verify_options: VerifyOptions<'a>,
    /// 要求されたアクション
    pub action: MoqtAction,
    /// 対象の Track Namespace のフィールド列
    pub namespace: &'a [&'a [u8]],
    /// 対象の Track Name
    pub track_name: &'a [u8],
    /// トークンを受け付けた接続の識別子
    ///
    /// アプリケーションが発行する opaque な値を使う。同じ接続の間は同じ値、別の
    /// 接続では別の値を渡すこと。詳細は [`BearerReplayCache`] を参照。
    pub connection_id: &'a [u8],
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
    /// base64url または標準 Base64 で包んだテキストの順に判別する。標準 Base64 は
    /// URL に埋め込む場合の表現 (draft-ietf-moq-c4m-01 §2 / §4) である。
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
                        && let Ok(bytes) = super::base64url::decode_base64_or_url(text.trim())
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
    ///
    /// 検証するのは署名だけであり、`catreplay` の接続束縛も `cti` / `exp` の必須条件も
    /// 検証しない。bearer トークンの接続束縛まで含めた検証には
    /// [`CatToken::verify_bearer`] を使うこと。
    pub fn verify<C: CoseCrypto>(&self, crypto: &C, key: &CoseKey) -> Result<(), CatError> {
        self.verify_with(crypto, key, &VerifyOptions::default())
    }

    /// トークンの署名 / MAC を検証する
    ///
    /// `options.expected_algorithm` が指定され、トークンの `alg` と一致しない場合は
    /// [`CatError::AlgorithmMismatch`] を返す。
    ///
    /// 検証するのは署名だけであり、`catreplay` の接続束縛も `cti` / `exp` の必須条件も
    /// 検証しない。bearer トークンの接続束縛まで含めた検証には
    /// [`CatToken::verify_bearer`] を使うこと。
    pub fn verify_with<C: CoseCrypto>(
        &self,
        crypto: &C,
        key: &CoseKey,
        options: &VerifyOptions<'_>,
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
        if let Some(expected) = options.expected_type {
            let actual = match &self.header.typ {
                Some(Value::TextString(text)) => Some(text.as_str()),
                _ => None,
            };
            if actual != Some(expected) {
                return Err(CatError::TypeMismatch);
            }
        }
        crypto.verify(algorithm, key, &self.signing_input, &self.signature)?;
        Ok(())
    }

    /// bearer トークンを検証し、catreplay の接続束縛を確認する
    ///
    /// draft-ietf-moq-c4m-02 §3 / §7.2 の bearer トークン用の入口である。次の順に
    /// 検証し、すべて成功したときだけ `cache` の束縛状態を変更する。途中で失敗した
    /// 場合は `cache` を変更しないため、失敗したトークンが束縛状態を汚染することは
    /// ない。
    ///
    /// 1. `cnf` 付きトークンを拒否する。§4.1 のとおり `cnf` 付きトークンは MOQT の
    ///    アクションに DPoP proof を伴わなければ受理できないため、bearer 用のこの
    ///    入口では扱わない
    /// 2. 署名 / MAC を [`CatToken::verify_with`] で検証する
    /// 3. [`CatClaims::validate`] で `exp` / `nbf` / `iss` / `aud` を検証する。
    ///    `clock_tolerance_seconds` には `cache` の leeway を使う
    /// 4. `moqt` クレームが要求されたアクションを認可するか
    ///    [`CatClaims::authorize`] で検証する
    /// 5. `catreplay` が 1 ([`CatReplay::SingleConnection`]) なら `cti` と `exp` を
    ///    必須とし、`exp` + leeway を保持期限として [`BearerReplayCache::check_and_bind`]
    ///    で接続照合・登録する
    ///
    /// `catreplay` が無い / 0 ([`CatReplay::NotProtected`]) の場合はリプレイ保護を
    /// 行わず、`cache` に記録しない。2 ([`CatReplay::ImplementationDefined`]) は
    /// 実装定義のため本実装では未対応として拒否する (0 と同じ扱いにはしない)。
    ///
    /// `catreplay` 1 の期限検証は `cache` の leeway を使うため、通常の `exp` 検証と
    /// 保持期限は同じ値で判定される。参照時刻が `exp` + leeway と等しい間は受理し、
    /// 超えた場合は期限切れとして拒否する。
    ///
    /// [`CatClaims::validate`] はクレームだけを見る stateless な検証であり、単独では
    /// 接続束縛を保証しない。bearer トークンの検証にはこの入口を使うこと。
    pub fn verify_bearer<C: CoseCrypto>(
        &self,
        crypto: &C,
        key: &CoseKey,
        options: &BearerVerificationOptions<'_>,
        cache: &mut BearerReplayCache,
    ) -> Result<(), CatError> {
        // cnf 付きトークンは DPoP proof を伴う検証 (別の入口) の対象である
        if self.claims.confirmation.is_some() {
            return Err(CatError::BearerConfirmationNotSupported);
        }
        self.verify_with(crypto, key, &options.verify_options)?;
        // 期限検証の leeway は保持期限と同じ値を使う
        self.claims.validate(&ClaimValidationOptions {
            reference_time_seconds: options.reference_time_seconds,
            clock_tolerance_seconds: cache.leeway_seconds(),
            expected_issuers: options.expected_issuers,
            expected_audiences: options.expected_audiences,
        })?;
        if !self
            .claims
            .authorize(options.action, options.namespace, options.track_name)
        {
            return Err(CatError::ActionNotAuthorized(options.action));
        }
        match self.claims.cat_replay {
            // replay 保護なしのトークンはキャッシュに記録しない
            None | Some(CatReplay::NotProtected) => Ok(()),
            Some(CatReplay::SingleConnection) => {
                let cti = self
                    .claims
                    .cwt_id
                    .as_deref()
                    .ok_or(CatError::CatReplayRequiresClaim("cti"))?;
                let expiration = self
                    .claims
                    .expiration
                    .ok_or(CatError::CatReplayRequiresClaim("exp"))?;
                let retain_until = expiration + cache.leeway_seconds();
                // exp が有限でも leeway との加算が無限大になる場合がある
                if !retain_until.is_finite() {
                    return Err(CatError::NonFiniteNumber("catreplay retention"));
                }
                // ここまで全て成功したときだけ束縛状態を変更する
                cache.check_and_bind(
                    cti,
                    options.connection_id,
                    retain_until,
                    options.reference_time_seconds,
                )?;
                Ok(())
            }
            Some(CatReplay::ImplementationDefined) => Err(CatError::UnsupportedCatReplay(
                CatReplay::ImplementationDefined,
            )),
        }
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

    /// `catreplay` を設定する
    ///
    /// 値 1 ([`CatReplay::SingleConnection`]) のトークンを発行する場合は、検証時に
    /// `cti` と `exp` が必須になるため [`CatTokenBuilder::cwt_id`] と
    /// [`CatTokenBuilder::expiration`] も設定すること。
    pub fn cat_replay(mut self, cat_replay: CatReplay) -> Self {
        self.claims.cat_replay = Some(cat_replay);
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
    /// トークンの `typ` が期待する値と一致しない
    TypeMismatch,
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
    /// `catreplay` が 0 / 1 / 2 以外の符号なし整数である
    InvalidCatReplayValue(u64),
    /// `catreplay` が 1 なのに必須のクレームが無い
    CatReplayRequiresClaim(&'static str),
    /// `catreplay` が実装定義 (2) のため本実装では未対応である
    UnsupportedCatReplay(CatReplay),
    /// bearer 検証で `cnf` 付きトークンを受けた
    BearerConfirmationNotSupported,
    /// 要求されたアクションが `moqt` クレームに認可されていない
    ActionNotAuthorized(MoqtAction),
    /// クレームの検証エラー
    Claims(ClaimValidationError),
    /// bearer トークンの接続束縛のエラー
    BearerReplay(BearerReplayError),
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
            Self::TypeMismatch => write!(f, "token typ does not match the expected type"),
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
            Self::InvalidCatReplayValue(value) => write!(f, "invalid catreplay value: {value}"),
            Self::CatReplayRequiresClaim(name) => {
                write!(f, "catreplay 1 requires the {name} claim")
            }
            Self::UnsupportedCatReplay(replay) => {
                write!(f, "catreplay value {} is not supported", replay.key())
            }
            Self::BearerConfirmationNotSupported => {
                write!(f, "bearer token must not have a cnf claim")
            }
            Self::ActionNotAuthorized(action) => {
                write!(f, "token does not authorize the {} action", action.name())
            }
            Self::Claims(error) => write!(f, "claim validation error: {error}"),
            Self::BearerReplay(error) => write!(f, "bearer replay error: {error}"),
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

impl From<ClaimValidationError> for CatError {
    fn from(error: ClaimValidationError) -> Self {
        Self::Claims(error)
    }
}

impl From<BearerReplayError> for CatError {
    fn from(error: BearerReplayError) -> Self {
        Self::BearerReplay(error)
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

#[cfg(test)]
mod tests {
    use super::{CLAIM_CAT_REPLAY, is_typed_claim_key};
    use crate::c4m::{CLAIM_MOQT, CLAIM_MOQT_REVAL};

    #[test]
    fn typed_claim_keys_include_c4m_claim_labels() {
        // draft-ietf-moq-c4m-02 §A.5 の Private Use の暫定 label を採用する
        assert_eq!(CLAIM_MOQT, -65537);
        assert_eq!(CLAIM_MOQT_REVAL, -65538);
        // 暫定 label は型付きで解釈する
        assert!(is_typed_claim_key(CLAIM_MOQT));
        assert!(is_typed_claim_key(CLAIM_MOQT_REVAL));
        // catreplay は型付きで解釈するため raw との重複判定にも含める
        assert!(is_typed_claim_key(CLAIM_CAT_REPLAY));
        // 旧 draft-01 のベクタの 327 / 328 は暫定 label の alias にしない
        assert!(!is_typed_claim_key(327));
        assert!(!is_typed_claim_key(328));
        // 負の label でも暫定 label 以外は型付きにしない (-65536 は Specification Required、
        // -65536 未満が Private Use。RFC 8392 §9.1.1)
        assert!(!is_typed_claim_key(-65536));
        assert!(!is_typed_claim_key(-65539));
    }
}
