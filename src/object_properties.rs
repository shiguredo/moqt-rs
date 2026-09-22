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

    /// バッファ全体を 1 つの Object Properties ブロックとしてデコードする
    ///
    /// [`decode`](Self::decode) と異なり、末尾に余分なバイトが残る場合は拒否する。
    /// draft-ietf-moq-transport-21 §11.1.3 (Object Properties): "Object Properties are
    /// serialized as a length in bytes followed by Key-Value-Pairs (see Figure 2)." および
    /// §8.4 (Track and Object Properties): "Object Properties (Section 11.1.3) are preceded by
    /// an explicit length field." のとおり、Properties Length と実データ長は一致しなければ
    /// ならない。
    ///
    /// # Errors
    ///
    /// `decode` の失敗要因 (KVP 層の malformed 等) は区別せず、末尾の余分バイトと同じ
    /// `ProtocolViolation("invalid object properties framing")` に写す。
    /// [`ObjectPropertyTracker::observe_object`] が従来からこの写像を使っており、
    /// 呼び出し元 (Session の受信経路と FETCH decoder) の挙動を変えないためである。
    pub(crate) fn decode_exact(buf: &[u8]) -> Result<Self, MessageError> {
        let (properties, consumed) = Self::decode(buf)
            .map_err(|_| MessageError::ProtocolViolation("invalid object properties framing"))?;
        if consumed != buf.len() {
            return Err(MessageError::ProtocolViolation(
                "invalid object properties framing",
            ));
        }
        Ok(properties)
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
    /// 指定 prop_type のインスタンス数を mutable リストと IMMUTABLE_PROPERTIES 内側の両方で数える
    ///
    /// draft-ietf-moq-transport-21 §10.8 (Prior Group ID Gap) / §10.9 (Prior Object ID Gap):
    /// "An Object MUST NOT contain more than one instance of this property." 同じ型が
    /// mutable リストと IMMUTABLE_PROPERTIES 内側の両方に現れた場合も 2 インスタンスと数える
    /// (§10.7 (Immutable Properties) が両方への出現を許すのは複数の値を許すプロパティに限る)。
    ///
    /// IMMUTABLE_PROPERTIES の内側が破損していてデコードできない場合、内側のインスタンスは
    /// 数えられない。wire 経路では decode 時に破損が拒否されるため、この制約が観測されるのは
    /// `push` で組み立てた入力だけである。
    fn count_instances(&self, prop_type: u64) -> usize {
        let inner = self.immutable_inner_properties();
        self.0
            .iter()
            .chain(inner.iter())
            .filter(|p| p.prop_type == prop_type)
            .count()
    }

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
        // framing の検証は `decode_exact` に集約する (Session の受信経路も同じ関数を使う)
        let properties = match properties_bytes {
            Some(bytes) => Some(ObjectProperties::decode_exact(bytes)?),
            None => None,
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
        // draft-ietf-moq-transport-21 §10.8 (Prior Group ID Gap) / §10.9 (Prior Object ID Gap):
        // "An Object MUST NOT contain more than one instance of this property." 同じ型が
        // mutable リストと IMMUTABLE_PROPERTIES 内側の両方に現れた場合も 2 インスタンスと数える。
        // §12.1 (Malformed Tracks) の列挙は網羅ではないため、この条件も malformed に含まれる。
        if let Some(properties) = properties {
            for prop_type in [PROP_PRIOR_GROUP_ID_GAP, PROP_PRIOR_OBJECT_ID_GAP] {
                if properties.count_instances(prop_type) > 1 {
                    return Err(MessageError::MalformedTrack(
                        "malformed track: duplicate Prior Group/Object ID Gap property",
                    ));
                }
            }
        }
        if self.prior_group_gaps.contains(group_id) {
            return Err(MessageError::MalformedTrack(
                "malformed track: group ID falls within a previously communicated gap",
            ));
        }
        if self
            .prior_object_gaps
            .get(&group_id)
            .is_some_and(|ranges| ranges.contains(object_id))
        {
            return Err(MessageError::MalformedTrack(
                "malformed track: object ID falls within a previously communicated gap",
            ));
        }

        if let Some(properties) = properties {
            if let Some(group_gap) = properties.prior_group_id_gap() {
                if let Some(previous_gap) = self.group_gap_values.get(&group_id)
                    && *previous_gap != group_gap
                {
                    return Err(MessageError::MalformedTrack(
                        "malformed track: PRIOR_GROUP_ID_GAP differs within the same group",
                    ));
                }
                self.group_gap_values.insert(group_id, group_gap);

                if group_gap > group_id {
                    return Err(MessageError::MalformedTrack(
                        "malformed track: PRIOR_GROUP_ID_GAP exceeds the current group ID",
                    ));
                }
                if group_gap > 0 {
                    let gap_start = group_id - group_gap;
                    let gap_end = group_id - 1;
                    if self.seen_groups.overlaps(gap_start, gap_end) {
                        return Err(MessageError::MalformedTrack(
                            "malformed track: PRIOR_GROUP_ID_GAP covers a previously received group",
                        ));
                    }
                    self.prior_group_gaps.insert(gap_start, gap_end);
                }
            }

            if let Some(object_gap) = properties.prior_object_id_gap() {
                if object_gap > object_id {
                    return Err(MessageError::MalformedTrack(
                        "malformed track: PRIOR_OBJECT_ID_GAP exceeds the current object ID",
                    ));
                }
                if object_gap > 0 {
                    let gap_start = object_id - object_gap;
                    let gap_end = object_id - 1;
                    let seen_objects = self.seen_objects.entry(group_id).or_default();
                    if seen_objects.overlaps(gap_start, gap_end) {
                        return Err(MessageError::MalformedTrack(
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

        // draft-ietf-moq-transport-21 §3.6 (Mandatory Track Properties): 必須トラックプロパティ
        // (0x4000-0x7FFF) は Track スコープのみ。Object Properties で受信したら malformed track。
        //
        // ただし §16.8 (Properties) の Table 14 が GREASE の Property Type
        // (`0x7f * N + 0x9D`) を Scope Any として予約しており、その一部 (N = 128 の 0x401D から
        // N = 256 の 0x7F9D まで) はこの範囲に入る。GREASE 値は IANA に登録された Property では
        // ないため、§3.6 の「Mandatory Track Property」は登録された必須トラックプロパティを指し、
        // 予約値である GREASE を含まないと解釈する。この解釈は §13 (Grease) の
        // "Endpoints MUST NOT close the session solely because they received an unknown value."
        // と §8.4 (Track and Object Properties) の未知 Property の転送 MUST に整合する。
        // GREASE 値は未知 Property として §8.4 / §16.8 に従い保持・転送する。
        // draft 内部の登録ポリシーと予約値の関係は将来の draft 改版で変わりうる。
        if (crate::track_properties::MANDATORY_TRACK_PROPERTY_MIN
            ..=crate::track_properties::MANDATORY_TRACK_PROPERTY_MAX)
            .contains(&prop_type)
            && !crate::grease::is_grease(prop_type)
        {
            return Err(MessageError::MalformedTrack(
                "mandatory property in object scope",
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
/// **payload の内容そのものは本トラッカーでは比較できない。** Session は Sans I/O で Object の
/// payload バイト列を受け取らないため、呼び出し側が算出した比較キー (`payload_key`) を渡す。
/// Session は payload 長 (subgroup 経路のみ) と status を符号化したキーを渡すため、
/// **同じ長さで内容だけが異なる payload は Session では検出できない**。
/// payload のバイト列を持つ層 (アプリ) がダイジェスト等を渡せば検出できる。
/// Malformed Track の誤検出で正常な track を落とすより見逃し側に倒す設計である。
///
/// **比較は両方 `Some` のときだけ行う。** `None` になるのは次の 3 通りである。
///
/// - [`observe_object_fields`](Self::observe_object_fields) は内容を渡さないため常に `None`
/// - `immutable_properties` は、その Object が IMMUTABLE_PROPERTIES (0x0B) を持たない場合に `None`
/// - `payload_key` は、呼び出し側が比較キーを算出しない場合に `None`
///   ([`observe_object_fields_with_content`](Self::observe_object_fields_with_content) の
///   呼び出し側の判断であり、Session は常に `Some` を渡す)
///
/// したがって **一方の Object だけが IMMUTABLE_PROPERTIES を持つ重複は検出しない**
/// (draft §10.7 は "This Property MUST NOT be modified or removed" と定めるため差異ではあるが、
/// 見逃し側に倒す)。記録は初回受信時の値のまま更新しないため、初回に `None` だった Object は
/// 以後も内容比較の対象にならない。
///
/// **保持量**: 1 レコードは immutables 長 (最大 65535 バイト) と payload_key 長を保持する。
/// [`prune_past_groups`](Self::prune_past_groups) を呼ぶか tracker 自体を破棄するまで減らないため
/// (同じ group 内の記録は prune でも残る)、`Session` のように prune を呼ばない利用者では
/// 受信 Object 数 × 上記サイズまで増え続ける。
///
/// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
#[derive(Debug, Default, Clone)]
pub struct ObjectFieldTracker {
    /// (group_id, object_id) → 初回受信時のフィールド値
    records: HashMap<(u64, u64), ObjectFieldRecord>,
}

/// 初回受信時に記録する Object のフィールド値
///
/// 比較はフィールド単位で行うため、構造体全体の等価比較は導出しない。
#[derive(Debug, Clone)]
struct ObjectFieldRecord {
    /// Object Forwarding Preference: subgroup stream 経由なら `true`、datagram 経由なら `false`
    is_subgroup: bool,
    /// Subgroup ID (datagram 経由の場合は `None`)
    subgroup_id: Option<u64>,
    /// Publisher Priority
    publisher_priority: u8,
    /// IMMUTABLE_PROPERTIES (0x0B) の内側の生バイト列 (draft §10.7 (Immutable Properties))
    ///
    /// Properties の長さに律速されるため生バイト列のまま保持する。
    immutable_properties: Option<Vec<u8>>,
    /// payload の比較キー (呼び出し側が算出したバイト列。詳細は [`ObjectFieldTracker`] の doc)
    payload_key: Option<Vec<u8>>,
}

/// 重複 Object のフィールド不一致エラー
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectFieldMismatch {
    /// 不一致が起きた Group ID
    pub group_id: u64,
    /// 不一致が起きた Object ID
    pub object_id: u64,
    /// 不一致理由 (ログと `Display` に出す英語メッセージ)
    pub reason: &'static str,
}

impl core::fmt::Display for ObjectFieldMismatch {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "object field mismatch (group_id={}, object_id={}): {}",
            self.group_id, self.object_id, self.reason
        )
    }
}

impl core::error::Error for ObjectFieldMismatch {}

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
        self.observe_object_fields_with_content(
            group_id,
            object_id,
            is_subgroup,
            subgroup_id,
            publisher_priority,
            None,
            None,
        )
    }

    /// 重複受信した Object を、immutable properties と payload の比較キーも含めて追跡する
    ///
    /// draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 6: "The same Object is
    /// received more than once with different Payload or other immutable properties."
    ///
    /// 初回受信時は記録して `Ok(())` を返す。判定規則と保持量は [`ObjectFieldTracker`] の
    /// doc を参照。
    ///
    /// `immutable_properties` は IMMUTABLE_PROPERTIES (0x0B) の内側の生バイト列
    /// (`ObjectProperties::immutable_properties` の戻り値)。`payload_key` は呼び出し側が算出した
    /// payload の比較キーである (payload のバイト列そのものは Session が保持しないため)。
    /// キーの長さは問わないが、record ごとに保持するため **短い固定長** が望ましく、
    /// payload 全体を渡すと保持量が payload 長に比例する。ダイジェストが衝突した場合は
    /// 「不一致と判定されない」= 見逃しになる。
    ///
    /// # Errors
    ///
    /// 同一 (group_id, object_id) の再受信でフィールドまたは内容が異なる場合に
    /// `ObjectFieldMismatch` を返す。
    #[expect(
        clippy::too_many_arguments,
        reason = "Forwarding Preference / Subgroup ID / Priority と条件 6 の内容比較を 1 回の観測で渡すため"
    )]
    pub fn observe_object_fields_with_content(
        &mut self,
        group_id: u64,
        object_id: u64,
        is_subgroup: bool,
        subgroup_id: Option<u64>,
        publisher_priority: u8,
        immutable_properties: Option<&[u8]>,
        payload_key: Option<&[u8]>,
    ) -> Result<(), ObjectFieldMismatch> {
        let key = (group_id, object_id);
        // 比較は引数の借用のまま行い、複製 (immutables は最大 65535 バイト) は
        // 初回受信で記録するときだけ作る
        let Some(prev) = self.records.get(&key) else {
            self.records.insert(
                key,
                ObjectFieldRecord {
                    is_subgroup,
                    subgroup_id,
                    publisher_priority,
                    immutable_properties: immutable_properties.map(<[u8]>::to_vec),
                    payload_key: payload_key.map(<[u8]>::to_vec),
                },
            );
            return Ok(());
        };
        // draft §7.1: Forwarding Preference / Subgroup ID / Priority の比較
        if prev.is_subgroup != is_subgroup {
            return Err(ObjectFieldMismatch {
                group_id,
                object_id,
                reason: "malformed track: duplicate Object with different Forwarding Preference",
            });
        }
        if prev.subgroup_id != subgroup_id {
            return Err(ObjectFieldMismatch {
                group_id,
                object_id,
                reason: "malformed track: duplicate Object with different Subgroup ID",
            });
        }
        if prev.publisher_priority != publisher_priority {
            return Err(ObjectFieldMismatch {
                group_id,
                object_id,
                reason: "malformed track: duplicate Object with different Priority",
            });
        }
        // draft §12.1 (Malformed Tracks) 条件 6: Payload / immutable properties の差異
        if let (Some(prev_immutables), Some(immutables)) =
            (prev.immutable_properties.as_deref(), immutable_properties)
            && prev_immutables != immutables
        {
            return Err(ObjectFieldMismatch {
                group_id,
                object_id,
                reason: "malformed track: duplicate Object with different immutable properties",
            });
        }
        if let (Some(prev_key), Some(key)) = (prev.payload_key.as_deref(), payload_key)
            && prev_key != key
        {
            return Err(ObjectFieldMismatch {
                group_id,
                object_id,
                reason: "malformed track: duplicate Object with different Payload",
            });
        }
        Ok(())
    }

    /// group 境界での prune: 指定 group より過去のエントリを削除する
    ///
    /// `ObjectPropertyTracker::prune_past_groups` と同じセマンティクス。
    /// 保持量を抑えたい呼び出し側が group 前進ごとに呼ぶ。保持量の詳細は
    /// [`ObjectFieldTracker`] の doc を参照。
    pub fn prune_past_groups(&mut self, ascending: bool, current_group: u64) {
        if ascending {
            self.records.retain(|&(g, _), _| g >= current_group);
        } else {
            self.records.retain(|&(g, _), _| g <= current_group);
        }
    }
}
