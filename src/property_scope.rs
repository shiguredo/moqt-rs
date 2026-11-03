//! MOQT Property の許可スコープと多重可否の判定 (内部モジュール)
//!
//! draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) は、エンドポイントが
//! Property を理解していてスコープ外で受信した場合に Track を malformed とすることを MUST
//! とする ("An endpoint that supports a Property and receives it outside its scope (e.g. a
//! Track-only Property as an Object Property) MUST treat the Track as malformed")。
//! §12.1 (Malformed Tracks) の条件 8 も同じ条件を列挙する。
//!
//! 同じ §8.4 は、Property の定義が複数を許す場合を除いて Track / Object に 1 インスタンスだけ
//! 載せてよいと定め ("A Track or Object MUST NOT contain more than one instance of the
//! Property.")、許される数より多く受信したエンドポイントに Track を malformed とすることを
//! MUST とする。§12.1 の条件 9 も同じ条件を列挙する。本モジュールは「この実装が意味を
//! 理解する Property」の許可スコープと多重可否を 1 箇所にまとめる。Track Properties
//! (`crate::track_properties`)・Object Properties (`crate::object_properties`)・LOC
//! Properties (`crate::loc`) が同じ表を参照することで、判定が複数箇所に分かれて片方だけ
//! 更新される事故を防ぐ。
//!
//! 未知の型と GREASE 値はスコープを持たず (`None`)、複数インスタンスを許す。§8.4 は Relay に
//! 未知 Property の転送と「複数インスタンスの許容」を MUST とし
//! ("MUST allow multiple instances of the Property")、§16.8 (Properties) は "Endpoints MUST
//! ignore unknown Property types" と定めるため、スコープや多重可否を推測して拒否しては
//! ならない。既知の型でも、本実装が解釈しない型 (例えば draft-ietf-moq-secure-objects の
//! Property) は未知として扱う。
//!
//! AUTHORIZATION_TOKEN (draft-ietf-moq-transport-23 §8.9 (Authorization Token Compression))
//! などの message parameter は別の名前空間の規則であり、Property の多重可否には流用しない。
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
use alloc::vec::Vec;

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

/// Property Type が複数インスタンスを許すかを返す
///
/// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) は「Property の定義が
/// 許す場合を除いて 1 インスタンスまで」と定め、理解できない Property については
/// "MUST allow multiple instances of the Property" とする。§16.8 (Properties) の
/// "Endpoints MUST ignore unknown Property types" も同じ扱いを求める。
///
/// この実装が意味を理解する既知の型 (draft-ietf-moq-transport-23 §16.8 Table 15 / Table 16 の
/// うち [`known_property_scope`] が返す型) は、いずれの定義節にも複数インスタンスを許す規則が
/// 無いため単一に限られる。LOC Properties の各定義 (draft-ietf-moq-loc-05 §2.3) にも複数を
/// 許す規則は無い。既知の型に複数を許す定義が加わった場合は、この関数の判定を型ごとに
/// 分ける必要がある。
///
/// draft-ietf-moq-transport-23 §10.7 (Immutable Properties) は、複数の値を許す Property に
/// 限り mutable リストと IMMUTABLE_PROPERTIES の内側の両方に同じ型が現れてよいと定める。
/// 単一の型を数えるときは両方のリストを合わせて数えること。
pub(crate) fn allows_multiple_instances(prop_type: u64) -> bool {
    known_property_scope(prop_type).is_none()
}

/// 単一に限られる既知 Property の型が重複しているかを返す
///
/// `prop_types` には、mutable リストと IMMUTABLE_PROPERTIES の内側を合わせた Track / Object
/// 内の全インスタンスの型を渡す (draft-ietf-moq-transport-23 §10.7 (Immutable Properties) の
/// "MUST search both")。複数インスタンスを許す型 (未知の型と GREASE 値) は数えない。
///
/// 重複を見つけた時点で `true` を返すため、未知の型を大量に含む入力でも走査は 1 回で済む。
/// 単一に限られる既知の型は [`known_property_scope`] が返す型だけであり、その個数は表の大きさ
/// で抑えられる。
pub(crate) fn has_duplicate_single_property(prop_types: impl IntoIterator<Item = u64>) -> bool {
    let mut seen: Vec<u64> = Vec::new();
    for prop_type in prop_types {
        if allows_multiple_instances(prop_type) {
            continue;
        }
        if seen.contains(&prop_type) {
            return true;
        }
        seen.push(prop_type);
    }
    false
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

    // ─── 多重可否 ─────────────────────────────────────────────

    /// 既知の型は単一に限られ、未知の型と GREASE 値は複数を許すこと
    ///
    /// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties): 既知の型は定義が
    /// 許す場合を除いて 1 インスタンスまで ("A Track or Object MUST NOT contain more than one
    /// instance of the Property.")。未知の型は "MUST allow multiple instances of the
    /// Property" (§16.8 の "Endpoints MUST ignore unknown Property types")。
    #[test]
    fn known_properties_allow_single_instance_only() {
        let known = [
            0x02_u64, 0x04, 0x06, 0x08, 0x09, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x22, 0x30, 0x3C,
            0x3E,
        ];
        for prop_type in known {
            assert!(
                known_property_scope(prop_type).is_some(),
                "テストフィクスチャの前提条件を満たす ({prop_type:#x} は既知型)"
            );
            assert!(
                !allows_multiple_instances(prop_type),
                "{prop_type:#x} は単一に限られること"
            );
        }
        for prop_type in [0x00_u64, 0x0A, 0x11, 0x32, 0x9D, 0x1000, 0x401D, 0x7F9D] {
            assert!(
                known_property_scope(prop_type).is_none(),
                "テストフィクスチャの前提条件を満たす ({prop_type:#x} は未知型)"
            );
            assert!(
                allows_multiple_instances(prop_type),
                "{prop_type:#x} は複数インスタンスを許すこと"
            );
        }
    }

    /// 既知の型の重複だけを検出し、未知の型の重複は検出しないこと
    #[test]
    fn duplicate_detection_ignores_unknown_types() {
        assert!(
            !has_duplicate_single_property(core::iter::empty()),
            "空の列は重複ではないこと"
        );
        assert!(
            !has_duplicate_single_property([0x1000_u64, 0x1000, 0x1001, 0x1001]),
            "未知の型の重複は許すこと"
        );
        assert!(
            !has_duplicate_single_property([0x9D_u64, 0x9D, 0x401D, 0x401D]),
            "GREASE 値の重複は許すこと"
        );
        assert!(
            has_duplicate_single_property([0x02_u64, 0x1000, 0x02]),
            "既知の型が 2 回現れたら重複とすること"
        );
        assert!(
            has_duplicate_single_property([0x0B_u64, 0x0B]),
            "IMMUTABLE_PROPERTIES も単一に限られること"
        );
        assert!(
            !has_duplicate_single_property([0x02_u64, 0x06, 0x0B, 0x1000]),
            "異なる既知の型の組み合わせは重複ではないこと"
        );
    }
}
