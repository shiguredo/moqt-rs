//! multi-variant track switching の packaging 検証 (publisher 専用)
//!
//! draft-ietf-moq-msf-02 §4.3 (Packaging for multi-variant track switching) は、alternate
//! group 内で keyframe 間隔の異なる複数 track を配信する場合に、各 MOQT Group が
//! キーフレームで始まること、1 Group に複数のキーフレームを置かないこと、各 track が
//! `keyframeSpacing` を持つこと、開始 Group ID が同一で以降 1 ずつ増えることを MUST とする。
//!
//! 本モジュールは encoder を持たない単体で検証・割り当てできる API を提供する。複数 variant の
//! encoder と実送信は未実装であり、Object 境界の time-alignment は encoder 出力に依存するため
//! 本モジュールの検証対象外である。

use shiguredo_moqt::error::MessageError;

/// alternate group 内の variant 群に割り当てる Group ID
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AltGroupGroupIds {
    /// 全 variant が共有する開始 Group ID
    pub start_group_id: u64,
    /// 各 variant の開始 Group ID (すべて `start_group_id` と同一)
    pub variant_start_group_ids: Vec<u64>,
}

impl AltGroupGroupIds {
    /// `group_index` 番目の Group ID を返す
    ///
    /// 開始 Group ID から 1 ずつ増える (§4.3)。variant ごとの Group ID は同一である。
    pub fn group_id(&self, group_index: u64) -> u64 {
        self.start_group_id + group_index
    }
}

/// alternate group の variant 群へ、共有する開始 Group ID を割り当てる
///
/// §4.3 の MUST に従い、全 variant の開始 Group ID を同一にする。以降の Group ID は
/// [`AltGroupGroupIds::group_id`] で 1 ずつ増やす。`variant_count` が 0 の場合は空の
/// variant 一覧を返す。
pub fn allocate_alt_group_group_ids(start_group_id: u64, variant_count: usize) -> AltGroupGroupIds {
    AltGroupGroupIds {
        start_group_id,
        variant_start_group_ids: vec![start_group_id; variant_count],
    }
}

/// variant の 1 Object 分のメタデータ
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VariantObject {
    /// MOQT Group ID
    pub group_id: u64,
    /// MOQT Object ID
    pub object_id: u64,
    /// この Object がキーフレームか
    pub is_keyframe: bool,
}

/// 1 variant の packaging メタデータ
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantPackaging {
    /// カタログの `keyframeSpacing` (§5.2.46)
    pub keyframe_spacing: Option<u64>,
    /// この variant の開始 Group ID
    pub start_group_id: u64,
    /// 送信する Object のメタデータ (Group の出現順)
    pub objects: Vec<VariantObject>,
}

/// multi-variant publisher の packaging を検証する
///
/// §4.3 の MUST のうち、入力のメタデータだけで判定できる次を検証する。
///
/// - 全 variant が `keyframeSpacing` を持つ
/// - 全 variant の開始 Group ID が同一
/// - 各 variant の Group ID が開始 ID から 1 ずつ増える
/// - 各 Group の先頭 Object がキーフレームで、キーフレームは 1 Group に 1 つだけ
///
/// Object 境界の time-alignment は variant ごとの encoder 出力に依存するため検証しない。
/// また、§4.3 は keyframe 間隔が異なる場合の MUST であり、単一 variant や共通の catalog
/// 検証には適用しない。
pub fn validate_multi_variant_packaging(variants: &[VariantPackaging]) -> Result<(), MessageError> {
    let Some(first) = variants.first() else {
        return Err(MessageError::InvalidCatalog(
            "multi-variant packaging requires at least one variant".to_string(),
        ));
    };
    for variant in variants {
        if variant.keyframe_spacing.is_none() {
            return Err(MessageError::InvalidCatalog(
                "every variant MUST specify keyframeSpacing".to_string(),
            ));
        }
        if variant.start_group_id != first.start_group_id {
            return Err(MessageError::InvalidCatalog(
                "all variants in an alternate group MUST share the same start group ID".to_string(),
            ));
        }
    }

    for variant in variants {
        let mut expected_group_id = variant.start_group_id;
        let mut current_group = None;
        let mut keyframes_in_group = 0_usize;
        for object in &variant.objects {
            if current_group != Some(object.group_id) {
                if let Some(previous) = current_group {
                    if object.group_id != previous + 1 {
                        return Err(MessageError::InvalidCatalog(
                            "group IDs in a variant MUST increase by 1".to_string(),
                        ));
                    }
                } else if object.group_id != expected_group_id {
                    return Err(MessageError::InvalidCatalog(
                        "variant MUST start at the shared start group ID".to_string(),
                    ));
                }
                current_group = Some(object.group_id);
                keyframes_in_group = 0;
                if !object.is_keyframe {
                    return Err(MessageError::InvalidCatalog(
                        "the first object of every group MUST be a keyframe".to_string(),
                    ));
                }
            }
            if object.is_keyframe {
                keyframes_in_group += 1;
                if keyframes_in_group > 1 {
                    return Err(MessageError::InvalidCatalog(
                        "a group MUST NOT contain more than one keyframe".to_string(),
                    ));
                }
            }
            expected_group_id = object.group_id;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(group_id: u64, object_id: u64, is_keyframe: bool) -> VariantObject {
        VariantObject {
            group_id,
            object_id,
            is_keyframe,
        }
    }

    fn valid_variant(start_group_id: u64) -> VariantPackaging {
        VariantPackaging {
            keyframe_spacing: Some(30),
            start_group_id,
            objects: vec![
                object(start_group_id, 0, true),
                object(start_group_id, 1, false),
                object(start_group_id + 1, 0, true),
            ],
        }
    }

    #[test]
    fn allocates_shared_start_group_ids() {
        let ids = allocate_alt_group_group_ids(100, 3);
        assert_eq!(ids.start_group_id, 100);
        assert_eq!(ids.variant_start_group_ids, vec![100, 100, 100]);
        assert_eq!(ids.group_id(0), 100);
        assert_eq!(ids.group_id(2), 102);
    }

    #[test]
    fn accepts_aligned_variants() {
        assert!(
            validate_multi_variant_packaging(&[valid_variant(100), valid_variant(100)]).is_ok()
        );
    }

    #[test]
    fn rejects_missing_keyframe_spacing() {
        let mut variant = valid_variant(100);
        variant.keyframe_spacing = None;
        assert!(validate_multi_variant_packaging(&[variant]).is_err());
    }

    #[test]
    fn rejects_different_start_group_ids() {
        assert!(
            validate_multi_variant_packaging(&[valid_variant(100), valid_variant(101)]).is_err()
        );
    }

    #[test]
    fn rejects_first_object_without_keyframe() {
        let mut variant = valid_variant(100);
        variant.objects[0].is_keyframe = false;
        assert!(validate_multi_variant_packaging(&[variant]).is_err());
    }

    #[test]
    fn rejects_multiple_keyframes_in_group() {
        let mut variant = valid_variant(100);
        variant.objects[1].is_keyframe = true;
        assert!(validate_multi_variant_packaging(&[variant]).is_err());
    }

    #[test]
    fn rejects_group_id_gap() {
        let variant = VariantPackaging {
            keyframe_spacing: Some(30),
            start_group_id: 100,
            objects: vec![object(100, 0, true), object(102, 0, true)],
        };
        assert!(validate_multi_variant_packaging(&[variant]).is_err());
    }
}
