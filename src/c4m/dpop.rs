//! DPoP proof の検証と発行
//!
//! draft-ietf-moq-c4m-02 §4.2 の CWT 形式の DPoP proof (`dpop-proof+cwt`) が標準で
//! ある。proof は COSE_Sign1 で、protected ヘッダに `alg` / `typ` / `kccs` を持ち、
//! ペイロードの CWT Claims Set に `cti` / `iat` / `actx` / `ath` を持つ。この形式は
//! [`CwtDpopProofCodec`] が扱う。`actx` と `ath` の claim label はドラフトで
//! `TBD_ACTX` / `TBD_ATH` のままなので、呼び出し側が明示する。
//! [`CwtDpopProof::verify_signature`] は proof 単独の署名を、
//! [`CwtDpopProof::verify_key_binding`] は `kccs` 公開鍵と CAT トークンの `cnf` の
//! 照合を行う。受理判定は [`CwtDpopProof::verify_against_cat_token`] が
//! [`CwtDpopVerification`] と [`CwtDpopReplayCache`] を受け取って、CAT の検証を含めて
//! 一括して行う。
//!
//! 旧 draft-ietf-moq-c4m-01 と draft-nandakumar-moq-generic-dpop-proof-00 の JWT 形式の
//! proof ([`DpopProof`]) は互換 API として残している。proof の署名は埋め込まれた JWK で
//! 検証し、`cnf` の JWK サムプリントとの一致 (鍵バインディング)、`catdpop` のウィンドウに
//! よる鮮度、jti によるリプレイ保護、Authorization Context (actx) の検証を行う。
//! ただし、この API は CAT トークンの署名・クレーム・scope を検証しない。

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use nojson::{RawJson, RawJsonValue};

use super::MoqtAction;
use super::base64url;
use super::cat::{
    CLAIM_CONFIRMATION, CLAIM_CWT_ID, CLAIM_ISSUED_AT, CONFIRMATION_COSE_KEY, CatClaims, CatError,
    CatReplay, CatToken, ClaimValidationOptions, Confirmation, VerifyOptions,
};
use super::cbor::{self, CborError, Value};
use super::cose::{Algorithm, CoseEncodingOptions, CoseError, CoseMessage, CoseSign1, Header};
use super::crypto::{CoseCrypto, CoseKey, CryptoError, DigestAlgorithm, default_signing_algorithm};
use super::jwk::{Jwk, JwkError};
use super::jwt::{JwsCompact, JwtError};
use crate::message::common::TrackNamespace;
use crate::name;

/// DPoP proof の JWT の `typ` (draft-nandakumar-moq-generic-dpop-proof-00 §4.3.1)
pub const DPOP_PROOF_JWT_TYPE: &str = "dpop-proof+jwt";

/// CWT 形式の DPoP proof の `typ` (draft-ietf-moq-c4m-02 §4.2)
pub const DPOP_PROOF_CWT_TYPE: &str = "dpop-proof+cwt";

/// MOQT の Authorization Context の `type` (draft-nandakumar-moq-generic-dpop-proof-00 §5.1)
pub const MOQT_AUTHORIZATION_CONTEXT_TYPE: &str = "moqt";

/// CWT 形式の actx の `type` のメンバーキー (draft-ietf-moq-c4m-02 §4.2)
const ACTX_TYPE: i64 = 0;

/// CWT 形式の actx の `action` のメンバーキー (draft-ietf-moq-c4m-02 §4.2)
const ACTX_ACTION: i64 = 1;

/// CWT 形式の actx の `namespace` のメンバーキー (draft-ietf-moq-c4m-02 §4.2)
const ACTX_NAMESPACE: i64 = 2;

/// CWT 形式の actx の `track` のメンバーキー (draft-ietf-moq-c4m-02 §4.2)
const ACTX_TRACK_NAME: i64 = 3;

/// 旧 JWT 形式の DPoP proof のヘッダ
/// (draft-nandakumar-moq-generic-dpop-proof-00 §4.3.1)
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

/// 旧 JWT 形式の DPoP proof の Authorization Context
/// (draft-nandakumar-moq-generic-dpop-proof-00 §4.2 / §5.1)
///
/// `tns` / `tn` は draft-ietf-moq-transport-23 §8.8 の正規シリアライズ (`crate::name`) を使う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationContext {
    /// `type`。MOQT では "moqt"
    pub context_type: String,
    /// `action`。旧 JWT 形式の DPoP proof のアクション識別子 (文字列)
    ///
    /// draft-02 §4.2 の CWT 形式の actx は整数の action コードを使うため、この文字列は
    /// 旧形式の proof にだけ現れる。
    pub action: String,
    /// `tns` (namespace のシリアライズ文字列)
    pub track_namespace: String,
    /// `tn` (track name のシリアライズ文字列)
    pub track_name: Option<String>,
    /// `resource` (任意)
    pub resource: Option<String>,
    /// actx の生 JSON
    ///
    /// `parameters` などのプロトコル固有の拡張フィールドは型付きで扱わないため、
    /// 解釈が必要な場合はこの文字列を nojson などで読む。
    pub raw: String,
}

impl AuthorizationContext {
    /// Authorization Context の JSON をデコードする
    ///
    /// メンバー名が重複している場合はエラーを返す。
    pub fn decode(text: &str) -> Result<Self, DpopError> {
        let json = RawJson::parse(text).map_err(json_error)?;
        let value = json.value();
        if let Some(name) = super::json::find_duplicate_member(value).map_err(json_error)? {
            return Err(DpopError::DuplicateMember(name));
        }
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

    /// 旧 JWT 形式の `action` がアクションと一致するかどうかを検証する
    ///
    /// 旧 JWT 形式の DPoP proof の `action` は文字列であり、
    /// [`MoqtAction::authorization_context`] が `Some` を返すアクションだけが一致し得る。
    /// `None` を返すアクション (SUBSCRIBE_TRACKS) は旧形式に対応する識別子が無いため、
    /// 常に [`DpopError::ActionMismatch`] を返して拒否する。
    pub fn verify_action(&self, action: MoqtAction) -> Result<(), DpopError> {
        if !action.matches_authorization_context(&self.action) {
            return Err(DpopError::ActionMismatch);
        }
        Ok(())
    }

    /// `tns` / `tn` が対象の Full Track Name と一致するかどうかを検証する
    ///
    /// `tns` / `tn` は draft-nandakumar-moq-generic-dpop-proof-00 §5.1.3 の正規
    /// シリアライズと比較する。`tn` が proof に無い場合はエラーを返す
    /// (旧 draft-ietf-moq-c4m-01 §3.1.2 は `tn` を必須としている)。
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
    /// (旧 draft-ietf-moq-c4m-01 §3.1.3) を前提とし、クエリパラメータを文字列として
    /// 比較する。パーセントエンコーディングは解釈しない。
    ///
    /// §3.1.3 が挙げる接続確立時の `moqt://<relay-endpoint>` (クエリ無し) は
    /// `tns` を持たないため、この関数は整合しないものとして拒否する (fail-closed)。
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

/// 旧 JWT 形式の DPoP proof のペイロード
/// (draft-nandakumar-moq-generic-dpop-proof-00 §4.3.2)
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
    ///
    /// クレーム名が重複している場合はエラーを返す。
    pub fn decode(text: &str) -> Result<Self, DpopError> {
        let json = RawJson::parse(text).map_err(json_error)?;
        let value = json.value();
        if let Some(name) = super::json::find_duplicate_member(value).map_err(json_error)? {
            return Err(DpopError::DuplicateMember(name));
        }
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

/// 旧 JWT 形式 (互換 API) の DPoP proof
///
/// draft-ietf-moq-c4m-02 の標準は CWT 形式であり、[`CwtDpopProof`] を使う。
#[derive(Debug, Clone, PartialEq)]
pub struct DpopProof {
    jws: JwsCompact,
    header: DpopProofHeader,
    claims: DpopProofClaims,
}

/// 旧 JWT 形式の DPoP proof を CAT トークンに束縛して検証するための入力
///
/// この入力を使う [`DpopProof::verify_against_token`] は proof 側だけを検証し、
/// CAT の署名・クレーム・scope は検証しない。
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
    /// proof と同時に送られたアクセストークン
    ///
    /// 送信された場合は `ath` の検証を必須にする (draft-nandakumar-moq-generic-dpop-proof-00
    /// §4.3.2)。アクセストークンを伴わない proof (token endpoint など) では `None` を
    /// 渡す。
    pub access_token: Option<&'a [u8]>,
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
    /// `cnf` の confirmation key 323 (IANA 登録の `jkt`) を優先し、323 が無い場合だけ
    /// 3 (draft-ietf-moq-c4m-01 のベクタが使う値。IANA では `kid`) と比較する。
    pub fn verify_key_binding<C: CoseCrypto>(
        &self,
        crypto: &C,
        confirmation: &Confirmation,
    ) -> Result<(), DpopError> {
        let thumbprint = self.header.jwk.thumbprint_sha256(crypto)?;
        let expected = confirmation.jkt().ok_or(DpopError::MissingThumbprint)?;
        if expected != thumbprint.as_slice() {
            return Err(DpopError::KeyBindingMismatch);
        }
        Ok(())
    }

    /// `iat` が現在時刻からウィンドウ内にあるかどうかを検証する
    ///
    /// ウィンドウは `catdpop` の値を呼び出し側が渡す。未来方向のずれも同じウィンドウ
    /// で制限する。現在時刻とウィンドウが有限でない場合はエラーにする (NaN は比較が
    /// 常に偽になり検証を素通りさせるため)。
    pub fn verify_freshness(
        &self,
        reference_time_seconds: f64,
        window_seconds: f64,
    ) -> Result<(), DpopError> {
        if !reference_time_seconds.is_finite() {
            return Err(DpopError::NonFiniteNumber("reference time"));
        }
        if !window_seconds.is_finite() {
            return Err(DpopError::NonFiniteNumber("freshness window"));
        }
        if !self.claims.issued_at.is_finite() {
            return Err(DpopError::NonFiniteNumber("iat"));
        }
        let delta = reference_time_seconds - self.claims.issued_at;
        if delta > window_seconds {
            return Err(DpopError::ProofExpired);
        }
        if -delta > window_seconds {
            return Err(DpopError::ProofNotYetValid);
        }
        Ok(())
    }

    /// `ath` をアクセストークン (文字列) と照合する
    ///
    /// `ath` が proof に無い場合は何も行わない。一括検証 API
    /// ([`DpopProof::verify_against_token`]) はアクセストークンを渡すと `ath` の
    /// 存在と一致を必須にする。
    pub fn verify_access_token_hash<C: CoseCrypto>(
        &self,
        crypto: &C,
        access_token: &str,
    ) -> Result<(), DpopError> {
        self.verify_access_token_hash_bytes(crypto, access_token.as_bytes())
    }

    /// `ath` をアクセストークン (バイト列) と照合する
    ///
    /// MOQT の AUTHORIZATION_TOKEN のように文字列ではないトークンに使う。`ath` が
    /// proof に無い場合は何も行わない。
    pub fn verify_access_token_hash_bytes<C: CoseCrypto>(
        &self,
        crypto: &C,
        access_token: &[u8],
    ) -> Result<(), DpopError> {
        let Some(expected) = &self.claims.access_token_hash else {
            return Ok(());
        };
        let digest = crypto
            .digest(DigestAlgorithm::Sha256, access_token)
            .map_err(DpopError::Crypto)?;
        if expected != &base64url::encode(&digest) {
            return Err(DpopError::AccessTokenHashMismatch);
        }
        Ok(())
    }

    /// Authorization Context (actx) を検証する
    ///
    /// `type` が "moqt"、`action` が一致し、`resource` が与えられている場合は
    /// `tns` / `tn` と整合し、`tns` / `tn` が対象と一致することを確認する。
    ///
    /// 検証は type → action → resource → target の順に行う。旧 JWT 形式の actx を
    /// 対象とするため、type が正しい場合、旧形式に対応する文字列を持たない
    /// SUBSCRIBE_TRACKS は [`DpopError::ActionMismatch`] で拒否する。CWT 形式の proof は
    /// [`CwtDpopProof::verify_authorization_context`] を使う。
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

    /// CAT トークンと同時に送られた DPoP proof を一通り検証する
    ///
    /// MOQT の AUTHORIZATION_TOKEN と proof を同時に受けた場合の入口であり、トークンの
    /// 生バイトを `ath` のアクセストークンとして扱う。アクセストークンを伴う proof に
    /// `ath` を必須とする (draft-nandakumar-moq-generic-dpop-proof-00 §4.3.2) ため、
    /// [`DpopProof::verify_against_token`] よりこちらを使うこと。
    ///
    /// 署名、`ath`、JWK サムプリントのバインディング、Authorization Context、
    /// `catdpop` のウィンドウによる鮮度、jti によるリプレイ保護の順に検証する。
    ///
    /// この API は proof 側だけを検証する。CAT トークンの署名・クレーム・scope は
    /// 検証しないため、呼び出し側が別途検証すること。CWT 形式と CAT の検証を
    /// まとめて行う場合は [`CwtDpopProof::verify_against_cat_token`] を使う。
    ///
    /// `token` は `CatToken::decode` に渡した生バイト (`CatToken::raw_token`) を
    /// `ath` に使う。発行側が別の表現 (base64url など) をハッシュしている場合は
    /// [`DpopProof::verify_against_token`] にその表現を渡すこと。
    #[expect(clippy::too_many_arguments)]
    pub fn verify_against_cat_token<C: CoseCrypto>(
        &self,
        crypto: &C,
        token: &crate::c4m::cat::CatToken,
        action: MoqtAction,
        namespace: &TrackNamespace,
        track_name: &[u8],
        reference_time_seconds: f64,
        default_window_seconds: f64,
        replay_cache: Option<&mut DpopReplayCache>,
    ) -> Result<(), DpopError> {
        let request = DpopVerification {
            token_claims: token.claims(),
            action,
            namespace,
            track_name,
            reference_time_seconds,
            default_window_seconds,
            access_token: Some(token.raw_token()),
        };
        self.verify_against_token(crypto, &request, replay_cache)
    }

    /// CAT トークンに束縛された DPoP proof を一通り検証する
    ///
    /// 署名、`ath` (アクセストークンを渡した場合)、JWK サムプリントのバインディング、
    /// Authorization Context、`catdpop` のウィンドウによる鮮度、jti によるリプレイ
    /// 保護の順に検証する。MOQT のようにアクセストークンと proof を同時に受ける場合は
    /// [`DpopProof::verify_against_cat_token`] を使うこと。
    ///
    /// `catdpop` が jti の処理を要求している場合 (`honor_jti` が真) は
    /// `replay_cache` が必須である。
    ///
    /// この API は proof 側だけを検証し、CAT の署名・クレーム・scope は検証しない。
    pub fn verify_against_token<C: CoseCrypto>(
        &self,
        crypto: &C,
        request: &DpopVerification<'_>,
        replay_cache: Option<&mut DpopReplayCache>,
    ) -> Result<(), DpopError> {
        self.verify_signature(crypto)?;
        match (
            request.access_token,
            self.claims.access_token_hash.as_deref(),
        ) {
            (Some(access_token), Some(_)) => {
                self.verify_access_token_hash_bytes(crypto, access_token)?;
            }
            // アクセストークンを伴う proof は `ath` が必須 (§4.3.2)
            (Some(_), None) => return Err(DpopError::MissingAccessTokenHash),
            (None, Some(_)) => return Err(DpopError::MissingAccessToken),
            (None, None) => {}
        }
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
/// エントリ数と jti 長の上限は持たないため、アプリケーションが主体ごとのレート制限や
/// jti の長さ制限で肥大化を防ぐこと (RFC 9449 §11.1 は大きすぎる jti の拒否や
/// ハッシュ化を推奨する)。
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
        if !issued_at.is_finite() {
            return Err(DpopError::NonFiniteNumber("iat"));
        }
        if !window_seconds.is_finite() {
            return Err(DpopError::NonFiniteNumber("freshness window"));
        }
        if !reference_time_seconds.is_finite() {
            return Err(DpopError::NonFiniteNumber("reference time"));
        }
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
    ///
    /// 期限切れの記録は次の [`CwtDpopReplayCache::check_and_record`] の成功経路で解放
    /// されるまで残るため、期限切れの記録だけが残っている場合も `false` を返す。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// 旧 JWT 形式の DPoP proof を発行するビルダー
///
/// クライアント側で proof を生成するために使う。署名鍵と埋め込む JWK が同じ公開鍵を
/// 表していることを確認してから署名する。draft-02 の標準は CWT 形式であり、
/// [`CwtDpopProofCodec`] を使う。
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
    /// `jwk` は埋め込む公開鍵で、`key` と同じ公開鍵でなければならない。`iat` が
    /// 有限でない場合はエラーを返す。
    ///
    /// `authorization_context` の `raw` にしか無い拡張フィールド (`parameters` など)
    /// は出力に含まれない。型付きの 5 つのフィールドだけが出力される。
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
        if !self.issued_at.is_finite() {
            return Err(DpopError::NonFiniteNumber("iat"));
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

/// CWT 形式の DPoP proof の Authorization Context (draft-ietf-moq-c4m-02 §4.2)
///
/// `namespace` と `track_name` は MOQT のメッセージが運ぶバイト列をそのまま保持し、
/// シリアライズも正規化もしない。`action` は draft-ietf-moq-c4m-02 §2.1 Table 1 の
/// 整数コードをそのまま保持するため、未知の整数も表現できる。既知のアクションへの
/// 解決と要求されたアクション・target との一致判定は [`CwtAuthorizationContext::verify_request`]
/// が行う。
///
/// `context_type` が "moqt" であることの検証は、発行時は [`CwtDpopProofCodec::build`]
/// が、受信時は [`CwtAuthorizationContext::verify_request`] が行う。
#[derive(Debug, Clone, PartialEq)]
pub struct CwtAuthorizationContext {
    /// `type`。MOQT では "moqt"
    pub context_type: String,
    /// `action`。整数のアクションコード
    pub action: i64,
    /// `namespace`。Track Namespace のフィールド列
    pub namespace: Option<Vec<Vec<u8>>>,
    /// `track`。Track Name
    pub track_name: Option<Vec<u8>>,
    /// 解釈しなかった actx のメンバー
    pub raw: Vec<(Value, Value)>,
}

impl CwtAuthorizationContext {
    /// `type` / `action` / `namespace` / `track` が要求と一致するか検証する
    ///
    /// draft-ietf-moq-c4m-02 §4.3 の 8 番目の条件に対応する。action ごとの target の
    /// 規則は次のとおりである。
    ///
    /// - SETUP: namespace も track も持たない。要求側と proof 側のどちらかに target が
    ///   あれば拒否する
    /// - PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS: namespace を必須と
    ///   し、track を持たない。track があれば拒否する
    /// - SUBSCRIBE / REQUEST_UPDATE / PUBLISH / FETCH / TRACK_STATUS: namespace と
    ///   track の両方を必須とする
    ///
    /// 欠落 (`None`) と空のバイト列 / 空の namespace 配列は区別する。存在する target は
    /// MOQT メッセージが運ぶフィールド列とバイト列をそのまま比較し、シリアライズも
    /// 正規化もしない (§4.2)。
    pub fn verify_request(
        &self,
        action: MoqtAction,
        namespace: Option<&[&[u8]]>,
        track_name: Option<&[u8]>,
    ) -> Result<(), DpopError> {
        if self.context_type != MOQT_AUTHORIZATION_CONTEXT_TYPE {
            return Err(DpopError::ContextTypeMismatch);
        }
        if self.action != action.key() {
            return Err(DpopError::ActionMismatch);
        }
        let (requires_namespace, requires_track) = action_target_requirements(action);
        if requires_namespace {
            let expected = namespace.ok_or(DpopError::MissingRequestTarget("namespace"))?;
            let actual = self
                .namespace
                .as_deref()
                .ok_or(DpopError::MissingActxTarget("namespace"))?;
            // フィールド数と各フィールドのバイト列をそのまま比較する
            if expected.len() != actual.len()
                || expected
                    .iter()
                    .zip(actual)
                    .any(|(expected, actual)| *expected != actual.as_slice())
            {
                return Err(DpopError::TargetMismatch);
            }
        } else {
            if namespace.is_some() {
                return Err(DpopError::UnexpectedRequestTarget("namespace"));
            }
            if self.namespace.is_some() {
                return Err(DpopError::UnexpectedActxTarget("namespace"));
            }
        }
        if requires_track {
            let expected = track_name.ok_or(DpopError::MissingRequestTarget("track"))?;
            let actual = self
                .track_name
                .as_deref()
                .ok_or(DpopError::MissingActxTarget("track"))?;
            if expected != actual {
                return Err(DpopError::TargetMismatch);
            }
        } else {
            if track_name.is_some() {
                return Err(DpopError::UnexpectedRequestTarget("track"));
            }
            if self.track_name.is_some() {
                return Err(DpopError::UnexpectedActxTarget("track"));
            }
        }
        Ok(())
    }
}

/// action が namespace / track を必須とするかを返す
///
/// 戻り値は (namespace が必要か, track が必要か) である。draft-ietf-moq-c4m-02 §4.2 は
/// 「Track Namespace (key 2) は namespace または track を対象とする action に現れ、
/// Track Name (key 3) は track を対象とする action に現れる」と定める。
fn action_target_requirements(action: MoqtAction) -> (bool, bool) {
    match action {
        MoqtAction::Setup => (false, false),
        MoqtAction::PublishNamespace
        | MoqtAction::SubscribeNamespace
        | MoqtAction::SubscribeTracks => (true, false),
        MoqtAction::Subscribe
        | MoqtAction::RequestUpdate
        | MoqtAction::Publish
        | MoqtAction::Fetch
        | MoqtAction::TrackStatus => (true, true),
    }
}

/// CWT 形式の DPoP proof のクレーム (draft-ietf-moq-c4m-02 §4.2)
///
/// `cti` / `iat` / `actx` / `ath` は必須である。型付きで解釈しないクレームは
/// [`CwtDpopProofClaims::raw`] にそのまま保持する。
#[derive(Debug, Clone, PartialEq)]
pub struct CwtDpopProofClaims {
    /// `cti` (7)。proof の一意な識別子
    pub cti: Vec<u8>,
    /// `iat` (6)。作成時刻 (UNIX 秒)
    pub issued_at: i64,
    /// `actx`。MOQT のアクション
    pub authorization_context: CwtAuthorizationContext,
    /// `ath`。CAT トークンの SHA-256
    pub access_token_hash: Vec<u8>,
    /// 解釈しなかったクレーム
    pub raw: Vec<(Value, Value)>,
}

/// CWT 形式の DPoP proof のヘッダ
#[derive(Debug, Clone, PartialEq)]
pub struct CwtDpopProofHeader {
    /// `alg`。非対称署名アルゴリズム
    pub algorithm: Algorithm,
    /// `typ`。"dpop-proof+cwt"
    pub typ: String,
    /// `kccs` の `cnf` から取り出した公開鍵
    pub confirmation_key: CoseKey,
}

/// CWT 形式の DPoP proof を CAT トークンに束縛して受理するかの検証入力
///
/// [`CwtDpopProof::verify_against_cat_token`] だけが使う入力である。呼び出し側が CAT の
/// 署名・クレーム・scope の検証を別々に済ませる必要はなく、この入力と proof を渡す
/// 1 つの入口がすべての条件を判定する。
#[derive(Debug, Clone, Copy)]
pub struct CwtDpopVerification<'a> {
    /// 提示された CAT トークン
    ///
    /// `ath` の照合には [`CatToken::raw_token`] が返す「提示されたままのバイト列」を
    /// 使い、再エンコードや Base64 の正規化をした表現は使わない。
    pub token: &'a CatToken,
    /// CAT の署名 / MAC を検証する信頼済み鍵
    pub token_key: &'a CoseKey,
    /// CAT の署名検証オプション
    pub token_verify_options: VerifyOptions<'a>,
    /// CAT のクレーム検証オプション
    ///
    /// `reference_time_seconds` は proof の鮮度と replay の記録にも使う。
    /// `revalidation_interval_seconds` は `moqt-reval` の受理可否の判定に使い、`None`
    /// (再検証できない) では `moqt-reval` を持つトークンを値が 0 でも拒否する。
    pub claim_options: ClaimValidationOptions<'a>,
    /// 要求されたアクション
    pub action: MoqtAction,
    /// 要求された Track Namespace のフィールド列
    ///
    /// 欠落と空の配列は区別する。SETUP では `None`、namespace を対象とするアクション
    /// では `Some` を渡す。
    pub namespace: Option<&'a [&'a [u8]]>,
    /// 要求された Track Name
    ///
    /// 欠落と空のバイト列は区別する。SETUP と namespace だけを対象とするアクションでは
    /// `None`、track を対象とするアクションでは `Some` を渡す。
    pub track_name: Option<&'a [u8]>,
    /// proof をデコードした [`CwtDpopProofCodec`]
    ///
    /// `actx` / `ath` の claim label はドラフトで未確定のため、proof をデコードした設定と
    /// 同じものを渡す。受理入口は proof のクレームがこの設定と矛盾しないことを確認する。
    pub codec: CwtDpopProofCodec,
    /// `catdpop` のウィンドウ (label 0) が無い場合に使う既定のウィンドウ (秒)
    ///
    /// 有限かつ 0 以上でなければならない。
    pub default_window_seconds: f64,
}

/// CWT 形式の DPoP proof の `cti` によるリプレイキャッシュ
///
/// draft-ietf-moq-c4m-02 §4.3 の 7 番目の条件を満たすための受信者側の状態である。
///
/// - 1 つのキャッシュは、同じトークン / proof を受理する 1 つの検証主体 (1 つのリレー
///   など) が所有し、その検証主体が受理する proof 群と全接続で共有する。接続ごとに
///   作り直してはならない。作り直すと、記録が接続とともに消えて、別の接続で同じ
///   `cti` を再利用できてしまう
/// - `max_window_seconds` は構築時に固定し、有限かつ正でなければならない。proof の
///   ウィンドウがこれを超える場合は [`DpopError::ReplayWindowTooLarge`] で拒否する
///   (fail-closed)。より大きいウィンドウの proof を受け入れる検証主体は、その最大値を
///   満たすキャッシュを作ること
/// - 記録は `cti` から保持期限 (`iat` + `max_window_seconds`) への写像である。参照時刻が
///   保持期限と等しい間は保持し、超えた後にだけ解放する。**現在の proof のウィンドウで
///   古い記録を破棄しない**。そのため、小さいウィンドウの proof を処理した後でも、既に
///   記録した `cti` が大きいウィンドウ内に残っていれば再送を [`DpopError::Replayed`] で
///   拒否できる
/// - 未失効の記録を追い出して別の `cti` を記録することはしない。記録数の上限は持たない
///   ため、肥大化を防ぐ必要がある場合は受理する proof 数を制限すること
/// - `cti` は proof のクレーム (RFC 8392 §3.1.7) のバイト列をそのままキーにする。`cti` は
///   発行者をまたいで一意であることを前提とする。`cti` の長さに上限は設けないため、
///   肥大化を防ぐ必要がある場合は `cti` 長と受理する proof 数を制限すること
/// - [`CwtDpopReplayCache::check_and_record`] が成功するたびに全記録を走査するため、
///   記録数に対して O(n) のコストがかかる
/// - 参照時刻は単調増加を前提とする。時計が巻き戻ると、解放済みの `cti` が再び受理され
///   得る
///
/// [`CatToken::verify_bearer`] が使う `BearerReplayCache` は bearer トークンの接続束縛
/// (draft-ietf-moq-c4m-02 §3) 用であり、proof の `cti` 用に代用してはならない。逆に
/// このキャッシュを bearer トークンの接続束縛に使ってもならない。両者はキーも保持期間も
/// 意味も異なる。リレー・クラスタ間の共有は行わない。
#[derive(Debug)]
pub struct CwtDpopReplayCache {
    /// 記録の保持期間の上限 (秒)。構築時に固定する
    max_window_seconds: f64,
    /// `cti` → 保持期限 (`iat` + `max_window_seconds`)
    entries: BTreeMap<Vec<u8>, f64>,
}

impl CwtDpopReplayCache {
    /// `cti` の記録に使う最大ウィンドウ (秒) を固定してキャッシュを作る
    ///
    /// `max_window_seconds` は有限かつ正でなければならない。不正な場合は
    /// [`DpopError::InvalidReplayWindow`] を返す。
    pub fn new(max_window_seconds: f64) -> Result<Self, DpopError> {
        if !max_window_seconds.is_finite() || max_window_seconds <= 0.0 {
            return Err(DpopError::InvalidReplayWindow);
        }
        Ok(Self {
            max_window_seconds,
            entries: BTreeMap::new(),
        })
    }

    /// 構築時に固定した最大ウィンドウ (秒) を返す
    pub fn max_window_seconds(&self) -> f64 {
        self.max_window_seconds
    }

    /// `cti` を確認し、記録できれば記録する
    ///
    /// `window_seconds` は proof の鮮度に使ったウィンドウである。次の順に判定する。
    ///
    /// 1. `issued_at` / `reference_time_seconds` / `window_seconds` が有限で、
    ///    `window_seconds` が 0 以上であること
    /// 2. `window_seconds <= max_window_seconds` であること。超える場合は
    ///    [`DpopError::ReplayWindowTooLarge`] を返す
    /// 3. 参照時刻が保持期限 (`iat` + `max_window_seconds`) を超えた記録だけを解放する
    ///    (等しい間は保持する)
    /// 4. 同じ `cti` が残っていれば [`DpopError::Replayed`] を返す
    /// 5. 記録する
    ///
    /// 1 / 2 / 4 で拒否した場合は新しい `cti` を記録せず、解放もしない。3 の解放は
    /// 4 の判定より前に行うが、保持期限を超えた記録を残さないための処理であり、
    /// 受理の可否には影響しない。
    ///
    /// [`CwtDpopProof::verify_against_cat_token`] は、`catdpop` が replay detection を
    /// 要求している場合にだけこの関数を呼ぶ。要求していない proof は記録しない。
    pub fn check_and_record(
        &mut self,
        cti: &[u8],
        issued_at: f64,
        reference_time_seconds: f64,
        window_seconds: f64,
    ) -> Result<(), DpopError> {
        if !issued_at.is_finite() {
            return Err(DpopError::NonFiniteNumber("iat"));
        }
        if !reference_time_seconds.is_finite() {
            return Err(DpopError::NonFiniteNumber("reference time"));
        }
        if !window_seconds.is_finite() || window_seconds < 0.0 {
            return Err(DpopError::InvalidFreshnessWindow);
        }
        if window_seconds > self.max_window_seconds {
            return Err(DpopError::ReplayWindowTooLarge);
        }
        let retain_until = issued_at + self.max_window_seconds;
        if !retain_until.is_finite() {
            return Err(DpopError::NonFiniteNumber("replay retention"));
        }
        // 参照時刻が保持期限と等しい間は記録を保持し、超えた後だけ解放する。現在の
        // proof のウィンドウではなく、キャッシュに固定した最大ウィンドウで判定する
        self.entries
            .retain(|_, retain_until| reference_time_seconds <= *retain_until);
        if self.entries.contains_key(cti) {
            return Err(DpopError::Replayed);
        }
        self.entries.insert(cti.to_vec(), retain_until);
        Ok(())
    }

    /// 記録数を返す
    ///
    /// 保持期限を過ぎた記録は次の [`CwtDpopReplayCache::check_and_record`] まで残るため、
    /// この数には期限切れの記録も含まれ得る。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 記録が空かどうかを返す
    ///
    /// 期限切れの記録は次の [`CwtDpopReplayCache::check_and_record`] の成功経路で解放
    /// されるまで残るため、期限切れの記録だけが残っている場合も `false` を返す。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// CWT 形式の DPoP proof
///
/// デコードで得られる型であり、署名の検証は
/// [`CwtDpopProof::verify_signature`]、`kccs` 公開鍵と CAT トークンの `cnf` の照合は
/// [`CwtDpopProof::verify_key_binding`] が行う。CAT トークンとの束縛・`actx` / `ath` /
/// 鮮度 / replay を含む受理判定は [`CwtDpopProof::verify_against_cat_token`] が一括して
/// 行う。
#[derive(Debug, Clone, PartialEq)]
pub struct CwtDpopProof {
    header: CwtDpopProofHeader,
    claims: CwtDpopProofClaims,
    signing_input: Vec<u8>,
    signature: Vec<u8>,
    /// デコードに使った `actx` の claim label
    actx_label: i64,
    /// デコードに使った `ath` の claim label
    ath_label: i64,
}

impl CwtDpopProof {
    /// ヘッダを返す
    pub fn header(&self) -> &CwtDpopProofHeader {
        &self.header
    }

    /// クレームを返す
    pub fn claims(&self) -> &CwtDpopProofClaims {
        &self.claims
    }

    /// 署名対象のバイト列 (RFC 9052 §4.4 の `Sig_structure`) を返す
    pub fn signing_input(&self) -> &[u8] {
        &self.signing_input
    }

    /// 署名を返す
    pub fn signature(&self) -> &[u8] {
        &self.signature
    }

    /// proof の署名を `kccs` の公開鍵で検証する
    ///
    /// 検証するのは proof 単独の署名だけであり、`cnf` の `ckt` / `jkt` との一致
    /// ([`CwtDpopProof::verify_key_binding`])、`ath` の照合、`iat` の鮮度、`cti` の
    /// replay 保護は行わない。
    pub fn verify_signature<C: CoseCrypto>(&self, crypto: &C) -> Result<(), DpopError> {
        crypto.verify(
            self.header.algorithm,
            &self.header.confirmation_key,
            &self.signing_input,
            &self.signature,
        )?;
        Ok(())
    }

    /// proof の `kccs` 公開鍵がトークンの `cnf` に束縛されているか検証する
    ///
    /// draft-ietf-moq-c4m-02 §4.3 の 4 番目の条件を、proof のヘッダにある COSE_Key と
    /// `cnf` の各 confirmation method について検証する。
    ///
    /// - `ckt` (5): 公開鍵の COSE Key Thumbprint (RFC 9679 §3、SHA-256) が値と一致すること
    /// - `jkt` (323): 公開鍵から作った等価な JWK の JWK サムプリント (RFC 7638、SHA-256)
    ///   が値と一致すること
    ///
    /// `ckt` と `jkt` が併存する場合は、同じ proof 公開鍵に対して両方の一致を要求する。
    /// 片方だけの一致では受理せず、`ckt` の不一致を `jkt` への fallback で救済しない。
    /// RFC 8747 §3.1 は `cnf` が単一の proof-of-possession 鍵だけを表すことを要求する
    /// ため、併存する method は同じ鍵を指さなければならない。
    ///
    /// `cnf` に `ckt` も `jkt` も無い場合は [`DpopError::MissingThumbprint`] を返す。
    /// この API が解釈しない method (COSE_Key (1) と未知の method) は RFC 8747 §3.1 に
    /// 従って無視し、認可の根拠には使わない。draft-ietf-moq-c4m-01 のベクタが `jkt` に
    /// 使う key 3 は IANA のレジストリでは `kid` であり、この API では解釈しない。
    ///
    /// proof の署名検証 ([`CwtDpopProof::verify_signature`]) は行わない。呼び出し側が
    /// 両方を検証すること。
    pub fn verify_key_binding<C: CoseCrypto>(
        &self,
        crypto: &C,
        confirmation: &Confirmation,
    ) -> Result<(), DpopError> {
        let expected_ckt = confirmation.ckt();
        let expected_jkt = confirmation.jwk_thumbprint.as_deref();
        // 既知の confirmation method が無い cnf は、どの鍵とも照合できないため拒否する
        if expected_ckt.is_none() && expected_jkt.is_none() {
            return Err(DpopError::MissingThumbprint);
        }
        let key = &self.header.confirmation_key;
        if let Some(expected) = expected_ckt {
            let actual = key.cose_key_thumbprint_sha256(crypto)?;
            if actual.as_slice() != expected {
                return Err(DpopError::KeyBindingMismatch);
            }
        }
        // ckt と jkt が併存する場合は ckt の照合後も jkt を照合する。ここで ckt だけを
        // 見て成功を返すと、同じ proof 公開鍵について jkt が一致していなくても受理して
        // しまうため、両方を必ず照合する
        if let Some(expected) = expected_jkt {
            let jwk = Jwk::from_cose_key(key)?;
            let actual = jwk.thumbprint_sha256(crypto)?;
            if actual.as_slice() != expected {
                return Err(DpopError::KeyBindingMismatch);
            }
        }
        Ok(())
    }

    /// `actx` の type / action / namespace / track が要求と一致するか検証する
    ///
    /// action ごとの target の規則は [`CwtAuthorizationContext::verify_request`] を
    /// 参照。`namespace` / `track_name` は欠落を `None` で表し、空の値と区別する。
    pub fn verify_authorization_context(
        &self,
        action: MoqtAction,
        namespace: Option<&[&[u8]]>,
        track_name: Option<&[u8]>,
    ) -> Result<(), DpopError> {
        self.claims
            .authorization_context
            .verify_request(action, namespace, track_name)
    }

    /// `iat` が参照時刻からウィンドウ内にあるか検証する
    ///
    /// 前方向と後方向に同じウィンドウを適用し、差の絶対値がウィンドウ以下なら受理する
    /// (境界を含む)。CWT proof の `iat` は draft-ietf-moq-c4m-02 §4.2 の CDDL で
    /// `int` と定められているため常に有限である。
    /// 参照時刻とウィンドウは有限で、ウィンドウは 0 以上でなければならない。
    pub fn verify_freshness(
        &self,
        reference_time_seconds: f64,
        window_seconds: f64,
    ) -> Result<(), DpopError> {
        if !reference_time_seconds.is_finite() {
            return Err(DpopError::NonFiniteNumber("reference time"));
        }
        if !window_seconds.is_finite() || window_seconds < 0.0 {
            return Err(DpopError::InvalidFreshnessWindow);
        }
        let issued_at = self.claims.issued_at as f64;
        let delta = reference_time_seconds - issued_at;
        if delta > window_seconds {
            return Err(DpopError::ProofExpired);
        }
        if -delta > window_seconds {
            return Err(DpopError::ProofNotYetValid);
        }
        Ok(())
    }

    /// proof のクレームが指定された claim label 設定と矛盾しないか確認する
    ///
    /// `actx` / `ath` は label が未確定のため、proof は呼び出し側が選んだ label で
    /// デコードされる。この proof をデコードした label 設定と、受理入口に渡された
    /// 設定が違えば、同じ proof に対する解釈が 2 通りになるため拒否する。
    fn verify_claim_labels(&self, codec: CwtDpopProofCodec) -> Result<(), DpopError> {
        if self.actx_label != codec.actx_label() || self.ath_label != codec.ath_label() {
            return Err(DpopError::ClaimLabelMismatch);
        }
        Ok(())
    }

    /// CWT 形式の DPoP proof を CAT トークンに束縛して受理できるか一括検証する
    ///
    /// draft-ietf-moq-c4m-02 §4.3 の条件を、CAT の検証を含めて 1 つの入口で判定する。
    /// 呼び出し側が署名・クレーム・scope の検証を別々に済ませたと仮定する API ではない。
    ///
    /// 検証は draft-ietf-moq-c4m-02 §4.3 の条件を次の順に行う。すべて成功したときだけ
    /// `cache` に replay 状態を記録し、途中で失敗した場合は記録しない。
    ///
    /// 1. [`CwtDpopVerification::default_window_seconds`] の値の検証
    ///    (有限かつ 0 以上)
    /// 2. CAT の署名 / MAC ([`CatToken::verify_with`])。信頼済み鍵と
    ///    [`CwtDpopVerification::token_verify_options`] を使う
    /// 3. CAT のクレーム (`exp` / `nbf` / `iss` / `aud` と `moqt-reval` の受理可否)
    ///    ([`CatClaims::validate`])
    /// 4. proof のクレームが [`CwtDpopVerification::codec`] の label 設定と一致すること
    /// 5. proof の署名 ([`CwtDpopProof::verify_signature`])
    /// 6. `kccs` の公開鍵と `cnf` の `ckt` / `jkt` の一致
    ///    ([`CwtDpopProof::verify_key_binding`])
    /// 7. `ath` と提示された CAT のバイト列の SHA-256 の一致
    /// 8. `actx` の type / action / namespace / track と要求の一致
    ///    ([`CwtDpopProof::verify_authorization_context`])
    /// 9. `iat` の鮮度 ([`CwtDpopProof::verify_freshness`])。ウィンドウは `catdpop` の
    ///    label 0 を使い、無い場合は [`CwtDpopVerification::default_window_seconds`] を
    ///    使う
    /// 10. `moqt` クレームによる scope 認可 ([`CatClaims::authorize`])
    /// 11. `cnf` と `catreplay` が併存するトークンの拒否
    /// 12. proof のウィンドウが cache の最大ウィンドウを超えないこと
    /// 13. `catdpop` が replay detection を要求している場合 (`honor_jti`) の `cti` の
    ///     確認と記録 ([`CwtDpopReplayCache::check_and_record`])
    ///
    /// `moqt-reval` の受理可否は [`CwtDpopVerification::claim_options`] の
    /// `revalidation_interval_seconds` で判定する。再検証そのもの (定期的に検証し直す
    /// こと) は実行しないため、非ゼロの `moqt-reval` を持つトークンを受理した利用側が
    /// その間隔以内で再検証すること (0 は再検証してはならない)。
    ///
    /// `cnf` を持たないトークンは鍵束縛を確認できないため [`DpopError::MissingConfirmation`]
    /// で拒否する。この入口は proof を必須とし、`cnf` を持つトークンのアクションを
    /// proof なしで認可する API ではない。`cnf` の key 3 は IANA では `kid` であり、
    /// 鍵束縛の根拠にしない ([`CwtDpopProof::verify_key_binding`])。
    ///
    /// scope 認可には要求の namespace / track をそのまま渡す。SETUP と namespace だけを
    /// 対象とするアクションでは欠落している側に空の値を渡す。
    ///
    /// `cache` は同じトークン / proof を受理する検証主体が全接続で共有する
    /// ([`CwtDpopReplayCache`])。replay detection を要求しない proof は記録しないが、
    /// proof のウィンドウが cache の `max_window_seconds` を超えないことの判定には常に
    /// 使うため、省略できない。`default_window_seconds` は `catdpop` に window が無い
    /// 場合の鮮度ウィンドウであり、cache の `max_window_seconds` 以下であることが
    /// 期待される。
    ///
    /// `self` は [`CwtDpopProofCodec::decode`] で解析済みの proof であること。§4.3 の
    /// 条件 2 (COSE_Sign1 / `typ` = "dpop-proof+cwt" / 非対称 `alg`) は decode が
    /// 検査するため、この入口では再検査しない。
    pub fn verify_against_cat_token<C: CoseCrypto>(
        &self,
        crypto: &C,
        request: &CwtDpopVerification<'_>,
        cache: &mut CwtDpopReplayCache,
    ) -> Result<(), DpopError> {
        // `catdpop` に window が無い場合に使う既定値も入口で検証する
        if !request.default_window_seconds.is_finite() || request.default_window_seconds < 0.0 {
            return Err(DpopError::InvalidFreshnessWindow);
        }
        // 2. CAT 自体の署名 / MAC を、呼び出し側が指定した信頼済み鍵で検証する
        request
            .token
            .verify_with(crypto, request.token_key, &request.token_verify_options)?;
        // 3. CAT の時刻・issuer・audience と moqt-reval の受理可否を検証する
        request
            .token
            .claims()
            .validate(&request.claim_options)
            .map_err(|error| DpopError::Cat(CatError::Claims(error)))?;
        // 4. proof をデコードした label 設定との一致を確認する
        self.verify_claim_labels(request.codec)?;
        // 5. proof 単独の署名を kccs の公開鍵で検証する
        self.verify_signature(crypto)?;
        // 6. cnf による鍵束縛を検証する。cnf が無いトークンは DPoP の受理対象にしない
        let confirmation = request
            .token
            .claims()
            .confirmation
            .as_ref()
            .ok_or(DpopError::MissingConfirmation)?;
        self.verify_key_binding(crypto, confirmation)?;
        // 6. ath は「提示されたままの CAT のバイト列」の SHA-256 と比較する。
        //    再エンコードや Base64 の正規化をした表現は使わない
        let token_hash = crypto.digest(DigestAlgorithm::Sha256, request.token.raw_token())?;
        if token_hash.as_slice() != self.claims.access_token_hash.as_slice() {
            return Err(DpopError::AccessTokenHashMismatch);
        }
        // 7. actx の type / action / namespace / track を要求と比較する
        self.verify_authorization_context(request.action, request.namespace, request.track_name)?;
        // 8. iat の鮮度を、catdpop のウィンドウ (無ければ既定値) で検証する
        let window_seconds = request
            .token
            .claims()
            .catdpop
            .as_ref()
            .map_or(request.default_window_seconds, |catdpop| {
                catdpop.window_seconds_or(request.default_window_seconds)
            });
        self.verify_freshness(request.claim_options.reference_time_seconds, window_seconds)?;
        // 10. moqt クレームの scope で要求 action が認可されるか検証する
        let namespace = request.namespace.unwrap_or(&[]);
        let track_name = request.track_name.unwrap_or(&[]);
        if !request
            .token
            .claims()
            .authorize(request.action, namespace, track_name)
        {
            return Err(DpopError::Cat(CatError::ActionNotAuthorized(
                request.action,
            )));
        }
        // 11. `cnf` による鍵束縛と `catreplay` の接続束縛が併存するトークンは、
        // この API では受理判定を行わない (bearer 用の接続束縛と proof 用の replay は
        // 別の仕組みであり、併存時の意味を定義していない)
        if request
            .token
            .claims()
            .cat_replay
            .is_some_and(|replay| replay != CatReplay::NotProtected)
        {
            return Err(DpopError::CatReplayWithConfirmation);
        }
        // 12. proof のウィンドウが cache の上限を超える場合は、replay detection の
        // 有無にかかわらず拒否する (cache が保持できない期間の proof を受理しない)
        if window_seconds > cache.max_window_seconds() {
            return Err(DpopError::ReplayWindowTooLarge);
        }
        // 13. ここまでの検証がすべて成功したときだけ cti を確認して記録する
        if request
            .token
            .claims()
            .catdpop
            .as_ref()
            .is_some_and(super::CatDpop::honors_jti)
        {
            cache.check_and_record(
                &self.claims.cti,
                self.claims.issued_at as f64,
                request.claim_options.reference_time_seconds,
                window_seconds,
            )?;
        }
        Ok(())
    }
}

/// CWT 形式の DPoP proof のコーデック
///
/// draft-ietf-moq-c4m-02 §4.2 の `actx` と `ath` の claim label は `TBD_ACTX` /
/// `TBD_ATH` のままであり、IANA の割当も未確定である。正式な label を捏造しない
/// ため、発行・解析・検証のすべてで同じ label 設定を呼び出し側から受け取る。
///
/// `actx` と `ath` が同じ値、またはいずれかが CWT の登録済み claim key (1〜8) と
/// 衝突する設定は [`CwtDpopProofCodec::new`] が拒否する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CwtDpopProofCodec {
    actx_label: i64,
    ath_label: i64,
}

impl CwtDpopProofCodec {
    /// claim label を指定してコーデックを作る
    ///
    /// `actx_label` と `ath_label` が同じ場合、またはいずれかが CWT の登録済み claim
    /// key (1〜8) と衝突する場合はエラーを返す。
    pub fn new(actx_label: i64, ath_label: i64) -> Result<Self, DpopError> {
        if actx_label == ath_label {
            return Err(DpopError::ClaimLabelConflict);
        }
        if (1..=8).contains(&actx_label) {
            return Err(DpopError::ReservedClaimLabel("actx"));
        }
        if (1..=8).contains(&ath_label) {
            return Err(DpopError::ReservedClaimLabel("ath"));
        }
        Ok(Self {
            actx_label,
            ath_label,
        })
    }

    /// `actx` の claim label を返す
    pub fn actx_label(&self) -> i64 {
        self.actx_label
    }

    /// `ath` の claim label を返す
    pub fn ath_label(&self) -> i64 {
        self.ath_label
    }

    /// CWT 形式の DPoP proof を発行する
    ///
    /// 保護ヘッダに `alg` / `typ` / `kccs` を置き、`kccs` の `cnf` に `key` の公開鍵
    /// だけを入れて署名する。`key` は秘密鍵を含まなければならない
    /// ([`CryptoError::MissingPrivateKey`] は [`DpopError::Crypto`] で返る)。
    ///
    /// 対称鍵 (HMAC) では発行しない ([`DpopError::UnsupportedAlgorithm`])。
    /// `actx.type` が "moqt" でない claims も発行しない
    /// ([`DpopError::ContextTypeMismatch`])。CWT タグ (61) は付けず、COSE_Sign1 タグ
    /// (18) だけを付ける。
    pub fn build<C: CoseCrypto>(
        &self,
        crypto: &C,
        key: &CoseKey,
        claims: &CwtDpopProofClaims,
    ) -> Result<Vec<u8>, DpopError> {
        let algorithm = default_signing_algorithm(key);
        if algorithm.is_mac() {
            return Err(DpopError::UnsupportedAlgorithm);
        }
        // actx の型は閉じた CDDL の固定値である。受理判定を行う利用側を待たず、
        // 発行側で MOQT 以外の type の proof を作らない
        if claims.authorization_context.context_type != MOQT_AUTHORIZATION_CONTEXT_TYPE {
            return Err(DpopError::ContextTypeMismatch);
        }
        let kccs = Value::Map(vec![(
            Value::integer(CLAIM_CONFIRMATION),
            Value::Map(vec![(
                Value::integer(CONFIRMATION_COSE_KEY),
                key.to_cose_key_map()?,
            )]),
        )]);
        let header = Header {
            algorithm: Some(algorithm),
            algorithm_identifier: Some(algorithm.identifier()),
            typ: Some(Value::TextString(String::from(DPOP_PROOF_CWT_TYPE))),
            kccs: Some(kccs),
            ..Header::default()
        };
        let protected = cbor::encode(&header.encode()?)?;
        let payload = cbor::encode(&self.encode_claims(claims)?)?;
        let mut message = CoseSign1 {
            protected,
            unprotected: Vec::new(),
            payload: Some(payload),
            signature: Vec::new(),
            cose_tagged: true,
            cwt_tagged: false,
        };
        let signing_input = CoseMessage::Sign1(message.clone()).signing_input()?;
        message.signature = crypto.sign(algorithm, key, &signing_input)?;
        Ok(CoseMessage::Sign1(message).encode(&CoseEncodingOptions {
            cose_tag: true,
            cwt_tag: false,
        })?)
    }

    /// CWT 形式の DPoP proof をデコードする
    ///
    /// 次の条件を満たさない proof は拒否する。
    ///
    /// - COSE_Sign1 であること
    /// - protected ヘッダに `alg` (非対称) / `typ` = "dpop-proof+cwt" / `kccs` があること
    /// - `kccs` の `cnf` に公開の COSE_Key があること
    /// - ペイロードが `cti` / `iat` / `actx` / `ath` を持つこと
    ///
    /// 署名の検証も、`actx.type` / action / namespace / track と要求の整合も行わない。
    /// これらは proof を受理するかの判定で確認する。COSE タグ 18 の付かない
    /// COSE_Sign1 も受理する。
    ///
    /// `crit` に `kccs` (14) を含む protected ヘッダは、[`Header`] が label 14 を
    /// 理解済みとして受理する。理解していない critical label は
    /// [`CoseError::UnsupportedCriticalHeader`] で拒否する。
    pub fn decode(&self, bytes: &[u8]) -> Result<CwtDpopProof, DpopError> {
        let message = CoseMessage::decode(bytes)?;
        let CoseMessage::Sign1(sign1) = &message else {
            return Err(DpopError::NotCoseSign1);
        };
        let header = message.header()?;
        let algorithm = header.algorithm.ok_or(DpopError::MissingHeader("alg"))?;
        let typ = match &header.typ {
            Some(Value::TextString(typ)) => typ.clone(),
            Some(_) => return Err(DpopError::UnexpectedType("typ")),
            None => return Err(DpopError::MissingHeader("typ")),
        };
        if typ != DPOP_PROOF_CWT_TYPE {
            return Err(DpopError::InvalidType(typ));
        }
        let kccs = header
            .kccs
            .as_ref()
            .ok_or(DpopError::MissingHeader("kccs"))?;
        let confirmation_key = decode_kccs_confirmation_key(kccs, algorithm)?;
        let payload = sign1
            .payload
            .as_deref()
            .ok_or(DpopError::Cose(CoseError::DetachedPayloadUnsupported))?;
        let claims = self.decode_claims(&cbor::decode(payload)?)?;
        let signing_input = message.signing_input()?;
        Ok(CwtDpopProof {
            header: CwtDpopProofHeader {
                algorithm,
                typ,
                confirmation_key,
            },
            claims,
            signing_input,
            signature: sign1.signature.clone(),
            actx_label: self.actx_label,
            ath_label: self.ath_label,
        })
    }

    /// クレームを CBOR のマップへエンコードする
    ///
    /// `raw` に必須クレームの label を置いた場合は [`DpopError::DuplicateClaim`] を
    /// 返す。`raw` 同士の重複ラベルは、CBOR へのエンコード時に
    /// [`CborError::DuplicateMapKey`] として拒否される。
    pub fn encode_claims(&self, claims: &CwtDpopProofClaims) -> Result<Value, DpopError> {
        let mut entries = vec![
            (
                Value::integer(CLAIM_ISSUED_AT),
                Value::integer(claims.issued_at),
            ),
            (
                Value::integer(CLAIM_CWT_ID),
                Value::ByteString(claims.cti.clone()),
            ),
            (
                Value::integer(self.actx_label),
                encode_cwt_authorization_context(&claims.authorization_context),
            ),
            (
                Value::integer(self.ath_label),
                Value::ByteString(claims.access_token_hash.clone()),
            ),
        ];
        for (key, _) in &claims.authorization_context.raw {
            if let Some(label) = key.as_int()
                && (ACTX_TYPE..=ACTX_TRACK_NAME).contains(&label)
            {
                return Err(DpopError::DuplicateClaim(label));
            }
        }
        for (key, value) in &claims.raw {
            // RFC 8392 §3: Claim Key は整数またはテキスト文字列である
            if key.as_int().is_none() && !matches!(key, Value::TextString(_)) {
                return Err(DpopError::UnexpectedType("claim key"));
            }
            if let Some(label) = key.as_int()
                && (label == CLAIM_ISSUED_AT
                    || label == CLAIM_CWT_ID
                    || label == self.actx_label
                    || label == self.ath_label)
            {
                return Err(DpopError::DuplicateClaim(label));
            }
            entries.push((key.clone(), value.clone()));
        }
        Ok(Value::Map(entries))
    }

    /// CBOR のマップからクレームをデコードする
    ///
    /// `cti` / `iat` / `actx` / `ath` を必須とし、型違反と重複を拒否する。これら
    /// 以外のクレームは値の型を問わず `raw` に保持する (RFC 8392 §3)。
    /// 必須クレームの代替にはしない。整数でもテキストでもない claim key は
    /// [`DpopError::UnexpectedType`] で拒否する。
    pub fn decode_claims(&self, value: &Value) -> Result<CwtDpopProofClaims, DpopError> {
        let entries = value.as_map().ok_or(DpopError::UnexpectedType("claims"))?;
        // claim key の重複は、決定論的な CBOR 表現に直してソートしてから隣接比較する。
        // wire 入力は `decode` が先に CBOR デコーダで重複を拒否するため、ここは公開 API に
        // `Value` を直接渡された場合の防御である。
        let mut encoded_keys = Vec::new();
        for (key, _) in entries {
            encoded_keys.push(cbor::encode(key)?);
        }
        let mut sorted_keys = encoded_keys.clone();
        sorted_keys.sort();
        for pair in sorted_keys.windows(2) {
            if pair[0] != pair[1] {
                continue;
            }
            let key = cbor::decode(&pair[0])?;
            return Err(match key {
                Value::TextString(name) => DpopError::DuplicateMember(name),
                key => match key.as_int() {
                    Some(label) => DpopError::DuplicateClaim(label),
                    None => DpopError::UnexpectedType("claim key"),
                },
            });
        }
        let mut cti = None;
        let mut issued_at = None;
        let mut authorization_context = None;
        let mut access_token_hash = None;
        let mut raw = Vec::new();
        for (key, entry) in entries {
            let Some(label) = key.as_int() else {
                if let Value::TextString(_) = key {
                    raw.push((key.clone(), entry.clone()));
                    continue;
                }
                return Err(DpopError::UnexpectedType("claim key"));
            };
            match label {
                CLAIM_CWT_ID => {
                    cti = Some(
                        entry
                            .as_bytes()
                            .ok_or(DpopError::UnexpectedType("cti"))?
                            .to_vec(),
                    );
                }
                CLAIM_ISSUED_AT => {
                    issued_at = Some(entry.as_int().ok_or(DpopError::UnexpectedType("iat"))?);
                }
                label if label == self.actx_label => {
                    authorization_context = Some(decode_cwt_authorization_context(entry)?);
                }
                label if label == self.ath_label => {
                    access_token_hash = Some(
                        entry
                            .as_bytes()
                            .ok_or(DpopError::UnexpectedType("ath"))?
                            .to_vec(),
                    );
                }
                _ => raw.push((key.clone(), entry.clone())),
            }
        }
        Ok(CwtDpopProofClaims {
            cti: cti.ok_or(DpopError::MissingClaim("cti"))?,
            issued_at: issued_at.ok_or(DpopError::MissingClaim("iat"))?,
            authorization_context: authorization_context.ok_or(DpopError::MissingClaim("actx"))?,
            access_token_hash: access_token_hash.ok_or(DpopError::MissingClaim("ath"))?,
            raw,
        })
    }
}

/// CWT 形式の actx をデコードする
///
/// `type` と `action` を必須とし、`namespace` と `track` を任意とする。解釈しない
/// メンバーは raw に保持する。
fn decode_cwt_authorization_context(value: &Value) -> Result<CwtAuthorizationContext, DpopError> {
    let entries = value.as_map().ok_or(DpopError::UnexpectedType("actx"))?;
    let mut context_type = None;
    let mut action = None;
    let mut namespace = None;
    let mut track_name = None;
    let mut raw = Vec::new();
    // actx の整数ラベルは数が少ないが、手組みの map でも二次関数的にならないよう
    // ソートして隣接比較する
    let mut labels: Vec<i64> = entries.iter().filter_map(|(key, _)| key.as_int()).collect();
    labels.sort();
    if let Some(pair) = labels.windows(2).find(|pair| pair[0] == pair[1]) {
        return Err(DpopError::DuplicateClaim(pair[0]));
    }
    for (key, entry) in entries {
        let Some(label) = key.as_int() else {
            raw.push((key.clone(), entry.clone()));
            continue;
        };
        match label {
            ACTX_TYPE => {
                context_type = Some(String::from(
                    entry
                        .as_text()
                        .ok_or(DpopError::UnexpectedType("actx.type"))?,
                ));
            }
            ACTX_ACTION => {
                action = Some(
                    entry
                        .as_int()
                        .ok_or(DpopError::UnexpectedType("actx.action"))?,
                );
            }
            ACTX_NAMESPACE => {
                let fields = entry
                    .as_array()
                    .ok_or(DpopError::UnexpectedType("actx.namespace"))?;
                let mut values = Vec::new();
                for field in fields {
                    values.push(
                        field
                            .as_bytes()
                            .ok_or(DpopError::UnexpectedType("actx.namespace"))?
                            .to_vec(),
                    );
                }
                namespace = Some(values);
            }
            ACTX_TRACK_NAME => {
                track_name = Some(
                    entry
                        .as_bytes()
                        .ok_or(DpopError::UnexpectedType("actx.track"))?
                        .to_vec(),
                );
            }
            _ => raw.push((key.clone(), entry.clone())),
        }
    }
    Ok(CwtAuthorizationContext {
        context_type: context_type.ok_or(DpopError::MissingClaim("actx.type"))?,
        action: action.ok_or(DpopError::MissingClaim("actx.action"))?,
        namespace,
        track_name,
        raw,
    })
}

/// CWT 形式の actx をエンコードする
fn encode_cwt_authorization_context(context: &CwtAuthorizationContext) -> Value {
    let mut entries = vec![
        (
            Value::integer(ACTX_TYPE),
            Value::TextString(context.context_type.clone()),
        ),
        (Value::integer(ACTX_ACTION), Value::integer(context.action)),
    ];
    if let Some(namespace) = &context.namespace {
        entries.push((
            Value::integer(ACTX_NAMESPACE),
            Value::Array(
                namespace
                    .iter()
                    .map(|field| Value::ByteString(field.clone()))
                    .collect(),
            ),
        ));
    }
    if let Some(track_name) = &context.track_name {
        entries.push((
            Value::integer(ACTX_TRACK_NAME),
            Value::ByteString(track_name.clone()),
        ));
    }
    entries.extend(context.raw.iter().cloned());
    Value::Map(entries)
}

/// `kccs` の `cnf` から確認用の公開鍵を取り出す
///
/// `kccs` は CWT Claims Set であるため、`cnf` 以外のクレームは無視する。`cnf` の
/// 中で `COSE_Key` (1) 以外の confirmation method も、RFC 8747 §3.1 の「理解しない
/// confirmation method は無視する」に従って無視する。
fn decode_kccs_confirmation_key(
    kccs: &Value,
    expected_algorithm: Algorithm,
) -> Result<CoseKey, DpopError> {
    let entries = kccs.as_map().ok_or(DpopError::UnexpectedType("kccs"))?;
    let cnf = entries
        .iter()
        .find(|(key, _)| key.as_int() == Some(CLAIM_CONFIRMATION))
        .map(|(_, value)| value)
        .ok_or(DpopError::MissingKccsConfirmation)?;
    let cnf_entries = cnf.as_map().ok_or(DpopError::UnexpectedType("cnf"))?;
    let key = cnf_entries
        .iter()
        .find(|(key, _)| key.as_int() == Some(CONFIRMATION_COSE_KEY))
        .map(|(_, value)| value)
        .ok_or(DpopError::MissingConfirmationKey)?;
    Ok(CoseKey::from_cose_key_map(key, Some(expected_algorithm))?)
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
    /// COSE の構造のエラー
    Cose(CoseError),
    /// JSON のパースに失敗した
    Json(String),
    /// ヘッダに必須のメンバーが無い
    MissingHeader(&'static str),
    /// CWT 形式の proof に必須のクレームが無い
    MissingClaim(&'static str),
    /// メンバーの型が期待と異なる
    UnexpectedType(&'static str),
    /// メンバー名が重複している
    DuplicateMember(String),
    /// CWT 形式の proof のクレームラベルが重複している
    DuplicateClaim(i64),
    /// 数値が有限でない (NaN / 無限大)
    NonFiniteNumber(&'static str),
    /// `typ` が "dpop-proof+jwt" / "dpop-proof+cwt" ではない
    InvalidType(String),
    /// 対称鍵アルゴリズムは使えない
    UnsupportedAlgorithm,
    /// CWT 形式の proof が COSE_Sign1 ではない
    NotCoseSign1,
    /// トークンに `cnf` が無い
    MissingConfirmation,
    /// `kccs` に `cnf` が無い
    MissingKccsConfirmation,
    /// `kccs` の `cnf` に確認用の COSE_Key が無い
    MissingConfirmationKey,
    /// `cnf` に `ckt` / `jkt` のサムプリントが無い
    MissingThumbprint,
    /// サムプリントが `cnf` の値と一致しない
    KeyBindingMismatch,
    /// 署名鍵と埋め込む JWK が一致しない
    KeyMismatch,
    /// `actx.type` が "moqt" ではない
    ContextTypeMismatch,
    /// `actx.action` が一致しない
    ActionMismatch,
    /// proof の target (JWT の `tns` / `tn`、CWT の namespace / track) が要求と一致しない
    TargetMismatch,
    /// `actx.tn` が無い
    MissingTrackName,
    /// `actx.resource` が `tns` / `tn` と整合しない
    ResourceInconsistent,
    /// `ath` がアクセストークンと一致しない
    AccessTokenHashMismatch,
    /// アクセストークンを伴う proof に `ath` が無い
    MissingAccessTokenHash,
    /// proof に `ath` があるがアクセストークンが渡されていない
    MissingAccessToken,
    /// `iat` が古すぎる
    ProofExpired,
    /// `iat` が未来すぎる
    ProofNotYetValid,
    /// 同じ proof 識別子 (`jti` / `cti`) を再び受信した
    Replayed,
    /// jti の検証が必要だがリプレイキャッシュが渡されていない
    ReplayCacheRequired,
    /// `actx` と `ath` の claim label が同じ
    ClaimLabelConflict,
    /// `actx` / `ath` に登録済みの CWT claim key (1〜8) を指定した
    ReservedClaimLabel(&'static str),
    /// CAT のエラー
    Cat(CatError),
    /// 要求 action に必要な target が要求に無い
    MissingRequestTarget(&'static str),
    /// 要求 action に存在しない target が要求にある
    UnexpectedRequestTarget(&'static str),
    /// `actx` に要求 action に必要な target が無い
    MissingActxTarget(&'static str),
    /// `actx` に要求 action に存在しない target がある
    UnexpectedActxTarget(&'static str),
    /// 鮮度ウィンドウが有限でない、または負である
    InvalidFreshnessWindow,
    /// リプレイキャッシュの最大ウィンドウが有限でない、または 0 以下である
    InvalidReplayWindow,
    /// proof の鮮度ウィンドウがリプレイキャッシュの最大ウィンドウを超えている
    ReplayWindowTooLarge,
    /// proof のクレームが指定された claim label 設定と矛盾する
    ClaimLabelMismatch,
    /// `cnf` による鍵束縛と `catreplay` の接続束縛が併存している
    CatReplayWithConfirmation,
}

impl fmt::Display for DpopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Jwt(error) => write!(f, "JWT error: {error}"),
            Self::Jwk(error) => write!(f, "JWK error: {error}"),
            Self::Crypto(error) => write!(f, "crypto error: {error}"),
            Self::Cose(error) => write!(f, "COSE error: {error}"),
            Self::Json(message) => write!(f, "invalid DPoP JSON: {message}"),
            Self::MissingHeader(name) => write!(f, "DPoP header member is missing: {name}"),
            Self::MissingClaim(name) => write!(f, "DPoP proof claim is missing: {name}"),
            Self::UnexpectedType(name) => write!(f, "unexpected DPoP member type: {name}"),
            Self::DuplicateMember(name) => write!(f, "duplicate DPoP member: {name}"),
            Self::DuplicateClaim(label) => write!(f, "duplicate DPoP claim label: {label}"),
            Self::NonFiniteNumber(name) => write!(f, "{name} must be finite"),
            Self::InvalidType(typ) => write!(f, "invalid DPoP typ: {typ}"),
            Self::UnsupportedAlgorithm => {
                write!(f, "DPoP proof must use an asymmetric algorithm")
            }
            Self::NotCoseSign1 => write!(f, "DPoP proof must be a COSE_Sign1"),
            Self::MissingConfirmation => write!(f, "token has no cnf claim"),
            Self::MissingKccsConfirmation => write!(f, "kccs has no cnf claim"),
            Self::MissingConfirmationKey => write!(f, "kccs cnf has no COSE_Key"),
            Self::MissingThumbprint => write!(f, "cnf has no usable thumbprint"),
            Self::KeyBindingMismatch => write!(f, "key thumbprint does not match cnf"),
            Self::KeyMismatch => write!(f, "signing key does not match the embedded JWK"),
            Self::ContextTypeMismatch => write!(f, "actx.type does not match"),
            Self::ActionMismatch => write!(f, "actx.action does not match"),
            Self::TargetMismatch => write!(f, "actx.tns or actx.tn does not match the target"),
            Self::MissingTrackName => write!(f, "actx.tn is missing"),
            Self::ResourceInconsistent => {
                write!(f, "actx.resource is inconsistent with tns or tn")
            }
            Self::AccessTokenHashMismatch => write!(f, "ath does not match the access token"),
            Self::MissingAccessTokenHash => {
                write!(
                    f,
                    "ath is required when an access token is sent with the proof"
                )
            }
            Self::MissingAccessToken => {
                write!(f, "the access token is required to verify the ath claim")
            }
            Self::ProofExpired => write!(f, "DPoP proof is too old"),
            Self::ProofNotYetValid => write!(f, "DPoP proof is issued in the future"),
            Self::Replayed => write!(f, "DPoP proof identifier was already used"),
            Self::ReplayCacheRequired => {
                write!(f, "catdpop requires jti replay protection")
            }
            Self::ClaimLabelConflict => {
                write!(f, "actx and ath claim labels must differ")
            }
            Self::ReservedClaimLabel(name) => {
                write!(
                    f,
                    "{name} claim label must not be a registered CWT claim key"
                )
            }
            Self::Cat(error) => write!(f, "CAT error: {error}"),
            Self::MissingRequestTarget(name) => {
                write!(f, "the request is missing the target: {name}")
            }
            Self::UnexpectedRequestTarget(name) => {
                write!(f, "the request has an unexpected target: {name}")
            }
            Self::MissingActxTarget(name) => write!(f, "actx is missing the target: {name}"),
            Self::UnexpectedActxTarget(name) => {
                write!(f, "actx has an unexpected target: {name}")
            }
            Self::InvalidFreshnessWindow => {
                write!(f, "freshness window must be finite and non-negative")
            }
            Self::InvalidReplayWindow => {
                write!(f, "maximum replay window must be finite and positive")
            }
            Self::ReplayWindowTooLarge => {
                write!(f, "freshness window exceeds the maximum replay window")
            }
            Self::ClaimLabelMismatch => {
                write!(f, "proof claims do not match the configured claim labels")
            }
            Self::CatReplayWithConfirmation => {
                write!(
                    f,
                    "catreplay is not supported together with a confirmation key"
                )
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

impl From<CoseError> for DpopError {
    fn from(error: CoseError) -> Self {
        Self::Cose(error)
    }
}

impl From<CborError> for DpopError {
    fn from(error: CborError) -> Self {
        Self::Cose(CoseError::Cbor(error))
    }
}

impl From<CatError> for DpopError {
    fn from(error: CatError) -> Self {
        Self::Cat(error)
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
    let number = member
        .as_number_str()
        .map_err(json_error)?
        .parse::<f64>()
        .map_err(|_| DpopError::UnexpectedType(name))?;
    if !number.is_finite() {
        return Err(DpopError::NonFiniteNumber(name));
    }
    Ok(number)
}
