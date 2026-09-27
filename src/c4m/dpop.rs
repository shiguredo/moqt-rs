//! DPoP proof の検証と発行 (draft-nandakumar-moq-generic-dpop-proof)
//!
//! C4M (draft-ietf-moq-c4m-01 §3.1.2) は [DPOP-PROOF] の JWT 形式の DPoP proof を
//! 使う。proof の署名は埋め込まれた JWK で検証し、`cnf` の JWK サムプリントとの
//! 一致 (鍵バインディング)、`catdpop` のウィンドウによる鮮度、jti によるリプレイ
//! 保護、Authorization Context (actx) の検証を行う。
//!
//! CWT 形式の DPoP proof (`dpop-proof+cwt`) は actx の claim label が TBD のため
//! 扱わない (draft-ietf-moq-c4m-01 §3.1.2 も JWT 形式を参照している)。

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use nojson::{RawJson, RawJsonValue};

use super::MoqtAction;
use super::base64url;
use super::cat::{CatClaims, Confirmation};
use super::cose::Algorithm;
use super::crypto::{CoseCrypto, CoseKey, CryptoError, DigestAlgorithm, default_signing_algorithm};
use super::jwk::{Jwk, JwkError};
use super::jwt::{JwsCompact, JwtError};
use crate::message::common::TrackNamespace;
use crate::name;

/// DPoP proof の JWT の `typ` (draft-nandakumar-moq-generic-dpop-proof §4.3.1)
pub const DPOP_PROOF_JWT_TYPE: &str = "dpop-proof+jwt";

/// MOQT の Authorization Context の `type` (draft-nandakumar-moq-generic-dpop-proof §5.1)
pub const MOQT_AUTHORIZATION_CONTEXT_TYPE: &str = "moqt";

/// DPoP proof の JWT ヘッダ (§4.3.1)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DpopProofHeader {
    /// `typ`。"dpop-proof+jwt" でなければならない
    pub typ: String,
    /// `alg`。非対称署名アルゴリズムでなければならない
    pub algorithm: Algorithm,
    /// `jwk`。proof の検証に使う公開鍵
    pub jwk: Jwk,
    /// `kid` (任意)
    pub key_id: Option<String>,
}

/// DPoP proof の Authorization Context (§4.2 / §5.1)
///
/// `tns` / `tn` は draft-ietf-moq-transport-21 §8.8 の正規シリアライズ (`crate::name`) を使う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationContext {
    /// `type`。MOQT では "moqt"
    pub context_type: String,
    /// `action`。C4M Table 2 のアクション識別子
    pub action: String,
    /// `tns` (namespace のシリアライズ文字列)
    pub track_namespace: String,
    /// `tn` (track name のシリアライズ文字列)
    pub track_name: Option<String>,
    /// `resource` (任意)
    pub resource: Option<String>,
    /// actx の生 JSON (拡張フィールドを保持する)
    pub raw: String,
}

impl AuthorizationContext {
    /// Authorization Context の JSON をデコードする
    pub fn decode(text: &str) -> Result<Self, DpopError> {
        let json = RawJson::parse(text).map_err(json_error)?;
        let value = json.value();
        Ok(Self {
            context_type: required_string(value, "type")?,
            action: required_string(value, "action")?,
            track_namespace: required_string(value, "tns")?,
            track_name: optional_string(value, "tn")?,
            resource: optional_string(value, "resource")?,
            raw: String::from(text),
        })
    }

    /// `type` が期待どおりかどうかを検証する
    pub fn verify_context_type(&self, expected: &str) -> Result<(), DpopError> {
        if self.context_type != expected {
            return Err(DpopError::ContextTypeMismatch);
        }
        Ok(())
    }

    /// `action` がアクションと一致するかどうかを検証する (C4M Table 2)
    pub fn verify_action(&self, action: MoqtAction) -> Result<(), DpopError> {
        if self.action != action.authorization_context() {
            return Err(DpopError::ActionMismatch);
        }
        Ok(())
    }

    /// `tns` / `tn` が対象の Full Track Name と一致するかどうかを検証する
    ///
    /// `tns` / `tn` は draft-nandakumar-moq-generic-dpop-proof §5.1.3 の正規
    /// シリアライズと比較する。`tn` が proof に無い場合はエラーを返す
    /// (draft-ietf-moq-c4m-01 §3.1.2 は `tn` を必須としている)。
    pub fn verify_target(
        &self,
        namespace: &TrackNamespace,
        track_name: &[u8],
    ) -> Result<(), DpopError> {
        if self.track_namespace != name::serialize_namespace(namespace) {
            return Err(DpopError::TargetMismatch);
        }
        let expected = self
            .track_name
            .as_deref()
            .ok_or(DpopError::MissingTrackName)?;
        if expected != name::serialize_track_name(track_name) {
            return Err(DpopError::TargetMismatch);
        }
        Ok(())
    }

    /// `resource` が指定されている場合に `tns` / `tn` と整合するかどうかを検証する
    ///
    /// `resource` は `moqt://<relay-endpoint>?tns=<namespace>&tn=<track>` の形式
    /// (draft-ietf-moq-c4m-01 §3.1.3) を前提とし、クエリパラメータを文字列として
    /// 比較する。パーセントエンコーディングは解釈しない。
    pub fn verify_resource_consistency(&self) -> Result<(), DpopError> {
        let Some(resource) = &self.resource else {
            return Ok(());
        };
        let query = resource
            .split_once('?')
            .map(|(_, query)| query)
            .unwrap_or("");
        let mut tns = None;
        let mut tn = None;
        for pair in query.split('&') {
            match pair.split_once('=') {
                Some(("tns", value)) => tns = Some(value),
                Some(("tn", value)) => tn = Some(value),
                _ => {}
            }
        }
        if tns != Some(self.track_namespace.as_str()) {
            return Err(DpopError::ResourceInconsistent);
        }
        if let Some(track_name) = &self.track_name
            && tn != Some(track_name.as_str())
        {
            return Err(DpopError::ResourceInconsistent);
        }
        Ok(())
    }
}

/// DPoP proof の JWT ペイロード (§4.3.2)
#[derive(Debug, Clone, PartialEq)]
pub struct DpopProofClaims {
    /// `jti`。proof の一意な識別子
    pub jti: String,
    /// `iat` (UNIX 秒)
    pub issued_at: f64,
    /// `actx`
    pub authorization_context: AuthorizationContext,
    /// `ath`。アクセストークンを伴う場合の base64url(SHA-256(token))
    pub access_token_hash: Option<String>,
    /// `nonce`
    pub nonce: Option<String>,
}

impl DpopProofClaims {
    /// ペイロードの JSON をデコードする
    pub fn decode(text: &str) -> Result<Self, DpopError> {
        let json = RawJson::parse(text).map_err(json_error)?;
        let value = json.value();
        let authorization_context = AuthorizationContext::decode(
            value
                .to_member("actx")
                .map_err(json_error)?
                .required()
                .map_err(json_error)?
                .extract()
                .text(),
        )?;
        Ok(Self {
            jti: required_string(value, "jti")?,
            issued_at: required_number(value, "iat")?,
            authorization_context,
            access_token_hash: optional_string(value, "ath")?,
            nonce: optional_string(value, "nonce")?,
        })
    }
}

/// DPoP proof
#[derive(Debug, Clone, PartialEq)]
pub struct DpopProof {
    jws: JwsCompact,
    header: DpopProofHeader,
    claims: DpopProofClaims,
}

/// DPoP proof を CAT トークンに束縛して検証するための入力
#[derive(Debug, Clone, Copy)]
pub struct DpopVerification<'a> {
    /// 検証対象の CAT のクレーム
    pub token_claims: &'a CatClaims,
    /// 要求されたアクション
    pub action: MoqtAction,
    /// 対象の Track Namespace
    pub namespace: &'a TrackNamespace,
    /// 対象の Track Name
    pub track_name: &'a [u8],
    /// 検証に使う現在時刻 (UNIX 秒)
    pub reference_time_seconds: f64,
    /// `catdpop` が無い / ウィンドウ未指定の場合に使う既定のウィンドウ (秒)
    pub default_window_seconds: f64,
}

impl DpopProof {
    /// DPoP proof の JWT をデコードする
    pub fn decode(input: &str) -> Result<Self, DpopError> {
        let jws = JwsCompact::decode(input)?;
        let typ = jws
            .header()
            .typ
            .clone()
            .ok_or(DpopError::MissingHeader("typ"))?;
        if typ != DPOP_PROOF_JWT_TYPE {
            return Err(DpopError::InvalidType(typ));
        }
        if jws.header().algorithm.is_mac() {
            return Err(DpopError::UnsupportedAlgorithm);
        }
        let jwk = jws
            .header()
            .jwk
            .clone()
            .ok_or(DpopError::MissingHeader("jwk"))?;
        let header = DpopProofHeader {
            typ,
            algorithm: jws.header().algorithm,
            jwk,
            key_id: jws.header().key_id.clone(),
        };
        let payload_text = core::str::from_utf8(jws.payload())
            .map_err(|_| DpopError::UnexpectedType("payload"))?;
        let claims = DpopProofClaims::decode(payload_text)?;
        Ok(Self {
            jws,
            header,
            claims,
        })
    }

    /// ヘッダを返す
    pub fn header(&self) -> &DpopProofHeader {
        &self.header
    }

    /// クレームを返す
    pub fn claims(&self) -> &DpopProofClaims {
        &self.claims
    }

    /// 署名対象のバイト列を返す
    pub fn signing_input(&self) -> &[u8] {
        self.jws.signing_input()
    }

    /// 署名を返す
    pub fn signature(&self) -> &[u8] {
        self.jws.signature()
    }

    /// proof の署名を埋め込み JWK で検証する
    pub fn verify_signature<C: CoseCrypto>(&self, crypto: &C) -> Result<(), DpopError> {
        let key = self.header.jwk.to_cose_key()?;
        self.jws.verify(crypto, &key)?;
        Ok(())
    }

    /// proof の JWK がトークンの `cnf` の JWK サムプリントと一致するか検証する
    ///
    /// `cnf` の confirmation key 323 (IANA 登録の `jkt`) と 3 (ドラフトのベクタ) の
    /// どちらでも一致すればよい。
    pub fn verify_key_binding<C: CoseCrypto>(
        &self,
        crypto: &C,
        confirmation: &Confirmation,
    ) -> Result<(), DpopError> {
        let thumbprint = self.header.jwk.thumbprint_sha256(crypto)?;
        let matches = confirmation
            .jwk_thumbprint
            .as_deref()
            .is_some_and(|expected| expected == thumbprint.as_slice())
            || confirmation
                .c4m_draft_jwk_thumbprint
                .as_deref()
                .is_some_and(|expected| expected == thumbprint.as_slice());
        if !matches {
            if confirmation.jkt().is_none() {
                return Err(DpopError::MissingThumbprint);
            }
            return Err(DpopError::KeyBindingMismatch);
        }
        Ok(())
    }

    /// `iat` が現在時刻からウィンドウ内にあるかどうかを検証する
    ///
    /// ウィンドウは `catdpop` の値を呼び出し側が渡す。未来方向のずれも同じウィンドウ
    /// で制限する。
    pub fn verify_freshness(
        &self,
        reference_time_seconds: f64,
        window_seconds: f64,
    ) -> Result<(), DpopError> {
        let delta = reference_time_seconds - self.claims.issued_at;
        if delta > window_seconds {
            return Err(DpopError::ProofExpired);
        }
        if -delta > window_seconds {
            return Err(DpopError::ProofNotYetValid);
        }
        Ok(())
    }

    /// `ath` をアクセストークンと照合する
    ///
    /// `ath` が proof に無い場合は何も行わない。
    pub fn verify_access_token_hash<C: CoseCrypto>(
        &self,
        crypto: &C,
        access_token: &str,
    ) -> Result<(), DpopError> {
        let Some(expected) = &self.claims.access_token_hash else {
            return Ok(());
        };
        let digest = crypto
            .digest(DigestAlgorithm::Sha256, access_token.as_bytes())
            .map_err(DpopError::Crypto)?;
        if expected != &base64url::encode(&digest) {
            return Err(DpopError::AccessTokenHashMismatch);
        }
        Ok(())
    }

    /// Authorization Context (actx) を検証する
    ///
    /// `type` が "moqt"、`action` が一致し、`tns` / `tn` が対象と一致し、`resource`
    /// が与えられている場合は `tns` / `tn` と整合することを確認する。
    pub fn verify_authorization_context(
        &self,
        action: MoqtAction,
        namespace: &TrackNamespace,
        track_name: &[u8],
    ) -> Result<(), DpopError> {
        let context = &self.claims.authorization_context;
        context.verify_context_type(MOQT_AUTHORIZATION_CONTEXT_TYPE)?;
        context.verify_action(action)?;
        context.verify_resource_consistency()?;
        context.verify_target(namespace, track_name)
    }

    /// CAT トークンに束縛された DPoP proof を一通り検証する
    ///
    /// 署名、JWK サムプリントのバインディング、Authorization Context、`catdpop` の
    /// ウィンドウによる鮮度、jti によるリプレイ保護の順に検証する。
    ///
    /// `catdpop` が jti の処理を要求している場合 (`honor_jti` が真) は
    /// `replay_cache` が必須である。
    pub fn verify_against_token<C: CoseCrypto>(
        &self,
        crypto: &C,
        request: &DpopVerification<'_>,
        replay_cache: Option<&mut DpopReplayCache>,
    ) -> Result<(), DpopError> {
        self.verify_signature(crypto)?;
        let confirmation = request
            .token_claims
            .confirmation
            .as_ref()
            .ok_or(DpopError::MissingConfirmation)?;
        self.verify_key_binding(crypto, confirmation)?;
        self.verify_authorization_context(request.action, request.namespace, request.track_name)?;
        let window_seconds = request
            .token_claims
            .catdpop
            .as_ref()
            .map_or(request.default_window_seconds, |catdpop| {
                catdpop.window_seconds_or(request.default_window_seconds)
            });
        self.verify_freshness(request.reference_time_seconds, window_seconds)?;
        if request
            .token_claims
            .catdpop
            .as_ref()
            .is_some_and(super::CatDpop::honors_jti)
        {
            let replay_cache = replay_cache.ok_or(DpopError::ReplayCacheRequired)?;
            replay_cache.check_and_record(
                &self.claims.jti,
                self.claims.issued_at,
                window_seconds,
                request.reference_time_seconds,
            )?;
        }
        Ok(())
    }
}

/// jti によるリプレイ保護のキャッシュ
///
/// アプリケーションが 1 つの主体 (セッションやアクセストークン) ごとに保持する。
#[derive(Debug, Clone, Default)]
pub struct DpopReplayCache {
    entries: Vec<(String, f64)>,
}

impl DpopReplayCache {
    /// 空のキャッシュを作る
    pub fn new() -> Self {
        Self::default()
    }

    /// jti を確認して記録する
    ///
    /// ウィンドウ外の古い記録は破棄する。同じ jti がウィンドウ内に存在する場合は
    /// [`DpopError::Replayed`] を返す。
    pub fn check_and_record(
        &mut self,
        jti: &str,
        issued_at: f64,
        window_seconds: f64,
        reference_time_seconds: f64,
    ) -> Result<(), DpopError> {
        self.entries
            .retain(|(_, recorded_at)| reference_time_seconds - recorded_at <= window_seconds);
        if self.entries.iter().any(|(recorded, _)| recorded == jti) {
            return Err(DpopError::Replayed);
        }
        self.entries.push((String::from(jti), issued_at));
        Ok(())
    }

    /// 記録数を返す
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 記録が空かどうかを返す
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// DPoP proof を発行するビルダー
///
/// クライアント側で proof を生成するために使う。署名鍵と埋め込む JWK が同じ公開鍵を
/// 表していることを確認してから署名する。
#[derive(Debug, Clone, PartialEq)]
pub struct DpopProofBuilder {
    /// `kid` (任意)
    pub key_id: Option<String>,
    /// `jti`
    pub jti: String,
    /// `iat` (UNIX 秒)
    pub issued_at: f64,
    /// `actx`
    pub authorization_context: AuthorizationContext,
    /// `ath` (任意)
    pub access_token_hash: Option<String>,
    /// `nonce` (任意)
    pub nonce: Option<String>,
}

impl DpopProofBuilder {
    /// 必須の値を指定してビルダーを作る
    pub fn new(
        jti: impl Into<String>,
        issued_at: f64,
        authorization_context: AuthorizationContext,
    ) -> Self {
        Self {
            key_id: None,
            jti: jti.into(),
            issued_at,
            authorization_context,
            access_token_hash: None,
            nonce: None,
        }
    }

    /// `kid` を設定する
    pub fn key_id(mut self, key_id: impl Into<String>) -> Self {
        self.key_id = Some(key_id.into());
        self
    }

    /// `ath` を設定する
    pub fn access_token_hash(mut self, access_token_hash: impl Into<String>) -> Self {
        self.access_token_hash = Some(access_token_hash.into());
        self
    }

    /// `nonce` を設定する
    pub fn nonce(mut self, nonce: impl Into<String>) -> Self {
        self.nonce = Some(nonce.into());
        self
    }

    /// proof の JWT を発行する
    ///
    /// `jwk` は埋め込む公開鍵で、`key` と同じ公開鍵でなければならない。
    pub fn build<C: CoseCrypto>(
        &self,
        crypto: &C,
        key: &CoseKey,
        jwk: &Jwk,
    ) -> Result<String, DpopError> {
        let algorithm = default_signing_algorithm(key);
        if algorithm.is_mac() {
            return Err(DpopError::UnsupportedAlgorithm);
        }
        if !jwk.matches_public_key(key)? {
            return Err(DpopError::KeyMismatch);
        }
        let header = format!(
            "{}",
            nojson::object(|f| {
                f.member("typ", DPOP_PROOF_JWT_TYPE)?;
                f.member("alg", algorithm.jose_name())?;
                f.member("jwk", jwk)?;
                match &self.key_id {
                    Some(key_id) => f.member("kid", key_id.as_str()),
                    None => Ok(()),
                }
            })
        );
        let context = &self.authorization_context;
        let payload = format!(
            "{}",
            nojson::object(|f| {
                f.member("jti", self.jti.as_str())?;
                f.member("iat", self.issued_at)?;
                f.member(
                    "actx",
                    nojson::object(|f| {
                        f.member("type", context.context_type.as_str())?;
                        f.member("action", context.action.as_str())?;
                        f.member("tns", context.track_namespace.as_str())?;
                        match &context.track_name {
                            Some(track_name) => f.member("tn", track_name.as_str()),
                            None => Ok(()),
                        }?;
                        match &context.resource {
                            Some(resource) => f.member("resource", resource.as_str()),
                            None => Ok(()),
                        }
                    }),
                )?;
                match &self.access_token_hash {
                    Some(access_token_hash) => f.member("ath", access_token_hash.as_str()),
                    None => Ok(()),
                }?;
                match &self.nonce {
                    Some(nonce) => f.member("nonce", nonce.as_str()),
                    None => Ok(()),
                }
            })
        );
        let header_segment = base64url::encode(header.as_bytes());
        let payload_segment = base64url::encode(payload.as_bytes());
        let signing_input = format!("{header_segment}.{payload_segment}");
        let signature = crypto.sign(algorithm, key, signing_input.as_bytes())?;
        Ok(format!("{signing_input}.{}", base64url::encode(&signature)))
    }
}

/// DPoP proof のエラー
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DpopError {
    /// JWT のエラー
    Jwt(JwtError),
    /// JWK のエラー
    Jwk(JwkError),
    /// 署名 / 検証のエラー
    Crypto(CryptoError),
    /// JSON のパースに失敗した
    Json(String),
    /// ヘッダに必須のメンバーが無い
    MissingHeader(&'static str),
    /// クレームに必須のメンバーが無い
    MissingClaim(&'static str),
    /// メンバーの型が期待と異なる
    UnexpectedType(&'static str),
    /// `typ` が "dpop-proof+jwt" ではない
    InvalidType(String),
    /// 対称鍵アルゴリズムは使えない
    UnsupportedAlgorithm,
    /// トークンに `cnf` が無い
    MissingConfirmation,
    /// `cnf` に JWK サムプリントが無い
    MissingThumbprint,
    /// JWK サムプリントが一致しない
    KeyBindingMismatch,
    /// 署名鍵と埋め込む JWK が一致しない
    KeyMismatch,
    /// `actx.type` が "moqt" ではない
    ContextTypeMismatch,
    /// `actx.action` が一致しない
    ActionMismatch,
    /// `actx.tns` / `actx.tn` が対象と一致しない
    TargetMismatch,
    /// `actx.tn` が無い
    MissingTrackName,
    /// `actx.resource` が `tns` / `tn` と整合しない
    ResourceInconsistent,
    /// `ath` がアクセストークンと一致しない
    AccessTokenHashMismatch,
    /// `iat` が古すぎる
    ProofExpired,
    /// `iat` が未来すぎる
    ProofNotYetValid,
    /// 同じ jti を再び受信した
    Replayed,
    /// jti の検証が必要だがリプレイキャッシュが渡されていない
    ReplayCacheRequired,
}

impl fmt::Display for DpopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Jwt(error) => write!(f, "JWT error: {error}"),
            Self::Jwk(error) => write!(f, "JWK error: {error}"),
            Self::Crypto(error) => write!(f, "crypto error: {error}"),
            Self::Json(message) => write!(f, "invalid DPoP JSON: {message}"),
            Self::MissingHeader(name) => write!(f, "DPoP header member is missing: {name}"),
            Self::MissingClaim(name) => write!(f, "DPoP claim is missing: {name}"),
            Self::UnexpectedType(name) => write!(f, "unexpected DPoP member type: {name}"),
            Self::InvalidType(typ) => write!(f, "invalid DPoP typ: {typ}"),
            Self::UnsupportedAlgorithm => {
                write!(f, "DPoP proof must use an asymmetric algorithm")
            }
            Self::MissingConfirmation => write!(f, "token has no cnf claim"),
            Self::MissingThumbprint => write!(f, "cnf has no JWK thumbprint"),
            Self::KeyBindingMismatch => write!(f, "JWK thumbprint does not match cnf"),
            Self::KeyMismatch => write!(f, "signing key does not match the embedded JWK"),
            Self::ContextTypeMismatch => write!(f, "actx.type does not match"),
            Self::ActionMismatch => write!(f, "actx.action does not match"),
            Self::TargetMismatch => write!(f, "actx.tns or actx.tn does not match the target"),
            Self::MissingTrackName => write!(f, "actx.tn is missing"),
            Self::ResourceInconsistent => {
                write!(f, "actx.resource is inconsistent with tns or tn")
            }
            Self::AccessTokenHashMismatch => write!(f, "ath does not match the access token"),
            Self::ProofExpired => write!(f, "DPoP proof is too old"),
            Self::ProofNotYetValid => write!(f, "DPoP proof is issued in the future"),
            Self::Replayed => write!(f, "DPoP proof jti was already used"),
            Self::ReplayCacheRequired => {
                write!(f, "catdpop requires jti replay protection")
            }
        }
    }
}

impl core::error::Error for DpopError {}

impl From<JwtError> for DpopError {
    fn from(error: JwtError) -> Self {
        Self::Jwt(error)
    }
}

impl From<JwkError> for DpopError {
    fn from(error: JwkError) -> Self {
        Self::Jwk(error)
    }
}

impl From<CryptoError> for DpopError {
    fn from(error: CryptoError) -> Self {
        Self::Crypto(error)
    }
}

fn json_error(error: nojson::JsonParseError) -> DpopError {
    DpopError::Json(error.to_string())
}

/// 必須の文字列メンバーを取り出す
fn required_string(value: RawJsonValue<'_, '_>, name: &'static str) -> Result<String, DpopError> {
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
) -> Result<Option<String>, DpopError> {
    let Some(member) = value.to_member(name).map_err(json_error)?.optional() else {
        return Ok(None);
    };
    let text: String = member.try_into().map_err(json_error)?;
    Ok(Some(text))
}

/// 必須の数値メンバーを取り出す
fn required_number(value: RawJsonValue<'_, '_>, name: &'static str) -> Result<f64, DpopError> {
    let member = value
        .to_member(name)
        .map_err(json_error)?
        .required()
        .map_err(json_error)?;
    member
        .as_number_str()
        .map_err(json_error)?
        .parse::<f64>()
        .map_err(|_| DpopError::UnexpectedType(name))
}
