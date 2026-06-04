//! Object-scoped Properties (draft-ietf-moq-transport-21 §16.8 (Properties) Table 14)
//!
//! SUBGROUP_OBJECT / ObjectDatagram のヘッダに含まれる Object 単位の Properties を扱う。
//! Track-scoped Properties は `track_properties` モジュールを参照。
//!
//! # ワイヤーフォーマット
//!
//! `Properties Length (varint) | Key-Value-Pairs...`
//!
//! Key-Value-Pair は draft-ietf-moq-transport-21 §8.3 (Key-Value-Pair Structure) の delta encoding。prop_type 昇順にソートし、
//! 差分を varint として書き出す。偶数型は varint 値、奇数型は長さ付きバイト列。
//!
//! # 対象 Property (draft-ietf-moq-transport-21 §16.8 (Properties) Table 14)
//!
//! | Type | Name                 | Scope        | Spec  |
//! |-----:|----------------------|--------------|-------|
//! | 0x02 | OBJECT_DELIVERY_TIMEOUT | Track,Object | draft-ietf-moq-transport-21 §10.2 (OBJECT_DELIVERY_TIMEOUT) |
//! | 0x06 | SUBGROUP_DELIVERY_TIMEOUT | Track,Object | draft-ietf-moq-transport-21 §10.1 (SUBGROUP_DELIVERY_TIMEOUT) |
//! | 0x0B | IMMUTABLE_PROPERTIES | Track,Object | draft-ietf-moq-transport-21 §10.7 (Immutable Properties) |
//! | 0x3C | PRIOR_GROUP_ID_GAP   | Object       | draft-ietf-moq-transport-21 §10.8 (Prior Group ID Gap) |
//! | 0x3E | PRIOR_OBJECT_ID_GAP  | Object       | draft-ietf-moq-transport-21 §10.9 (Prior Object ID Gap) |
//!
//! 0x02 / 0x06 は Track scope の定義元 (`track_properties` モジュール) と共通の型番号で、
//! Object scope では本モジュールのアクセサ (`object_delivery_timeout` / `subgroup_delivery_timeout`) から参照する。
//!
//! IMMUTABLE_PROPERTIES (0x0B) は奇数型だが、その値は「入れ子の Key-Value-Pair リスト」。
//! 入れ子の IMMUTABLE_PROPERTIES は許されず、検出した場合は PROTOCOL_VIOLATION
//! (draft-ietf-moq-transport-21 §10.7 (Immutable Properties): "An Object contains an Immutable Properties property that contains
//! another Immutable Properties key" → malformed)。
//!
//! # 注意
//!
//! 本モジュールの Property Type / scope / 解釈は draft 由来であり、将来の改訂で
//! 変更される可能性がある。

use crate::track_properties::{
    PROP_IMMUTABLE_PROPERTIES, PROP_OBJECT_DELIVERY_TIMEOUT, PROP_SUBGROUP_DELIVERY_TIMEOUT,
};
use crate::{error::MessageError, varint};
use alloc::vec::Vec;
use hashbrown::HashMap;

/// PRIOR_GROUP_ID_GAP (draft-ietf-moq-transport-21 §10.8 (Prior Group ID Gap), Property Type 0x3C)
///
/// 直前の存在しない Group の個数を示す varint。
pub const PROP_PRIOR_GROUP_ID_GAP: u64 = 0x3C;

/// PRIOR_OBJECT_ID_GAP (draft-ietf-moq-transport-21 §10.9 (Prior Object ID Gap), Property Type 0x3E)
///
/// 直前の存在しない Object の個数を示す varint。
pub const PROP_PRIOR_OBJECT_ID_GAP: u64 = 0x3E;

/// Object Property の値
///
/// - 偶数型: varint 値
/// - 奇数型: 長さ付きバイト列 (最大 65535 バイト)
///
/// IMMUTABLE_PROPERTIES (0x0B) は奇数型として `Bytes` で保持する。中身の
/// K-V ペアは再帰デコード時に検証される (nested IMMUTABLE_PROPERTIES は禁止)。
/// チャンクとしての解釈は application 層の責務。
#[derive(Debug, Clone, PartialEq)]
pub enum ObjectPropertyValue {
    /// 偶数型の値 (varint)
    VarInt(u64),
    /// 奇数型の値 (バイト列、最大 65535 バイト)
    Bytes(Vec<u8>),
}

/// 単一の Object Property
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectProperty {
    /// プロパティ型 (draft-ietf-moq-transport-21 §16.8 Table 14)
    pub prop_type: u64,
    /// プロパティ値 (偶数型は varint、奇数型はバイト列)
    pub value: ObjectPropertyValue,
}

/// Object-scoped Properties のコレクション
///
/// `encode()` / `decode()` は先頭の Properties Length varint を含む完全なブロックを扱う。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ObjectProperties(Vec<ObjectProperty>);

impl ObjectProperties {
    /// 空のコレクションを作成する
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// プロパティを末尾に追加する
    pub fn push(&mut self, prop: ObjectProperty) {
        self.0.push(prop);
    }

    /// コレクションが空かどうかを返す
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// プロパティ数を返す
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// 内部のプロパティスライスを返す
    pub fn as_slice(&self) -> &[ObjectProperty] {
        &self.0
    }

    /// 全プロパティのイテレータを返す
    pub fn iter(&self) -> impl Iterator<Item = &ObjectProperty> {
        self.0.iter()
    }

    /// Object Properties ブロック全体を `buf` に追記する
    ///
    /// 空の場合は Properties Length = 0 のみを書き出す (draft-ietf-moq-transport-21 §11.1.3 (Object Properties) / §11.3.1 (Subgroup Header))。
    /// prop_type の昇順にソートし、delta encoding で圧縮する。
    ///
    /// # Errors
    ///
    /// - 偶数型に `Bytes` 値、または奇数型に `VarInt` 値: `ProtocolViolation`
    /// - 同一 prop_type の重複: `ProtocolViolation`
    /// - IMMUTABLE_PROPERTIES の内側に IMMUTABLE_PROPERTIES を含む: `ProtocolViolation`
    /// - バイト列が 65535 バイトを超える: `PayloadTooLong`
    pub fn encode(&self, buf: &mut Vec<u8>) -> Result<(), MessageError> {
        if self.0.is_empty() {
            varint::encode(0, buf);
            return Ok(());
        }

        let mut sorted = self.0.clone();
        sorted.sort_by_key(|p| p.prop_type);

        // 重複 prop_type を検出する
        for w in sorted.windows(2) {
            if w[0].prop_type == w[1].prop_type {
                return Err(MessageError::ProtocolViolation(
                    "duplicate object property type",
                ));
            }
        }

        let mut inner = Vec::new();
        let mut prev_type: u64 = 0;

        for prop in &sorted {
            match (&prop.value, prop.prop_type % 2) {
                (ObjectPropertyValue::VarInt(_), 0) => {}
                (ObjectPropertyValue::Bytes(_), 1) => {}
                _ => {
                    return Err(MessageError::ProtocolViolation(
                        "object property type/value parity mismatch",
                    ));
                }
            }

            crate::kvp::encode_delta_key(prev_type, prop.prop_type, &mut inner);
            prev_type = prop.prop_type;

            match &prop.value {
                ObjectPropertyValue::VarInt(v) => {
                    varint::encode(*v, &mut inner);
                }
                ObjectPropertyValue::Bytes(b) => {
                    if b.len() > 65535 {
                        return Err(MessageError::PayloadTooLong);
                    }
                    // IMMUTABLE_PROPERTIES の内側に IMMUTABLE_PROPERTIES を含まないか検証する
                    if prop.prop_type == PROP_IMMUTABLE_PROPERTIES {
                        validate_immutable_inner(b)?;
                    }
                    varint::encode(b.len() as u64, &mut inner);
                    inner.extend_from_slice(b);
                }
            }
        }

        varint::encode(inner.len() as u64, buf);
        buf.extend_from_slice(&inner);
        Ok(())
    }

    /// バッファ先頭から Object Properties ブロックをデコードし `(properties, 消費バイト数)` を返す
    ///
    /// フォーマット: `Properties Length (varint) | Key-Value-Pairs...`
    pub fn decode(buf: &[u8]) -> Result<(Self, usize), MessageError> {
        let mut pos = 0;
        let (prop_len, n) = varint::decode(&buf[pos..])?;
        pos += n;

        if prop_len == 0 {
            return Ok((Self::new(), pos));
        }

        let prop_len = varint::checked_len(prop_len, buf[pos..].len())?;

        let inner = &buf[pos..pos + prop_len];
        pos += prop_len;

        let props = decode_kv_pairs(inner, true)?;
        Ok((Self(props), pos))
    }

    /// OBJECT_DELIVERY_TIMEOUT の値を返す (draft-ietf-moq-transport-21 §10.2)
    pub fn object_delivery_timeout(&self) -> Option<u64> {
        self.find_varint(PROP_OBJECT_DELIVERY_TIMEOUT)
    }

    /// SUBGROUP_DELIVERY_TIMEOUT の値を返す (draft-ietf-moq-transport-21 §10.1)
    pub fn subgroup_delivery_timeout(&self) -> Option<u64> {
        self.find_varint(PROP_SUBGROUP_DELIVERY_TIMEOUT)
    }

    /// PRIOR_GROUP_ID_GAP の値を返す
    pub fn prior_group_id_gap(&self) -> Option<u64> {
        self.find_varint(PROP_PRIOR_GROUP_ID_GAP)
    }

    /// PRIOR_OBJECT_ID_GAP の値を返す
    pub fn prior_object_id_gap(&self) -> Option<u64> {
        self.find_varint(PROP_PRIOR_OBJECT_ID_GAP)
    }

    /// IMMUTABLE_PROPERTIES の生バイト列を返す
    ///
    /// 中身は入れ子 K-V ペア列 (draft-ietf-moq-transport-21 §10.7 (Immutable Properties))。
    /// 検証はデコード時に済んでいるため、生バイト列の解釈は application 層の責務。
    pub fn immutable_properties(&self) -> Option<&[u8]> {
        for p in &self.0 {
            if p.prop_type == PROP_IMMUTABLE_PROPERTIES
                && let ObjectPropertyValue::Bytes(ref b) = p.value
            {
                return Some(b);
            }
        }
        None
    }

    /// IMMUTABLE_PROPERTIES (0x0B) の内側 KVP をデコードして返す
    ///
    /// 破損していれば空 Vec を返す (draft-ietf-moq-transport-21 §10.7 (Immutable Properties)「MUST search both」対応)。
    fn immutable_inner_properties(&self) -> Vec<ObjectProperty> {
        let Some(bytes) = self.immutable_properties() else {
            return Vec::new();
        };
        decode_kv_pairs(bytes, false).unwrap_or_default()
    }

    /// 偶数型 Object Property から varint 値を取得する
    ///
    /// draft-ietf-moq-transport-21 §10.7 (Immutable Properties)「MUST search both」 に従い、mutable リストに無ければ
    /// IMMUTABLE_PROPERTIES (0x0B) の内側も探索する。mutable リストを優先する。
    ///
    /// draft-ietf-moq-transport-21 §3.3.2 (Range Filters) の OBJECT_PROPERTY_FILTER は任意の
    /// 偶数 Property Type を対象にできるため、型別 accessor では足りず本メソッドを公開している。
    pub fn find_varint(&self, prop_type: u64) -> Option<u64> {
        // mutable リストを先に、続けて IMMUTABLE_PROPERTIES の内側を探索する (mutable 優先)
        let inner = self.immutable_inner_properties();
        for p in self.0.iter().chain(inner.iter()) {
            if p.prop_type == prop_type
                && let ObjectPropertyValue::VarInt(v) = p.value
            {
                return Some(v);
            }
        }
        None
    }
}

#[derive(Debug, Clone, Default)]
struct RangeSet(Vec<(u64, u64)>);

impl RangeSet {
    fn contains(&self, value: u64) -> bool {
        self.0
            .iter()
            .any(|&(start, end)| start <= value && value <= end)
    }

    fn overlaps(&self, start: u64, end: u64) -> bool {
        if start > end {
            return false;
        }
        self.0
            .iter()
            .any(|&(existing_start, existing_end)| !(end < existing_start || existing_end < start))
    }

    fn insert_point(&mut self, value: u64) {
        self.insert(value, value);
    }

    fn insert(&mut self, mut start: u64, mut end: u64) {
        if start > end {
            return;
        }

        let mut idx = 0;
        while idx < self.0.len() {
            let (existing_start, existing_end) = self.0[idx];
            if end.saturating_add(1) < existing_start {
                break;
            }
            if existing_end.saturating_add(1) < start {
                idx += 1;
                continue;
            }

            start = start.min(existing_start);
            end = end.max(existing_end);
            self.0.remove(idx);
        }

        self.0.insert(idx, (start, end));
    }
}

/// Object Properties 由来の malformed track 条件を追跡する
///
/// `PRIOR_GROUP_ID_GAP` / `PRIOR_OBJECT_ID_GAP` は単一 object のデコードだけでは
/// 検証しきれないため、受信済み object 列に対する状態を保持する。
/// 呼び出し側は track ごとに 1 つ保持し、Subgroup / Datagram / FETCH で受けた
/// object を到着順に `observe_object*` へ渡す。
#[derive(Debug, Clone, Default)]
pub struct ObjectPropertyTracker {
    seen_groups: RangeSet,
    prior_group_gaps: RangeSet,
    group_gap_values: HashMap<u64, u64>,
    seen_objects: HashMap<u64, RangeSet>,
    prior_object_gaps: HashMap<u64, RangeSet>,
}

impl ObjectPropertyTracker {
    /// 空のトラッカーを作成する
    pub fn new() -> Self {
        Self::default()
    }

    /// 受信した object を Properties 生バイト列つきで追跡する
    ///
    /// `properties_bytes` は `ObjectProperties::encode()` が出力する
    /// `Properties Length | Key-Value-Pairs` 全体を渡す。
    pub fn observe_object(
        &mut self,
        group_id: u64,
        object_id: u64,
        properties_bytes: Option<&[u8]>,
    ) -> Result<(), MessageError> {
        let properties = if let Some(bytes) = properties_bytes {
            let (properties, consumed) = ObjectProperties::decode(bytes).map_err(|_| {
                MessageError::ProtocolViolation("malformed track: invalid object properties")
            })?;
            if consumed != bytes.len() {
                return Err(MessageError::ProtocolViolation(
                    "malformed track: invalid object properties",
                ));
            }
            Some(properties)
        } else {
            None
        };

        self.observe_decoded_object(group_id, object_id, properties.as_ref())
    }

    /// 受信した object をデコード済み Properties つきで追跡する
    pub fn observe_decoded_object(
        &mut self,
        group_id: u64,
        object_id: u64,
        properties: Option<&ObjectProperties>,
    ) -> Result<(), MessageError> {
        if self.prior_group_gaps.contains(group_id) {
            return Err(MessageError::ProtocolViolation(
                "malformed track: group ID falls within a previously communicated gap",
            ));
        }
        if self
            .prior_object_gaps
            .get(&group_id)
            .is_some_and(|ranges| ranges.contains(object_id))
        {
            return Err(MessageError::ProtocolViolation(
                "malformed track: object ID falls within a previously communicated gap",
            ));
        }

        if let Some(properties) = properties {
            if let Some(group_gap) = properties.prior_group_id_gap() {
                if let Some(previous_gap) = self.group_gap_values.get(&group_id)
                    && *previous_gap != group_gap
                {
                    return Err(MessageError::ProtocolViolation(
                        "malformed track: PRIOR_GROUP_ID_GAP differs within the same group",
                    ));
                }
                self.group_gap_values.insert(group_id, group_gap);

                if group_gap > group_id {
                    return Err(MessageError::ProtocolViolation(
                        "malformed track: PRIOR_GROUP_ID_GAP exceeds the current group ID",
                    ));
                }
                if group_gap > 0 {
                    let gap_start = group_id - group_gap;
                    let gap_end = group_id - 1;
                    if self.seen_groups.overlaps(gap_start, gap_end) {
                        return Err(MessageError::ProtocolViolation(
                            "malformed track: PRIOR_GROUP_ID_GAP covers a previously received group",
                        ));
                    }
                    self.prior_group_gaps.insert(gap_start, gap_end);
                }
            }

            if let Some(object_gap) = properties.prior_object_id_gap() {
                if object_gap > object_id {
                    return Err(MessageError::ProtocolViolation(
                        "malformed track: PRIOR_OBJECT_ID_GAP exceeds the current object ID",
                    ));
                }
                if object_gap > 0 {
                    let gap_start = object_id - object_gap;
                    let gap_end = object_id - 1;
                    let seen_objects = self.seen_objects.entry(group_id).or_default();
                    if seen_objects.overlaps(gap_start, gap_end) {
                        return Err(MessageError::ProtocolViolation(
                            "malformed track: PRIOR_OBJECT_ID_GAP covers a previously received object",
                        ));
                    }
                    self.prior_object_gaps
                        .entry(group_id)
                        .or_default()
                        .insert(gap_start, gap_end);
                }
            }
        }

        self.seen_groups.insert_point(group_id);
        self.seen_objects
            .entry(group_id)
            .or_default()
            .insert_point(object_id);
        Ok(())
    }

    /// group 境界 prune: 指定 group より過去の per-group エントリを削除する
    ///
    /// FETCH decoder で group が前進した時に呼ぶ。
    /// - ascending: current_group 未満の group を prune
    /// - descending: current_group 超過の group を prune
    ///
    /// cross-group な `seen_groups` / `prior_group_gaps` は
    /// 将来 object の PRIOR_GROUP_ID_GAP 検証に必要なため残置する。
    pub fn prune_past_groups(&mut self, ascending: bool, current_group: u64) {
        if ascending {
            // current_group 未満の group をすべて削除
            self.group_gap_values.retain(|&g, _| g >= current_group);
            self.seen_objects.retain(|&g, _| g >= current_group);
            self.prior_object_gaps.retain(|&g, _| g >= current_group);
        } else {
            // current_group 超過の group をすべて削除
            self.group_gap_values.retain(|&g, _| g <= current_group);
            self.seen_objects.retain(|&g, _| g <= current_group);
            self.prior_object_gaps.retain(|&g, _| g <= current_group);
        }
    }
}

/// IMMUTABLE_PROPERTIES の中身バイト列を検証する
///
/// 内側に IMMUTABLE_PROPERTIES を含む場合は `ProtocolViolation`。
fn validate_immutable_inner(bytes: &[u8]) -> Result<(), MessageError> {
    decode_kv_pairs(bytes, false)?;
    Ok(())
}

/// 長さ付きバッファから K-V ペア列をパースする
///
/// `allow_immutable` が false の場合、IMMUTABLE_PROPERTIES (0x0B) の出現を拒否する
/// (入れ子 IMMUTABLE_PROPERTIES は draft-ietf-moq-transport-21 §10.7 (Immutable Properties) により禁止)。
fn decode_kv_pairs(
    inner: &[u8],
    allow_immutable: bool,
) -> Result<Vec<ObjectProperty>, MessageError> {
    let mut props = Vec::new();
    let mut inner_pos = 0;
    let mut prev_type: u64 = 0;

    while inner_pos < inner.len() {
        let (prop_type, delta) = crate::kvp::decode_delta_key(prev_type, inner, &mut inner_pos)?;

        // delta == 0 は重複 (最初のエントリを除く)
        if delta == 0 && !props.is_empty() {
            return Err(MessageError::ProtocolViolation(
                "duplicate object property type",
            ));
        }

        if !allow_immutable && prop_type == PROP_IMMUTABLE_PROPERTIES {
            return Err(MessageError::ProtocolViolation(
                "nested IMMUTABLE_PROPERTIES is not allowed (draft-ietf-moq-transport-21 §10.7 (Immutable Properties))",
            ));
        }

        // draft-ietf-moq-transport-21 §3.6 (Mandatory Track Properties): 必須トラックプロパティ (0x4000-0x7FFF) は
        // Track スコープのみ。Object Properties で受信したら malformed track
        if (crate::track_properties::MANDATORY_TRACK_PROPERTY_MIN
            ..=crate::track_properties::MANDATORY_TRACK_PROPERTY_MAX)
            .contains(&prop_type)
        {
            return Err(MessageError::ProtocolViolation(
                "malformed track: mandatory property in object scope",
            ));
        }

        prev_type = prop_type;

        let value = if prop_type % 2 == 0 {
            let (v, n) = varint::decode(&inner[inner_pos..])?;
            inner_pos += n;
            ObjectPropertyValue::VarInt(v)
        } else {
            let (len, n) = varint::decode(&inner[inner_pos..])?;
            inner_pos += n;
            if len > 65535 {
                return Err(MessageError::ProtocolViolation(
                    "object property value length exceeds 65535",
                ));
            }
            let len = varint::checked_len(len, inner[inner_pos..].len())?;
            let bytes = inner[inner_pos..inner_pos + len].to_vec();
            inner_pos += len;

            // IMMUTABLE_PROPERTIES 自体は許可するが内側を検証する
            if prop_type == PROP_IMMUTABLE_PROPERTIES {
                validate_immutable_inner(&bytes)?;
            }

            ObjectPropertyValue::Bytes(bytes)
        };

        props.push(ObjectProperty { prop_type, value });
    }

    Ok(props)
}

// ─── 重複 Object のフィールド一貫性追跡 (draft §12.1 条件 6/7, §7.1) ─────────────────

/// 重複受信した Object のフィールド一貫性を追跡するトラッカー
///
/// draft-ietf-moq-transport-21 §7.1 (Caching Relays):
/// "An endpoint that receives a duplicate Object with a different Forwarding Preference,
/// Subgroup ID, Priority or Payload MUST treat the track as Malformed."
///
/// draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 6/7 の検出に使う。
/// 判定単位は Object 単位 (Group ID, Object ID)。同一 Track 内で Object ごとに
/// Forwarding Preference が異なるだけの正当な Track を Malformed 扱いしない
/// (draft §11.1.1: "Object Forwarding Preference is a property of an individual Object
/// and can vary among Objects in the same Track")。
///
/// **Payload は保持しない。** Session は Sans I/O で Object の payload バイト列を受け取らず、
/// 保持すればメモリ消費が入力サイズに比例するため。§7.1 が挙げる 4 フィールドのうち
/// Payload の比較は Session では原理的にできない。Forwarding Preference / Subgroup ID /
/// Priority の 3 つに絞る。
///
/// **IMMUTABLE_PROPERTIES (0x0B) の差異検出も持たない。** draft §12.1 条件 6 の
/// "other immutable properties" のうち IMMUTABLE_PROPERTIES (draft §10.7 (Immutable Properties)) の
/// raw バイト列一致検証は、Track 単位に初回バイト列 (または hash) を保持する必要があるが、
/// Sans I/O + no_std 制約下でメモリ消費を入力サイズに比例させる保持は許容できない。Payload と同じ
/// 理由で Session ではなく app / relay 層 (payload や immutable metadata に触れられる層) の責務とする。
/// draft §10.7 の "This Property MUST NOT be modified or removed and the serialization ... MUST NOT change."
/// に対する endpoint 実装契約は Session の外側に置く。
///
/// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
#[derive(Debug, Default, Clone)]
pub struct ObjectFieldTracker {
    /// (group_id, object_id) → 初回受信時のフィールド値
    records: HashMap<(u64, u64), ObjectFieldRecord>,
}

/// 初回受信時に記録する Object のフィールド値
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObjectFieldRecord {
    /// Object Forwarding Preference: subgroup stream 経由なら `true`、datagram 経由なら `false`
    is_subgroup: bool,
    /// Subgroup ID (datagram 経由の場合は `None`)
    subgroup_id: Option<u64>,
    /// Publisher Priority
    publisher_priority: u8,
}

/// 重複 Object のフィールド不一致エラー
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectFieldMismatch {
    /// 不一致が起きた Group ID
    pub group_id: u64,
    /// 不一致が起きた Object ID
    pub object_id: u64,
    /// 不一致理由 (ログ用の英語メッセージ)
    pub reason: &'static str,
}

impl ObjectFieldTracker {
    /// 空のトラッカーを作成する
    pub fn new() -> Self {
        Self::default()
    }

    /// Object のフィールドを記録し、重複受信時に一貫性を検証する
    ///
    /// 初回受信時は記録して `Ok(())` を返す。同一 (group_id, object_id) の再受信時に
    /// Forwarding Preference / Subgroup ID / Priority のいずれかが異なれば
    /// `Err(ObjectFieldMismatch)` を返す。
    pub fn observe_object_fields(
        &mut self,
        group_id: u64,
        object_id: u64,
        is_subgroup: bool,
        subgroup_id: Option<u64>,
        publisher_priority: u8,
    ) -> Result<(), ObjectFieldMismatch> {
        let key = (group_id, object_id);
        let record = ObjectFieldRecord {
            is_subgroup,
            subgroup_id,
            publisher_priority,
        };
        match self.records.get(&key) {
            Some(prev) => {
                // draft §7.1: Forwarding Preference / Subgroup ID / Priority の比較
                if prev.is_subgroup != record.is_subgroup {
                    return Err(ObjectFieldMismatch {
                        group_id,
                        object_id,
                        reason: "malformed track: duplicate Object with different Forwarding Preference",
                    });
                }
                if prev.subgroup_id != record.subgroup_id {
                    return Err(ObjectFieldMismatch {
                        group_id,
                        object_id,
                        reason: "malformed track: duplicate Object with different Subgroup ID",
                    });
                }
                if prev.publisher_priority != record.publisher_priority {
                    return Err(ObjectFieldMismatch {
                        group_id,
                        object_id,
                        reason: "malformed track: duplicate Object with different Priority",
                    });
                }
                Ok(())
            }
            None => {
                self.records.insert(key, record);
                Ok(())
            }
        }
    }

    /// group 境界での prune: 指定 group より過去のエントリを削除する
    ///
    /// `ObjectPropertyTracker::prune_past_groups` と同じセマンティクス。
    /// Object 単位でフィールド値を持つと保持量が増えるため、group 境界で prune する。
    pub fn prune_past_groups(&mut self, ascending: bool, current_group: u64) {
        if ascending {
            self.records.retain(|&(g, _), _| g >= current_group);
        } else {
            self.records.retain(|&(g, _), _| g <= current_group);
        }
    }
}
