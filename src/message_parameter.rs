//! Message Parameters (draft-ietf-moq-transport-21 §9.20 (Control Message Parameters))
//!
//! Message Parameters はカウントプレフィックス付きで、
//! 型ごとに固有のエンコーディングを持つ。
//! 未知のパラメータ型は PROTOCOL_VIOLATION を返す (スキップ不可)。
//!
//! ワイヤーフォーマット:
//!   Number of Parameters (vi64)
//!   For each:
//!     Type Delta (vi64)
//!     Value (型固有エンコーディング)
//!
//! Value エンコーディング:
//!   - uint8: 1 バイト固定
//!   - varint: vi64
//!   - Location: vi64 (Group) + vi64 (Object)
//!   - Length-prefixed: vi64 (length) + bytes
use crate::{
    error::MessageError,
    message::common::{Location, TrackNamespace},
    varint,
};
use alloc::vec::Vec;
use hashbrown::HashSet;

// ─── 既知パラメータ型定数 ──────────────────────────────────────

/// OBJECT_DELIVERY_TIMEOUT (varint, draft-ietf-moq-transport-21 §9.20.5 (OBJECT_DELIVERY_TIMEOUT Parameter))
pub const PARAM_OBJECT_DELIVERY_TIMEOUT: u64 = 0x02;
/// AUTHORIZATION_TOKEN (length-prefixed)
pub const PARAM_AUTHORIZATION_TOKEN: u64 = 0x03;
/// RENDEZVOUS_TIMEOUT (varint)
pub const PARAM_RENDEZVOUS_TIMEOUT: u64 = 0x04;
/// SUBGROUP_DELIVERY_TIMEOUT (varint, draft-ietf-moq-transport-21 §9.20.4 (SUBGROUP_DELIVERY_TIMEOUT Parameter))
pub const PARAM_SUBGROUP_DELIVERY_TIMEOUT: u64 = 0x06;
/// EXPIRES (varint)
pub const PARAM_EXPIRES: u64 = 0x08;
/// LARGEST_OBJECT (Location)
pub const PARAM_LARGEST_OBJECT: u64 = 0x09;
/// FORWARD (uint8)
pub const PARAM_FORWARD: u64 = 0x10;
/// SUBSCRIBER_PRIORITY (uint8)
pub const PARAM_SUBSCRIBER_PRIORITY: u64 = 0x20;
/// LOCATION_FILTER (length-prefixed)
pub const PARAM_LOCATION_FILTER: u64 = 0x21;
/// GROUP_ORDER (uint8)
pub const PARAM_GROUP_ORDER: u64 = 0x22;
/// FILL_TIMEOUT (varint, draft-ietf-moq-transport-21 §9.20.6 (FILL TIMEOUT Parameter))
pub const PARAM_FILL_TIMEOUT: u64 = 0x0A;
/// FILL_PARAMETERS (length-prefixed, draft-ietf-moq-transport-21 §9.20.16 (FILL PARAMETERS Parameter))
pub const PARAM_FILL_PARAMETERS: u64 = 0x23;
/// NEW_GROUP_REQUEST (varint)
pub const PARAM_NEW_GROUP_REQUEST: u64 = 0x32;
/// TRACK_NAMESPACE_PREFIX (Track Namespace 形式, draft-ietf-moq-transport-21 §9.20.21 (TRACK_NAMESPACE_PREFIX Parameter))
pub const PARAM_TRACK_NAMESPACE_PREFIX: u64 = 0x34;
/// INCLUDE_PROPERTIES (uint8, draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter))
pub const PARAM_INCLUDE_PROPERTIES: u64 = 0x35;

// ─── Range Filter パラメータ (draft-ietf-moq-transport-21 §3.3.2 (Range Filters)) ───

/// SUBGROUP_FILTER (length-prefixed, draft-ietf-moq-transport-21 §3.3.2)
pub const PARAM_SUBGROUP_FILTER: u64 = 0x25;
/// OBJECTID_FILTER (length-prefixed, draft-ietf-moq-transport-21 §3.3.2)
pub const PARAM_OBJECTID_FILTER: u64 = 0x26;
/// PRIORITY_FILTER (length-prefixed, draft-ietf-moq-transport-21 §3.3.2)
pub const PARAM_PRIORITY_FILTER: u64 = 0x27;
/// OBJECT_PROPERTY_FILTER (length-prefixed, draft-ietf-moq-transport-21 §3.3.2)
pub const PARAM_OBJECT_PROPERTY_FILTER: u64 = 0x28;
/// TRACK_PROPERTY_FILTER (length-prefixed, draft-ietf-moq-transport-21 §3.3.2)
pub const PARAM_TRACK_PROPERTY_FILTER: u64 = 0x29;

// ─── FILL_PARAMETERS 内側スコープ ──────────────────────────────

/// FILL_PARAMETERS 内に出現可能なパラメータ型 (draft-ietf-moq-transport-21 §9.20.16 (FILL PARAMETERS Parameter) Table 6)
///
/// 外側メッセージとは別のパラメータスコープであり、同一型が外側と内側の両方に
/// 現れても重複とみなさない。Table 6 外の受信時は PROTOCOL_VIOLATION で
/// セッションを閉じなければならない (MUST)。
/// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
pub(crate) const FILL_PARAMETERS_ALLOWED_PARAMS: &[u64] = &[
    PARAM_FILL_TIMEOUT,
    PARAM_SUBSCRIBER_PRIORITY,
    PARAM_LOCATION_FILTER,
    PARAM_GROUP_ORDER,
    PARAM_SUBGROUP_FILTER,
    PARAM_OBJECTID_FILTER,
    PARAM_PRIORITY_FILTER,
    PARAM_OBJECT_PROPERTY_FILTER,
];

// ─── Value エンコーディング種別 ────────────────────────────────

/// パラメータ型に応じた値エンコーディングの種別
#[derive(Debug, Clone, Copy, PartialEq)]
enum ValueEncoding {
    /// 1 バイト固定
    Uint8,
    /// vi64
    VarInt,
    /// vi64 (Group) + vi64 (Object)
    Location,
    /// vi64 (length) + bytes
    LengthPrefixed,
    /// Track Namespace 形式: vi64 (count) + per-field (vi64 length + bytes)
    TrackNamespacePrefix,
}

/// パラメータ型からエンコーディング種別を返す
///
/// 未知の型は PROTOCOL_VIOLATION を返す (draft-ietf-moq-transport-21 §9.20 (Control Message Parameters))
fn value_encoding(param_type: u64) -> Result<ValueEncoding, MessageError> {
    match param_type {
        PARAM_OBJECT_DELIVERY_TIMEOUT => Ok(ValueEncoding::VarInt),
        PARAM_AUTHORIZATION_TOKEN => Ok(ValueEncoding::LengthPrefixed),
        PARAM_RENDEZVOUS_TIMEOUT => Ok(ValueEncoding::VarInt),
        PARAM_SUBGROUP_DELIVERY_TIMEOUT => Ok(ValueEncoding::VarInt),
        PARAM_EXPIRES => Ok(ValueEncoding::VarInt),
        PARAM_LARGEST_OBJECT => Ok(ValueEncoding::Location),
        PARAM_FORWARD => Ok(ValueEncoding::Uint8),
        PARAM_SUBSCRIBER_PRIORITY => Ok(ValueEncoding::Uint8),
        PARAM_LOCATION_FILTER => Ok(ValueEncoding::LengthPrefixed),
        PARAM_GROUP_ORDER => Ok(ValueEncoding::Uint8),
        PARAM_FILL_TIMEOUT => Ok(ValueEncoding::VarInt),
        PARAM_FILL_PARAMETERS => Ok(ValueEncoding::LengthPrefixed),
        PARAM_NEW_GROUP_REQUEST => Ok(ValueEncoding::VarInt),
        PARAM_TRACK_NAMESPACE_PREFIX => Ok(ValueEncoding::TrackNamespacePrefix),
        PARAM_INCLUDE_PROPERTIES => Ok(ValueEncoding::Uint8),
        PARAM_SUBGROUP_FILTER => Ok(ValueEncoding::LengthPrefixed),
        PARAM_OBJECTID_FILTER => Ok(ValueEncoding::LengthPrefixed),
        PARAM_PRIORITY_FILTER => Ok(ValueEncoding::LengthPrefixed),
        PARAM_OBJECT_PROPERTY_FILTER => Ok(ValueEncoding::LengthPrefixed),
        PARAM_TRACK_PROPERTY_FILTER => Ok(ValueEncoding::LengthPrefixed),
        _ => Err(MessageError::ProtocolViolation(
            "unknown message parameter type",
        )),
    }
}

// ─── AuthorizationToken ───────────────────────────────────────

/// AUTHORIZATION_TOKEN の Token 構造 (draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter))
///
/// Token {
///   Alias Type (vi64),
///   [Token Alias (vi64),]
///   [Token Type (vi64),]
///   [Token Value (..)]
/// }
///
/// Alias Type ごとにフィールドの有無が異なる:
///   DELETE (0x0):    Token Alias のみ
///   REGISTER (0x1):  Token Alias + Token Type + Token Value
///   USE_ALIAS (0x2): Token Alias のみ
///   USE_VALUE (0x3): Token Type + Token Value (Alias なし)
#[derive(Debug, Clone, PartialEq)]
pub enum AuthorizationToken {
    /// DELETE (0x0): 登録済み Alias を削除する
    Delete {
        /// 削除対象の Token Alias
        alias: u64,
    },
    /// REGISTER (0x1): Alias を Token Type/Value に関連付ける
    Register {
        /// 登録する Token Alias
        alias: u64,
        /// Token の種別
        token_type: u64,
        /// Token の値 (種別固有のバイト列)
        token_value: Vec<u8>,
    },
    /// USE_ALIAS (0x2): 登録済み Alias を参照する
    UseAlias {
        /// 参照する登録済み Token Alias
        alias: u64,
    },
    /// USE_VALUE (0x3): Token Type/Value を直接指定する (Alias なし)
    UseValue {
        /// Token の種別
        token_type: u64,
        /// Token の値 (種別固有のバイト列)
        token_value: Vec<u8>,
    },
}

impl AuthorizationToken {
    /// Length-prefixed バイト列から Token 構造をデコードする
    ///
    /// draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter):
    /// デコード失敗時は KEY_VALUE_FORMATTING_ERROR を返す。
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, MessageError> {
        let mut pos = 0;
        let (alias_type, n) = varint::decode(bytes).map_err(|_| {
            MessageError::KeyValueFormattingError("failed to decode Token Alias Type")
        })?;
        pos += n;

        match alias_type {
            // DELETE (0x0): Token Alias のみ
            0 => {
                let (alias, n) = varint::decode(&bytes[pos..]).map_err(|_| {
                    MessageError::KeyValueFormattingError("failed to decode Token Alias in DELETE")
                })?;
                pos += n;
                if pos != bytes.len() {
                    return Err(MessageError::KeyValueFormattingError(
                        "DELETE token has trailing bytes",
                    ));
                }
                Ok(Self::Delete { alias })
            }
            // REGISTER (0x1): Token Alias + Token Type + Token Value
            1 => {
                let (alias, n) = varint::decode(&bytes[pos..]).map_err(|_| {
                    MessageError::KeyValueFormattingError(
                        "failed to decode Token Alias in REGISTER",
                    )
                })?;
                pos += n;
                let (token_type, n) = varint::decode(&bytes[pos..]).map_err(|_| {
                    MessageError::KeyValueFormattingError("failed to decode Token Type in REGISTER")
                })?;
                pos += n;
                // Token Value は残りのバイト列すべて
                let token_value = bytes[pos..].to_vec();
                Ok(Self::Register {
                    alias,
                    token_type,
                    token_value,
                })
            }
            // USE_ALIAS (0x2): Token Alias のみ
            2 => {
                let (alias, n) = varint::decode(&bytes[pos..]).map_err(|_| {
                    MessageError::KeyValueFormattingError(
                        "failed to decode Token Alias in USE_ALIAS",
                    )
                })?;
                pos += n;
                if pos != bytes.len() {
                    return Err(MessageError::KeyValueFormattingError(
                        "USE_ALIAS token has trailing bytes",
                    ));
                }
                Ok(Self::UseAlias { alias })
            }
            // USE_VALUE (0x3): Token Type + Token Value (Alias なし)
            3 => {
                let (token_type, n) = varint::decode(&bytes[pos..]).map_err(|_| {
                    MessageError::KeyValueFormattingError(
                        "failed to decode Token Type in USE_VALUE",
                    )
                })?;
                pos += n;
                let token_value = bytes[pos..].to_vec();
                Ok(Self::UseValue {
                    token_type,
                    token_value,
                })
            }
            _ => Err(MessageError::KeyValueFormattingError(
                "unknown Token Alias Type",
            )),
        }
    }

    /// alias 解決なしで (Token Type, Token Value) を返す
    ///
    /// REGISTER / USE_VALUE の場合は Some を返し、DELETE / USE_ALIAS の場合は None を返す。
    /// USE_ALIAS の alias 解決はセッション状態に依存するため、コーデック層では不可能。
    pub(crate) fn token_type_value(&self) -> Option<(u64, &[u8])> {
        match self {
            Self::Register {
                token_type,
                token_value,
                ..
            } => Some((*token_type, token_value)),
            Self::UseValue {
                token_type,
                token_value,
            } => Some((*token_type, token_value)),
            Self::Delete { .. } | Self::UseAlias { .. } => None,
        }
    }

    /// SETUP の AUTHORIZATION_TOKEN として許可されるか検証する
    ///
    /// draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter):
    /// SETUP で DELETE / USE_ALIAS は PROTOCOL_VIOLATION。encode / decode の両経路で
    /// 同一の検証を行うため本メソッドに集約する。
    pub(crate) fn validate_setup_scope(&self) -> Result<(), MessageError> {
        match self {
            Self::Delete { .. } => Err(MessageError::ProtocolViolation(
                "DELETE alias type is not allowed in SETUP",
            )),
            Self::UseAlias { .. } => Err(MessageError::ProtocolViolation(
                "USE_ALIAS alias type is not allowed in SETUP",
            )),
            _ => Ok(()),
        }
    }

    /// Token 構造をバイト列にエンコードする
    pub(crate) fn encode_to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        match self {
            Self::Delete { alias } => {
                varint::encode(0, &mut buf);
                varint::encode(*alias, &mut buf);
            }
            Self::Register {
                alias,
                token_type,
                token_value,
            } => {
                varint::encode(1, &mut buf);
                varint::encode(*alias, &mut buf);
                varint::encode(*token_type, &mut buf);
                buf.extend_from_slice(token_value);
            }
            Self::UseAlias { alias } => {
                varint::encode(2, &mut buf);
                varint::encode(*alias, &mut buf);
            }
            Self::UseValue {
                token_type,
                token_value,
            } => {
                varint::encode(3, &mut buf);
                varint::encode(*token_type, &mut buf);
                buf.extend_from_slice(token_value);
            }
        }
        buf
    }
}

// ─── LocationFilter ─────────────────────────────────────────

/// LOCATION_FILTER の解決コンテキスト (draft-ietf-moq-transport-21 §3.3.1 (Location Filters))
///
/// End フィールド省略時の扱いが subscription と Fetch で異なる。subscription は
/// open-ended (終端なし)、Fetch は End = Largest Object になる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationFilterContext {
    /// Subscription (SUBSCRIBE 由来の subscription / REQUEST_UPDATE)
    Subscription,
    /// Fetch (FETCH 要求。FILL_PARAMETERS 内の fill 要求を含む。現状の呼び出しは
    /// subscription のみだが、FETCH 要求処理で End 既定を区別するために必要)
    Fetch,
}

/// REQUEST_UPDATE における LOCATION_FILTER の更新指示 (draft-ietf-moq-transport-21 §3.3.1 (Location Filters))
///
/// Length 0 は no filter を表し、REQUEST_UPDATE ではフィルタ削除になる。
/// パラメータ省略 (値 unchanged) と区別するため 3 状態で返す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationFilterUpdate {
    /// パラメータ省略時は値 unchanged
    Unchanged,
    /// Length 0 はフィルタ削除 (unfiltered に戻す)
    Removed,
    /// 新しいフィルタに置き換える
    Set(LocationFilter),
}

/// LOCATION_FILTER の typed 表現 (draft-ietf-moq-transport-21 §3.3.1 (Location Filters))
///
/// ワイヤ形式は Length-prefixed な optional vi64 群であり、Length (バイト数) で
/// フィールド数が決まる。フィールド数と意味の対応は次のとおり。
/// - 1 フィールド: StartGroup (Largest Object 相対)
/// - 2 フィールド: StartGroup + StartObject (両方 0 なら Next Object、そうでなければ absolute)
/// - 3 フィールド: absolute Start + EndGroupDelta (End Group の全 Object を含む)
/// - 4 フィールド: absolute Start + EndGroupDelta + EndObject
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationFilter {
    /// 相対 StartGroup のみ (1 フィールド)
    RelativeGroup {
        /// Largest Group からの相対オフセット (draft-ietf-moq-transport-21 §3.3.1)
        start_group: u64,
    },
    /// Next Object (2 フィールドとも 0)
    NextObject,
    /// Absolute Start (2 フィールド。両方 0 の組み合わせは NextObject になる)
    AbsoluteStart {
        /// 開始 Location (絶対値)
        start: Location,
    },
    /// Absolute Range (3 フィールド。End Group の全 Object を含む)
    AbsoluteRange {
        /// 開始 Location (絶対値)
        start: Location,
        /// 開始 Group からの End Group の差分
        end_group_delta: u64,
    },
    /// Absolute Range (4 フィールド。EndObject までを含む)
    AbsoluteRangeWithEnd {
        /// 開始 Location (絶対値)
        start: Location,
        /// 開始 Group からの End Group の差分
        end_group_delta: u64,
        /// 終端 Group 内の終端 Object ID
        end_object: u64,
    },
}

impl LocationFilter {
    /// typed filter を wire format のバイト列へエンコードする
    ///
    /// AbsoluteStart {0, 0} は wire 上区別できないため NextObject と同一バイト列
    /// ([0x00, 0x00]) になる。decode すると NextObject に正規化される。
    /// absolute {0, 0} の open-ended は unfiltered と等価であり、購読側は
    /// フィルタ省略で表す。
    pub fn encode_to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        match self {
            Self::RelativeGroup { start_group } => {
                varint::encode(*start_group, &mut buf);
            }
            // Next Object は StartGroup = 0 / StartObject = 0 の 2 フィールドで表す
            Self::NextObject => {
                varint::encode(0, &mut buf);
                varint::encode(0, &mut buf);
            }
            Self::AbsoluteStart { start } => {
                varint::encode(start.group_id, &mut buf);
                varint::encode(start.object_id, &mut buf);
            }
            Self::AbsoluteRange {
                start,
                end_group_delta,
            } => {
                varint::encode(start.group_id, &mut buf);
                varint::encode(start.object_id, &mut buf);
                varint::encode(*end_group_delta, &mut buf);
            }
            Self::AbsoluteRangeWithEnd {
                start,
                end_group_delta,
                end_object,
            } => {
                varint::encode(start.group_id, &mut buf);
                varint::encode(start.object_id, &mut buf);
                varint::encode(*end_group_delta, &mut buf);
                varint::encode(*end_object, &mut buf);
            }
        }
        buf
    }

    /// wire format のバイト列から typed filter をデコードする
    ///
    /// draft-ietf-moq-transport-21 §3.3.1 (Location Filters):
    /// フィールド数が 0 / 5 以上の場合と壊れた varint は
    /// KEY_VALUE_FORMATTING_ERROR、StartGroup + EndGroupDelta が 2^64 - 1 を
    /// 超える場合は PROTOCOL_VIOLATION として扱う。
    ///
    /// 空バイト列 (Length 0 = no filter) はフィルタ値を持たないため受け付けない。
    /// Length 0 の扱い (REQUEST_UPDATE での削除等) は呼び出し側が
    /// `MessageParameters::location_filter_update` で判定する。
    ///
    /// 2 フィールドとも 0 の AbsoluteStart は wire 上区別できないため
    /// NextObject に正規化される。absolute {0, 0} の open-ended は unfiltered と
    /// 等価であり、購読側はフィルタ省略で表す。
    pub fn decode(bytes: &[u8]) -> Result<Self, MessageError> {
        let mut pos = 0;
        let mut fields = [0u64; 4];
        let mut field_count = 0;
        while pos < bytes.len() {
            if field_count == 4 {
                return Err(MessageError::KeyValueFormattingError(
                    "LOCATION_FILTER has more than 4 fields",
                ));
            }
            let (value, n) = varint::decode(&bytes[pos..]).map_err(|_| {
                MessageError::KeyValueFormattingError("failed to decode field in LOCATION_FILTER")
            })?;
            fields[field_count] = value;
            field_count += 1;
            pos += n;
        }

        let filter = match field_count {
            0 => {
                return Err(MessageError::KeyValueFormattingError(
                    "LOCATION_FILTER requires 1 to 4 fields",
                ));
            }
            1 => Self::RelativeGroup {
                start_group: fields[0],
            },
            2 if fields[0] == 0 && fields[1] == 0 => Self::NextObject,
            2 => Self::AbsoluteStart {
                start: Location {
                    group_id: fields[0],
                    object_id: fields[1],
                },
            },
            3 => {
                let start = Location {
                    group_id: fields[0],
                    object_id: fields[1],
                };
                check_end_group_overflow(start.group_id, fields[2])?;
                Self::AbsoluteRange {
                    start,
                    end_group_delta: fields[2],
                }
            }
            // ループ内ガードで 5 フィールド以上を弾いているため、ここでは 4 のみ到達する
            _ => {
                let start = Location {
                    group_id: fields[0],
                    object_id: fields[1],
                };
                check_end_group_overflow(start.group_id, fields[2])?;
                Self::AbsoluteRangeWithEnd {
                    start,
                    end_group_delta: fields[2],
                    end_object: fields[3],
                }
            }
        };

        Ok(filter)
    }

    /// フィルタの実効 Start Location を Largest Object から導出する
    /// (draft-ietf-moq-transport-21 §3.3.1 (Location Filters))
    ///
    /// `largest` は当該フィルタが相対参照する Largest Object。呼び出し側が役割に応じて
    /// 解決時点の値を渡す。未配信 (`None`) の相対フィルタは先頭 `{0, 0}` から始める。
    ///
    /// - RelativeGroup: `{Largest Group + 1 - StartGroup, 0}`。StartGroup = 0 で Next Group、
    ///   1 で現 Group になる。計算結果が 0 未満なら 0、2^64 - 1 超過なら 2^64 - 1 に
    ///   クランプするため常時 `Some` を返す
    /// - NextObject: `{Largest Group, Largest Object + 1}`。`object + 1` が
    ///   オーバーフローする場合は `Largest` 自体に飽和させる。飽和後の下限は
    ///   既配信済みの最大位置自身を指すため、再送されない限り何も通さず、
    ///   後続 Group の新規 Object は辞書順で下限以上のため通過する
    ///   (存在しない `{Largest Group, u64::MAX + 1}` と観測等価)。
    ///   `None` (下限なし・全通し) は返さない。
    ///   未配信の `Some({0, 0})` とは区別される
    /// - AbsoluteStart / AbsoluteRange / AbsoluteRangeWithEnd: 明示された `start` をそのまま返す
    ///
    /// この節番号・規則は draft-ietf-moq-transport-21 由来であり将来の draft 改版で変わる可能性がある。
    pub fn effective_start_location(&self, largest: Option<&Location>) -> Option<Location> {
        match self {
            Self::RelativeGroup { start_group } => match largest {
                Some(l) => {
                    // `{Largest Group + 1 - StartGroup, 0}` を u128 で求め、0 未満は 0、
                    // 2^64 - 1 超過は 2^64 - 1 にクランプする
                    let base = u128::from(l.group_id) + 1;
                    let start_group = base
                        .saturating_sub(u128::from(*start_group))
                        .min(u128::from(u64::MAX)) as u64;
                    Some(Location {
                        group_id: start_group,
                        object_id: 0,
                    })
                }
                None => Some(Location {
                    group_id: 0,
                    object_id: 0,
                }),
            },
            Self::NextObject => match largest {
                Some(l) => Some(Location {
                    group_id: l.group_id,
                    object_id: l.object_id.saturating_add(1),
                }),
                None => Some(Location {
                    group_id: 0,
                    object_id: 0,
                }),
            },
            Self::AbsoluteStart { start }
            | Self::AbsoluteRange { start, .. }
            | Self::AbsoluteRangeWithEnd { start, .. } => Some(*start),
        }
    }

    /// フィルタの実効 End Location を導出する
    /// (draft-ietf-moq-transport-21 §3.3.1 (Location Filters))
    ///
    /// End フィールド省略時の扱いはコンテキストで異なる。subscription は open-ended
    /// (終端なし) のため `None`、Fetch は End = Largest Object のため `largest` を返す。
    /// 未配信 (`None`) の Fetch は終端未定のため `None` を返し、呼び出し側
    /// (FETCH 要求処理) が INVALID_RANGE 等で扱う。
    ///
    /// AbsoluteRange (EndObject 省略) は End Group の全 Object を含むため
    /// `{End Group, u64::MAX}` を返す。AbsoluteRangeWithEnd は
    /// `{StartGroup + EndGroupDelta, EndObject}` を返す。
    ///
    /// `StartGroup + EndGroupDelta` のオーバーフローは decode 時に PROTOCOL_VIOLATION 化される
    /// ため wire 由来の値では起こらない。ただし `LocationFilter` は公開 enum で decode を経ず
    /// in-memory 構築もできるため、パニックを避けて `u64::MAX` に飽和させる。
    /// この節番号・規則は draft-ietf-moq-transport-21 由来であり将来の draft 改版で変わる可能性がある。
    pub fn effective_end_location(
        &self,
        largest: Option<&Location>,
        context: LocationFilterContext,
    ) -> Option<Location> {
        match self {
            Self::RelativeGroup { .. } | Self::NextObject | Self::AbsoluteStart { .. } => {
                match context {
                    LocationFilterContext::Subscription => None,
                    LocationFilterContext::Fetch => largest.cloned(),
                }
            }
            Self::AbsoluteRange {
                start,
                end_group_delta,
            } => Some(Location {
                group_id: start.group_id.saturating_add(*end_group_delta),
                object_id: u64::MAX,
            }),
            Self::AbsoluteRangeWithEnd {
                start,
                end_group_delta,
                end_object,
            } => Some(Location {
                group_id: start.group_id.saturating_add(*end_group_delta),
                object_id: *end_object,
            }),
        }
    }
}

/// LOCATION_FILTER の End Group 導出時のオーバーフローを検証する
///
/// draft-ietf-moq-transport-21 §3.3.1 (Location Filters): StartGroup + EndGroupDelta が
/// 2^64 - 1 を超えたら PROTOCOL_VIOLATION でセッションを閉じなければならない。
/// Delta = 0 は当該 Group の残り全部であり常に正当。
fn check_end_group_overflow(start_group: u64, end_group_delta: u64) -> Result<(), MessageError> {
    if end_group_delta != 0 && start_group.checked_add(end_group_delta).is_none() {
        return Err(MessageError::ProtocolViolation(
            "LOCATION_FILTER end group ID overflows u64",
        ));
    }
    Ok(())
}

// ─── MessageParameterValue ─────────────────────────────────────

/// Message Parameter の値 (型固有エンコーディング)
#[derive(Debug, Clone, PartialEq)]
pub enum MessageParameterValue {
    /// uint8: 1 バイト固定
    Uint8(u8),
    /// varint: vi64
    VarInt(u64),
    /// Location: Group (vi64) + Object (vi64)
    Location {
        /// Group ID
        group: u64,
        /// Object ID
        object: u64,
    },
    /// Length-prefixed: vi64 (length) + bytes
    LengthPrefixed(Vec<u8>),
    /// FILL_PARAMETERS の内側パラメータ群 (draft-ietf-moq-transport-21 §9.20.16 (FILL PARAMETERS Parameter))
    ///
    /// ワイヤ上は length-prefixed バイト列であり、内側は独立メッセージの
    /// Parameters として (カウント + デルタエンコードで) エンコードされる
    /// (draft-ietf-moq-transport-21 §16.7 (Message Parameters))。
    FillParameters(MessageParameters),
    /// AUTHORIZATION_TOKEN の Token 構造 (draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter))
    AuthorizationToken(AuthorizationToken),
    /// TRACK_NAMESPACE_PREFIX (Track Namespace 形式, draft-ietf-moq-transport-21 §9.20.21 (TRACK_NAMESPACE_PREFIX Parameter))
    TrackNamespacePrefix(TrackNamespace),
}

// ─── MessageParameter ──────────────────────────────────────────

/// 単一の Message Parameter
#[derive(Debug, Clone, PartialEq)]
pub struct MessageParameter {
    /// パラメータ型 (draft-ietf-moq-transport-21 §9.20)
    pub param_type: u64,
    /// 型固有エンコーディングの値
    pub value: MessageParameterValue,
}

// ─── MessageParameters ─────────────────────────────────────────

/// Message Parameters リスト (draft-ietf-moq-transport-21 §9.20 (Control Message Parameters))
///
/// カウントプレフィックス付き。エンコード時に `param_type` の昇順にソートして
/// デルタエンコードを適用する。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MessageParameters(Vec<MessageParameter>);

impl MessageParameters {
    /// 空のパラメータリストを作成する
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// パラメータを末尾に追加する
    pub fn push(&mut self, p: MessageParameter) {
        self.0.push(p);
    }

    /// パラメータ数を返す
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// パラメータが空かどうかを返す
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 内部のパラメータスライスを返す
    pub fn as_slice(&self) -> &[MessageParameter] {
        &self.0
    }

    /// LARGEST_OBJECT パラメータを設定する
    ///
    /// 既存の LARGEST_OBJECT があれば置き換え、なければ追加する。
    pub fn set_largest_object(&mut self, group_id: u64, object_id: u64) {
        if let Some(existing) = self
            .0
            .iter_mut()
            .find(|p| p.param_type == PARAM_LARGEST_OBJECT)
        {
            existing.value = MessageParameterValue::Location {
                group: group_id,
                object: object_id,
            };
        } else {
            self.0.push(MessageParameter {
                param_type: PARAM_LARGEST_OBJECT,
                value: MessageParameterValue::Location {
                    group: group_id,
                    object: object_id,
                },
            });
        }
    }

    /// Message Parameters をバッファにエンコードする (draft-ietf-moq-transport-21 §9.20 (Control Message Parameters))
    ///
    /// フォーマット: Number(vi64) + デルタエンコードされたパラメータ列
    ///
    /// draft-ietf-moq-transport-21 §9.20 (Control Message Parameters): パラメータ定義が明示的に複数インスタンスを
    /// 許可していない限り、同一 Parameter Type の重複送信は禁止されている。重複がある場合は
    /// `InvalidParameter` を返す。
    pub fn encode(&self, buf: &mut Vec<u8>) -> Result<(), MessageError> {
        // 同一 Parameter Type が複数ある場合、安定ソートにより push 順が保たれたまま隣接し、
        // 2 件目以降は delta=0 でエンコードされる
        let mut sorted = self.0.clone();
        sorted.sort_by_key(|p| p.param_type);

        // draft-ietf-moq-transport-21 §9.20 (Control Message Parameters): "Senders MUST NOT repeat the same
        // Parameter Type in a message unless the parameter definition explicitly allows multiple
        // instances of that type to be sent in a single message."
        // 明示的に複数出現を許可されているのは以下の 2 種:
        //   - AUTHORIZATION_TOKEN (0x03): draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter)
        //   - Range Filter (0x25-0x29): draft-ietf-moq-transport-21 §3.3.2 (Range Filters)
        for window in sorted.windows(2) {
            let param_type = window[0].param_type;
            if param_type == window[1].param_type
                && param_type != PARAM_AUTHORIZATION_TOKEN
                && !is_range_filter_type(param_type)
            {
                return Err(MessageError::InvalidParameter);
            }
        }

        // draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter): AUTHORIZATION_TOKEN の
        // (Token Type, Token Value) は alias 解決後に一意でなければならない。
        // USE_ALIAS の alias 解決はセッション状態依存のためコーデック層では検証不可。
        validate_auth_token_uniqueness(sorted.iter().filter_map(|p| {
            if p.param_type == PARAM_AUTHORIZATION_TOKEN
                && let MessageParameterValue::AuthorizationToken(ref token) = p.value
            {
                Some(token)
            } else {
                None
            }
        }))?;

        varint::encode(sorted.len() as u64, buf);

        let mut prev_type: u64 = 0;
        for param in &sorted {
            // draft-ietf-moq-transport-21 §9.20 (Control Message Parameters): 型と値形式の整合性 + 値域を検証する
            validate_param_encoding(param)?;
            crate::kvp::encode_delta_key(prev_type, param.param_type, buf);
            encode_value(&param.value, buf)?;
            prev_type = param.param_type;
        }
        Ok(())
    }

    /// バッファの先頭から Message Parameters をデコードし `(Self, 消費バイト数)` を返す
    pub fn decode(buf: &[u8]) -> Result<(Self, usize), MessageError> {
        Self::decode_inner(buf, true)
    }

    /// `decode` の実装本体
    ///
    /// `allow_fill` が false のときは FILL_PARAMETERS の入れ子を拒否する。
    fn decode_inner(buf: &[u8], allow_fill: bool) -> Result<(Self, usize), MessageError> {
        let mut pos = 0;

        let (count, n) = varint::decode(&buf[pos..])?;
        pos += n;

        // 各パラメータは最低 2 バイト (delta varint + value) 必要なので、
        // 残りバッファの半分を超える count は不正
        let remaining = buf.len() - pos;
        // count (u64) を usize に切り捨ててから比較すると 32bit 環境で巨大 count が
        // ガードをすり抜けるため、u64 空間で比較する ((remaining / 2) as u64 は widening で安全)
        if count > (remaining / 2) as u64 {
            return Err(MessageError::ProtocolViolation(
                "message parameter count exceeds buffer capacity",
            ));
        }

        let mut params = Vec::new();
        let mut prev_type: u64 = 0;

        for _ in 0..count {
            let (param_type, delta) = crate::kvp::decode_delta_key(prev_type, buf, &mut pos)?;

            // draft-ietf-moq-transport-21 §9.20 (Control Message Parameters): パラメータ定義が明示的に許可して
            // いない同一型の繰り返しは PROTOCOL_VIOLATION。許可される 2 種は `encode()` の注記を参照。
            // Range Filter の (Parameter Type, SetID, Property Type) 重複は codec 層ではなく
            // `validate_range_filters()` が INVALID_FILTER 相当として検出する。
            // delta == 0 かつ最初のパラメータでない場合が同一型の繰り返しにあたる。
            if delta == 0
                && !params.is_empty()
                && param_type != PARAM_AUTHORIZATION_TOKEN
                && !is_range_filter_type(param_type)
            {
                return Err(MessageError::ProtocolViolation(
                    "duplicate message parameter type",
                ));
            }

            // 未知のパラメータ型は PROTOCOL_VIOLATION
            let encoding = value_encoding(param_type)?;
            let (mut value, consumed) = decode_value(encoding, &buf[pos..])?;
            pos += consumed;

            // draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter): AUTHORIZATION_TOKEN は Token 構造として検証する
            // draft-ietf-moq-transport-21 §8.3 (Key-Value-Pair Structure): 既知 Type の Value が定義された serialization と
            // 一致しない場合は KEY_VALUE_FORMATTING_ERROR
            if param_type == PARAM_AUTHORIZATION_TOKEN
                && let MessageParameterValue::LengthPrefixed(ref bytes) = value
            {
                let token = AuthorizationToken::decode(bytes)?;
                value = MessageParameterValue::AuthorizationToken(token);
            }

            if param_type == PARAM_LOCATION_FILTER
                && let MessageParameterValue::LengthPrefixed(ref bytes) = value
            {
                validate_location_filter_bytes(bytes)?;
            }

            // draft-ietf-moq-transport-21 §9.20.16 (FILL PARAMETERS Parameter):
            // 内側パラメータ群を別スコープとしてデコードし、Table 6 外は
            // PROTOCOL_VIOLATION とする (MUST close the session)。
            // FILL_PARAMETERS の内側に FILL_PARAMETERS は出現し得ないため、
            // 再帰する前に拒否してスタックオーバーフローを防ぐ。
            if param_type == PARAM_FILL_PARAMETERS {
                if !allow_fill {
                    return Err(MessageError::ProtocolViolation(
                        "FILL_PARAMETERS must not be nested",
                    ));
                }
                if let MessageParameterValue::LengthPrefixed(ref bytes) = value {
                    let inner = decode_fill_parameters(bytes)?;
                    value = MessageParameterValue::FillParameters(inner);
                }
            }

            // draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter) / §9.20.9 (GROUP ORDER Parameter) /
            // draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
            // encode 側と同じ値域検証を行う
            if let MessageParameterValue::Uint8(v) = &value {
                validate_uint8_param_value(param_type, *v)?;
            }

            params.push(MessageParameter { param_type, value });
            prev_type = param_type;
        }

        // draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter): AUTHORIZATION_TOKEN の
        // (Token Type, Token Value) は alias 解決後に一意でなければならない
        validate_auth_token_uniqueness(params.iter().filter_map(|p| {
            if p.param_type == PARAM_AUTHORIZATION_TOKEN
                && let MessageParameterValue::AuthorizationToken(ref token) = p.value
            {
                Some(token)
            } else {
                None
            }
        }))?;

        Ok((Self(params), pos))
    }

    // ─── アクセサ ──────────────────────────────────────────────

    /// OBJECT_DELIVERY_TIMEOUT (type 0x02) の値を返す
    pub fn object_delivery_timeout(&self) -> Option<u64> {
        self.find_varint(PARAM_OBJECT_DELIVERY_TIMEOUT)
    }

    /// SUBGROUP_DELIVERY_TIMEOUT (type 0x06) の値を返す (draft-ietf-moq-transport-21 §9.20.4)
    pub fn subgroup_delivery_timeout(&self) -> Option<u64> {
        self.find_varint(PARAM_SUBGROUP_DELIVERY_TIMEOUT)
    }

    /// FILL_TIMEOUT (type 0x0A) の値を返す (draft-ietf-moq-transport-21 §9.20.6 (FILL TIMEOUT Parameter))
    pub fn fill_timeout(&self) -> Option<u64> {
        self.find_varint(PARAM_FILL_TIMEOUT)
    }

    /// FILL_PARAMETERS (type 0x23) の内側パラメータ群を返す
    ///
    /// draft-ietf-moq-transport-21 §9.20.16 (FILL PARAMETERS Parameter):
    /// 存在しない場合は `None` を返す。内側は外側とは別のパラメータスコープ
    /// (Table 6) であり、subscription 状態として保持されない。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub fn fill_parameters(&self) -> Option<&MessageParameters> {
        for p in &self.0 {
            if p.param_type == PARAM_FILL_PARAMETERS
                && let MessageParameterValue::FillParameters(ref inner) = p.value
            {
                return Some(inner);
            }
        }
        None
    }

    /// 指定型のパラメータをすべて除去する
    ///
    /// draft-ietf-moq-transport-21 §9.20.16 (FILL PARAMETERS Parameter):
    /// FILL_PARAMETERS は運んできたメッセージにのみ適用され subscription 状態として
    /// 保持されない (sticky 対象外) ため、累積パラメータ (`pending_update_params`)
    /// への保持から除外する用途に使う。
    pub(crate) fn remove_type(&mut self, param_type: u64) {
        self.0.retain(|p| p.param_type != param_type);
    }

    /// すべての AUTHORIZATION_TOKEN (type 0x03) を返す
    ///
    /// draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter): AUTHORIZATION_TOKEN は同一メッセージ内で複数回出現できる
    pub fn authorization_tokens(&self) -> Vec<&AuthorizationToken> {
        self.0
            .iter()
            .filter_map(|p| {
                if p.param_type == PARAM_AUTHORIZATION_TOKEN
                    && let MessageParameterValue::AuthorizationToken(ref t) = p.value
                {
                    Some(t)
                } else {
                    None
                }
            })
            .collect()
    }

    /// RENDEZVOUS_TIMEOUT (type 0x04) の値を返す
    pub fn rendezvous_timeout(&self) -> Option<u64> {
        self.find_varint(PARAM_RENDEZVOUS_TIMEOUT)
    }

    /// EXPIRES (type 0x08) パラメータが存在するかどうかを返す
    ///
    /// EXPIRES=0 もパラメータとしては存在する。
    /// `has_expires() == true` の場合でも `expires()` は `None` を返しうる
    /// （EXPIRES=0 のとき）。
    /// draft-ietf-moq-transport-21 §9.20.17（将来の draft で変更される可能性がある）
    pub fn has_expires(&self) -> bool {
        self.0.iter().any(|p| p.param_type == PARAM_EXPIRES)
    }

    /// EXPIRES (type 0x08) の値を返す
    ///
    /// EXPIRES=0 または不在の場合は `None` を返す。
    /// draft-ietf-moq-transport-21 §9.20.17（将来の draft で変更される可能性がある）
    /// では「EXPIRES が 0 または不在なら subscription は expire しないか、
    /// 不明な時刻に expire する」と規定される。本実装では両者を「期限なし」
    /// （`None`）として正規化する。
    pub fn expires(&self) -> Option<u64> {
        self.find_varint(PARAM_EXPIRES).filter(|&v| v != 0)
    }

    /// LARGEST_OBJECT (type 0x09) の値を (group, object) として返す
    pub fn largest_object(&self) -> Option<(u64, u64)> {
        for p in &self.0 {
            if p.param_type == PARAM_LARGEST_OBJECT
                && let MessageParameterValue::Location { group, object } = &p.value
            {
                return Some((*group, *object));
            }
        }
        None
    }

    /// FORWARD (type 0x10) の値を返す
    pub fn forward(&self) -> Option<u8> {
        self.find_uint8(PARAM_FORWARD)
    }

    /// INCLUDE_PROPERTIES (type 0x35) の値を返す
    ///
    /// draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
    /// 0 (Properties を送らない) / 1 (送る)。不在時は default 1 として扱う。
    pub fn include_properties(&self) -> Option<u8> {
        self.find_uint8(PARAM_INCLUDE_PROPERTIES)
    }

    /// SUBSCRIBER_PRIORITY (type 0x20) の値を返す
    pub fn subscriber_priority(&self) -> Option<u8> {
        self.find_uint8(PARAM_SUBSCRIBER_PRIORITY)
    }

    /// LOCATION_FILTER (type 0x21) の値を返す
    pub fn location_filter(&self) -> Option<&[u8]> {
        self.find_length_prefixed(PARAM_LOCATION_FILTER)
    }

    /// LOCATION_FILTER (type 0x21) を typed filter として返す
    ///
    /// パラメータ省略時と Length 0 (no filter) のときは `None` を返す。
    /// REQUEST_UPDATE での削除指示と省略の区別が必要な場合は
    /// `location_filter_update` を使う。
    pub fn location_filter_typed(&self) -> Result<Option<LocationFilter>, MessageError> {
        match self.location_filter_update()? {
            LocationFilterUpdate::Set(filter) => Ok(Some(filter)),
            LocationFilterUpdate::Unchanged | LocationFilterUpdate::Removed => Ok(None),
        }
    }

    /// LOCATION_FILTER (type 0x21) の更新指示を返す
    ///
    /// draft-ietf-moq-transport-21 §3.3.1 (Location Filters): Length 0 は no filter であり、
    /// REQUEST_UPDATE ではフィルタ削除になる。パラメータ省略 (値 unchanged) と区別する。
    pub fn location_filter_update(&self) -> Result<LocationFilterUpdate, MessageError> {
        let Some(bytes) = self.location_filter() else {
            return Ok(LocationFilterUpdate::Unchanged);
        };
        if bytes.is_empty() {
            return Ok(LocationFilterUpdate::Removed);
        }
        LocationFilter::decode(bytes).map(LocationFilterUpdate::Set)
    }

    /// GROUP_ORDER (type 0x22) の値を返す
    pub fn group_order(&self) -> Option<u8> {
        self.find_uint8(PARAM_GROUP_ORDER)
    }

    /// NEW_GROUP_REQUEST (type 0x32) の値を返す
    pub fn new_group_request(&self) -> Option<u64> {
        self.find_varint(PARAM_NEW_GROUP_REQUEST)
    }

    /// TRACK_NAMESPACE_PREFIX (type 0x34) の値を返す (draft-ietf-moq-transport-21 §9.20.21 (TRACK_NAMESPACE_PREFIX Parameter))
    pub fn track_namespace_prefix(&self) -> Option<&TrackNamespace> {
        for p in &self.0 {
            if p.param_type == PARAM_TRACK_NAMESPACE_PREFIX
                && let MessageParameterValue::TrackNamespacePrefix(ref ns) = p.value
            {
                return Some(ns);
            }
        }
        None
    }

    /// 指定した Range Filter 型 (0x25-0x29) の全インスタンスのバイト列を出現順に返す
    ///
    /// draft-ietf-moq-transport-21 §3.3.2 (Range Filters): Range Filter は同一 Parameter Type が
    /// 同一メッセージ内で複数回出現できる。1 インスタンスは 1 SetID しか持てず、SetID ごとの
    /// 結果は OR 結合される (SetID=0 OR SetID=1 OR ... SetID=255) ため、フィルタを正しく評価するには
    /// 全インスタンスが必要になる。単一インスタンスしか返さない検索ヘルパーでは表現できない。
    ///
    /// フィルタの実際の適用 (§3.3.2 の `Pass = Forward AND Location Filters AND Range Filters`) は
    /// 本クレートの責務ではなく、返したバイト列を解釈する呼び出し側が行う。
    ///
    /// REQUEST_UPDATE の削除指示にあたる Length=0 のインスタンスも空スライスとして含まれる
    /// (フィルタとしては評価できない)。呼び出し側で除外すること。
    /// Range Filter 型以外を渡した場合は空の `Vec` を返す。
    /// この節番号・規則は draft-ietf-moq-transport-21 由来であり将来の draft 改版で変わる可能性がある。
    pub fn range_filters(&self, param_type: u64) -> Vec<&[u8]> {
        if !is_range_filter_type(param_type) {
            return Vec::new();
        }
        self.0
            .iter()
            .filter_map(|p| {
                if p.param_type == param_type
                    && let MessageParameterValue::LengthPrefixed(ref bytes) = p.value
                {
                    Some(bytes.as_slice())
                } else {
                    None
                }
            })
            .collect()
    }

    /// 指定型の Range Filter をデコードして [`RangeFilterSet`] の列で返す
    ///
    /// draft-ietf-moq-transport-21 §3.3.2 (Range Filters) のデルタエンコーディングを解決し、
    /// 絶対値の範囲として扱えるようにする。範囲を 1 つも持たないインスタンス
    /// (Length=0 の削除指示や SetID のみ) と、パースできないインスタンスは除外する。
    ///
    /// パース不能な入力は `validate_range_filters` が受信・送信の両経路で先に弾いているため、
    /// 状態として保持する時点では現れない。
    pub fn range_filter_sets(&self, param_type: u64) -> Vec<RangeFilterSet> {
        self.range_filters(param_type)
            .into_iter()
            .filter_map(|bytes| decode_range_filter_set(param_type, bytes))
            .filter(|set| !set.ranges.is_empty())
            .collect()
    }

    /// SUBSCRIBE_TRACKS の Parameters から resulting PUBLISH に載せる
    /// initial subscription parameters を導出する
    ///
    /// draft-ietf-moq-transport-21 §9.18.1 (Parameters on SUBSCRIBE_TRACKS):
    /// SUBSCRIBE_TRACKS 由来の PUBLISH には SUBSCRIBE_TRACKS の Parameters が
    /// initial subscription parameters として明示的に載る。
    /// PUBLISH に出現可能な subset (`PUBLISH_ALLOWED_PARAMS`) のみ残し、
    /// AUTHORIZATION TOKEN は除外する (送信者義務。draft-ietf-moq-transport-21
    /// §9.20.3 (AUTHORIZATION TOKEN Parameter) の MUST NOT copy)。
    /// 本メソッドは濾過のみ行い、TRACK_SUBSCRIPTION 側の FORWARD 蓄積状態は見ない。
    /// 蓄積値と明示値が競合した場合は、呼び出し側で明示値を優先して解決すること。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub fn resulting_publish_parameters(&self) -> MessageParameters {
        let mut result = MessageParameters::new();
        for p in &self.0 {
            if p.param_type == PARAM_AUTHORIZATION_TOKEN {
                continue;
            }
            if crate::message::PUBLISH_ALLOWED_PARAMS.contains(&p.param_type) {
                result.0.push(p.clone());
            }
        }
        result
    }

    /// 後続の REQUEST_UPDATE のパラメータをマージする (draft-ietf-moq-transport-21 Appendix A.3 (Since draft-ietf-moq-transport-17) #1540, draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions))
    ///
    /// 各パラメータ型について後の値で上書きする (field-by-field merge)。
    /// AUTHORIZATION_TOKEN (0x03) は複数回出現が許可されているため上書きせず追記する。
    ///
    /// Range Filter (0x25-0x29) は同一 Parameter Type が複数インスタンス存在しうるため、型単位で
    /// まとめて置換する (draft-ietf-moq-transport-21 §3.3.2: "In REQUEST_UPDATE, Length of 0 removes
    /// the filter; non-zero replaces it entirely.")。型単位の置換であり、sets と Property Types を
    /// 含めて全体が対象になる。セマンティクスは以下のとおり:
    ///
    /// - `other` に型 X が出現した場合: `self` の型 X を全削除し、`other` の非ゼロインスタンスを
    ///   すべて追加する (Length=0 のみなら追加は 0 件 = 削除だけになる)
    /// - `other` に型 X が含まれない場合: `self` の型 X は変更しない
    ///   (§3.3.2: "If a filter parameter is omitted from REQUEST_UPDATE, it is unchanged.")
    ///
    /// 同一 REQUEST_UPDATE 内に型 X の Length=0 と非ゼロが混在した場合、全削除のあとに非ゼロを
    /// 追加するため非ゼロが残る。§3.3.2 はこの混在について規定しておらず、本実装の選択である。
    ///
    /// Range Filter 型の値が `LengthPrefixed` 以外の場合 (公開 API による in-memory 構築でのみ
    /// 起こりうる) も型単位の置換対象として扱う。その値は `encode()` が `InvalidParameter`、
    /// `validate_range_filters()` が `ProtocolViolation` で検出する。
    pub fn merge_from(&mut self, other: &MessageParameters) {
        // 削除パス: other に出現した Range Filter 型を self からまとめて削除する。
        // 逐次ループ内で型ごとに「全削除 → 追加」を行うと、other に同一型が 2 件以上あるとき
        // 2 件目の削除で 1 件目に追加したインスタンスまで消えてしまうため、削除を先に済ませる。
        // Range Filter 型は 5 種しかないので、重複を除いて `retain` の線形探索を
        // `other` のパラメータ数ではなく型数 (最大 5) で抑える
        let mut reset_types: Vec<u64> = Vec::new();
        for p in other.as_slice() {
            if is_range_filter_type(p.param_type) && !reset_types.contains(&p.param_type) {
                reset_types.push(p.param_type);
            }
        }
        self.0.retain(|e| !reset_types.contains(&e.param_type));

        // 追加パス: Range Filter は非ゼロ Length のみ追加し、それ以外は従来どおり型ごとに上書きする
        for p in other.as_slice() {
            if is_range_filter_type(p.param_type) {
                // draft-ietf-moq-transport-21 §3.3.2: Length=0 は削除のみを意味するので追加しない
                if let MessageParameterValue::LengthPrefixed(bytes) = &p.value
                    && bytes.is_empty()
                {
                    continue;
                }
                self.0.push(p.clone());
                continue;
            }
            if p.param_type != PARAM_AUTHORIZATION_TOKEN
                && let Some(existing) = self.0.iter_mut().find(|e| e.param_type == p.param_type)
            {
                existing.value = p.value.clone();
                continue;
            }
            self.0.push(p.clone());
        }
    }

    // ─── スコープ検証 ──────────────────────────────────────────

    /// デコードしたパラメータがメッセージ種別に対して許可されているか検証する
    ///
    /// draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope): 許可されていないメッセージ種別に出現した場合は
    /// PROTOCOL_VIOLATION を返す。
    pub fn validate_scope(&self, allowed: &[u64]) -> Result<(), MessageError> {
        for p in &self.0 {
            if !allowed.contains(&p.param_type) {
                return Err(MessageError::ProtocolViolation(
                    "message parameter not allowed in this message type",
                ));
            }
        }
        Ok(())
    }

    /// Range Filter パラメータ (0x25-0x29) が 1 つでも含まれているかを返す
    ///
    /// draft-ietf-moq-transport-21 §3.3.2 (Range Filters) の検証を行うかどうかの判定に使う。
    /// `count_range_filters()` が 0 でも Range Filter パラメータ自体は存在しうるため、
    /// Range の総数ではなくパラメータの有無で判定しなければ検証がすり抜ける。
    /// Range 総数が 0 になるのは次の場合:
    /// - Length=0 (REQUEST_UPDATE でのフィルタ削除指示)
    /// - 0x25-0x27 でペイロードが SetID のみ
    /// - 0x28 / 0x29 でペイロードが SetID + Property Type のみ
    /// - 内部構造のパースに失敗した (`count_range_filters()` が数えられず 0 に畳まれる)
    ///
    /// この節番号・規則は draft-ietf-moq-transport-21 由来であり将来の draft 改版で変わる可能性がある。
    pub fn has_range_filters(&self) -> bool {
        self.0.iter().any(|p| is_range_filter_type(p.param_type))
    }

    /// Range Filter パラメータ (0x25-0x29) に含まれる Range の総数を返す
    ///
    /// draft-ietf-moq-transport-21 §3.3.2 (Range Filters): MAX_FILTER_RANGES は
    /// 全 Range Filter パラメータ内の Range 総数を制限する。
    /// 内部構造のパースに失敗した場合は 0 を返す (パースエラーは別途検証する)。
    pub fn count_range_filters(&self) -> u64 {
        self.0
            .iter()
            .filter(|p| is_range_filter_type(p.param_type))
            .filter_map(|p| match &p.value {
                MessageParameterValue::LengthPrefixed(bytes) => {
                    count_ranges_in_filter(p.param_type, bytes)
                }
                _ => None,
            })
            .sum()
    }

    /// Range Filter パラメータの内部構造を検証する
    ///
    /// draft-ietf-moq-transport-21 §3.3.2 (Range Filters):
    /// - デルタエンコーディングの結果が 2^64-1 を超える場合は INVALID_FILTER
    /// - 同一メッセージ内で (Parameter Type, SetID, Property Type) が重複する場合は INVALID_FILTER
    ///
    /// 同一 Parameter Type の複数出現そのものは仕様上許可されているため codec 層では弾かない。
    /// Property Type フィールドを持たない 0x25-0x27 では判定キーが実質 (Parameter Type, SetID) に
    /// なる (`parse_range_filter_header` が Property Type に `None` を返す)。
    pub fn validate_range_filters(&self) -> Result<(), MessageError> {
        let mut seen: alloc::vec::Vec<(u64, u8, Option<u64>)> = alloc::vec::Vec::new();
        for p in &self.0 {
            if !is_range_filter_type(p.param_type) {
                continue;
            }
            let MessageParameterValue::LengthPrefixed(bytes) = &p.value else {
                return Err(MessageError::ProtocolViolation(
                    "Range Filter parameter has non-bytes value",
                ));
            };
            // draft-ietf-moq-transport-21 §3.3.2: Length=0 は REQUEST_UPDATE でのフィルタ削除を表し、
            // SetID フィールド自体がワイヤ上に存在しない (ペイロードが空)。仕様の重複規則が対象とする
            // (Parameter Type, SetID, Property Type) を持たないため重複検証から除外する。
            if bytes.is_empty() {
                continue;
            }
            let (set_id, property_type, _) = parse_range_filter_header(p.param_type, bytes).ok_or(
                MessageError::ProtocolViolation("malformed Range Filter header"),
            )?;
            // デルタ溢出検証
            validate_range_filter_deltas(p.param_type, bytes)?;
            seen.push((p.param_type, set_id, property_type));
        }
        // 重複検証: (Parameter Type, SetID, Property Type)
        //
        // キーごとに線形探索すると比較回数がインスタンス数の 2 乗になる。同一 Parameter Type の
        // 複数出現が許可された結果、1 メッセージに載る Range Filter の個数は制御メッセージ長
        // (2^16-1 バイト) 律速まで増え、かつ Range を持たないインスタンスは MAX_FILTER_RANGES の
        // 上限判定 (Range 総数) を通過してここへ到達する。ネットワーク入力で二次オーダーの
        // 計算量を踏ませないため、ソートしてから隣接比較する。
        seen.sort_unstable();
        if seen.windows(2).any(|w| w[0] == w[1]) {
            return Err(MessageError::ProtocolViolation(
                "duplicate Range Filter (Parameter Type, SetID, Property Type)",
            ));
        }
        Ok(())
    }

    // ─── 検索ヘルパー ──────────────────────────────────────────

    /// uint8 型パラメータの値を検索する
    fn find_uint8(&self, param_type: u64) -> Option<u8> {
        for p in &self.0 {
            if p.param_type == param_type
                && let MessageParameterValue::Uint8(v) = p.value
            {
                return Some(v);
            }
        }
        None
    }

    /// varint 型パラメータの値を検索する
    fn find_varint(&self, param_type: u64) -> Option<u64> {
        for p in &self.0 {
            if p.param_type == param_type
                && let MessageParameterValue::VarInt(v) = p.value
            {
                return Some(v);
            }
        }
        None
    }

    /// length-prefixed 型パラメータの値を検索する
    fn find_length_prefixed(&self, param_type: u64) -> Option<&[u8]> {
        for p in &self.0 {
            if p.param_type == param_type
                && let MessageParameterValue::LengthPrefixed(ref v) = p.value
            {
                return Some(v);
            }
        }
        None
    }
}

// ─── 値のエンコード/デコード ───────────────────────────────────

/// uint8 値パラメータの値域を検証する
///
/// - FORWARD (0x10): 0 または 1 のみ (draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter))
/// - GROUP_ORDER (0x22): 1 (Ascending) または 2 (Descending) のみ (draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter))
/// - INCLUDE_PROPERTIES (0x35): 0 または 1 のみ (draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter))
///
/// encode と decode の両経路で同一の検証を行い、検証の非対称を防ぐ。
fn validate_uint8_param_value(param_type: u64, value: u8) -> Result<(), MessageError> {
    match param_type {
        PARAM_FORWARD if value > 1 => Err(MessageError::ProtocolViolation(
            "FORWARD parameter value must be 0 or 1",
        )),
        PARAM_GROUP_ORDER if value != 1 && value != 2 => Err(MessageError::ProtocolViolation(
            "GROUP_ORDER parameter value must be 1 (Ascending) or 2 (Descending)",
        )),
        PARAM_INCLUDE_PROPERTIES if value > 1 => Err(MessageError::ProtocolViolation(
            "INCLUDE_PROPERTIES parameter value must be 0 or 1",
        )),
        _ => Ok(()),
    }
}

/// パラメータ型と値の整合性を検証する (encode 側)
///
/// decode 側は `value_encoding()` + `decode_value()` で型と値形式の整合性を保証しているが、
/// encode 側でも同等の検証を行い、RFC 非準拠のワイヤーフォーマット生成を防ぐ。
fn validate_param_encoding(param: &MessageParameter) -> Result<(), MessageError> {
    let expected = value_encoding(param.param_type)?;
    // エンコーディング種別が同じでも variant が異なる型があるため、
    // 型ごとに許可する variant を厳密に判定する。
    //   - 0x03 AUTHORIZATION_TOKEN: AuthorizationToken のみ
    //   - 0x21 LOCATION_FILTER: LengthPrefixed のみ
    //   - 0x23 FILL_PARAMETERS: FillParameters のみ
    // encoding の一致だけで受理すると、encode は通るのに decode が別 variant へ
    // 解釈したり拒否したりするラウンドトリップ不整合が生じる。
    let actual_matches = match &param.value {
        MessageParameterValue::Uint8(_) => matches!(expected, ValueEncoding::Uint8),
        MessageParameterValue::VarInt(_) => matches!(expected, ValueEncoding::VarInt),
        MessageParameterValue::Location { .. } => matches!(expected, ValueEncoding::Location),
        MessageParameterValue::LengthPrefixed(_) => {
            matches!(expected, ValueEncoding::LengthPrefixed)
                && !matches!(
                    param.param_type,
                    PARAM_AUTHORIZATION_TOKEN | PARAM_FILL_PARAMETERS
                )
        }
        MessageParameterValue::FillParameters(_) => {
            matches!(expected, ValueEncoding::LengthPrefixed)
                && param.param_type == PARAM_FILL_PARAMETERS
        }
        MessageParameterValue::AuthorizationToken(_) => {
            matches!(expected, ValueEncoding::LengthPrefixed)
                && param.param_type == PARAM_AUTHORIZATION_TOKEN
        }
        MessageParameterValue::TrackNamespacePrefix(_) => {
            matches!(expected, ValueEncoding::TrackNamespacePrefix)
        }
    };
    if !actual_matches {
        return Err(MessageError::InvalidParameter);
    }

    // draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter) / §9.20.9 (GROUP ORDER Parameter) /
    // draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
    // decode 側と同じ値域検証を行う
    if let MessageParameterValue::Uint8(v) = &param.value {
        validate_uint8_param_value(param.param_type, *v)?;
    }

    if param.param_type == PARAM_LOCATION_FILTER
        && let MessageParameterValue::LengthPrefixed(bytes) = &param.value
    {
        validate_location_filter_bytes(bytes)?;
    }

    // draft-ietf-moq-transport-21 §9.20.16 (FILL PARAMETERS Parameter):
    // encode 側でも内側スコープ (Table 6) を検証し、decode 側との非対称を防ぐ。
    // 型 0x23 が FillParameters variant のみを受理することは上の match で保証済み。
    // 内側の重複・値域検証は `encode_value` 側の `inner.encode()` が行う。
    if let MessageParameterValue::FillParameters(inner) = &param.value {
        inner.validate_scope(FILL_PARAMETERS_ALLOWED_PARAMS)?;
    }

    Ok(())
}

/// LOCATION_FILTER の値バイト列を検証する
///
/// 空バイト列 (Length 0 = no filter) はフィルタ値を持たない削除指示であり、
/// `LocationFilter::decode` の対象外として受け付ける。
fn validate_location_filter_bytes(bytes: &[u8]) -> Result<(), MessageError> {
    if bytes.is_empty() {
        return Ok(());
    }
    LocationFilter::decode(bytes)?;
    Ok(())
}

/// FILL_PARAMETERS の値バイト列を内側パラメータ群としてデコードする
///
/// draft-ietf-moq-transport-21 §9.20.16 (FILL PARAMETERS Parameter):
/// 値は独立メッセージの Parameters としてのエンコーディング
/// (draft-ietf-moq-transport-21 §16.7 (Message Parameters)) であり、
/// 空バイト列は内側パラメータなし (subscription の値を使う) として受け付ける。
/// 内側の末尾に余剰バイトがあれば KEY_VALUE_FORMATTING_ERROR、
/// Table 6 外のパラメータがあれば PROTOCOL_VIOLATION を返す。
/// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
fn decode_fill_parameters(bytes: &[u8]) -> Result<MessageParameters, MessageError> {
    if bytes.is_empty() {
        return Ok(MessageParameters::new());
    }
    let (inner, consumed) = MessageParameters::decode_inner(bytes, false)?;
    if consumed != bytes.len() {
        return Err(MessageError::KeyValueFormattingError(
            "FILL_PARAMETERS has trailing bytes",
        ));
    }
    inner.validate_scope(FILL_PARAMETERS_ALLOWED_PARAMS)?;
    Ok(inner)
}

/// 値をバッファにエンコードする
fn encode_value(value: &MessageParameterValue, buf: &mut Vec<u8>) -> Result<(), MessageError> {
    match value {
        MessageParameterValue::Uint8(v) => {
            buf.push(*v);
        }
        MessageParameterValue::VarInt(v) => {
            varint::encode(*v, buf);
        }
        MessageParameterValue::Location { group, object } => {
            varint::encode(*group, buf);
            varint::encode(*object, buf);
        }
        MessageParameterValue::LengthPrefixed(bytes) => {
            // draft-ietf-moq-transport-21 §8.3 (Key-Value-Pair Structure): 値長上限は 2^16-1 バイト
            if bytes.len() > 65535 {
                return Err(MessageError::ProtocolViolation(
                    "message parameter value length exceeds 65535",
                ));
            }
            varint::encode(bytes.len() as u64, buf);
            buf.extend_from_slice(bytes);
        }
        MessageParameterValue::AuthorizationToken(token) => {
            let bytes = token.encode_to_bytes();
            varint::encode(bytes.len() as u64, buf);
            buf.extend_from_slice(&bytes);
        }
        MessageParameterValue::FillParameters(inner) => {
            // draft-ietf-moq-transport-21 §9.20.16 (FILL PARAMETERS Parameter):
            // 内側は独立メッセージの Parameters としてエンコードする
            // (draft-ietf-moq-transport-21 §16.7 (Message Parameters))。
            let mut inner_buf = Vec::new();
            inner.encode(&mut inner_buf)?;
            // draft-ietf-moq-transport-21 §8.3 (Key-Value-Pair Structure): 値長上限は 2^16-1 バイト
            if inner_buf.len() > 65535 {
                return Err(MessageError::ProtocolViolation(
                    "message parameter value length exceeds 65535",
                ));
            }
            varint::encode(inner_buf.len() as u64, buf);
            buf.extend_from_slice(&inner_buf);
        }
        MessageParameterValue::TrackNamespacePrefix(ns) => {
            // draft-ietf-moq-transport-21 §9.20.21 (TRACK_NAMESPACE_PREFIX Parameter): Track Namespace 形式でエンコード
            ns.encode_to(buf)?;
        }
    }
    Ok(())
}

/// エンコーディング種別に従って値をデコードする
fn decode_value(
    encoding: ValueEncoding,
    buf: &[u8],
) -> Result<(MessageParameterValue, usize), MessageError> {
    match encoding {
        ValueEncoding::Uint8 => {
            if buf.is_empty() {
                return Err(MessageError::UnexpectedEof);
            }
            Ok((MessageParameterValue::Uint8(buf[0]), 1))
        }
        ValueEncoding::VarInt => {
            let (v, n) = varint::decode(buf)?;
            Ok((MessageParameterValue::VarInt(v), n))
        }
        ValueEncoding::Location => {
            let (group, n1) = varint::decode(buf)?;
            let (object, n2) = varint::decode(&buf[n1..])?;
            Ok((MessageParameterValue::Location { group, object }, n1 + n2))
        }
        ValueEncoding::LengthPrefixed => {
            let (len, n) = varint::decode(buf)?;
            // draft-ietf-moq-transport-21 §8.3 (Key-Value-Pair Structure): 値長上限は 2^16-1 バイト
            if len > 65535 {
                return Err(MessageError::ProtocolViolation(
                    "message parameter value length exceeds 65535",
                ));
            }
            let len_usize = varint::checked_len(len, buf[n..].len())?;
            let bytes = buf[n..n + len_usize].to_vec();
            Ok((MessageParameterValue::LengthPrefixed(bytes), n + len_usize))
        }
        ValueEncoding::TrackNamespacePrefix => {
            // draft-ietf-moq-transport-21 §9.20.21 (TRACK_NAMESPACE_PREFIX Parameter): Track Namespace 形式でデコード
            let mut pos = 0;
            let ns = TrackNamespace::decode_from(buf, &mut pos)?;
            Ok((MessageParameterValue::TrackNamespacePrefix(ns), pos))
        }
    }
}

/// AUTHORIZATION_TOKEN の (Token Type, Token Value) 一意性を検証する
///
/// draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter):
/// "The AUTHORIZATION TOKEN parameter MAY be repeated within a message as long as the
/// combination of Token Type and Token Value are unique after resolving any aliases."
///
/// USE_ALIAS の alias 解決はセッション状態に依存するため、コーデック層では検証不可能。
/// REGISTER / USE_VALUE の (Token Type, Token Value) のみを対象とする。
///
/// 線形走査ではトークン数の 2 乗の比較になるため、no_std 対応の
/// `hashbrown::HashSet` で重複を判定する。
pub(crate) fn validate_auth_token_uniqueness<'a>(
    tokens: impl Iterator<Item = &'a AuthorizationToken>,
) -> Result<(), MessageError> {
    let mut seen: HashSet<(u64, &[u8])> = HashSet::new();
    for token in tokens {
        if let Some((token_type, token_value)) = token.token_type_value()
            && !seen.insert((token_type, token_value))
        {
            return Err(MessageError::MalformedAuthToken(
                "duplicate (Token Type, Token Value) in AUTHORIZATION_TOKEN parameters",
            ));
        }
    }
    Ok(())
}

// ─── Range Filter 内部構造パース (draft-ietf-moq-transport-21 §3.3.2) ───

/// デコード済みの Range Filter インスタンス 1 つ (draft-ietf-moq-transport-21 §3.3.2 (Range Filters))
///
/// ワイヤ上のデルタエンコーディングを解決した絶対値の範囲列を持つ。
///
/// §3.3.2: "Filter parameters with the same SetID are AND'd; distinct SetIDs are OR'd."
/// つまり評価側は `set_id` でグループ化して AND し、グループ間を OR する。
/// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeFilterSet {
    /// SetID (8 bits)
    pub set_id: u8,
    /// Property Type。OBJECT_PROPERTY_FILTER (0x28) / TRACK_PROPERTY_FILTER (0x29) のみ `Some`
    pub property_type: Option<u64>,
    /// inclusive な範囲の並び。End が `None` の要素は上限なし
    /// (§3.3.2: "End is optional in the last pair; if omitted it indicates the last Range is open-ended.")
    pub ranges: Vec<(u64, Option<u64>)>,
}

impl RangeFilterSet {
    /// 値がいずれかの範囲に含まれるか判定する
    ///
    /// 範囲が 1 つも無い場合 (Length=0 や SetID のみのインスタンス) は `false` を返す。
    /// 空の Range Filter は「何も通さない」ではなく「フィルタとして機能しない」ため、
    /// 呼び出し側は範囲を持たないインスタンスを評価対象から外すこと。
    pub fn contains(&self, value: u64) -> bool {
        self.ranges.iter().any(|(start, end)| match end {
            Some(end) => value >= *start && value <= *end,
            None => value >= *start,
        })
    }
}

/// Range Filter のバイト列をデコードして [`RangeFilterSet`] にする
///
/// デルタは §3.3.2 の規則で解決する。最初の Start は 0 から、以降の Start は直前の End から、
/// End は同じ Range の Start からのデルタである。パースできない場合は `None` を返す。
fn decode_range_filter_set(param_type: u64, bytes: &[u8]) -> Option<RangeFilterSet> {
    let (set_id, property_type, mut pos) = parse_range_filter_header(param_type, bytes)?;
    let mut ranges = Vec::new();
    let mut prev_end: u64 = 0;
    while pos < bytes.len() {
        let (start_delta, n) = varint::decode(&bytes[pos..]).ok()?;
        pos += n;
        let start = prev_end.checked_add(start_delta)?;
        if pos < bytes.len() {
            let (end_delta, n) = varint::decode(&bytes[pos..]).ok()?;
            pos += n;
            let end = start.checked_add(end_delta)?;
            ranges.push((start, Some(end)));
            prev_end = end;
        } else {
            ranges.push((start, None));
            prev_end = start;
        }
    }
    Some(RangeFilterSet {
        set_id,
        property_type,
        ranges,
    })
}

/// Range Filter パラメータ型 (0x25-0x29) かどうかを返す
fn is_range_filter_type(param_type: u64) -> bool {
    matches!(
        param_type,
        PARAM_SUBGROUP_FILTER
            | PARAM_OBJECTID_FILTER
            | PARAM_PRIORITY_FILTER
            | PARAM_OBJECT_PROPERTY_FILTER
            | PARAM_TRACK_PROPERTY_FILTER
    )
}

/// Range Filter のヘッダ (SetID, Property Type) とヘッダ長をパースする
///
/// ワイヤフォーマット: SetID (8 bits) | [Property Type (vi64)] | Range...
/// Property Type は OBJECT_PROPERTY_FILTER (0x28) / TRACK_PROPERTY_FILTER (0x29) のみ。
/// 成功時は (set_id, property_type, header_len) を返す。property_type は 0x28/0x29 では Some、
/// それ以外は None。header_len (SetID + Property Type のバイト長) は範囲走査を開始する位置として
/// `count_ranges_in_filter` / `validate_range_filter_deltas` からも使用される。
/// ヘッダの解釈は本関数に集約する。
///
/// Length=0 (空バイト列) は SetID フィールドを持たないため `None` を返す。
/// 呼び出し側が Length=0 を事前に除外すること。
fn parse_range_filter_header(param_type: u64, bytes: &[u8]) -> Option<(u8, Option<u64>, usize)> {
    let set_id = *bytes.first()?;
    if matches!(
        param_type,
        PARAM_OBJECT_PROPERTY_FILTER | PARAM_TRACK_PROPERTY_FILTER
    ) {
        let (property_type, n) = varint::decode(&bytes[1..]).ok()?;
        // draft-ietf-moq-transport-21 §9.20.14 / §9.20.15: Property Type MUST be even
        // (KVP の偶数 Type = varint 値のみフィルタ可能。奇数 Type = バイト列は不可)
        if property_type % 2 != 0 {
            return None;
        }
        Some((set_id, Some(property_type), 1 + n))
    } else {
        Some((set_id, None, 1))
    }
}

/// Range Filter 内の Range 個数を数える
///
/// ワイヤフォーマット: SetID (8 bits) | [Property Type (vi64)] | Range...
/// Range { Start (vi64), [End (vi64)] } — 最終 Range の End は省略可能。
fn count_ranges_in_filter(param_type: u64, bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return Some(0);
    }
    let (_, _, mut pos) = parse_range_filter_header(param_type, bytes)?;
    let mut count: u64 = 0;
    while pos < bytes.len() {
        // Start (vi64)
        let (_, n) = varint::decode(&bytes[pos..]).ok()?;
        pos += n;
        count += 1;
        // End (vi64) — 省略可能 (最終 Range)
        if pos < bytes.len() {
            let (_, n) = varint::decode(&bytes[pos..]).ok()?;
            pos += n;
        }
    }
    Some(count)
}

/// Range Filter のデルタエンコーディング溢出と値域を検証する
///
/// draft-ietf-moq-transport-21 §3.3.2: "If adding the delta would exceed 2^64-1, the request MUST be
/// rejected with INVALID_FILTER."
/// draft-ietf-moq-transport-21 §9.20.13: "If a decoded value exceeds 255, the endpoint MUST reject
/// this with REQUEST_ERROR with error code INVALID_FILTER." (PRIORITY_FILTER の値 > 255)
///
/// 本 codec 層は `MessageError::ProtocolViolation` を返し、session 層 (`validate_range_filters` in
/// `src/session/core.rs`) がそれを wire code `REQUEST_INVALID_FILTER` にラップして REQUEST_ERROR
/// を送出する 2 層構造である。`MessageParameters::decode` 単体テストからは `PROTOCOL_VIOLATION`
/// として観測されるが、ワイヤ上の最終 code は draft の規定どおり `INVALID_FILTER`。
fn validate_range_filter_deltas(param_type: u64, bytes: &[u8]) -> Result<(), MessageError> {
    if bytes.is_empty() {
        return Ok(());
    }
    let (_, _, mut pos) = parse_range_filter_header(param_type, bytes).ok_or(
        MessageError::ProtocolViolation("malformed Range Filter header"),
    )?;
    let is_priority_filter = param_type == PARAM_PRIORITY_FILTER;
    // デルタ基底: 最初の Start は 0 から、以降の Start は直前 End から、End は同 Start から
    let mut prev_end: u64 = 0;
    while pos < bytes.len() {
        let (start_delta, n) = varint::decode(&bytes[pos..])
            .map_err(|_| MessageError::ProtocolViolation("malformed Range Filter Start delta"))?;
        pos += n;
        let start = prev_end
            .checked_add(start_delta)
            .ok_or(MessageError::ProtocolViolation(
                "Range Filter Start delta overflows u64",
            ))?;
        // draft-ietf-moq-transport-21 §9.20.13: Publisher Priority は 8 bit フィールド
        if is_priority_filter && start > 255 {
            return Err(MessageError::ProtocolViolation(
                "PRIORITY_FILTER value exceeds 255",
            ));
        }
        if pos < bytes.len() {
            let (end_delta, n) = varint::decode(&bytes[pos..])
                .map_err(|_| MessageError::ProtocolViolation("malformed Range Filter End delta"))?;
            pos += n;
            let end = start
                .checked_add(end_delta)
                .ok_or(MessageError::ProtocolViolation(
                    "Range Filter End delta overflows u64",
                ))?;
            // draft-ietf-moq-transport-21 §9.20.13: Publisher Priority は 8 bit フィールド
            if is_priority_filter && end > 255 {
                return Err(MessageError::ProtocolViolation(
                    "PRIORITY_FILTER value exceeds 255",
                ));
            }
            prev_end = end;
        } else {
            // 最終 Range が End なし (open-ended)
            prev_end = start;
        }
    }
    Ok(())
}
