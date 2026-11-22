//! LOCATION FILTER パラメータの値のエンコード / デコード (draft-ietf-moq-transport-23 §9.19.9 (LOCATION FILTER Parameter))
//!
//! 公開型 [`LocationFilter`](super::LocationFilter) / [`LocationFilterContext`](super::LocationFilterContext) /
//! [`LocationFilterUpdate`](super::LocationFilterUpdate) の定義は親モジュール
//! (`crate::message_parameter`) に置き、本モジュールは [`LocationFilter`](super::LocationFilter) の
//! `impl` と private ヘルパだけを持つ。外部へは公開しない。
use super::{LocationFilter, LocationFilterContext};
use crate::{error::MessageError, message::common::Location, varint};
use alloc::vec::Vec;

// Location Filter Type (draft-ietf-moq-transport-23 §9.19.9 (LOCATION FILTER Parameter))
// 0x06 以上は PROTOCOL_VIOLATION であり、定数は定義しない。

/// Type 0x00 (None): フィルタなし
const LOCATION_FILTER_TYPE_NONE: u64 = 0x00;
/// Type 0x01 (Relative Start): 相対 StartGroup が続く
const LOCATION_FILTER_TYPE_RELATIVE_START: u64 = 0x01;
/// Type 0x02 (Absolute Start): StartGroup と StartObject が続く
const LOCATION_FILTER_TYPE_ABSOLUTE_START: u64 = 0x02;
/// Type 0x03 (Absolute Start, Group End): StartGroup / StartObject / EndGroupDelta が続く
const LOCATION_FILTER_TYPE_ABSOLUTE_START_GROUP_END: u64 = 0x03;
/// Type 0x04 (Absolute Range): StartGroup / StartObject / EndGroupDelta / EndObject が続く
const LOCATION_FILTER_TYPE_ABSOLUTE_RANGE: u64 = 0x04;
/// Type 0x05 (Next Object): フィールドなし
const LOCATION_FILTER_TYPE_NEXT_OBJECT: u64 = 0x05;

impl LocationFilter {
    /// typed filter を wire format のバイト列へエンコードする
    ///
    /// Location Filter Type を含む値そのもののバイト列を返す (パラメータの
    /// Type Delta と Length は含まない)。
    ///
    /// `StartGroup + EndGroupDelta` が 2^64 - 1 を超える値は §9.19.9 が
    /// PROTOCOL_VIOLATION を求める不正形であり、この関数は検証しない。
    /// `MessageParameters::encode` が値域を検証して拒否する。
    pub fn encode_to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        match self {
            // Type 0x00 は Type のみで、後続フィールドを持たない
            Self::NoFilter => varint::encode(LOCATION_FILTER_TYPE_NONE, &mut buf),
            Self::RelativeGroup { start_group } => {
                varint::encode(LOCATION_FILTER_TYPE_RELATIVE_START, &mut buf);
                varint::encode(*start_group, &mut buf);
            }
            Self::AbsoluteStart { start } => {
                varint::encode(LOCATION_FILTER_TYPE_ABSOLUTE_START, &mut buf);
                varint::encode(start.group_id, &mut buf);
                varint::encode(start.object_id, &mut buf);
            }
            Self::AbsoluteRange {
                start,
                end_group_delta,
            } => {
                varint::encode(LOCATION_FILTER_TYPE_ABSOLUTE_START_GROUP_END, &mut buf);
                varint::encode(start.group_id, &mut buf);
                varint::encode(start.object_id, &mut buf);
                varint::encode(*end_group_delta, &mut buf);
            }
            Self::AbsoluteRangeWithEnd {
                start,
                end_group_delta,
                end_object,
            } => {
                varint::encode(LOCATION_FILTER_TYPE_ABSOLUTE_RANGE, &mut buf);
                varint::encode(start.group_id, &mut buf);
                varint::encode(start.object_id, &mut buf);
                varint::encode(*end_group_delta, &mut buf);
                varint::encode(*end_object, &mut buf);
            }
            // Type 0x05 は Type のみで、後続フィールドを持たない
            Self::NextObject => varint::encode(LOCATION_FILTER_TYPE_NEXT_OBJECT, &mut buf),
        }
        buf
    }

    /// wire format のバイト列から typed filter をデコードする
    ///
    /// 公開 [`LocationFilter::encode_to_bytes`] の逆関数であり、Location Filter 単体の
    /// バイト列を扱う利用者向けに公開する。バイト列は Location Filter 1 つ分とみなし、
    /// 余剰バイトがあれば `MalformedKeyValue` を返す。
    ///
    /// # Errors
    ///
    /// - Location Filter Type が 0x06 以上: `InvalidParameter`
    ///   (draft-ietf-moq-transport-23 §9.19.9 (LOCATION FILTER Parameter))
    /// - Type が要求するフィールドがバッファ終端で欠落: `UnexpectedEof`
    /// - `StartGroup + EndGroupDelta` が 2^64 - 1 を超える: `DeltaEncodingOverflow`
    ///   (draft-ietf-moq-transport-23 §9.19.9 (LOCATION FILTER Parameter))
    /// - フィールドを読み切ったあとに余剰バイトがある: `MalformedKeyValue`
    pub fn decode(buf: &[u8]) -> Result<Self, MessageError> {
        let (filter, consumed) = Self::decode_partial(buf)?;
        if consumed != buf.len() {
            return Err(MessageError::MalformedKeyValue(
                "LOCATION_FILTER has trailing bytes",
            ));
        }
        Ok(filter)
    }

    /// 先頭の Location Filter Type と、その Type が定める必須フィールドだけを
    /// デコードして `(filter, 消費バイト数)` を返す
    ///
    /// draft-ietf-moq-transport-23 §9.19.9 (LOCATION FILTER Parameter):
    /// Type ごとの必須フィールド数は 0x00 / 0x05 が 0 個、0x01 が 1 個、
    /// 0x02 が 2 個、0x03 が 3 個、0x04 が 4 個である。Type が一意にフィールド数を
    /// 定めるため、フィールド数不一致を表すワイヤは存在しない。
    ///
    /// 欠落したフィールド (バッファ終端) は `UnexpectedEof`、Type 0x06 以上は
    /// `InvalidParameter`、`StartGroup + EndGroupDelta` のオーバーフローは
    /// `DeltaEncodingOverflow` になる
    /// (draft-ietf-moq-transport-23 §9.19.9 (LOCATION FILTER Parameter))。
    ///
    /// 親モジュールの値デコーダが消費バイト数を必要とするため `pub(super)` にしている。
    /// クレート外へは公開しない。
    pub(super) fn decode_partial(buf: &[u8]) -> Result<(Self, usize), MessageError> {
        let (filter_type, mut pos) = varint::decode(buf)?;

        let filter = match filter_type {
            LOCATION_FILTER_TYPE_NONE => Self::NoFilter,
            LOCATION_FILTER_TYPE_RELATIVE_START => {
                let (start_group, n) = varint::decode(&buf[pos..])?;
                pos += n;
                Self::RelativeGroup { start_group }
            }
            LOCATION_FILTER_TYPE_ABSOLUTE_START => {
                let start = decode_location(buf, &mut pos)?;
                Self::AbsoluteStart { start }
            }
            LOCATION_FILTER_TYPE_ABSOLUTE_START_GROUP_END => {
                let start = decode_location(buf, &mut pos)?;
                let (end_group_delta, n) = varint::decode(&buf[pos..])?;
                pos += n;
                Self::AbsoluteRange {
                    start,
                    end_group_delta,
                }
            }
            LOCATION_FILTER_TYPE_ABSOLUTE_RANGE => {
                let start = decode_location(buf, &mut pos)?;
                let (end_group_delta, n) = varint::decode(&buf[pos..])?;
                pos += n;
                let (end_object, n) = varint::decode(&buf[pos..])?;
                pos += n;
                Self::AbsoluteRangeWithEnd {
                    start,
                    end_group_delta,
                    end_object,
                }
            }
            LOCATION_FILTER_TYPE_NEXT_OBJECT => Self::NextObject,
            // draft-ietf-moq-transport-23 §9.19.9 (LOCATION FILTER Parameter): "If an endpoint
            // receives any other Location Filter Type, it MUST close the session with
            // INVALID_PARAMETER."
            _ => {
                return Err(MessageError::InvalidParameter);
            }
        };

        filter.validate()?;
        Ok((filter, pos))
    }

    /// 値が §9.19.9 の制約を満たすか検証する
    ///
    /// draft-ietf-moq-transport-23 §9.19.9: `StartGroup + EndGroupDelta` が
    /// 2^64 - 1 を超える場合は DELTA_ENCODING_OVERFLOW でセッションを閉じなければならない。
    /// public enum は decode を経ずに構築できるため、decode 側と encode 側の両方の検証に使う。
    ///
    /// 親モジュールのエンコード / デコード検証から呼ぶため `pub(super)` にしている。
    /// クレート外へは公開しない。
    pub(super) fn validate(&self) -> Result<(), MessageError> {
        match self {
            Self::AbsoluteRange {
                start,
                end_group_delta,
            }
            | Self::AbsoluteRangeWithEnd {
                start,
                end_group_delta,
                ..
            } => check_end_group_overflow(start.group_id, *end_group_delta),
            _ => Ok(()),
        }
    }

    /// フィルタの実効 Start Location を Largest Object から導出する
    /// (draft-ietf-moq-transport-23 §3.3.1 (Location Filters))
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
    /// - NoFilter: フィルタなしのため track 全体の先頭 `{0, 0}` を返す
    ///
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
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
            // NoFilter は track 全体を通すため、下限は先頭 `{0, 0}` になる
            Self::NoFilter => Some(Location {
                group_id: 0,
                object_id: 0,
            }),
        }
    }

    /// フィルタ自身が明示する End Location を返す
    ///
    /// draft-ietf-moq-transport-23 §9.19.9 (LOCATION FILTER Parameter) の Table 6 のうち、
    /// End を運ぶのは Type 0x03 (Absolute Start, Group End) と Type 0x04 (Absolute Range)
    /// だけである。End を省略した FETCH の End は Largest Object であり
    /// (同節 "When they are omitted from a Fetch, the EndGroup and EndObject are Largest
    /// Object." / §3.2 (Fetch))、要求そのものが固定した終端ではない。
    ///
    /// [`effective_end_location`](Self::effective_end_location) が Fetch context で
    /// Largest Object を終端として返すのに対し、本メソッドは要求が明示した終端だけを返す。
    /// FETCH の応答範囲を Largest Object と区別して扱うために使う。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub fn explicit_end_location(&self) -> Option<Location> {
        match self {
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
            Self::RelativeGroup { .. }
            | Self::NextObject
            | Self::AbsoluteStart { .. }
            | Self::NoFilter => None,
        }
    }

    /// フィルタの実効 End Location を導出する
    /// (draft-ietf-moq-transport-23 §3.3.1 (Location Filters) / §9.19.9 (LOCATION FILTER Parameter))
    ///
    /// End フィールド省略時の扱いはコンテキストで異なる。subscription は open-ended
    /// (終端なし) のため `None`、Fetch は End = Largest Object のため `largest` を返す。
    /// 未配信 (`None`) の Fetch は終端未定のため `None` を返す (単独 FETCH では絶対形式の
    /// 範囲判定に委ね、fill では Largest Object が不明なため開設しない)。
    /// NoFilter も open-ended として同じ扱いになる。
    ///
    /// AbsoluteRange (EndObject 省略) は End Group の全 Object を含むため
    /// `{End Group, u64::MAX}` を返す。AbsoluteRangeWithEnd は
    /// `{StartGroup + EndGroupDelta, EndObject}` を返す。
    ///
    /// `StartGroup + EndGroupDelta` のオーバーフローは decode 時に DELTA_ENCODING_OVERFLOW 化
    /// されるため wire 由来の値では起こらない。ただし `LocationFilter` は公開 enum で decode を経ず
    /// in-memory 構築もできるため、パニックを避けて `u64::MAX` に飽和させる。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub fn effective_end_location(
        &self,
        largest: Option<&Location>,
        context: LocationFilterContext,
    ) -> Option<Location> {
        match self {
            Self::RelativeGroup { .. }
            | Self::NextObject
            | Self::AbsoluteStart { .. }
            | Self::NoFilter => match context {
                LocationFilterContext::Subscription => None,
                LocationFilterContext::Fetch => largest.cloned(),
            },
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

    /// Largest Object を基準に範囲が定まる相対フィルタか返す
    ///
    /// draft-ietf-moq-transport-23 §3.3.1 (Location Filters) は Location Filter の一部が
    /// Largest Object を基準に定義されるとし、§9.19.9 (LOCATION FILTER Parameter) の
    /// Table 6 は Type 0x01 (Relative Start) の Start Location を
    /// `{Largest Object.Group + 1 - StartGroup, 0}`、Type 0x05 (Next Object) の Start を
    /// Next Object と定める。この 2 形式だけが Largest Object を参照する相対フィルタであり、
    /// 残りの形式 (Type 0x00 / 0x02 / 0x03 / 0x04) は絶対値だけで範囲が定まる。
    ///
    /// draft-ietf-moq-transport-23 §3.2 (Fetch): "If the Location filter is relative
    /// (see Section 9.19.9), the publisher MUST return REQUEST_ERROR with error code
    /// INVALID_RANGE." の判定は解決後の Location ではなくフィルタの形式で行う。
    /// 解決値で判定すると、Largest Object が未観測の相対フィルタが `{0, 0}` に解決され、
    /// 絶対指定のフィルタと区別できなくなる。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub fn is_relative(self) -> bool {
        matches!(self, Self::RelativeGroup { .. } | Self::NextObject)
    }
}

/// Location Filter の StartGroup / StartObject をデコードし `pos` を進める
///
/// draft-ietf-moq-transport-23 §9.19.9 (LOCATION FILTER Parameter): Type 0x02 以上が
/// 持つ Start Location は vi64 (Group) + vi64 (Object) の絶対値である。
/// バッファ終端でフィールドが欠落した場合は `UnexpectedEof` を返す。
fn decode_location(buf: &[u8], pos: &mut usize) -> Result<Location, MessageError> {
    let (group_id, n) = varint::decode(&buf[*pos..])?;
    *pos += n;
    let (object_id, n) = varint::decode(&buf[*pos..])?;
    *pos += n;
    Ok(Location {
        group_id,
        object_id,
    })
}

/// LOCATION_FILTER の End Group 導出時のオーバーフローを検証する
///
/// draft-ietf-moq-transport-23 §9.19.9 (LOCATION FILTER Parameter): StartGroup + EndGroupDelta が
/// 2^64 - 1 を超えたら DELTA_ENCODING_OVERFLOW でセッションを閉じなければならない。
/// Delta = 0 は当該 Group の残り全部であり常に正当。
fn check_end_group_overflow(start_group: u64, end_group_delta: u64) -> Result<(), MessageError> {
    if end_group_delta != 0 && start_group.checked_add(end_group_delta).is_none() {
        return Err(MessageError::DeltaEncodingOverflow(
            "LOCATION_FILTER end group ID overflows u64",
        ));
    }
    Ok(())
}
