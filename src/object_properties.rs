//! Object-scoped Properties (draft-ietf-moq-transport-23 §16.8 (Properties) Table 15)
//!
//! SUBGROUP_OBJECT / ObjectDatagram のヘッダに含まれる Object 単位の Properties を扱う。
//! Track-scoped Properties は `track_properties` モジュールを参照。
//! 重複 Object の検出で使う [`DeliveryMode`] と [`ObjectFieldTracker`] も本モジュールに置く。
//!
//! # ワイヤーフォーマット
//!
//! `Properties Length (varint) | Key-Value-Pairs...`
//!
//! Key-Value-Pair は draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure) の delta encoding。prop_type 昇順にソートし、
//! 差分を varint として書き出す。偶数型は varint 値、奇数型は長さ付きバイト列。
//!
//! # 対象 Property (draft-ietf-moq-transport-23 §16.8 (Properties) Table 15)
//!
//! | Type | Name                 | Scope        | Spec  |
//! |-----:|----------------------|--------------|-------|
//! | 0x02 | OBJECT_DELIVERY_TIMEOUT | Track,Object | draft-ietf-moq-transport-23 §10.2 (OBJECT_DELIVERY_TIMEOUT) |
//! | 0x06 | SUBGROUP_DELIVERY_TIMEOUT | Track,Object | draft-ietf-moq-transport-23 §10.1 (SUBGROUP_DELIVERY_TIMEOUT) |
//! | 0x0B | IMMUTABLE_PROPERTIES | Track,Object | draft-ietf-moq-transport-23 §10.7 (Immutable Properties) |
//! | 0x3C | PRIOR_GROUP_ID_GAP   | Object       | draft-ietf-moq-transport-23 §10.8 (Prior Group ID Gap) |
//! | 0x3E | PRIOR_OBJECT_ID_GAP  | Object       | draft-ietf-moq-transport-23 §10.9 (Prior Object ID Gap) |
//!
//! 0x02 / 0x06 は Track scope の定義元 (`track_properties` モジュール) と共通の型番号で、
//! Object scope では本モジュールのアクセサ (`object_delivery_timeout` / `subgroup_delivery_timeout`) から参照する。
//!
//! このほか draft-ietf-moq-loc-05 §6.1 (MOQ Properties Registry) Table 1 の
//! 0x11 CODEC_STRING (Scope=Track, Object) を扱う。`crate::property` の共有検証で
//! encode / decode 時に UTF-8 を検証する。
//!
//! # スコープ検証と多重検証
//!
//! draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) は、エンドポイントが意味を
//! 理解する Property をスコープ外で受信した場合に Track を malformed とすることを MUST とする
//! (§12.1 (Malformed Tracks) 条件 8)。本モジュールの decode は、この実装が意味を理解する
//! Track-only Property (0x04 / 0x0E / 0x22 / 0x30 など) を Object Properties で検出すると
//! `MalformedTrack` を返す。mutable リストと IMMUTABLE_PROPERTIES の内側の両方に同じ規則を
//! 適用する (§10.7 (Immutable Properties))。未知の型はスコープを推測できないため拒否しない。
//! ローカル入力の `encode` は受信 Track の判定と混同しないよう `ProtocolViolation` で拒否する。
//!
//! 同じ §8.4 は、Property の定義が複数を許す場合を除いて 1 インスタンスまでしか載せては
//! ならないと定め、許される数より多く受信した場合も Track を malformed とすることを MUST と
//! する (§12.1 条件 9)。本モジュールの decode は同一 Type をそのまま保持して [`ObjectProperties::validate`]
//! で判定し、未知の Type の複数は正当として受理する (§8.4 の "MUST allow multiple instances
//! of the Property")。多重可否の表は `crate::property_scope` に集約する。
//!
//! IMMUTABLE_PROPERTIES (0x0B) は奇数型だが、その値は「入れ子の Key-Value-Pair リスト」。
//! 入れ子の IMMUTABLE_PROPERTIES は許されず、検出した場合は MalformedTrack
//! (draft-ietf-moq-transport-23 §10.7 (Immutable Properties): "An Object contains an Immutable Properties property that contains
//! another Immutable Properties key" → malformed)。
//!
//! # 注意
//!
//! 本モジュールの Property Type / scope / 解釈は draft 由来であり、将来の改訂で
//! 変更される可能性がある。

use crate::property::{CODEC_STRING_TYPE, validate_codec_string_utf8};
use crate::track_properties::{
    PROP_IMMUTABLE_PROPERTIES, PROP_OBJECT_DELIVERY_TIMEOUT, PROP_SUBGROUP_DELIVERY_TIMEOUT,
};
use crate::{error::MessageError, varint};
use alloc::vec::Vec;
use hashbrown::HashMap;

/// PRIOR_GROUP_ID_GAP (draft-ietf-moq-transport-23 §10.8 (Prior Group ID Gap), Property Type 0x3C)
///
/// 直前の存在しない Group の個数を示す varint。
pub const PROP_PRIOR_GROUP_ID_GAP: u64 = 0x3C;

/// PRIOR_OBJECT_ID_GAP (draft-ietf-moq-transport-23 §10.9 (Prior Object ID Gap), Property Type 0x3E)
///
/// 直前の存在しない Object の個数を示す varint。
pub const PROP_PRIOR_OBJECT_ID_GAP: u64 = 0x3E;

/// Object scope に Track 専用の既知 Property が現れたときの説明
///
/// 受信時の `MalformedTrack` とローカル入力検証時の `ProtocolViolation` が同じ理由を
/// 指すように 1 箇所に置く (どちらも `MessageError` の `&'static str` である)。
const TRACK_ONLY_IN_OBJECT_SCOPE: &str = "Track-only property in Object scope";

/// 単一に限られる既知 Property が複数現れたときの説明
///
/// 受信時の `MalformedTrack` とローカル入力検証時の `ProtocolViolation` が同じ理由を
/// 指すように 1 箇所に置く (どちらも `MessageError` の `&'static str` である)。
const DUPLICATE_OBJECT_PROPERTY: &str = "duplicate object property";

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
    /// プロパティ型 (draft-ietf-moq-transport-23 §16.8 Table 15)
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
    /// 空の場合は Properties Length = 0 のみを書き出す (draft-ietf-moq-transport-23 §11.1.2 (Object Properties) / §11.3.1 (Subgroup Header))。
    /// prop_type の昇順にソートし、delta encoding で圧縮する。未知の型は複数インスタンスを
    /// 保持したまま書き出す (draft-ietf-moq-transport-23 §8.4 (Track and Object Properties)
    /// の "MUST allow multiple instances of the Property")。同一 Type の並び順はソートが
    /// 安定であるため入力順を保つ。
    ///
    /// # Errors
    ///
    /// - 偶数型に `Bytes` 値、または奇数型に `VarInt` 値: `ProtocolViolation`
    /// - 単一に限られる既知 prop_type の重複 (IMMUTABLE_PROPERTIES の内側との重複を含む):
    ///   `ProtocolViolation` (ローカル入力の検証であり、受信 Track の malformed 判定とは別)
    /// - Track 専用の既知 Property (MAX_CACHE_DURATION など) を含む: `ProtocolViolation`
    ///   (ローカル入力の検証であり、受信 Track の malformed 判定とは別)
    /// - IMMUTABLE_PROPERTIES の内側に IMMUTABLE_PROPERTIES を含む: `ProtocolViolation`
    /// - バイト列が 65535 バイトを超える: `FieldLengthExceeded`
    ///   (draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure) の値長上限 2^16-1)
    /// - CODEC_STRING (0x11) の値が不正な UTF-8: `MalformedKeyValue`
    ///   (draft-ietf-moq-loc-05 §2.3.1.3 (Codec String) / draft-ietf-moq-transport-23 §8.3
    ///   (Key-Value-Pair Structure) の MALFORMED_KEY_VALUE。最上位と
    ///   IMMUTABLE_PROPERTIES の内側)
    pub fn encode(&self, buf: &mut Vec<u8>) -> Result<(), MessageError> {
        if self.0.is_empty() {
            varint::encode(0, buf);
            return Ok(());
        }

        // ローカル入力の意味検証 (スコープと多重可否) を行う。受信 Track の malformed 判定と
        // 混同しないよう、MalformedTrack を ProtocolViolation に写す
        // (draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1
        // (Malformed Tracks) 条件 8 / 9)。
        self.validate().map_err(|err| match err {
            MessageError::MalformedTrack(reason) => MessageError::ProtocolViolation(reason),
            other => other,
        })?;

        let mut sorted = self.0.clone();
        sorted.sort_by_key(|p| p.prop_type);

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
                        return Err(MessageError::FieldLengthExceeded(
                            "object property value length exceeds 65535",
                        ));
                    }
                    // CODEC_STRING (0x11) の値を UTF-8 として検証する。内側の 0x11 は
                    // 下の validate_immutable_inner (decode_kv_pairs) が検証する。
                    if prop.prop_type == CODEC_STRING_TYPE {
                        validate_codec_string_utf8(b)?;
                    }
                    // IMMUTABLE_PROPERTIES の内側に IMMUTABLE_PROPERTIES を含まないか、
                    // 内側の KVP が書式として壊れていないかを検証する。内側の意味検証
                    // (スコープと多重可否) は上の `validate` が両リストを合わせて行う。
                    if prop.prop_type == PROP_IMMUTABLE_PROPERTIES {
                        validate_immutable_inner(b, KvpContext::Encode)?;
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
    /// draft-ietf-moq-transport-23 §11.1.2 (Object Properties): "Object Properties are
    /// serialized as a length in bytes followed by Key-Value-Pairs (see Figure 4)." および
    /// §8.4 (Track and Object Properties): "Object Properties (Section 11.1.2) are preceded by
    /// an explicit length field." のとおり、Properties Length と実データ長は一致しなければ
    /// ならない。
    ///
    /// # Errors
    ///
    /// `decode` の失敗要因 (KVP 層の malformed、長さ超過、デルタ溢出など) は原因を保ったまま
    /// 返す。**`MalformedTrack` は `ProtocolViolation` などへ畳まずそのまま透過する。**
    /// draft-ietf-moq-transport-23 §12.1 (Malformed Tracks) はセッション終了ではなく購読・
    /// fetch の cancel を求めるため、呼び出し側 (`Session` の data plane) は戻り値の variant で
    /// セッション終了と cancel を分岐する。末尾に余分バイトが残る場合は、Properties Length が
    /// 実際の Key-Value-Pair 列と一致しない = Object の直列化を復号できないため
    /// `MalformedObject` (MALFORMED_OBJECT 0x201) とする
    /// (draft-ietf-moq-transport-23 §11.1.2 (Object Properties) / §12.2
    /// (Session Termination Codes))。
    pub(crate) fn decode_exact(buf: &[u8]) -> Result<Self, MessageError> {
        // `buf` は Properties Length を含む完結したブロックであるため、内側の varint が
        // 途中で切れることは「追加データ待ち」ではなく直列化の破損を意味する。
        // データ不足の `UnexpectedEof` は MALFORMED_OBJECT に写す。
        let (properties, consumed) = Self::decode(buf).map_err(|err| match err {
            MessageError::UnexpectedEof => MessageError::MalformedObject(
                "object properties are shorter than their declared length",
            ),
            other => other,
        })?;
        if consumed != buf.len() {
            return Err(MessageError::MalformedObject(
                "object properties length does not match its contents",
            ));
        }
        Ok(properties)
    }

    /// バッファ先頭から Object Properties ブロックをデコードし `(properties, 消費バイト数)` を返す
    ///
    /// フォーマット: `Properties Length (varint) | Key-Value-Pairs...`
    ///
    /// 同一 Type の複数インスタンスは構造として保持してから [`ObjectProperties::validate`] で
    /// 意味検証する。未知の Type の複数は draft-ietf-moq-transport-23 §8.4 (Track and Object
    /// Properties) の "MUST allow multiple instances of the Property" により正当である。
    ///
    /// # Errors
    ///
    /// - CODEC_STRING (0x11) の値が不正な UTF-8: `MalformedKeyValue`
    ///   (MUST。draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure) の
    ///   MALFORMED_KEY_VALUE / draft-ietf-moq-loc-05 §2.3.1.3 (Codec String))。
    ///   mutable リストと IMMUTABLE_PROPERTIES の内側の両方を検証する
    /// - 書式違反 (delta-key のオーバーフロー、宣言長超過、切り詰めなど):
    ///   `MalformedKeyValue` / `FieldLengthExceeded` / `DeltaEncodingOverflow` /
    ///   `UnexpectedEof` (draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure))
    /// - §8.4 (Track and Object Properties) / §12.1 (Malformed Tracks) 条件 8 の
    ///   スコープ違反 (Track 専用の既知 Property を Object Properties で受信)、
    ///   §3.7 (Mandatory to Understand Track Properties) の Mandatory Track Property を
    ///   Object scope で受信した場合、および §12.1 条件 9 の多重違反 (単一に限られる既知
    ///   Property の重複) に `MalformedTrack` を返す。mutable リストと IMMUTABLE_PROPERTIES
    ///   の内側の両方を検証する (§10.7 (Immutable Properties))。
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

        let props = decode_kv_pairs(inner, true, KvpContext::Decode)?;
        let properties = Self(props);
        properties.validate()?;
        Ok((properties, pos))
    }

    /// Object Properties の意味検証 (スコープと多重可否) を行う
    ///
    /// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1 (Malformed
    /// Tracks) 条件 8 / 9 の検証をまとめて行う。mutable リストと IMMUTABLE_PROPERTIES (0x0B)
    /// の内側を合わせて調べる (§10.7 (Immutable Properties) の "MUST search both")。
    ///
    /// - この実装が意味を理解する Track-only Property を Object scope で検出したら
    ///   `MalformedTrack`
    /// - §3.7 (Mandatory to Understand Track Properties) の 0x4000-0x7FFF のうち GREASE
    ///   予約値でない型を検出したら `MalformedTrack`
    /// - 単一に限られる既知 Property が 2 インスタンス以上あれば `MalformedTrack`。同じ型が
    ///   mutable リストと内側の両方に現れた場合も 2 インスタンスと数える
    ///
    /// 未知の型はスコープと多重可否を推測せず受理する (§8.4 の転送 MUST / §16.8 (Properties)
    /// の "Endpoints MUST ignore unknown Property types")。
    ///
    /// `push` で積んだ IMMUTABLE_PROPERTIES の内側が破損していてデコードできない場合、内側は
    /// 調べられない ([`ObjectProperties::find_varint`] と同じ制約)。wire 経由の値は `decode` が
    /// 内側の書式を検証済みなので、この制約が観測されるのは in-memory 構築の入力だけである。
    ///
    /// # Errors
    ///
    /// 上記のいずれかに該当する場合に `MalformedTrack`。
    pub fn validate(&self) -> Result<(), MessageError> {
        let inner = self.immutable_inner_properties();
        for p in self.0.iter().chain(inner.iter()) {
            validate_object_property_type(p.prop_type)?;
        }
        // draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1
        // (Malformed Tracks) 条件 9: 単一に限られる既知 Property は 1 インスタンスまで。
        if crate::property_scope::has_duplicate_single_property(
            self.0.iter().chain(inner.iter()).map(|p| p.prop_type),
        ) {
            return Err(MessageError::MalformedTrack(DUPLICATE_OBJECT_PROPERTY));
        }
        Ok(())
    }

    /// OBJECT_DELIVERY_TIMEOUT の値を返す (draft-ietf-moq-transport-23 §10.2)
    pub fn object_delivery_timeout(&self) -> Option<u64> {
        self.find_varint(PROP_OBJECT_DELIVERY_TIMEOUT)
    }

    /// SUBGROUP_DELIVERY_TIMEOUT の値を返す (draft-ietf-moq-transport-23 §10.1)
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
    /// 中身は入れ子 K-V ペア列 (draft-ietf-moq-transport-23 §10.7 (Immutable Properties))。
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
    /// 破損していれば空 Vec を返す (draft-ietf-moq-transport-23 §10.7 (Immutable Properties)「MUST search both」対応)。
    /// 最初の IMMUTABLE_PROPERTIES のみを対象とする。0x0B は単一に限られるため、`decode` が
    /// 保持した重複や `push` で積んだ重複は [`ObjectProperties::validate`] が malformed Track
    /// として拒否する。2 つ目以降の内側を探索しないのは、その前提が破れた入力に対する防御で
    /// ある。
    fn immutable_inner_properties(&self) -> Vec<ObjectProperty> {
        let Some(bytes) = self.immutable_properties() else {
            return Vec::new();
        };
        decode_kv_pairs(bytes, false, KvpContext::Decode).unwrap_or_default()
    }

    /// 偶数型 Object Property から varint 値を取得する
    ///
    /// draft-ietf-moq-transport-23 §10.7 (Immutable Properties)「MUST search both」に従い、mutable リストに無ければ
    /// IMMUTABLE_PROPERTIES (0x0B) の内側も探索する。mutable リストを優先する。
    ///
    /// draft-ietf-moq-transport-23 §3.3.2 (Range Filters) の OBJECT_PROPERTY_FILTER は任意の
    /// 偶数 Property Type を対象にできるため、型別 accessor では足りず本メソッドを公開している。
    /// 同じ型が複数インスタンスある場合 (未知の型で許される重複) は最初のインスタンスを返す。
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

    /// 指定 prop_type のインスタンス数を mutable リストと IMMUTABLE_PROPERTIES 内側の両方で数える
    ///
    /// draft-ietf-moq-transport-23 §10.8 (Prior Group ID Gap) / §10.9 (Prior Object ID Gap):
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
}

/// 数値の閉区間集合
///
/// 追加時に隣接する区間を併合するため、連続した範囲を 1 区間で保持する。
/// 不存在区間と既出 Object 位置の照合 (draft-ietf-moq-transport-23 §2.1.2 (Object States)) に
/// `Session` の Track 履歴からも使う。
#[derive(Debug, Clone, Default)]
pub(crate) struct RangeSet(Vec<(u64, u64)>);

impl RangeSet {
    /// 指定値が区間に含まれるか
    pub(crate) fn contains(&self, value: u64) -> bool {
        self.0
            .iter()
            .any(|&(start, end)| start <= value && value <= end)
    }

    /// 指定区間と重なる区間があるか
    pub(crate) fn overlaps(&self, start: u64, end: u64) -> bool {
        if start > end {
            return false;
        }
        self.0
            .iter()
            .any(|&(existing_start, existing_end)| !(end < existing_start || existing_end < start))
    }

    /// 単一値を区間として追加する
    pub(crate) fn insert_point(&mut self, value: u64) {
        self.insert(value, value);
    }

    /// 閉区間を追加し、隣接・重複する区間を併合する
    pub(crate) fn insert(&mut self, mut start: u64, mut end: u64) {
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
        // draft-ietf-moq-transport-23 §10.8 (Prior Group ID Gap) / §10.9 (Prior Object ID Gap):
        // "An Object MUST NOT contain more than one instance of this property." 同じ型が
        // mutable リストと IMMUTABLE_PROPERTIES 内側の両方に現れた場合も 2 インスタンスと数える。
        // §12.1 (Malformed Tracks) の列挙は網羅ではないため、この条件も malformed に含まれる。
        if let Some(properties) = properties {
            for prop_type in [PROP_PRIOR_GROUP_ID_GAP, PROP_PRIOR_OBJECT_ID_GAP] {
                if properties.count_instances(prop_type) > 1 {
                    return Err(MessageError::MalformedTrack(
                        "duplicate Prior Group/Object ID Gap property",
                    ));
                }
            }
        }
        if self.prior_group_gaps.contains(group_id) {
            return Err(MessageError::MalformedTrack(
                "group ID falls within a previously communicated gap",
            ));
        }
        if self
            .prior_object_gaps
            .get(&group_id)
            .is_some_and(|ranges| ranges.contains(object_id))
        {
            return Err(MessageError::MalformedTrack(
                "object ID falls within a previously communicated gap",
            ));
        }

        if let Some(properties) = properties {
            if let Some(group_gap) = properties.prior_group_id_gap() {
                if let Some(previous_gap) = self.group_gap_values.get(&group_id)
                    && *previous_gap != group_gap
                {
                    return Err(MessageError::MalformedTrack(
                        "PRIOR_GROUP_ID_GAP differs within the same group",
                    ));
                }
                self.group_gap_values.insert(group_id, group_gap);

                if group_gap > group_id {
                    return Err(MessageError::MalformedTrack(
                        "PRIOR_GROUP_ID_GAP exceeds the current group ID",
                    ));
                }
                if group_gap > 0 {
                    let gap_start = group_id - group_gap;
                    let gap_end = group_id - 1;
                    if self.seen_groups.overlaps(gap_start, gap_end) {
                        return Err(MessageError::MalformedTrack(
                            "PRIOR_GROUP_ID_GAP covers a previously received group",
                        ));
                    }
                    self.prior_group_gaps.insert(gap_start, gap_end);
                }
            }

            if let Some(object_gap) = properties.prior_object_id_gap() {
                if object_gap > object_id {
                    return Err(MessageError::MalformedTrack(
                        "PRIOR_OBJECT_ID_GAP exceeds the current object ID",
                    ));
                }
                if object_gap > 0 {
                    let gap_start = object_id - object_gap;
                    let gap_end = object_id - 1;
                    let seen_objects = self.seen_objects.entry(group_id).or_default();
                    if seen_objects.overlaps(gap_start, gap_end) {
                        return Err(MessageError::MalformedTrack(
                            "PRIOR_OBJECT_ID_GAP covers a previously received object",
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
    /// group 単位の寿命を呼び出し側が管理する場合に使う。
    /// - ascending: current_group 未満の group を prune
    /// - descending: current_group 超過の group を prune
    ///
    /// 単一 stream の Group 前進を根拠に prune すると、同じ Track の別 request が参照している
    /// 既出 Object / 不存在区間の照合情報を失う。Track 単位で履歴を共有する `Session` は、
    /// Group 単位の保持数の上限に達したときだけ、最も古い Group を破棄するために呼ぶ
    /// (`PeerTrackHistory::prepare_group` 参照)。それ以外では Session の生存期間にわたって
    /// 保持し、他の request が参照する照合情報を失わない。
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

    /// 指定 Group の検証状態だけを破棄する
    ///
    /// Track 単位の履歴が Group 数の上限に達したとき、最も古い Group の状態だけを O(1) で
    /// 破棄するために使う。`seen_groups` / `prior_group_gaps` の Group 区間は、将来の
    /// PRIOR_GROUP_ID_GAP 検証に必要な cross-group の情報であるため残す。
    pub(crate) fn remove_group(&mut self, group_id: u64) {
        self.group_gap_values.remove(&group_id);
        self.seen_objects.remove(&group_id);
        self.prior_object_gaps.remove(&group_id);
    }

    /// 既出 Object を記録している Group 数 (診断用)
    #[cfg(test)]
    pub(crate) fn seen_object_group_count(&self) -> usize {
        self.seen_objects.len()
    }
}

/// Object scope の 1 インスタンスについて、型のスコープと Mandatory Track Property を検証する
///
/// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) の "An endpoint that
/// supports a Property and receives it outside its scope (e.g. a Track-only Property as an
/// Object Property) MUST treat the Track as malformed" / §12.1 (Malformed Tracks) 条件 8 と、
/// draft-ietf-moq-transport-23 §3.7 (Mandatory to Understand Track Properties) の
/// "An Object received with a Mandatory Track Property as an Object Property is malformed"。
/// mutable リストと IMMUTABLE_PROPERTIES の内側のどちらにも適用する (§10.7 (Immutable
/// Properties))。
///
/// draft-ietf-moq-transport-23 §3.7 の 0x4000-0x7FFF のうち GREASE 予約値
/// (§16.8 (Properties) Table 15 が Scope Any として予約する `0x7f * N + 0x9D`) は、IANA に
/// 登録された Property ではないため Mandatory Track Property に含めない。この解釈は §13
/// (Grease) の "Endpoints MUST NOT close the session solely because they received an unknown
/// value." と §8.4 の未知 Property の転送 MUST に整合する。GREASE 値は未知 Property として
/// §8.4 / §16.8 に従い保持・転送する。draft 内部の登録ポリシーと予約値の関係は将来の draft
/// 改版で変わりうる。
fn validate_object_property_type(prop_type: u64) -> Result<(), MessageError> {
    if (crate::track_properties::MANDATORY_TRACK_PROPERTY_MIN
        ..=crate::track_properties::MANDATORY_TRACK_PROPERTY_MAX)
        .contains(&prop_type)
        && !crate::grease::is_grease(prop_type)
    {
        return Err(MessageError::MalformedTrack(
            "mandatory property in object scope",
        ));
    }
    if crate::property_scope::is_track_only(prop_type) {
        return Err(MessageError::MalformedTrack(TRACK_ONLY_IN_OBJECT_SCOPE));
    }
    Ok(())
}

/// KVP 列を解析する文脈 (入れ子 IMMUTABLE_PROPERTIES の分類を決める)
///
/// draft-ietf-moq-transport-23 §10.7 (Immutable Properties) / §12.1 (Malformed Tracks) は
/// IMMUTABLE_PROPERTIES の内側に別の IMMUTABLE_PROPERTIES を含む Object の Track を malformed
/// と定める。したがって受信した Object Properties の decode では `MalformedTrack` を返す。
/// 一方、ローカル入力の encode は受信 Track の判定ではないため `ProtocolViolation` のまま
/// 据え置く (セッション終了と cancel を戻り値で分岐する呼び出し側の契約を崩さないため)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KvpContext {
    /// 受信した Object Properties の decode
    Decode,
    /// ローカル入力の encode
    Encode,
}

/// IMMUTABLE_PROPERTIES の中身バイト列を書式として検証する
///
/// 内側に IMMUTABLE_PROPERTIES を含む場合、KVP の宣言長が残りバイトを超える場合、
/// CODEC_STRING (0x11) の値が不正な UTF-8 の場合に失敗する (入れ子 IMMUTABLE_PROPERTIES は
/// draft-ietf-moq-transport-23 §10.7 (Immutable Properties) により禁止、CODEC_STRING の
/// UTF-8 は同 §8.3 (Key-Value-Pair Structure) / draft-ietf-moq-loc-05 §2.3.1.3
/// (Codec String))。内側の意味検証 (スコープと多重可否) は
/// [`ObjectProperties::validate`] が mutable リストと合わせて行う。
///
/// `context` は入れ子 IMMUTABLE_PROPERTIES の分類を決める
/// ([`KvpContext`] の doc 参照)。
fn validate_immutable_inner(bytes: &[u8], context: KvpContext) -> Result<(), MessageError> {
    decode_kv_pairs(bytes, false, context)?;
    Ok(())
}

/// 長さ付きバッファから K-V ペア列をパースする
///
/// 構造と値の書式 (CODEC_STRING (0x11) の UTF-8) を読み、意味検証 (スコープ
/// [draft-ietf-moq-transport-23 §8.4 (Track and Object Properties)] と多重可否
/// [§12.1 (Malformed Tracks) 条件 9]) は [`ObjectProperties::validate`] が行う。同一 Type の KVP はそのまま複数読み取る (§8.4 の
/// "MUST allow multiple instances of the Property" により未知の Type の複数は正当である)。
///
/// `allow_immutable` が false の場合、IMMUTABLE_PROPERTIES (0x0B) の出現を拒否する
/// (入れ子 IMMUTABLE_PROPERTIES は draft-ietf-moq-transport-23 §10.7 (Immutable Properties) により
/// 禁止)。拒否時の分類は `context` に従う ([`KvpContext`] の doc 参照)。
fn decode_kv_pairs(
    inner: &[u8],
    allow_immutable: bool,
    context: KvpContext,
) -> Result<Vec<ObjectProperty>, MessageError> {
    let mut props = Vec::new();
    let mut inner_pos = 0;
    let mut prev_type: u64 = 0;

    while inner_pos < inner.len() {
        let (prop_type, _delta) = crate::kvp::decode_delta_key(prev_type, inner, &mut inner_pos)?;

        if !allow_immutable && prop_type == PROP_IMMUTABLE_PROPERTIES {
            let reason = "nested IMMUTABLE_PROPERTIES is not allowed (draft-ietf-moq-transport-23 §10.7 (Immutable Properties))";
            return Err(match context {
                // 受信した Object の malformed 条件 (§10.7 / §12.1)
                KvpContext::Decode => MessageError::MalformedTrack(reason),
                KvpContext::Encode => MessageError::ProtocolViolation(reason),
            });
        }
        prev_type = prop_type;

        let value = if prop_type % 2 == 0 {
            let (v, n) = varint::decode(&inner[inner_pos..])?;
            inner_pos += n;
            ObjectPropertyValue::VarInt(v)
        } else {
            let (len, n) = varint::decode(&inner[inner_pos..])?;
            inner_pos += n;
            // draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure): 値長上限 2^16-1 を
            // 超える Length は FIELD_LENGTH_EXCEEDED で閉じる
            if len > 65535 {
                return Err(MessageError::FieldLengthExceeded(
                    "object property value length exceeds 65535",
                ));
            }
            let len = varint::checked_len(len, inner[inner_pos..].len())?;
            let bytes = inner[inner_pos..inner_pos + len].to_vec();
            inner_pos += len;

            // CODEC_STRING (0x11) の値は UTF-8 文字列である。受信側が Type を理解する場合、
            // Value が定義された serialization と一致しなければ MALFORMED_KEY_VALUE で
            // セッションを閉じる (draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair
            // Structure) / draft-ietf-moq-loc-05 §2.3.1.3 (Codec String))。
            if prop_type == CODEC_STRING_TYPE {
                validate_codec_string_utf8(&bytes)?;
            }

            // IMMUTABLE_PROPERTIES 自体は許可するが内側を検証する
            if prop_type == PROP_IMMUTABLE_PROPERTIES {
                validate_immutable_inner(&bytes, context)?;
            }

            ObjectPropertyValue::Bytes(bytes)
        };

        props.push(ObjectProperty { prop_type, value });
    }

    Ok(props)
}

// ─── 重複 Object のフィールド一貫性追跡 (draft-ietf-moq-transport-23 §12.1 条件 6/7, §7.1) ───

/// Object の Delivery Mode (draft-ietf-moq-transport-23 §2.1.1 (Object Fields))
///
/// Object を Subgroup で送ったか Datagram で送ったかを表す。Original Publisher が
/// 初回送信の方法で Object の Delivery Mode を確定し、購読では Object を Delivery Mode に
/// 従って送らなければならない (MUST)。同一 Track 内で Object ごとに異なってよい
/// (§2.1: "An Original Publisher MAY use both Subgroups and Datagrams within a Group or Track.")。
///
/// この用語と規則は draft 由来であり将来の draft 改版で変わる可能性がある。
/// Subgroup ID は Delivery Mode とは別の Object フィールドであり、本 enum は保持しない
/// (Subgroup ID は `observe_object_fields` 系の別引数で渡す)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryMode {
    /// Subgroup 経由で送られた
    Subgroup,
    /// Datagram 経由で送られた
    Datagram,
}

/// 重複受信した Object のフィールド一貫性を追跡するトラッカー
///
/// draft-ietf-moq-transport-23 §7.1 (Caching Relays):
/// "An endpoint that receives a duplicate Object with a different Delivery Mode,
/// Subgroup ID, Priority or Payload MUST treat the track as Malformed."
///
/// draft-ietf-moq-transport-23 §12.1 (Malformed Tracks) 条件 6/7 の検出に使う。
/// 判定単位は Object 単位 (Group ID, Object ID)。同一 Track 内で Object ごとに
/// Delivery Mode が異なるだけの正当な Track を Malformed 扱いしない
/// (§2.1 の "An Original Publisher MAY use both Subgroups and Datagrams within a Group or
/// Track." と §5.1.2 (Scheduling Algorithm) の "If the two objects have different Delivery
/// Modes the datagram is sent first.")。
///
/// **送信側の強制は行わない。** §2.1.1 は Original Publisher の初回送信が Delivery Mode を
/// 確定すると定めるが、本トラッカーは受信側の重複検出専用であり、確定済みの Delivery Mode と
/// 異なる送信を禁止する追跡は持たない (relay 非対応のため、初回送信そのものが確定行為になる)。
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
/// 保持量: 1 レコードは immutables 長 (最大 65535 バイト) と payload_key 長を保持する。
/// [`ObjectFieldTracker::observe_object_fields_with_content`] が group の変化を検出した時点で
/// [`prune_past_groups`](Self::prune_past_groups) を呼び、さらに
/// [`ObjectFieldTracker::MAX_RECORDS`] を超えた分を古い記録から破棄するため、
/// 1 subscription あたりの保持量は有界である (破棄した Object の重複は検出しない)。
///
/// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
#[derive(Debug, Clone)]
pub struct ObjectFieldTracker {
    /// (group_id, object_id) → 初回受信時のフィールド値
    records: HashMap<(u64, u64), ObjectFieldRecord>,
    /// 直前に観測した group (group の変化を検出して prune するために保持する)
    last_group: Option<u64>,
    /// 実効 group 順序 (true = Ascending)。prune と上限超過時の破棄方向に使う
    ascending: bool,
}

impl Default for ObjectFieldTracker {
    /// group 順序を Ascending (draft-ietf-moq-transport-23 §10.5 (DEFAULT PUBLISHER GROUP ORDER) の既定) として作る
    ///
    /// 順序が不明な文脈向けの既定であり、subscription の実効順序が分かる場合は
    /// [`ObjectFieldTracker::new`] を使うこと。
    fn default() -> Self {
        Self {
            records: HashMap::new(),
            last_group: None,
            ascending: true,
        }
    }
}

/// 初回受信時に記録する Object のフィールド値
///
/// 比較はフィールド単位で行うため、構造体全体の等価比較は導出しない。
#[derive(Debug, Clone)]
struct ObjectFieldRecord {
    /// Object の Delivery Mode (Subgroup 経由 / Datagram 経由)
    delivery_mode: DeliveryMode,
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
    /// 保持する記録数の上限
    ///
    /// 1 レコードは IMMUTABLE_PROPERTIES の生バイト列 (最大 65535 バイト。draft-ietf-moq-transport-23
    /// §8.3 (Key-Value-Pair Structure) の "The maximum length of a value is 2^16-1 bytes") と
    /// payload_key を保持するため、datagram の
    /// [`MAX_DATAGRAM_TRACKING_ENTRIES_PER_SUBSCRIPTION`](crate::session::core::MAX_DATAGRAM_TRACKING_ENTRIES_PER_SUBSCRIPTION)
    /// (100_000) と同じ値は使えない (最悪 6.5 GB)。1_000 件なら最悪約 65.5 MB に収まり、
    /// 30 fps の映像では約 33 秒分の検出窓になる。
    /// 超過時は [`ObjectFieldTracker::observe_object_fields_with_content`] が古い記録から破棄する。
    pub const MAX_RECORDS: usize = 1_000;

    /// 空のトラッカーを作成する
    ///
    /// `ascending` は subscription の実効 group 順序 (draft-ietf-moq-transport-23 §5.1.1) を
    /// `true` = Ascending (0x1) / `false` = Descending (0x2) で渡す。順序が不明な場合は
    /// [`ObjectFieldTracker::default`] (Ascending) を使ってよい。
    pub fn new(ascending: bool) -> Self {
        Self {
            records: HashMap::new(),
            last_group: None,
            ascending,
        }
    }

    /// 記録している Object 数を返す (診断用)
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.records.len()
    }

    /// 実効 group 順序が Ascending かどうかを返す (診断用)
    #[cfg(test)]
    pub(crate) fn is_ascending(&self) -> bool {
        self.ascending
    }

    /// Object のフィールドを記録し、重複受信時に一貫性を検証する
    ///
    /// 初回受信時は記録して `Ok(())` を返す。同一 (group_id, object_id) の再受信時に
    /// Delivery Mode / Subgroup ID / Priority のいずれかが異なれば
    /// `Err(ObjectFieldMismatch)` を返す。
    ///
    /// `delivery_mode` が [`DeliveryMode::Datagram`] の場合は `subgroup_id` に `None` を渡す
    /// (draft-ietf-moq-transport-23 §2.1.1: "Objects sent in Datagrams do not have a Subgroup ID.")。
    pub fn observe_object_fields(
        &mut self,
        group_id: u64,
        object_id: u64,
        delivery_mode: DeliveryMode,
        subgroup_id: Option<u64>,
        publisher_priority: u8,
    ) -> Result<(), ObjectFieldMismatch> {
        self.observe_object_fields_with_content(
            group_id,
            object_id,
            delivery_mode,
            subgroup_id,
            publisher_priority,
            None,
            None,
        )
    }

    /// 重複受信した Object を、immutable properties と payload の比較キーも含めて追跡する
    ///
    /// draft-ietf-moq-transport-23 §12.1 (Malformed Tracks) 条件 6: "The same Object is
    /// received more than once with different Payload or other immutable properties."
    ///
    /// 初回受信時は記録して `Ok(())` を返す。判定規則と保持量は [`ObjectFieldTracker`] の
    /// doc を参照。
    ///
    /// 比較を確定させた後に、group が変化していれば
    /// [`prune_past_groups`](Self::prune_past_groups) で過去 group の記録を破棄し、
    /// [`ObjectFieldTracker::MAX_RECORDS`] を超えていれば古い記録を破棄する。破棄された Object の
    /// 重複は検出しない (known limitation: §12.1 (Malformed Tracks) 条件 6 と §7.1 (Caching Relays)
    /// の重複検出が及ばない範囲がある)。
    ///
    /// `delivery_mode` が [`DeliveryMode::Datagram`] の場合は `subgroup_id` に `None` を渡す
    /// (draft-ietf-moq-transport-23 §2.1.1: "Objects sent in Datagrams do not have a Subgroup ID.")。
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
        reason = "Delivery Mode / Subgroup ID / Priority と条件 6 の内容比較を 1 回の観測で渡すため"
    )]
    pub fn observe_object_fields_with_content(
        &mut self,
        group_id: u64,
        object_id: u64,
        delivery_mode: DeliveryMode,
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
                    delivery_mode,
                    subgroup_id,
                    publisher_priority,
                    immutable_properties: immutable_properties.map(<[u8]>::to_vec),
                    payload_key: payload_key.map(<[u8]>::to_vec),
                },
            );
            // 観測を確定させた後に group の変化へ追随し、上限を超えた分を破棄する
            self.prune_on_group_change(group_id);
            return Ok(());
        };
        // 比較結果は保持量の調整より先に確定させる (破棄した Object は以後比較されない。
        // 見逃し側に倒すため、破棄で不一致が消えることはあっても新たな不一致は生まれない)
        let mismatch = if prev.delivery_mode != delivery_mode {
            // draft §7.1: Delivery Mode の比較
            Some(ObjectFieldMismatch {
                group_id,
                object_id,
                reason: "malformed track: duplicate Object with different Delivery Mode",
            })
        } else if prev.subgroup_id != subgroup_id {
            // draft §7.1: Subgroup ID の比較
            Some(ObjectFieldMismatch {
                group_id,
                object_id,
                reason: "malformed track: duplicate Object with different Subgroup ID",
            })
        } else if prev.publisher_priority != publisher_priority {
            // draft §7.1: Priority の比較
            Some(ObjectFieldMismatch {
                group_id,
                object_id,
                reason: "malformed track: duplicate Object with different Priority",
            })
        } else if let (Some(prev_immutables), Some(immutables)) =
            (prev.immutable_properties.as_deref(), immutable_properties)
            && prev_immutables != immutables
        {
            // draft §12.1 (Malformed Tracks) 条件 6: immutable properties の差異
            Some(ObjectFieldMismatch {
                group_id,
                object_id,
                reason: "malformed track: duplicate Object with different immutable properties",
            })
        } else if let (Some(prev_key), Some(key)) = (prev.payload_key.as_deref(), payload_key)
            && prev_key != key
        {
            // draft §12.1 (Malformed Tracks) 条件 6 / §7.1 (Caching Relays): Payload の差異
            Some(ObjectFieldMismatch {
                group_id,
                object_id,
                reason: "malformed track: duplicate Object with different Payload",
            })
        } else {
            None
        };
        self.prune_on_group_change(group_id);
        match mismatch {
            Some(mismatch) => Err(mismatch),
            None => Ok(()),
        }
    }

    /// group が変化していれば過去 group の記録を破棄し、その後で件数上限を適用する
    ///
    /// prune は group が変わったときだけ実行する (同じ group 内の観測で毎回走査しない)。
    fn prune_on_group_change(&mut self, group_id: u64) {
        if self.last_group != Some(group_id) {
            self.last_group = Some(group_id);
            self.prune_past_groups(self.ascending, group_id);
        }
        self.enforce_record_cap();
    }

    /// 記録数が [`ObjectFieldTracker::MAX_RECORDS`] を超えている間、古い記録を破棄する
    ///
    /// 1. group が複数あるときは最も古い group (`ascending` なら最小の group_id、そうでなければ
    ///    最大の group_id) の記録を group 単位でまとめて破棄する
    /// 2. 1 group しか無いときは、その group の最も古い object_id から超過分を破棄する
    ///
    /// 破棄した Object の重複は検出しない (見逃し側に倒す)。
    fn enforce_record_cap(&mut self) {
        while self.records.len() > Self::MAX_RECORDS {
            let Some(oldest_group) = self.oldest_group() else {
                return;
            };
            if self.records.keys().all(|&(g, _)| g == oldest_group) {
                // 1 group しか無い: 古い object_id から超過分をまとめて破棄する
                let mut keys: Vec<(u64, u64)> = self.records.keys().copied().collect();
                keys.sort_unstable_by_key(|&(_, object_id)| object_id);
                let excess = self.records.len() - Self::MAX_RECORDS;
                for key in keys.into_iter().take(excess) {
                    self.records.remove(&key);
                }
            } else {
                self.records.retain(|&(g, _), _| g != oldest_group);
            }
        }
    }

    /// 最も古い group の group_id を返す (`ascending` なら最小、そうでなければ最大)
    fn oldest_group(&self) -> Option<u64> {
        if self.ascending {
            self.records.keys().map(|&(g, _)| g).min()
        } else {
            self.records.keys().map(|&(g, _)| g).max()
        }
    }

    /// group 境界での prune: 指定 group より過去のエントリを削除する
    ///
    /// `ObjectPropertyTracker::prune_past_groups` と同じセマンティクス。
    /// [`ObjectFieldTracker::observe_object_fields_with_content`] が group の変化を検出した時点で
    /// 自動的に呼ぶため、通常は呼び出し側が明示的に呼ぶ必要はない。保持量の詳細は
    /// [`ObjectFieldTracker`] の doc を参照。
    pub fn prune_past_groups(&mut self, ascending: bool, current_group: u64) {
        if ascending {
            self.records.retain(|&(g, _), _| g >= current_group);
        } else {
            self.records.retain(|&(g, _), _| g <= current_group);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ObjectFieldTracker の上限と破棄の規則 (draft-ietf-moq-transport-23 §12.1 (Malformed Tracks) の
    // 重複検出を有界の保持量で行うための best-effort な破棄) を検証する。
    //
    // `records` は private のため、件数は `len` で検査する (`tests/` は公開 API にだけ書く規約)。

    /// 1 group に上限を超える Object を観測すると、古い object_id から破棄されて上限に収まる
    #[test]
    fn single_group_records_are_capped() {
        let mut tracker = ObjectFieldTracker::new(true);
        let newest = ObjectFieldTracker::MAX_RECORDS as u64 + 9;
        for object_id in 0..=newest {
            tracker
                .observe_object_fields(0, object_id, DeliveryMode::Subgroup, Some(0), 0)
                .expect("重複ではない Object は受理されること");
        }
        assert_eq!(tracker.len(), ObjectFieldTracker::MAX_RECORDS);
        // 残っている記録は比較され、破棄された古い記録は比較されない (見逃し側)
        assert!(
            tracker
                .observe_object_fields(0, newest, DeliveryMode::Subgroup, Some(0), 1)
                .is_err(),
            "上限内に残っている Object は比較されること"
        );
        assert!(
            tracker
                .observe_object_fields(0, 0, DeliveryMode::Subgroup, Some(0), 9)
                .is_ok(),
            "破棄された Object は比較されないこと"
        );
    }

    /// 最新 group だけで上限を超え、かつ他 group も存在する場合は、
    /// 他 group の破棄 (1 段階目) に続いて最新 group の古い記録が破棄される (2 段階目)
    #[test]
    fn newest_group_over_cap_evicts_other_group_then_own_oldest() {
        let mut tracker = ObjectFieldTracker::new(true);
        let excess = 50_u64;
        let newest_group = 5_u64;
        // 新しい group を上限 + 超過分まで埋める
        for object_id in 0..(ObjectFieldTracker::MAX_RECORDS as u64 + excess) {
            tracker
                .observe_object_fields(newest_group, object_id, DeliveryMode::Subgroup, Some(0), 0)
                .expect("重複ではない Object は受理されること");
        }
        // ascending の prune では残る古い group を 1 件だけ観測する
        tracker
            .observe_object_fields(3, 0, DeliveryMode::Subgroup, Some(0), 0)
            .expect("重複ではない Object は受理されること");
        assert_eq!(
            tracker.len(),
            ObjectFieldTracker::MAX_RECORDS,
            "他 group の破棄後も最新 group だけで上限を超えるため、最新 group の古い記録も破棄されること"
        );
        // 古い group は破棄されている
        assert!(
            tracker
                .observe_object_fields(3, 0, DeliveryMode::Subgroup, Some(0), 9)
                .is_ok(),
            "破棄された group の Object は比較されないこと"
        );
        // 最新 group の新しい記録は残り、古い記録は破棄されている
        let newest_object = ObjectFieldTracker::MAX_RECORDS as u64 + excess - 1;
        assert!(
            tracker
                .observe_object_fields(
                    newest_group,
                    newest_object,
                    DeliveryMode::Subgroup,
                    Some(0),
                    9
                )
                .is_err(),
            "最新 group の新しい Object は比較されること"
        );
        assert!(
            tracker
                .observe_object_fields(newest_group, 0, DeliveryMode::Subgroup, Some(0), 9)
                .is_ok(),
            "最新 group の古い Object は破棄されること"
        );
    }

    /// 上限超過時は最も古い group が group 単位で破棄される
    ///
    /// ascending では group_id が最小の group が、descending では最大の group が最も古い
    /// (descending では group_id が大きい group から先に届くため)。
    #[test]
    fn eviction_drops_oldest_group() {
        for (ascending, oldest_group, newest_group) in [(true, 3_u64, 5_u64), (false, 7_u64, 5_u64)]
        {
            let mut tracker = ObjectFieldTracker::new(ascending);
            // 先に新しい group を上限まで埋め、後から最も古い group を観測する。
            // prune は両方を残すため、上限判定で最も古い group が破棄される
            for object_id in 0..(ObjectFieldTracker::MAX_RECORDS as u64) {
                tracker
                    .observe_object_fields(
                        newest_group,
                        object_id,
                        DeliveryMode::Subgroup,
                        Some(0),
                        0,
                    )
                    .expect("重複ではない Object は受理されること");
            }
            tracker
                .observe_object_fields(oldest_group, 0, DeliveryMode::Subgroup, Some(0), 0)
                .expect("重複ではない Object は受理されること");
            assert_eq!(tracker.len(), ObjectFieldTracker::MAX_RECORDS);
            assert!(
                tracker
                    .observe_object_fields(oldest_group, 0, DeliveryMode::Subgroup, Some(0), 9)
                    .is_ok(),
                "ascending = {ascending} で最も古い group ({oldest_group}) は破棄されること"
            );
            assert!(
                tracker
                    .observe_object_fields(newest_group, 0, DeliveryMode::Subgroup, Some(0), 9)
                    .is_err(),
                "ascending = {ascending} で新しい group ({newest_group}) は残ること"
            );
        }
    }

    /// group が変化した時点で過去 group の記録が破棄される (ascending / descending)
    #[test]
    fn group_change_prunes_past_groups() {
        // ascending: current 未満の group を破棄する
        let mut ascending = ObjectFieldTracker::new(true);
        ascending
            .observe_object_fields(1, 0, DeliveryMode::Subgroup, Some(0), 0)
            .expect("重複ではない Object は受理されること");
        ascending
            .observe_object_fields(2, 0, DeliveryMode::Subgroup, Some(0), 0)
            .expect("重複ではない Object は受理されること");
        assert_eq!(
            ascending.len(),
            1,
            "ascending では過去 group が破棄されること"
        );
        assert!(
            ascending
                .observe_object_fields(1, 0, DeliveryMode::Subgroup, Some(0), 9)
                .is_ok(),
            "破棄された過去 group の Object は比較されないこと"
        );

        // descending: current 超過の group を破棄する
        let mut descending = ObjectFieldTracker::new(false);
        descending
            .observe_object_fields(2, 0, DeliveryMode::Subgroup, Some(0), 0)
            .expect("重複ではない Object は受理されること");
        descending
            .observe_object_fields(1, 0, DeliveryMode::Subgroup, Some(0), 0)
            .expect("重複ではない Object は受理されること");
        assert_eq!(
            descending.len(),
            1,
            "descending では過去 group が破棄されること"
        );
        assert!(
            descending
                .observe_object_fields(2, 0, DeliveryMode::Subgroup, Some(0), 9)
                .is_ok(),
            "破棄された過去 group の Object は比較されないこと"
        );
    }
}
