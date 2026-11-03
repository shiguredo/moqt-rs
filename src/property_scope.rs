//! MOQT Property の許可スコープの判定 (内部モジュール)
//!
//! draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) は、エンドポイントが
//! Property を理解していてスコープ外で受信した場合に Track を malformed とすることを MUST
//! とする ("An endpoint that supports a Property and receives it outside its scope (e.g. a
//! Track-only Property as an Object Property) MUST treat the Track as malformed")。
//! §12.1 (Malformed Tracks) の条件 8 も同じ条件を列挙する。
//!
//! 本モジュールは「この実装が意味を理解する Property」の許可スコープを 1 箇所にまとめる。
//! Track Properties (`crate::track_properties`) と Object Properties
//! (`crate::object_properties`) の両方が同じ表を参照することで、Track-only / Object-only の
//! 判定が 2 箇所に分かれて片方だけ更新される事故を防ぐ。
//!
//! 未知の型と GREASE 値は `None` を返す。§8.4 は Relay に未知 Property の転送を MUST とし、
//! §16.8 (Properties) は "Endpoints MUST ignore unknown Property types" と定めるため、
//! スコープを推測して拒否してはならない。既知の型でも、本実装が解釈しない型 (例えば
//! draft-ietf-moq-secure-objects の Property) は `None` になる。
//!
//! 節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。

use crate::loc::{
    PROP_AUDIO_CONFIG, PROP_AUDIO_LEVEL, PROP_TIMESCALE, PROP_TIMESTAMP, PROP_VIDEO_CONFIG,
    PROP_VIDEO_FRAME_MARKING,
};
use crate::object_properties::{PROP_PRIOR_GROUP_ID_GAP, PROP_PRIOR_OBJECT_ID_GAP};
use crate::track_properties::{
    PROP_DEFAULT_PUBLISHER_GROUP_ORDER, PROP_DEFAULT_PUBLISHER_PRIORITY, PROP_DYNAMIC_GROUPS,
    PROP_IMMUTABLE_PROPERTIES, PROP_MAX_CACHE_DURATION, PROP_OBJECT_DELIVERY_TIMEOUT,
    PROP_SUBGROUP_DELIVERY_TIMEOUT,
};

/// Property の許可スコープ (draft-ietf-moq-transport-23 §16.8 (Properties) Table 15 / Table 16)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PropertyScope {
    /// Track Properties にのみ現れてよい
    Track,
    /// Object Properties にのみ現れてよい
    Object,
    /// Track Properties と Object Properties の両方に現れてよい
    Both,
}

/// この実装が意味を理解する Property Type の許可スコープを返す
///
/// draft-ietf-moq-transport-23 §16.8 (Properties) の Table 15 (MOQT 定義) と Table 16
/// (他 draft の暫定登録。本実装は draft-ietf-moq-loc の 6 型を `crate::loc` で解釈する) の
/// うち、本実装がアクセサを持つ型を返す。解釈しない型と GREASE 値は `None` を返す。
///
/// Table 16 の 0x0A (ENCRYPTED_LIST) / 0x32 (PADDING) は draft-ietf-moq-secure-objects の
/// 定義であり本実装は解釈しないため返さない (§16.8 の "Endpoints MUST ignore unknown
/// Property types")。
pub(crate) fn known_property_scope(prop_type: u64) -> Option<PropertyScope> {
    match prop_type {
        // draft-ietf-moq-transport-23 §16.8 Table 15: Track と Object の両方
        PROP_OBJECT_DELIVERY_TIMEOUT
        | PROP_SUBGROUP_DELIVERY_TIMEOUT
        | PROP_IMMUTABLE_PROPERTIES => Some(PropertyScope::Both),
        // draft-ietf-moq-transport-23 §16.8 Table 15: Track のみ
        PROP_MAX_CACHE_DURATION
        | PROP_DEFAULT_PUBLISHER_PRIORITY
        | PROP_DEFAULT_PUBLISHER_GROUP_ORDER
        | PROP_DYNAMIC_GROUPS => Some(PropertyScope::Track),
        // draft-ietf-moq-transport-23 §16.8 Table 15: Object のみ
        // (§10.8 (Prior Group ID Gap) / §10.9 (Prior Object ID Gap))
        PROP_PRIOR_GROUP_ID_GAP | PROP_PRIOR_OBJECT_ID_GAP => Some(PropertyScope::Object),
        // draft-ietf-moq-transport-23 §16.8 Table 16 / draft-ietf-moq-loc-05 §6.1:
        // Track と Object の両方
        PROP_TIMESCALE | PROP_VIDEO_CONFIG | PROP_AUDIO_CONFIG => Some(PropertyScope::Both),
        // draft-ietf-moq-transport-23 §16.8 Table 16 / draft-ietf-moq-loc-05 §6.1:
        // Object のみ
        PROP_TIMESTAMP | PROP_VIDEO_FRAME_MARKING | PROP_AUDIO_LEVEL => Some(PropertyScope::Object),
        _ => None,
    }
}

/// Track Properties に現れてはならない既知 Property (Object 専用) かを返す
///
/// 未知の型と GREASE 値は `false` を返す。
pub(crate) fn is_object_only(prop_type: u64) -> bool {
    known_property_scope(prop_type) == Some(PropertyScope::Object)
}

/// Object Properties に現れてはならない既知 Property (Track 専用) かを返す
///
/// 未知の型と GREASE 値は `false` を返す。0x4000-0x7FFF の Mandatory Track Property 範囲は
/// 範囲そのものが Track 専用であり (§3.7 / §16.8 の登録ポリシー)、型ごとの表ではなく
/// `crate::object_properties` が範囲で判定する。
pub(crate) fn is_track_only(prop_type: u64) -> bool {
    known_property_scope(prop_type) == Some(PropertyScope::Track)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MOQT 定義の 9 型のスコープが §16.8 Table 15 と一致すること
    ///
    /// Table 15 の Scope 列をそのまま期待値として書き下す。表の更新 (型の追加・スコープの
    /// 変更) があったときに、実装の取りこぼしをここで検出する。
    #[test]
    fn moqt_properties_match_table_15() {
        let expected = [
            (0x02_u64, PropertyScope::Both), // OBJECT_DELIVERY_TIMEOUT
            (0x04, PropertyScope::Track),    // MAX_CACHE_DURATION
            (0x06, PropertyScope::Both),     // SUBGROUP_DELIVERY_TIMEOUT
            (0x0B, PropertyScope::Both),     // IMMUTABLE_PROPERTIES
            (0x0E, PropertyScope::Track),    // DEFAULT_PUBLISHER_PRIORITY
            (0x22, PropertyScope::Track),    // DEFAULT_PUBLISHER_GROUP_ORDER
            (0x30, PropertyScope::Track),    // DYNAMIC_GROUPS
            (0x3C, PropertyScope::Object),   // PRIOR_GROUP_ID_GAP
            (0x3E, PropertyScope::Object),   // PRIOR_OBJECT_ID_GAP
        ];
        for (prop_type, scope) in expected {
            assert_eq!(
                known_property_scope(prop_type),
                Some(scope),
                "{prop_type:#x} のスコープが Table 15 と一致しない"
            );
        }
    }

    /// LOC の 6 型のスコープが draft-ietf-moq-loc-05 §6.1 と一致すること
    #[test]
    fn loc_properties_match_registry() {
        let expected = [
            (0x10_u64, PropertyScope::Object), // TIMESTAMP
            (0x08, PropertyScope::Both),       // TIMESCALE
            (0x09, PropertyScope::Object),     // VIDEO_FRAME_MARKING
            (0x0C, PropertyScope::Object),     // AUDIO_LEVEL
            (0x0D, PropertyScope::Both),       // VIDEO_CONFIG
            (0x0F, PropertyScope::Both),       // AUDIO_CONFIG
        ];
        for (prop_type, scope) in expected {
            assert_eq!(
                known_property_scope(prop_type),
                Some(scope),
                "{prop_type:#x} のスコープが LOC のレジストリと一致しない"
            );
        }
    }

    /// 解釈しない型はスコープを返さない
    ///
    /// draft-ietf-moq-secure-objects の 0x0A / 0x32 は Table 16 に登録があるが本実装は
    /// 意味を理解しないため、スコープ推測で拒否しない (§16.8 の MUST ignore)。
    #[test]
    fn unknown_properties_have_no_scope() {
        for prop_type in [0x00_u64, 0x0A, 0x32, 0x1000, 0x4000, 0x7FFF] {
            assert_eq!(
                known_property_scope(prop_type),
                None,
                "{prop_type:#x} は既知型として扱わないこと"
            );
            assert!(!is_track_only(prop_type));
            assert!(!is_object_only(prop_type));
        }
    }

    /// GREASE 値は Track-only / Object-only のどちらにもならない
    ///
    /// draft-ietf-moq-transport-23 §16.8 の Table 15 は GREASE を Scope Any として予約し、
    /// §13 (Grease) は未知値の受信だけでのセッション終了を禁じる。
    #[test]
    fn grease_values_have_no_scope() {
        for grease in [0x9D_u64, 0x401D, 0x7F9D] {
            assert!(
                crate::grease::is_grease(grease),
                "テストフィクスチャの前提条件を満たす ({grease:#x} は GREASE 値)"
            );
            assert_eq!(known_property_scope(grease), None);
            assert!(!is_track_only(grease));
            assert!(!is_object_only(grease));
        }
    }

    /// Track-only / Object-only の述語が相互排他であること
    #[test]
    fn predicates_are_mutually_exclusive() {
        for prop_type in 0..0x50_u64 {
            assert!(
                !(is_track_only(prop_type) && is_object_only(prop_type)),
                "{prop_type:#x} が両方の述語で真になった"
            );
        }
    }
}
