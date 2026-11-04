//! MOQT の認可トークン (C4M) のコーデック
//!
//! draft-ietf-moq-c4m-02 (Authorization scheme for MOQT using Common Access
//! Tokens) の `moqt` クレーム (map 形式) と draft-01 の `moqt-reval` クレーム、
//! その基盤となる CBOR (RFC 8949) / COSE (RFC 9052) / CAT (CTA-5007-B) の
//! トークンを扱う。
//!
//! - [`cbor`](crate::c4m::cbor): CBOR のコーデック
//! - [`cose`](crate::c4m::cose): COSE の構造とアルゴリズム定義
//! - [`crypto`](crate::c4m::crypto): 署名 / 検証を抽象化する trait と鍵表現
//! - [`cat`](crate::c4m::cat): CAT のクレームとトークンの発行 / 検証
//! - [`jwk`](crate::c4m::jwk): JWK (RFC 7517) と JWK サムプリント (RFC 7638)
//! - [`jwt`](crate::c4m::jwt): JWS compact (RFC 7515) の JWT
//! - [`dpop`](crate::c4m::dpop): DPoP proof の検証と発行 (draft-nandakumar-moq-generic-dpop-proof)
//!
//! 本モジュールは draft-ietf-moq-c4m-02 へ段階的に追随している。draft は将来改訂
//! される可能性がある。
//!
//! このモジュールは I/O も時計も持たない。時刻は検証 API の引数で渡す。

pub mod cat;
pub mod cbor;
pub mod cose;
pub mod crypto;
pub mod dpop;
pub mod jwk;
pub mod jwt;

pub(crate) mod base64url;
pub(crate) mod json;

use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use cbor::{CborError, Value};

/// `moqt` クレームの claim key
///
/// draft-ietf-moq-c4m-02 §8 Table 2 は Key を `TBD_MOQT (1+2)` とし、IANA の割当は
/// 未確定である。そのため付録 A 冒頭のとおり §A.5 のベクタは Private Use の範囲
/// (RFC 8392 §9.1.1) の -65537 をプレースホルダとして使う。本ライブラリもこの
/// 暫定値を使う。
///
/// 旧 draft-01 のベクタが使う 327 は、この label の alias として解釈しない。327 を
/// キーに持つ claim は未知の claim として [`cat::CatClaims::raw`] に保持し、型付き
/// フィールドへは写さない。この暫定値は IANA の割当が確定した時点で置き換える。
pub const CLAIM_MOQT: i64 = -65537;

/// `moqt-reval` クレームの claim key
///
/// draft-ietf-moq-c4m-02 §8 Table 2 は Key を `TBD_MOQT (1+2)` とし、§2.2 は
/// `TBD_MOQT_REVAL` とする。いずれにせよ IANA の割当は未確定である。そのため
/// 付録 A 冒頭のとおり §A.5 のベクタは Private Use の範囲 (RFC 8392 §9.1.1) の
/// -65538 をプレースホルダとして使い、本ライブラリもこの暫定値を使う。
///
/// 旧 draft-01 のベクタが使う 328 は、この label の alias として解釈しない。328 を
/// キーに持つ claim は未知の claim として [`cat::CatClaims::raw`] に保持し、型付き
/// フィールドへは写さない。この暫定値は IANA の割当が確定した時点で置き換える。
pub const CLAIM_MOQT_REVAL: i64 = -65538;

/// `bin-match` の prefix マッチを表す match-type (draft-ietf-moq-c4m-01 §2.1)
pub const MATCH_TYPE_PREFIX: i64 = 1;

/// `bin-match` の suffix マッチを表す match-type (draft-ietf-moq-c4m-01 §2.1)
pub const MATCH_TYPE_SUFFIX: i64 = 2;

/// MOQT のアクション (draft-ietf-moq-c4m-02 §2.1 Table 1)
///
/// `moqt` クレームの action は整数であり、SETUP=1 から SUBSCRIBE_TRACKS=9 までの
/// 9 種類が定義されている。旧 draft の CLIENT_SETUP=0 / SERVER_SETUP=1 という 2 つの
/// action は draft-02 で SETUP=1 に統合された。
///
/// draft-02 §4.2 の CWT 形式の DPoP proof の actx も、整数の action コードとして
/// 同じ値を使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MoqtAction {
    /// SETUP (1)
    Setup,
    /// PUBLISH_NAMESPACE (2)
    PublishNamespace,
    /// SUBSCRIBE_NAMESPACE (3)
    SubscribeNamespace,
    /// SUBSCRIBE (4)
    Subscribe,
    /// REQUEST_UPDATE (5)
    RequestUpdate,
    /// PUBLISH (6)
    Publish,
    /// FETCH (7)
    Fetch,
    /// TRACK_STATUS (8)
    TrackStatus,
    /// SUBSCRIBE_TRACKS (9)
    SubscribeTracks,
}

impl MoqtAction {
    /// すべてのアクション (draft-02 §2.1 Table 1 の key 1 〜 9 の順)
    ///
    /// 先頭の index は 0 だが、各要素の key は 1 から始まる。index を key として
    /// 使わず、key から action を引く場合は [`MoqtAction::from_key`] を使う。
    pub const ALL: [Self; 9] = [
        Self::Setup,
        Self::PublishNamespace,
        Self::SubscribeNamespace,
        Self::Subscribe,
        Self::RequestUpdate,
        Self::Publish,
        Self::Fetch,
        Self::TrackStatus,
        Self::SubscribeTracks,
    ];

    /// `moqt` クレームのアクション整数を返す (draft-02 §2.1 Table 1)
    pub const fn key(self) -> i64 {
        match self {
            Self::Setup => 1,
            Self::PublishNamespace => 2,
            Self::SubscribeNamespace => 3,
            Self::Subscribe => 4,
            Self::RequestUpdate => 5,
            Self::Publish => 6,
            Self::Fetch => 7,
            Self::TrackStatus => 8,
            Self::SubscribeTracks => 9,
        }
    }

    /// アクション整数からアクションを返す (draft-02 §2.1 Table 1)
    ///
    /// Table 1 に無い整数は `None` を返す。旧 draft の CLIENT_SETUP=0 は既知の
    /// アクションの alias にせず、未知の整数と同じく `None` を返す。
    pub const fn from_key(key: i64) -> Option<Self> {
        match key {
            1 => Some(Self::Setup),
            2 => Some(Self::PublishNamespace),
            3 => Some(Self::SubscribeNamespace),
            4 => Some(Self::Subscribe),
            5 => Some(Self::RequestUpdate),
            6 => Some(Self::Publish),
            7 => Some(Self::Fetch),
            8 => Some(Self::TrackStatus),
            9 => Some(Self::SubscribeTracks),
            _ => None,
        }
    }

    /// MOQT のメッセージ名を返す (ログとデバッグ用)
    pub const fn name(self) -> &'static str {
        match self {
            Self::Setup => "SETUP",
            Self::PublishNamespace => "PUBLISH_NAMESPACE",
            Self::SubscribeNamespace => "SUBSCRIBE_NAMESPACE",
            Self::Subscribe => "SUBSCRIBE",
            Self::RequestUpdate => "REQUEST_UPDATE",
            Self::Publish => "PUBLISH",
            Self::Fetch => "FETCH",
            Self::TrackStatus => "TRACK_STATUS",
            Self::SubscribeTracks => "SUBSCRIBE_TRACKS",
        }
    }

    /// 旧 JWT 形式の DPoP proof の Authorization Context で使うアクション識別子を返す
    ///
    /// draft-02 §4.2 の DPoP proof は actx に整数の action コード (§2.1 Table 1) を
    /// 入れるため、この文字列の対応表は使わない。ここでは旧形式の JWT proof との
    /// 互換のために文字列を返す。
    ///
    /// 旧形式に対応する文字列が無い SUBSCRIBE_TRACKS は `None` を返す。利用側は
    /// `None` を「一致しない」として拒否すること。
    pub const fn authorization_context(self) -> Option<&'static str> {
        match self {
            Self::Setup => Some("SETUP"),
            Self::PublishNamespace => Some("PUB_NS"),
            Self::SubscribeNamespace => Some("SUB_NS"),
            Self::Subscribe => Some("SUBSCRIBE"),
            Self::RequestUpdate => Some("REQ_UPDATE"),
            Self::Publish => Some("PUBLISH"),
            Self::Fetch => Some("FETCH"),
            Self::TrackStatus => Some("TRK_STATUS"),
            Self::SubscribeTracks => None,
        }
    }

    /// 旧 JWT 形式の Authorization Context のアクション識別子と一致するかどうかを返す
    ///
    /// [`MoqtAction::authorization_context`] が `None` を返すアクション
    /// (SUBSCRIBE_TRACKS) は常に `false` を返す。
    pub fn matches_authorization_context(self, value: &str) -> bool {
        self.authorization_context() == Some(value)
    }
}

/// `bin-match` (draft-ietf-moq-c4m-01 §2.1)
///
/// バイト文字列は完全一致、`[match-type, match-value]` は prefix / suffix マッチを
/// 表す。マッチはバイト単位で行い、正規化はしない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Match {
    /// 完全一致 (`bstr`)
    Exact(Vec<u8>),
    /// 前方一致 (`[1, bstr]`)
    Prefix(Vec<u8>),
    /// 後方一致 (`[2, bstr]`)
    Suffix(Vec<u8>),
}

impl Match {
    /// 値がマッチするかどうかを返す
    pub fn matches(&self, value: &[u8]) -> bool {
        match self {
            Self::Exact(pattern) => value == pattern.as_slice(),
            Self::Prefix(pattern) => value.starts_with(pattern),
            Self::Suffix(pattern) => value.ends_with(pattern),
        }
    }

    /// `bin-match` をデコードする
    pub fn decode(value: &Value) -> Result<Self, C4mError> {
        match value {
            Value::ByteString(pattern) => Ok(Self::Exact(pattern.clone())),
            Value::Array(items) => {
                if items.len() != 2 {
                    return Err(C4mError::InvalidMatchArrayLength(items.len()));
                }
                let match_type = items[0]
                    .as_int()
                    .ok_or(C4mError::UnexpectedType("match-type"))?;
                let pattern = items[1]
                    .as_bytes()
                    .ok_or(C4mError::UnexpectedType("match-value"))?
                    .to_vec();
                match match_type {
                    MATCH_TYPE_PREFIX => Ok(Self::Prefix(pattern)),
                    MATCH_TYPE_SUFFIX => Ok(Self::Suffix(pattern)),
                    other => Err(C4mError::InvalidMatchType(other)),
                }
            }
            _ => Err(C4mError::UnexpectedType("bin-match")),
        }
    }

    /// `bin-match` をエンコードする
    pub fn encode(&self) -> Value {
        match self {
            Self::Exact(pattern) => Value::ByteString(pattern.clone()),
            Self::Prefix(pattern) => Value::Array(vec![
                Value::integer(MATCH_TYPE_PREFIX),
                Value::ByteString(pattern.clone()),
            ]),
            Self::Suffix(pattern) => Value::Array(vec![
                Value::integer(MATCH_TYPE_SUFFIX),
                Value::ByteString(pattern.clone()),
            ]),
        }
    }
}

/// `moqt-ns-match` (draft-ietf-moq-c4m-01 §2.1)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamespaceMatch {
    /// `bin-match` による名前空間フィールドのマッチ
    Match(Match),
    /// `nil`。名前空間の末尾にだけ現れ、それ以上のフィールドが無いことを要求する
    End,
}

/// `moqt-scope` (draft-ietf-moq-c4m-01 §2.1)
///
/// アクションの配列と、省略可能な名前空間マッチの配列 / トラック名マッチを持つ。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MoqtScope {
    /// 認可するアクションの整数値 (draft-02 §2.1 Table 1)
    ///
    /// CDDL の `moqt-action = int` に合わせて整数のまま保持し、i64 で表せる未知の
    /// 整数もデコードで失わない。認可時は [`MoqtAction::key`] とだけ照合するため、
    /// 未知の整数が既知 action の許可として働くことはない。i64 で表せない整数は
    /// 解釈できない action として拒否する。
    pub actions: Vec<i64>,
    /// 名前空間フィールドのマッチ。空の場合はすべての名前空間にマッチする
    pub namespace: Vec<NamespaceMatch>,
    /// トラック名のマッチ。`None` の場合はすべてのトラック名にマッチする
    pub track: Option<Match>,
}

impl MoqtScope {
    /// アクションを指定してスコープを作る
    pub fn new(actions: impl IntoIterator<Item = MoqtAction>) -> Self {
        Self {
            actions: actions.into_iter().map(MoqtAction::key).collect(),
            namespace: Vec::new(),
            track: None,
        }
    }

    /// アクションを追加する
    pub fn action(mut self, action: MoqtAction) -> Self {
        self.actions.push(action.key());
        self
    }

    /// 名前空間フィールドのマッチを追加する
    pub fn namespace_match(mut self, namespace_match: NamespaceMatch) -> Self {
        self.namespace.push(namespace_match);
        self
    }

    /// 名前空間の末尾を固定する `nil` を追加する
    pub fn namespace_end(self) -> Self {
        self.namespace_match(NamespaceMatch::End)
    }

    /// トラック名のマッチを設定する
    pub fn track(mut self, track: Match) -> Self {
        self.track = Some(track);
        self
    }

    /// アクションと Full Track Name がこのスコープで認可されるかどうかを返す
    ///
    /// `namespace` は Track Namespace のフィールド列、`track_name` は Track Name を
    /// 表す。マッチはバイト単位で行う (§2.1)。
    ///
    /// `namespace` が空で `track` だけを持つスコープは CDDL の位置指定で表現できない
    /// ([`MoqtScope::encode`] は拒否する)。デコードで得たスコープには現れないが、
    /// 直接構築した場合は任意の名前空間に対してトラックマッチだけを適用する。
    pub fn allows(&self, action: MoqtAction, namespace: &[&[u8]], track_name: &[u8]) -> bool {
        if !self.actions.contains(&action.key()) {
            return false;
        }
        // nil は末尾にだけ置ける (末尾以外にあれば不正なスコープとして拒否する)
        if self.namespace[..self.namespace.len().saturating_sub(1)]
            .iter()
            .any(|namespace_match| matches!(namespace_match, NamespaceMatch::End))
        {
            return false;
        }
        let mut index = 0;
        let mut requires_end = false;
        for namespace_match in &self.namespace {
            match namespace_match {
                // nil は名前空間の末尾に一致することを要求する
                NamespaceMatch::End => requires_end = true,
                NamespaceMatch::Match(matcher) => {
                    let Some(field) = namespace.get(index) else {
                        return false;
                    };
                    if !matcher.matches(field) {
                        return false;
                    }
                    index += 1;
                }
            }
        }
        if requires_end && index != namespace.len() {
            return false;
        }
        // 末尾に nil が無い場合、残りの名前空間フィールドは任意 (§2.1)
        match &self.track {
            Some(matcher) => matcher.matches(track_name),
            None => true,
        }
    }

    /// `moqt-scope` をデコードする
    pub fn decode(value: &Value) -> Result<Self, C4mError> {
        let items = value
            .as_array()
            .ok_or(C4mError::UnexpectedType("moqt-scope"))?;
        if items.is_empty() || items.len() > 3 {
            return Err(C4mError::InvalidScopeLength(items.len()));
        }
        let action_values = items[0]
            .as_array()
            .ok_or(C4mError::UnexpectedType("moqt-actions"))?;
        if action_values.is_empty() {
            return Err(C4mError::EmptyActions);
        }
        let mut actions = Vec::new();
        for action in action_values {
            actions.push(
                action
                    .as_int()
                    .ok_or(C4mError::UnexpectedType("moqt-action"))?,
            );
        }
        let mut namespace = Vec::new();
        if let Some(matches) = items.get(1) {
            let matches = matches
                .as_array()
                .ok_or(C4mError::UnexpectedType("moqt-ns-match"))?;
            if matches.is_empty() {
                return Err(C4mError::EmptyNamespaceMatch);
            }
            for (position, item) in matches.iter().enumerate() {
                if matches!(item, Value::Null) {
                    if position != matches.len() - 1 {
                        return Err(C4mError::NilNotLast);
                    }
                    namespace.push(NamespaceMatch::End);
                } else {
                    namespace.push(NamespaceMatch::Match(Match::decode(item)?));
                }
            }
        }
        let track = match items.get(2) {
            Some(value) => Some(Match::decode(value)?),
            None => None,
        };
        Ok(Self {
            actions,
            namespace,
            track,
        })
    }

    /// `moqt-scope` をエンコードする
    ///
    /// `actions` が空の場合、`nil` が末尾以外にある場合、および名前空間マッチ無しで
    /// トラックマッチだけを持つ場合はエラーを返す。後者は CDDL の位置指定で表現できず、
    /// 黙ってトラック制限を落とすと認可が広がるためである。
    pub fn encode(&self) -> Result<Value, C4mError> {
        if self.actions.is_empty() {
            return Err(C4mError::EmptyActions);
        }
        let mut items = vec![Value::Array(
            self.actions
                .iter()
                .map(|action| Value::integer(*action))
                .collect(),
        )];
        if self.namespace.is_empty() {
            if self.track.is_some() {
                return Err(C4mError::TrackWithoutNamespace);
            }
            return Ok(Value::Array(items));
        }
        for (position, namespace_match) in self.namespace.iter().enumerate() {
            if matches!(namespace_match, NamespaceMatch::End)
                && position != self.namespace.len() - 1
            {
                return Err(C4mError::NilNotLast);
            }
        }
        items.push(Value::Array(
            self.namespace
                .iter()
                .map(|namespace_match| match namespace_match {
                    NamespaceMatch::Match(matcher) => matcher.encode(),
                    NamespaceMatch::End => Value::Null,
                })
                .collect(),
        ));
        if let Some(track) = &self.track {
            items.push(track.encode());
        }
        Ok(Value::Array(items))
    }
}

/// `moqt` クレームの map の scopes キー (draft-ietf-moq-c4m-02 §2.1)
///
/// キー 0 以外は将来の改訂のために予約されている (§2.1.1)。
pub const MOQT_SCOPES_KEY: i64 = 0;

/// `moqt` クレーム (draft-ietf-moq-c4m-02 §2.1)
///
/// claim value は CBOR map で、キー 0 にアクションスコープの配列を持つ。いずれかの
/// スコープが認可すれば許可となり、評価順は問わない (§2.1.3)。
///
/// キー 0 以外のキーは将来の改訂のために予約されている (§2.1.1)。拒否の詳細は
/// [`MoqtClaim::decode`] を参照。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MoqtClaim {
    /// 認可スコープ
    pub scopes: Vec<MoqtScope>,
}

impl MoqtClaim {
    /// 空のクレームを作る
    pub fn new() -> Self {
        Self::default()
    }

    /// スコープを追加する
    pub fn scope(mut self, scope: MoqtScope) -> Self {
        self.scopes.push(scope);
        self
    }

    /// アクションと Full Track Name が認可されるかどうかを返す
    ///
    /// いずれかのスコープが認可すれば `true` を返す。これは claim 内の複数スコープの
    /// OR であり、Composite の `or` クレームと他のクレームを連動させた claim set ごとの
    /// 評価とは異なる (§2.1.3)。
    pub fn authorize(&self, action: MoqtAction, namespace: &[&[u8]], track_name: &[u8]) -> bool {
        self.scopes
            .iter()
            .any(|scope| scope.allows(action, namespace, track_name))
    }

    /// `moqt` クレームをデコードする
    ///
    /// CBOR map だけを受理する。キー 0 が無い場合、キー 0 の値が配列でない場合、
    /// scopes が空の場合、キー 0 が重複する場合、i64 で表せないものを含め解釈できない
    /// キーやキー 0 以外のキーを含む場合はエラーを返す。キー 0 以外のキーを無視すると
    /// 認可範囲が広がるため、fail-closed で拒否する (§2.1.1)。旧 draft-01 の配列だけの
    /// 値も map ではないため、型不一致のエラーになる。各スコープのデコードエラー
    /// ([`MoqtScope::decode`] が返す空アクション・長さ不正・型不正など) もそのまま返す。
    ///
    /// キー 0 の重複は wire 由来の `Value` では CBOR デコーダが先に拒否するため、
    /// [`C4mError::DuplicateScopesKey`] は公開 API に直接 `Value` を渡した場合の
    /// 二重防御である。
    pub fn decode(value: &Value) -> Result<Self, C4mError> {
        let entries = value
            .as_map()
            .ok_or(C4mError::UnexpectedType("moqt claim"))?;
        let mut scopes_value = None;
        for (key, entry) in entries {
            let key = key
                .as_int()
                .ok_or(C4mError::UnexpectedType("moqt claim key"))?;
            if key != MOQT_SCOPES_KEY {
                return Err(C4mError::UnknownMoqtClaimKey(key));
            }
            // キー 0 の重複は map のキーの一意性 (RFC 8949 §5.6) に反する。wire 由来の
            // Value では cbor::decode が DuplicateMapKey で先に拒否するため、この分岐は
            // 公開 API に直接 Value を渡した場合の二重防御である
            if scopes_value.replace(entry).is_some() {
                return Err(C4mError::DuplicateScopesKey);
            }
        }
        let scopes_value = scopes_value.ok_or(C4mError::MissingScopesKey)?;
        let items = scopes_value
            .as_array()
            .ok_or(C4mError::UnexpectedType("moqt-scopes"))?;
        if items.is_empty() {
            return Err(C4mError::EmptyScopes);
        }
        let mut scopes = Vec::new();
        for item in items {
            scopes.push(MoqtScope::decode(item)?);
        }
        Ok(Self { scopes })
    }

    /// `moqt` クレームをエンコードする
    ///
    /// キー 0 に scopes 配列を持つ CBOR map を返す (§2.1)。scopes が空の場合は
    /// エラーを返す。キーは 0 のみである。
    pub fn encode(&self) -> Result<Value, C4mError> {
        if self.scopes.is_empty() {
            return Err(C4mError::EmptyScopes);
        }
        let mut items = Vec::new();
        for scope in &self.scopes {
            items.push(scope.encode()?);
        }
        Ok(Value::Map(vec![(
            Value::integer(MOQT_SCOPES_KEY),
            Value::Array(items),
        )]))
    }
}

/// `catdpop` クレーム (CTA-5007-B / draft-ietf-moq-c4m-01 §3.1.1)
///
/// DPoP proof の処理設定を持つ。label 0 が受理ウィンドウ (秒)、label 1 が jti に
/// よるリプレイ保護を行うかどうかを表す。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CatDpop {
    /// label 0: DPoP proof を受理する時間ウィンドウ (秒)
    pub window_seconds: Option<f64>,
    /// label 1: jti によるリプレイ保護を行うかどうか
    pub honor_jti: Option<bool>,
    /// 解釈しなかった設定
    pub raw: Vec<(i64, Value)>,
}

impl CatDpop {
    /// ウィンドウと jti の扱いを指定して作る
    pub fn new(window_seconds: f64, honor_jti: bool) -> Self {
        Self {
            window_seconds: Some(window_seconds),
            honor_jti: Some(honor_jti),
            raw: Vec::new(),
        }
    }

    /// `catdpop` をデコードする
    ///
    /// ウィンドウは整数と浮動小数点の両方、jti の扱いは真偽値と整数 (0 / 1) の
    /// 両方を受ける (付録 A.4 のベクタは整数を使う)。
    pub fn decode(value: &Value) -> Result<Self, C4mError> {
        let entries = value.as_map().ok_or(C4mError::UnexpectedType("catdpop"))?;
        let mut catdpop = Self::default();
        for (key, entry) in entries {
            match key.as_int() {
                Some(0) => {
                    let window = entry
                        .as_number()
                        .ok_or(C4mError::UnexpectedType("catdpop window"))?;
                    if !window.is_finite() {
                        return Err(C4mError::NonFiniteNumber("catdpop window"));
                    }
                    catdpop.window_seconds = Some(window);
                }
                Some(1) => {
                    catdpop.honor_jti = match entry {
                        Value::Bool(value) => Some(*value),
                        value => value.as_int().map(|value| value != 0),
                    };
                    if catdpop.honor_jti.is_none() {
                        return Err(C4mError::UnexpectedType("catdpop honor jti"));
                    }
                }
                Some(label) => catdpop.raw.push((label, entry.clone())),
                None => return Err(C4mError::UnexpectedType("catdpop label")),
            }
        }
        Ok(catdpop)
    }

    /// `catdpop` をエンコードする
    ///
    /// label 1 はドラフトの例に合わせて整数 (1 / 0) で書く。ウィンドウが有限でない
    /// 場合はエラーを返す (デコード側が拒否する値を持つトークンを発行しないため)。
    pub fn encode(&self) -> Result<Value, C4mError> {
        let mut entries = Vec::new();
        if let Some(window) = self.window_seconds {
            if !window.is_finite() {
                return Err(C4mError::NonFiniteNumber("catdpop window"));
            }
            entries.push((Value::integer(0), number_value(window)));
        }
        if let Some(honor_jti) = self.honor_jti {
            entries.push((
                Value::integer(1),
                Value::integer(if honor_jti { 1 } else { 0 }),
            ));
        }
        for (key, value) in &self.raw {
            entries.push((Value::integer(*key), value.clone()));
        }
        Ok(Value::Map(entries))
    }

    /// ウィンドウを返す (未指定の場合は `default` を返す)
    pub fn window_seconds_or(&self, default: f64) -> f64 {
        self.window_seconds.unwrap_or(default)
    }

    /// jti によるリプレイ保護を行うかどうかを返す
    ///
    /// 未指定の場合は `false` を返す。
    pub fn honors_jti(&self) -> bool {
        self.honor_jti.unwrap_or(false)
    }
}

/// 数値クレームを CBOR のデータ項目へエンコードする
///
/// 整数値は整数として、非整数値は浮動小数点数としてエンコードする。CBOR の決定論的
/// エンコードは値が同じでも整数と浮動小数点を区別するため、入力の表現をなるべく
/// 保つ。
pub(crate) fn number_value(number: f64) -> Value {
    // i64::MAX as f64 は丸めで 2^63 になるため、上限は半開区間で比較する
    const I64_MIN_EXACT: f64 = i64::MIN as f64;
    const I64_MAX_EXCLUSIVE: f64 = 9_223_372_036_854_775_808.0;
    if number.is_finite() && (I64_MIN_EXACT..I64_MAX_EXCLUSIVE).contains(&number) {
        let integer = number as i64;
        if integer as f64 == number {
            return Value::integer(integer);
        }
    }
    Value::Float(number)
}

/// C4M のクレームのエンコード / デコードエラー
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum C4mError {
    /// CBOR のデコードに失敗した
    Cbor(CborError),
    /// 期待した型と異なる
    UnexpectedType(&'static str),
    /// アクションの配列が空である
    EmptyActions,
    /// スコープの配列が空である
    EmptyScopes,
    /// `moqt` クレームの map にキー 0 (scopes) が無い
    MissingScopesKey,
    /// `moqt` クレームの map にキー 0 (scopes) が重複している
    DuplicateScopesKey,
    /// `moqt` クレームの map に認識しないキーがある (draft-ietf-moq-c4m-02 §2.1.1)
    UnknownMoqtClaimKey(i64),
    /// マッチの配列の要素数が 2 でない
    InvalidMatchArrayLength(usize),
    /// match-type が 1 (prefix) / 2 (suffix) のいずれでもない
    InvalidMatchType(i64),
    /// `nil` が名前空間マッチの末尾以外にある
    NilNotLast,
    /// 名前空間マッチの配列が空である
    EmptyNamespaceMatch,
    /// スコープの配列の要素数が 1 〜 3 の範囲外である
    InvalidScopeLength(usize),
    /// 名前空間マッチ無しでトラックマッチだけを持つスコープは表現できない
    TrackWithoutNamespace,
    /// 数値が有限でない (NaN / 無限大)
    NonFiniteNumber(&'static str),
}

impl fmt::Display for C4mError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cbor(error) => write!(f, "CBOR error: {error}"),
            Self::UnexpectedType(expected) => write!(f, "expected {expected}"),
            Self::EmptyActions => write!(f, "moqt-scope has no actions"),
            Self::EmptyScopes => write!(f, "moqt claim has no scopes"),
            Self::MissingScopesKey => write!(f, "moqt claim has no scopes key (0)"),
            Self::DuplicateScopesKey => write!(f, "moqt claim has duplicate scopes key (0)"),
            Self::UnknownMoqtClaimKey(key) => write!(f, "unknown moqt claim key: {key}"),
            Self::InvalidMatchArrayLength(length) => {
                write!(f, "bin-match array must have 2 elements, got {length}")
            }
            Self::InvalidMatchType(match_type) => write!(f, "invalid match type: {match_type}"),
            Self::NilNotLast => write!(f, "nil must be the last namespace match"),
            Self::EmptyNamespaceMatch => write!(f, "moqt-ns-match array is empty"),
            Self::InvalidScopeLength(length) => {
                write!(f, "moqt-scope must have 1 to 3 elements, got {length}")
            }
            Self::TrackWithoutNamespace => {
                write!(f, "moqt-scope has a track match without a namespace match")
            }
            Self::NonFiniteNumber(name) => write!(f, "{name} must be finite"),
        }
    }
}

impl core::error::Error for C4mError {}

impl From<CborError> for C4mError {
    fn from(error: CborError) -> Self {
        Self::Cbor(error)
    }
}
