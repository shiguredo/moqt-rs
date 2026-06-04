//! Track Properties (draft-ietf-moq-transport-21 §8.4 (Track and Object Properties))
//!
//! Track Properties は KVP 形式 (Setup Options と同一)。
//! カウントプレフィックスなし、メッセージペイロードの残り全体を読む。
//! 偶数型は varint 値、奇数型は長さ付きバイト列。
//!
//! SUBSCRIBE_OK, PUBLISH, FETCH_OK のメッセージ末尾に含まれる。
//! Message Parameters の後に配置される.
//!
//! # 対象 Property (draft-ietf-moq-transport-21 §16.8 (Properties) Table 14)
//!
//! | Type | Name                          | Scope        | Spec |
//! |-----:|-------------------------------|--------------|------|
//! | 0x02 | OBJECT_DELIVERY_TIMEOUT       | Track,Object | draft-ietf-moq-transport-21 §10.2 (OBJECT_DELIVERY_TIMEOUT) |
//! | 0x04 | MAX_CACHE_DURATION            | Track        | draft-ietf-moq-transport-21 §10.3 (MAX CACHE DURATION) |
//! | 0x06 | SUBGROUP_DELIVERY_TIMEOUT     | Track,Object | draft-ietf-moq-transport-21 §10.1 (SUBGROUP_DELIVERY_TIMEOUT) |
//! | 0x0B | IMMUTABLE_PROPERTIES          | Track,Object | draft-ietf-moq-transport-21 §10.7 (Immutable Properties) |
//! | 0x0E | DEFAULT_PUBLISHER_PRIORITY    | Track        | draft-ietf-moq-transport-21 §10.4 (DEFAULT PUBLISHER PRIORITY) |
//! | 0x22 | DEFAULT_PUBLISHER_GROUP_ORDER | Track        | draft-ietf-moq-transport-21 §10.5 (DEFAULT PUBLISHER GROUP ORDER) |
//! | 0x30 | DYNAMIC_GROUPS                | Track        | draft-ietf-moq-transport-21 §10.6 (DYNAMIC GROUPS) |
//!
//! 書式は `object_properties` モジュールの対応表と揃えている。
use crate::{error::MessageError, varint};
use alloc::vec::Vec;

/// OBJECT_DELIVERY_TIMEOUT (Property Type 0x02, draft-ietf-moq-transport-21 §10.2 (OBJECT_DELIVERY_TIMEOUT))
pub const PROP_OBJECT_DELIVERY_TIMEOUT: u64 = 0x02;
/// MAX_CACHE_DURATION (Property Type 0x04, draft-ietf-moq-transport-21 §10.3 (MAX CACHE DURATION))
///
/// この節番号・定義は draft-ietf-moq-transport-21 由来であり、将来の draft 改版で
/// 変わる可能性がある。
pub const PROP_MAX_CACHE_DURATION: u64 = 0x04;
/// SUBGROUP_DELIVERY_TIMEOUT (Property Type 0x06, draft-ietf-moq-transport-21 §10.1 (SUBGROUP_DELIVERY_TIMEOUT))
pub const PROP_SUBGROUP_DELIVERY_TIMEOUT: u64 = 0x06;
/// DEFAULT_PUBLISHER_PRIORITY (Property Type 0x0E)
pub const PROP_DEFAULT_PUBLISHER_PRIORITY: u64 = 0x0E;
/// DEFAULT_PUBLISHER_GROUP_ORDER (Property Type 0x22)
pub const PROP_DEFAULT_PUBLISHER_GROUP_ORDER: u64 = 0x22;
/// DYNAMIC_GROUPS (Property Type 0x30)
pub const PROP_DYNAMIC_GROUPS: u64 = 0x30;
/// IMMUTABLE_PROPERTIES (Property Type 0x0B, draft-ietf-moq-transport-21 §10.7 (Immutable Properties))
///
/// 奇数型だが値は「入れ子の Track Property Key-Value-Pair 列」。Original Publisher のみが
/// 付与でき、Relay は改変・削除してはならない。入れ子の IMMUTABLE_PROPERTIES は禁止
/// (draft-ietf-moq-transport-21 §10.7 (Immutable Properties))。この節番号は draft-ietf-moq-transport-21 由来であり、将来の
/// draft 改版で変わる可能性がある。
pub const PROP_IMMUTABLE_PROPERTIES: u64 = 0x0B;

/// 必須トラックプロパティの範囲下限 (draft-ietf-moq-transport-21 §3.6 (Mandatory Track Properties))
pub const MANDATORY_TRACK_PROPERTY_MIN: u64 = 0x4000;
/// 必須トラックプロパティの範囲上限 (draft-ietf-moq-transport-21 §3.6 (Mandatory Track Properties))
pub const MANDATORY_TRACK_PROPERTY_MAX: u64 = 0x7FFF;

/// Track Property の値
///
/// - 偶数型: varint 値
/// - 奇数型: 長さ付きバイト列 (最大 65535 バイト)
#[derive(Debug, Clone, PartialEq)]
pub enum TrackPropertyValue {
    /// 偶数型の値 (varint)
    VarInt(u64),
    /// 奇数型の値 (バイト列、最大 65535 バイト)
    Bytes(Vec<u8>),
}

/// 単一の Track Property
#[derive(Debug, Clone, PartialEq)]
pub struct TrackProperty {
    /// プロパティ型 (draft-ietf-moq-transport-21 §8.4)
    pub prop_type: u64,
    /// プロパティ値 (偶数型は varint、奇数型はバイト列)
    pub value: TrackPropertyValue,
}

/// Track Properties リスト (draft-ietf-moq-transport-21 §8.4 (Track and Object Properties))
///
/// カウントプレフィックスなし。エンコード時に `prop_type` の昇順にソートして
/// デルタエンコードを適用する。空の場合は 0 バイトにエンコードされる。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrackProperties(Vec<TrackProperty>);

impl TrackProperties {
    /// 空のコレクションを作成する
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// プロパティを末尾に追加する
    pub fn push(&mut self, p: TrackProperty) {
        self.0.push(p);
    }

    /// プロパティ数を返す
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// コレクションが空かどうかを返す
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 内部のプロパティスライスを返す
    pub fn as_slice(&self) -> &[TrackProperty] {
        &self.0
    }

    /// Track Properties をバッファにエンコードする (draft-ietf-moq-transport-21 §8.4 (Track and Object Properties))
    ///
    /// カウントプレフィックスなし。デルタエンコードされた KVP を直接書き出す。
    /// 空の場合は何も書き出さない (0 バイト)。
    ///
    /// # Errors
    ///
    /// - 同一 `prop_type` の重複: `ProtocolViolation`
    /// - バイト列が 65535 バイトを超える: `PayloadTooLong`
    pub fn encode(&self, buf: &mut Vec<u8>) -> Result<(), MessageError> {
        let mut sorted = self.0.clone();
        sorted.sort_by_key(|p| p.prop_type);

        // 重複 prop_type を検出する
        // Track Properties には AUTHORIZATION_TOKEN のような重複許容プロパティが存在しないため一律拒否する
        for w in sorted.windows(2) {
            if w[0].prop_type == w[1].prop_type {
                return Err(MessageError::ProtocolViolation(
                    "duplicate track property type",
                ));
            }
        }

        let mut prev_type: u64 = 0;
        for prop in &sorted {
            // draft-ietf-moq-transport-21 §8.4 (Track and Object Properties): 偶数型は varint、奇数型は長さ付きバイト列
            // 型と値形式の整合性を検証する
            match (&prop.value, prop.prop_type % 2) {
                (TrackPropertyValue::VarInt(_), 0) => {}
                (TrackPropertyValue::Bytes(_), 1) => {}
                _ => return Err(MessageError::InvalidParameter),
            }
            // draft-ietf-moq-transport-21 §10.4 (DEFAULT PUBLISHER PRIORITY) / draft-ietf-moq-transport-21 §10.5 (DEFAULT PUBLISHER GROUP ORDER) / draft-ietf-moq-transport-21 §10.6 (DYNAMIC GROUPS): 既知 Track Property の値域を検証する
            validate_track_property_value_range(prop.prop_type, &prop.value)?;
            crate::kvp::encode_delta_key(prev_type, prop.prop_type, buf);
            match &prop.value {
                TrackPropertyValue::VarInt(v) => {
                    varint::encode(*v, buf);
                }
                TrackPropertyValue::Bytes(bytes) => {
                    // draft-ietf-moq-transport-21 §8.4 (Track and Object Properties):
                    // 奇数型の値長上限は 2^16-1 バイト。object_properties / loc と同様に
                    // encode 側でも検証し、上限超過のワイヤーフォーマット生成を防ぐ。
                    if bytes.len() > 65535 {
                        return Err(MessageError::PayloadTooLong);
                    }
                    // draft-ietf-moq-transport-21 §10.7 (Immutable Properties): IMMUTABLE_PROPERTIES の内側を Track Property KVP 列として
                    // 検証する (入れ子の 0x0B を拒否)。検証のみで、バイト列はそのまま再出力する
                    // (draft-ietf-moq-transport-21 §10.7 (Immutable Properties)「serialization MUST NOT change」)。
                    if prop.prop_type == PROP_IMMUTABLE_PROPERTIES {
                        decode_track_kv_pairs(bytes, false)?;
                    }
                    varint::encode(bytes.len() as u64, buf);
                    buf.extend_from_slice(bytes);
                }
            }
            prev_type = prop.prop_type;
        }
        Ok(())
    }

    /// バッファ全体から Track Properties をデコードする
    ///
    /// カウントプレフィックスなし。バッファ末尾まで KVP を読む。
    /// 空バッファの場合は空の `TrackProperties` を返す。
    /// IMMUTABLE_PROPERTIES (0x0B) は内側を Track Property KVP 列として検証する (draft-ietf-moq-transport-21 §10.7 (Immutable Properties))。
    pub fn decode(buf: &[u8]) -> Result<Self, MessageError> {
        Ok(Self(decode_track_kv_pairs(buf, true)?))
    }

    /// 偶数型 Track Property から varint 値を取得する
    ///
    /// draft-ietf-moq-transport-21 §10.7 (Immutable Properties)「MUST search both」 に従い、mutable リストに無ければ
    /// IMMUTABLE_PROPERTIES (0x0B) の内側も探索する。mutable リストを優先する。
    pub fn find_varint(&self, prop_type: u64) -> Option<u64> {
        // mutable リストを先に、続けて IMMUTABLE_PROPERTIES の内側を探索する (mutable 優先)
        let inner = self.immutable_inner_properties();
        for p in self.0.iter().chain(inner.iter()) {
            if p.prop_type == prop_type
                && let TrackPropertyValue::VarInt(v) = p.value
            {
                return Some(v);
            }
        }
        None
    }

    /// DYNAMIC_GROUPS (Property Type 0x30) の値を返す
    ///
    /// draft-ietf-moq-transport-21 §10.6 (DYNAMIC GROUPS): 0 または 1 のみ。`1` は dynamic groups を
    /// サポートすることを示す。parameter が含まれない場合は `None`。
    pub fn dynamic_groups(&self) -> Option<u64> {
        self.find_varint(PROP_DYNAMIC_GROUPS)
    }

    /// DEFAULT_PUBLISHER_PRIORITY (Property Type 0x0E) の値を返す
    ///
    /// draft-ietf-moq-transport-21 §10.4 (DEFAULT PUBLISHER PRIORITY): 0-255。
    /// "Subgroups and Datagrams for this subscription inherit this priority, unless they
    /// specifically override it." / "If omitted, the Default Publisher Priority is 128."
    ///
    /// 省略時の 128 は適用せず `None` を返す。デフォルト適用は「宣言されていない」ことを
    /// 区別できる呼び出し側 (Session) の責務とする。`u8` に収まらない値は
    /// `push` による in-memory 構築でのみ起こりうるため、その場合も `None` を返す。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub fn default_publisher_priority(&self) -> Option<u8> {
        self.find_varint(PROP_DEFAULT_PUBLISHER_PRIORITY)
            .and_then(|v| u8::try_from(v).ok())
    }

    /// DEFAULT_PUBLISHER_GROUP_ORDER (Property Type 0x22) の値を返す
    ///
    /// draft-ietf-moq-transport-21 §10.5 (DEFAULT PUBLISHER GROUP ORDER): Ascending (0x1) または
    /// Descending (0x2)。"If omitted, the publisher's preference is Ascending (0x1)."
    ///
    /// これは **publisher の選好** であり、§9.20.9 (GROUP ORDER Parameter) が運ぶ
    /// subscriber からの要求 (`Subscription::group_order`) とは別の値である。
    /// 省略時の Ascending は適用せず `None` を返す。`u8` に収まらない値は
    /// `push` による in-memory 構築でのみ起こりうるため、その場合も `None` を返す。
    pub fn default_publisher_group_order(&self) -> Option<u8> {
        self.find_varint(PROP_DEFAULT_PUBLISHER_GROUP_ORDER)
            .and_then(|v| u8::try_from(v).ok())
    }

    /// DELIVERY_TIMEOUT (Property Type 0x02) の値を返す
    pub fn object_delivery_timeout(&self) -> Option<u64> {
        self.find_varint(PROP_OBJECT_DELIVERY_TIMEOUT)
    }

    /// SUBGROUP_DELIVERY_TIMEOUT (Property Type 0x06) の値を返す (draft-ietf-moq-transport-21 §10.1)
    pub fn subgroup_delivery_timeout(&self) -> Option<u64> {
        self.find_varint(PROP_SUBGROUP_DELIVERY_TIMEOUT)
    }

    /// prop_type が既知の必須プロパティかどうかを返す (draft-ietf-moq-transport-21 §3.6 (Mandatory Track Properties))
    ///
    /// 0x4000-0x7FFF 範囲のうち、この実装が認識しているプロパティ型を返す。
    /// draft-ietf-moq-transport-21 時点ではこの範囲に定義済みのプロパティは存在しない。
    fn is_known_mandatory(_prop_type: u64) -> bool {
        // draft-ietf-moq-transport-21 時点では 0x4000-0x7FFF 範囲の既知プロパティは定義されていない
        false
    }

    /// 未知の必須プロパティ (0x4000-0x7FFF で未定義のもの) を含むかどうかを返す
    ///
    /// draft-ietf-moq-transport-21 §3.6 (Mandatory Track Properties): 未知の必須プロパティを受信したエンドポイントは
    /// PUBLISH なら REQUEST_ERROR (UNSUPPORTED_EXTENSION)、SUBSCRIBE_OK なら購読キャンセルを行う。
    /// draft-ietf-moq-transport-21 §10.7 (Immutable Properties)「MUST search both」に従い、mutable リストに加えて
    /// IMMUTABLE_PROPERTIES (0x0B) の内側も探索する。
    pub fn has_unknown_mandatory(&self) -> bool {
        let is_unknown_mandatory = |prop_type: u64| {
            (MANDATORY_TRACK_PROPERTY_MIN..=MANDATORY_TRACK_PROPERTY_MAX).contains(&prop_type)
                && !Self::is_known_mandatory(prop_type)
        };
        if self.0.iter().any(|p| is_unknown_mandatory(p.prop_type)) {
            return true;
        }
        // IMMUTABLE_PROPERTIES の内側も探索する
        self.immutable_inner_properties()
            .iter()
            .any(|p| is_unknown_mandatory(p.prop_type))
    }

    /// IMMUTABLE_PROPERTIES (0x0B) の内側 Track Property 列をデコードして返す
    ///
    /// draft-ietf-moq-transport-21 §10.7 (Immutable Properties)「MUST search both」の統合検索に使う。IMMUTABLE_PROPERTIES が無い場合や、
    /// 内側が破損している (例: `push` で積んだ不正値) 場合は空 `Vec` を返す。getter は `Option` /
    /// bool しか返せずエラーを表現できないため、破損時は探索を打ち切る (mutable で見つからなければ
    /// `None` 相当)。decode 経由で構築された値の内側は decode 時に検証済みのため妥当である。
    ///
    /// 最初の IMMUTABLE_PROPERTIES (0x0B) のみを対象とする。encode / decode は同一 prop_type の重複を
    /// 拒否するため top-level の 0x0B は高々 1 つだが、`push` で複数積んだ破損入力では 2 つ目以降の
    /// 内側を探索しない (上記のとおり破損入力では探索を打ち切る方針)。
    fn immutable_inner_properties(&self) -> Vec<TrackProperty> {
        for p in &self.0 {
            if p.prop_type == PROP_IMMUTABLE_PROPERTIES
                && let TrackPropertyValue::Bytes(ref b) = p.value
            {
                return decode_track_kv_pairs(b, false).unwrap_or_default();
            }
        }
        Vec::new()
    }
}

/// 既知 Track Property の値域を検証する (draft-ietf-moq-transport-21 §10.4 (DEFAULT PUBLISHER PRIORITY) / draft-ietf-moq-transport-21 §10.5 (DEFAULT PUBLISHER GROUP ORDER) / draft-ietf-moq-transport-21 §10.6 (DYNAMIC GROUPS))
///
/// encode と decode の両方から呼び、mutable リストと IMMUTABLE_PROPERTIES の内側に同一の
/// 値域検証を適用する。
fn validate_track_property_value_range(
    prop_type: u64,
    value: &TrackPropertyValue,
) -> Result<(), MessageError> {
    match (prop_type, value) {
        // draft-ietf-moq-transport-21 §10.4 (DEFAULT PUBLISHER PRIORITY): DEFAULT_PUBLISHER_PRIORITY は 0-255 のみ
        (PROP_DEFAULT_PUBLISHER_PRIORITY, TrackPropertyValue::VarInt(v)) if *v > 255 => Err(
            MessageError::ProtocolViolation("DEFAULT_PUBLISHER_PRIORITY must be 0..255"),
        ),
        // draft-ietf-moq-transport-21 §10.5 (DEFAULT PUBLISHER GROUP ORDER): DEFAULT_PUBLISHER_GROUP_ORDER は 1 (Ascending) または 2 (Descending) のみ
        (PROP_DEFAULT_PUBLISHER_GROUP_ORDER, TrackPropertyValue::VarInt(v))
            if *v != 1 && *v != 2 =>
        {
            Err(MessageError::ProtocolViolation(
                "DEFAULT_PUBLISHER_GROUP_ORDER must be 1 (Ascending) or 2 (Descending)",
            ))
        }
        // draft-ietf-moq-transport-21 §10.6 (DYNAMIC GROUPS): DYNAMIC_GROUPS は 0 または 1 のみ
        (PROP_DYNAMIC_GROUPS, TrackPropertyValue::VarInt(v)) if *v > 1 => Err(
            MessageError::ProtocolViolation("DYNAMIC_GROUPS must be 0 or 1"),
        ),
        _ => Ok(()),
    }
}

/// Track Property の Key-Value-Pair 列をバッファ全体からデコードする (draft-ietf-moq-transport-21 §8.4 (Track and Object Properties))
///
/// カウントプレフィックスなしで `buf` 末尾まで読む。`TrackProperties::decode` 本体、
/// IMMUTABLE_PROPERTIES (0x0B) の内側検証 (encode / decode)、統合検索の内側展開の各所から呼ばれる。
///
/// `allow_immutable` が false の場合、IMMUTABLE_PROPERTIES (0x0B) の出現を拒否する。入れ子の
/// IMMUTABLE_PROPERTIES は draft-ietf-moq-transport-21 §10.7 (Immutable Properties) により malformed である。なお draft-ietf-moq-transport-21 §10.7 (Immutable Properties) の
/// malformed 条件文は "An Object contains ..." と Object を主語にするが、draft-ietf-moq-transport-21 §10.7 (Immutable Properties) 冒頭 が
/// IMMUTABLE の中身を Track or Object Property と対称に定義することを根拠に、Track scope でも入れ子を
/// 禁止する。
///
/// Object scope (`object_properties::decode_kv_pairs`) と異なり、必須トラックプロパティ範囲
/// (0x4000-0x7FFF) は拒否しない。これらは Track scope では正当な必須プロパティであり、draft-ietf-moq-transport-21 §3.6 (Mandatory Track Properties)
/// は Object scope でのみ malformed と規定する (極性が逆転する)。
fn decode_track_kv_pairs(
    buf: &[u8],
    allow_immutable: bool,
) -> Result<Vec<TrackProperty>, MessageError> {
    let mut pos = 0;
    let mut props = Vec::new();
    let mut prev_type: u64 = 0;

    while pos < buf.len() {
        let (prop_type, delta) = crate::kvp::decode_delta_key(prev_type, buf, &mut pos)?;

        // delta == 0 は重複 (最初のエントリを除く)
        // 先頭エントリは prev_type=0 からの delta であり prop_type=0 を意味する正規エンコードで重複ではない
        if delta == 0 && !props.is_empty() {
            return Err(MessageError::ProtocolViolation(
                "duplicate track property type",
            ));
        }

        // draft-ietf-moq-transport-21 §10.7 (Immutable Properties): 入れ子の IMMUTABLE_PROPERTIES は禁止
        if !allow_immutable && prop_type == PROP_IMMUTABLE_PROPERTIES {
            return Err(MessageError::ProtocolViolation(
                "nested IMMUTABLE_PROPERTIES is not allowed (draft-ietf-moq-transport-21 §10.7 (Immutable Properties))",
            ));
        }

        let value = if prop_type % 2 == 0 {
            // 偶数型: varint 値
            let (v, n) = varint::decode(&buf[pos..])?;
            pos += n;
            TrackPropertyValue::VarInt(v)
        } else {
            // 奇数型: 長さ付きバイト列
            let (len, n) = varint::decode(&buf[pos..])?;
            pos += n;
            if len > 65535 {
                return Err(MessageError::ProtocolViolation(
                    "track property value too long",
                ));
            }
            let len = varint::checked_len(len, buf[pos..].len())?;
            let bytes = buf[pos..pos + len].to_vec();
            pos += len;

            // draft-ietf-moq-transport-21 §10.7 (Immutable Properties): IMMUTABLE_PROPERTIES 自体は許可するが、内側を Track Property KVP 列
            // として検証する (内側の入れ子 0x0B を拒否する)。空 (length=0) の内側は許容される。
            if prop_type == PROP_IMMUTABLE_PROPERTIES {
                decode_track_kv_pairs(&bytes, false)?;
            }

            TrackPropertyValue::Bytes(bytes)
        };

        // draft-ietf-moq-transport-21 §10.4 (DEFAULT PUBLISHER PRIORITY) / draft-ietf-moq-transport-21 §10.5 (DEFAULT PUBLISHER GROUP ORDER) / draft-ietf-moq-transport-21 §10.6 (DYNAMIC GROUPS): 既知 Track Property の値域を検証する
        validate_track_property_value_range(prop_type, &value)?;

        props.push(TrackProperty { prop_type, value });
        prev_type = prop_type;
    }

    Ok(props)
}
