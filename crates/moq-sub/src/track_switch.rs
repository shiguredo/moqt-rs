//! alternate group 内の multi-variant track switching (§6.3)
//!
//! draft-ietf-moq-msf-02 §6.3 (Multi-Variant Fast Track Switching) の switch point 判定、
//! 初期購読先の選択、購読の切り替え状態を提供する。実際の SUBSCRIBE / UNSUBSCRIBE と
//! decoder の再初期化は呼び出し側が [`SwitchAction`] に従って行う。
//!
//! 開始 Group ID は catalog の track object には含まれないため、呼び出し側が
//! [`VariantTrack::start_group_id`] として渡す。§4.3 は alternate group 内の全 track の開始
//! Group ID が同一であることを MUST とする。

use shiguredo_moqt::msf::MsfTrack;

/// switching 対象の variant track
#[derive(Debug, Clone, Copy)]
pub struct VariantTrack<'a> {
    /// カタログの track
    pub track: &'a MsfTrack,
    /// alternate group の開始 Group ID (§4.3)
    pub start_group_id: u64,
}

/// switch 先の受信開始位置
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwitchTarget {
    /// target track で受信を開始する Group ID
    pub group_id: u64,
    /// target track で受信を開始する Object ID (常に 0)
    pub object_id: u64,
}

/// 切り替え時に呼び出し側が行う操作
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwitchAction {
    /// 購読解除する旧 variant の index
    pub old_index: usize,
    /// 購読を開始する新 variant の index
    pub new_index: usize,
    /// 新 variant の受信開始位置
    pub target: SwitchTarget,
    /// decoder 状態を作り直すか (常に true)
    pub reset_decoder: bool,
}

/// 利用可能帯域と keyframeSpacing から初期購読先を選ぶ
///
/// §6.3: 帯域制約を満たす最も近い switch point を持つ track、すなわち `keyframeSpacing` が
/// 最小の track を選ぶ (SHOULD)。帯域が既知なら `bitrate` が帯域以下の track だけを候補にし、
/// 該当が無ければ `bitrate` が最小の track を選ぶ。`keyframeSpacing` が None または 0 の
/// track は切り替え候補にしない。帯域不明なら `keyframeSpacing` 最小、同値なら `bitrate` 最小。
pub fn select_initial_variant(
    variants: &[VariantTrack<'_>],
    bandwidth: Option<u64>,
) -> Option<usize> {
    let candidates = variants
        .iter()
        .enumerate()
        .filter(|(_, variant)| {
            variant
                .track
                .keyframe_spacing
                .is_some_and(|spacing| spacing > 0)
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }

    let within_bandwidth = bandwidth.map(|bandwidth| {
        candidates
            .iter()
            .copied()
            .filter(|(_, variant)| {
                variant
                    .track
                    .bitrate
                    .is_some_and(|bitrate| bitrate <= bandwidth)
            })
            .collect::<Vec<_>>()
    });

    let pool = match within_bandwidth {
        Some(pool) if !pool.is_empty() => pool,
        Some(_) => {
            let min_bitrate = candidates
                .iter()
                .filter_map(|(_, variant)| variant.track.bitrate)
                .min();
            candidates
                .iter()
                .copied()
                .filter(|(_, variant)| variant.track.bitrate == min_bitrate)
                .collect()
        }
        None => candidates,
    };

    pool.into_iter()
        .min_by_key(|(_, variant)| {
            (
                variant.track.keyframe_spacing.unwrap_or(u64::MAX),
                variant.track.bitrate.unwrap_or(u64::MAX),
            )
        })
        .map(|(index, _)| index)
}

/// source の Group / Object が target への valid switch point かを判定する
///
/// §6.3: source の keyframeSpacing を S、target を T、alternate group の開始 Group ID を G0 と
/// すると、`(G - G0) * S + N` が T で割り切れるときだけ切り替えられる。target の Group ID は
/// `G0 + ((G - G0) * S + N) / T` で、Object ID 0 から受信する。開始 Group ID が source と
/// target で異なる場合、S / T が None または 0 の場合は切り替え不可。
pub fn switch_target(
    source: &VariantTrack<'_>,
    target: &VariantTrack<'_>,
    group_id: u64,
    object_id: u64,
) -> Option<SwitchTarget> {
    if source.start_group_id != target.start_group_id {
        return None;
    }
    let source_spacing = source.track.keyframe_spacing.filter(|value| *value > 0)?;
    let target_spacing = target.track.keyframe_spacing.filter(|value| *value > 0)?;
    let group_offset = group_id.checked_sub(source.start_group_id)?;
    let media_offset = group_offset
        .checked_mul(source_spacing)?
        .checked_add(object_id)?;
    if !media_offset.is_multiple_of(target_spacing) {
        return None;
    }
    Some(SwitchTarget {
        group_id: source
            .start_group_id
            .checked_add(media_offset / target_spacing)?,
        object_id: 0,
    })
}

/// 現在の variant と切り替え履歴を保持する購読状態
#[derive(Debug, Clone)]
pub struct TrackSwitchState {
    /// 現在購読している variant の index
    pub current_index: usize,
}

impl TrackSwitchState {
    /// 初期購読先を確定して状態を作る
    pub fn new(initial_index: usize) -> Self {
        Self {
            current_index: initial_index,
        }
    }

    /// valid switch point なら target へ切り替える
    ///
    /// 戻り値は旧購読の解除、新購読の開始位置、decoder 再初期化を含む。invalid なら状態を
    /// 変えず `None` を返す。
    pub fn switch_to(
        &mut self,
        variants: &[VariantTrack<'_>],
        target_index: usize,
        group_id: u64,
        object_id: u64,
    ) -> Option<SwitchAction> {
        if target_index == self.current_index {
            return None;
        }
        let source = variants.get(self.current_index)?;
        let target = variants.get(target_index)?;
        let target = switch_target(source, target, group_id, object_id)?;
        let old_index = self.current_index;
        self.current_index = target_index;
        Some(SwitchAction {
            old_index,
            new_index: target_index,
            target,
            reset_decoder: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shiguredo_moqt::msf::MsfPackaging;

    fn variant<'a>(track: &'a MsfTrack, start_group_id: u64) -> VariantTrack<'a> {
        VariantTrack {
            track,
            start_group_id,
        }
    }

    fn track(name: &str, spacing: Option<u64>, bitrate: Option<u64>) -> MsfTrack {
        let mut track = MsfTrack::new(name.to_string(), MsfPackaging::Loc, true);
        track.keyframe_spacing = spacing;
        track.bitrate = bitrate;
        track
    }

    #[test]
    fn switch_point_formula_uses_nonzero_start_group() {
        let source = track("source", Some(30), Some(1_000));
        let target = track("target", Some(60), Some(2_000));
        let variants = [variant(&source, 100), variant(&target, 100)];
        let result = switch_target(&variants[0], &variants[1], 102, 0)
            .expect("有効な switch point であること");
        assert_eq!(result.group_id, 101);
        assert_eq!(result.object_id, 0);

        // (G - G0) * S + N = 2 * 30 + 1 = 61 は 60 で割り切れない
        assert!(switch_target(&variants[0], &variants[1], 102, 1).is_none());
    }

    #[test]
    fn switch_rejects_different_start_group_ids() {
        let source = track("source", Some(30), Some(1_000));
        let target = track("target", Some(60), Some(2_000));
        assert!(switch_target(&variant(&source, 100), &variant(&target, 101), 100, 0).is_none());
    }

    #[test]
    fn switch_rejects_missing_or_zero_spacing() {
        let source = track("source", None, Some(1_000));
        let target = track("target", Some(60), Some(2_000));
        assert!(switch_target(&variant(&source, 0), &variant(&target, 0), 0, 0).is_none());

        let source = track("source", Some(0), Some(1_000));
        assert!(switch_target(&variant(&source, 0), &variant(&target, 0), 0, 0).is_none());

        let source = track("source", Some(30), Some(1_000));
        let target = track("target", Some(0), Some(2_000));
        assert!(switch_target(&variant(&source, 0), &variant(&target, 0), 0, 0).is_none());
    }

    #[test]
    fn initial_selection_respects_bandwidth_and_spacing() {
        let low = track("low", Some(30), Some(500));
        let high = track("high", Some(60), Some(2_000));
        let variants = [variant(&low, 0), variant(&high, 0)];
        assert_eq!(
            select_initial_variant(&variants, Some(1_000)),
            Some(0),
            "帯域内で keyframeSpacing 最小の track を選ぶこと"
        );
        assert_eq!(
            select_initial_variant(&variants, Some(100)),
            Some(0),
            "帯域内に無ければ bitrate 最小の track を選ぶこと"
        );
        assert_eq!(
            select_initial_variant(&variants, None),
            Some(0),
            "帯域不明なら keyframeSpacing 最小の track を選ぶこと"
        );
    }

    #[test]
    fn initial_selection_excludes_missing_spacing() {
        let unknown = track("unknown", None, Some(100));
        let known = track("known", Some(60), Some(2_000));
        let variants = [variant(&unknown, 0), variant(&known, 0)];
        assert_eq!(select_initial_variant(&variants, None), Some(1));
    }

    #[test]
    fn state_switch_returns_unsubscribe_and_decoder_reset() {
        let source = track("source", Some(30), Some(1_000));
        let target = track("target", Some(60), Some(2_000));
        let variants = [variant(&source, 100), variant(&target, 100)];
        let mut state = TrackSwitchState::new(0);
        let action = state
            .switch_to(&variants, 1, 102, 0)
            .expect("valid switch point で切り替えること");
        assert_eq!(action.old_index, 0);
        assert_eq!(action.new_index, 1);
        assert_eq!(
            action.target,
            SwitchTarget {
                group_id: 101,
                object_id: 0
            }
        );
        assert!(action.reset_decoder, "decoder 状態を作り直すこと");
        assert_eq!(state.current_index, 1);

        let action = state.switch_to(&variants, 0, 102, 1);
        assert!(action.is_none());
        assert_eq!(state.current_index, 1);
    }
}
