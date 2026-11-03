//! peer publisher から観測した Track 単位の Object 存在・不存在の履歴
//!
//! draft-ietf-moq-transport-23 §2.1.2 (Object States) は Object の状態を Unknown / Exists /
//! Gone / Does Not Exist の 4 状態に分け、不存在の通知を恒久的かつ権威的と定める。
//! §12.1 (Malformed Tracks) 条件 10 は「不存在と記録した後の Object」と「Object を受信した
//! 後に届いた不存在通知」の両方を Malformed Track とする。
//!
//! 不存在は FETCH 応答の End of Non-Existent Range (§11.4.2 (End of Range)) だけでなく、
//! Range Filters を持たない FETCH 応答の暗黙 gap (§3.2.2 (Gaps in a Fetch Stream)) と
//! Prior Group / Object ID Gap (§10.8 / §10.9) でも通知される。一方 Gone / Unknown /
//! Timed-Out は存在を否定しないため、不存在として記録してはならない (§2.1.2 の
//! "Because another source can still hold a Gone Object, receiving an Object that was
//! recorded as Gone or Unknown is not an error.")。
//!
//! 本モジュールはこの判定を Track (同一 peer の Full Track Name) 単位で行う。
//! 履歴は subscription / FETCH / fill fetch stream をまたいで共有し、単一 stream の Group 前進や
//! 1 件の request の破棄では消さない。節番号・規則は draft 由来であり、将来の draft 改版で
//! 変更される可能性がある。

use crate::error::MessageError;
use crate::message::common::{Location, TrackNamespace};
use crate::object_properties::{ObjectProperties, ObjectPropertyTracker, RangeSet};
use alloc::vec::Vec;
use hashbrown::HashMap;

/// Full Track Name (namespace と name)
pub(super) type TrackKey = (TrackNamespace, Vec<u8>);

/// 存在しないことが確定した Location の範囲
///
/// 不存在は「連続した Location の範囲」で通知される (§2.1.2 の "expressed as a contiguous
/// range of non-existent objects")。Group の全域が対象になることがあるため、Group 単位の
/// 範囲と Group 内 Object ID 単位の範囲を分けて保持する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MissingRange {
    /// 単一 Group 内の Object ID 区間 (両端を含む)
    Objects {
        /// 対象 Group ID
        group_id: u64,
        /// 区間の開始 Object ID (含む)
        start: u64,
        /// 区間の終了 Object ID (含む)
        end: u64,
    },
    /// Group ID 区間の全域 (すべての Object ID)
    Groups {
        /// 区間の開始 Group ID (含む)
        start: u64,
        /// 区間の終了 Group ID (含む)
        end: u64,
    },
}

/// Location の集合 (存在または不存在)
#[derive(Debug, Clone, Default)]
struct ObjectLocationSet {
    /// Group 全域が対象の区間
    groups: RangeSet,
    /// Group ごとの Object ID 区間
    objects: HashMap<u64, RangeSet>,
}

impl ObjectLocationSet {
    /// 指定 Location が含まれるか
    fn contains(&self, location: Location) -> bool {
        self.groups.contains(location.group_id)
            || self
                .objects
                .get(&location.group_id)
                .is_some_and(|ranges| ranges.contains(location.object_id))
    }

    /// 指定範囲と重なる Location があるか
    fn overlaps(&self, range: MissingRange) -> bool {
        match range {
            MissingRange::Objects {
                group_id,
                start,
                end,
            } => {
                self.groups.contains(group_id)
                    || self
                        .objects
                        .get(&group_id)
                        .is_some_and(|ranges| ranges.overlaps(start, end))
            }
            MissingRange::Groups { start, end } => {
                self.groups.overlaps(start, end)
                    || self
                        .objects
                        .keys()
                        .any(|&group_id| start <= group_id && group_id <= end)
            }
        }
    }

    /// 単一 Object の存在を記録する
    fn insert_object(&mut self, location: Location) {
        self.objects
            .entry(location.group_id)
            .or_default()
            .insert_point(location.object_id);
    }

    /// 不存在範囲を記録する
    fn insert(&mut self, range: MissingRange) {
        match range {
            MissingRange::Objects {
                group_id,
                start,
                end,
            } => {
                self.objects.entry(group_id).or_default().insert(start, end);
            }
            MissingRange::Groups { start, end } => self.groups.insert(start, end),
        }
    }
}

/// Track 単位の存在・不存在履歴
///
/// 履歴を参照している request (`Subscriber` 役の subscription / FETCH) と受理済み受信 data
/// stream の数を保持し、参照が 0 になった時点で破棄する。単一 request の破棄では消さず、
/// 他の参照が残る間は同じ Track の照合に使う。
#[derive(Debug, Clone)]
pub(super) struct PeerTrackHistory {
    /// 存在が確認できた Location
    existing: ObjectLocationSet,
    /// 存在しないことが確定した Location
    missing: ObjectLocationSet,
    /// Prior Group / Object ID Gap の検証状態
    ///
    /// 条件 10 の逆方向 (不存在通知が既出 Object を覆う) と Prior Gap の一貫性を
    /// Track 単位で共有するために保持する。
    properties: ObjectPropertyTracker,
    /// この履歴を参照している request と受理済み data stream の数
    refs: usize,
}

impl PeerTrackHistory {
    /// 参照 0 の履歴を作る
    pub(super) fn new() -> Self {
        Self {
            existing: ObjectLocationSet::default(),
            missing: ObjectLocationSet::default(),
            properties: ObjectPropertyTracker::new(),
            refs: 0,
        }
    }

    /// 参照を 1 つ増やす
    pub(super) fn acquire(&mut self) {
        self.refs += 1;
    }

    /// 参照を 1 つ減らし、0 になったかを返す
    pub(super) fn release(&mut self) -> bool {
        self.refs = self.refs.saturating_sub(1);
        self.refs == 0
    }

    /// Object の存在を記録する
    ///
    /// 既に不存在と確定した Location なら条件 10 として拒否する。Prior Gap の検証も
    /// 同じ履歴で行い、別 request で受信した Object と矛盾する Gap 通知を検出する。
    ///
    /// # Errors
    ///
    /// 不存在区間に含まれる Object を受信した場合、または Prior Gap の一貫性違反を
    /// 検出した場合に `MalformedTrack`。
    pub(super) fn observe_object(
        &mut self,
        location: Location,
        properties: Option<&ObjectProperties>,
    ) -> Result<(), MessageError> {
        if self.missing.contains(location) {
            return Err(MessageError::MalformedTrack(
                "malformed track: object received in a range recorded as non-existent",
            ));
        }
        // Prior Gap の検証は Track 単位で共有する。別 request で受信した Object を覆う
        // Gap 通知や、Gap 内の Object 受信をここで検出する。
        self.properties.observe_decoded_object(
            location.group_id,
            location.object_id,
            properties,
        )?;
        self.existing.insert_object(location);
        Ok(())
    }

    /// 不存在範囲を記録する
    ///
    /// 既に存在が確認できた Location を覆う範囲なら条件 10 として拒否する。
    ///
    /// # Errors
    ///
    /// 既出 Object を覆う範囲を通知された場合に `MalformedTrack`。
    pub(super) fn observe_missing(&mut self, range: MissingRange) -> Result<(), MessageError> {
        if self.existing.overlaps(range) {
            return Err(MessageError::MalformedTrack(
                "malformed track: non-existent range covers a received object",
            ));
        }
        self.missing.insert(range);
        Ok(())
    }
}

/// 応答順で先行 Location の直後から後続 Location までの範囲を返す
///
/// Fetch 応答では Group 内の Object ID が常に昇順で、Group は要求された Group Order に
/// 従って移動する (§3.2.1 (Fetch Object Delivery))。`from` が `None` のときは `anchor` を
/// 応答の先頭とみなし、`anchor` 自身を含める。`include_to` が true なら `to` を含める。
///
/// 戻り値は Group 内の部分区間・Group 全域・その両方に分解される。要求範囲への
/// 切り詰めは呼び出し側 (`clamp_to_request`) が行う。
fn ordered_region(
    from: Option<Location>,
    anchor: Location,
    to: Location,
    include_to: bool,
) -> Vec<MissingRange> {
    let (start_group, start_object) = match from {
        Some(previous) => (previous.group_id, previous.object_id.checked_add(1)),
        None => (anchor.group_id, Some(anchor.object_id)),
    };
    let end_object = if include_to {
        Some(to.object_id)
    } else {
        to.object_id.checked_sub(1)
    };

    let mut ranges = Vec::new();
    if start_group == to.group_id {
        if let (Some(start), Some(end)) = (start_object, end_object)
            && start <= end
        {
            ranges.push(MissingRange::Objects {
                group_id: to.group_id,
                start,
                end,
            });
        }
        return ranges;
    }

    // 先行 Group の残り (Object ID 昇順のため、先行 Object より後ろの全体)
    if let Some(start) = start_object {
        ranges.push(MissingRange::Objects {
            group_id: start_group,
            start,
            end: u64::MAX,
        });
    }
    // 経由する Group (Group Order の昇順・降順のどちらでも、Group ID の数値区間で表せる)
    let (low_group, high_group) = if start_group < to.group_id {
        (start_group, to.group_id)
    } else {
        (to.group_id, start_group)
    };
    // 経由 Group が 1 つ以上あるか (両端の Group は別途部分区間として扱う)
    if high_group > low_group + 1 {
        ranges.push(MissingRange::Groups {
            start: low_group + 1,
            end: high_group - 1,
        });
    }
    // 後続 Group の先頭から `to` まで
    if let Some(end) = end_object {
        ranges.push(MissingRange::Objects {
            group_id: to.group_id,
            start: 0,
            end,
        });
    }
    ranges
}

/// 不存在範囲を要求範囲 `[start, end]` に切り詰める
///
/// §3.2.2 (Gaps in a Fetch Stream) の gap は要求範囲の内側だけを対象とする。降順で
/// Start と End の Group が異なる場合、応答の先頭 Group の上限や末尾 Group の下限が
/// 要求範囲の端になるため、切り詰めを怠ると要求外の Location を不存在として記録してしまう。
fn clamp_to_request(
    ranges: Vec<MissingRange>,
    start: Location,
    end: Option<Location>,
) -> Vec<MissingRange> {
    let mut clamped = Vec::new();
    for range in ranges {
        clamp_one(range, start, end, &mut clamped);
    }
    clamped
}

/// 1 つの範囲を要求範囲へ切り詰めて `out` に追加する
fn clamp_one(
    range: MissingRange,
    start: Location,
    end: Option<Location>,
    out: &mut Vec<MissingRange>,
) {
    let end = match end {
        Some(end) => end,
        // 応答範囲の終端が未確定のうちは上限側を切り詰めない (確定後に再判定する)
        None => Location {
            group_id: u64::MAX,
            object_id: u64::MAX,
        },
    };
    match range {
        MissingRange::Objects {
            group_id,
            start: object_start,
            end: object_end,
        } => {
            if group_id < start.group_id || group_id > end.group_id {
                return;
            }
            let object_start = if group_id == start.group_id {
                object_start.max(start.object_id)
            } else {
                object_start
            };
            let object_end = if group_id == end.group_id {
                object_end.min(end.object_id)
            } else {
                object_end
            };
            if object_start <= object_end {
                out.push(MissingRange::Objects {
                    group_id,
                    start: object_start,
                    end: object_end,
                });
            }
        }
        MissingRange::Groups {
            start: group_start,
            end: group_end,
        } => {
            let group_start = group_start.max(start.group_id);
            let group_end = group_end.min(end.group_id);
            if group_start > group_end {
                return;
            }
            // 端の Group は要求範囲の Object ID で切り詰める必要があるため、
            // Group 全域ではなく Object ID 区間として記録する
            let mut full_start = group_start;
            let mut full_end = group_end;
            if group_start == start.group_id && start.object_id > 0 {
                out.push(MissingRange::Objects {
                    group_id: group_start,
                    start: start.object_id,
                    end: u64::MAX,
                });
                match group_start.checked_add(1) {
                    Some(next) => full_start = next,
                    None => return,
                }
            }
            if group_end == end.group_id && end.object_id < u64::MAX {
                out.push(MissingRange::Objects {
                    group_id: group_end,
                    start: 0,
                    end: end.object_id,
                });
                full_end = group_end.saturating_sub(1);
            }
            if full_start <= full_end {
                out.push(MissingRange::Groups {
                    start: full_start,
                    end: full_end,
                });
            }
        }
    }
}

/// 応答順で 2 つの entry の間にある範囲を返す (両端を含まない)
pub(super) fn gap_between_entries(from: Location, to: Location) -> Vec<MissingRange> {
    ordered_region(Some(from), from, to, false)
}

/// 応答の先頭から最初の entry までの範囲を返す (先頭を含み、entry を含まない)
pub(super) fn leading_gap(anchor: Location, first: Location) -> Vec<MissingRange> {
    ordered_region(None, anchor, first, false)
}

/// 最後の entry から応答の終端までの範囲を返す (entry を含まず、終端を含む)
pub(super) fn trailing_gap(last: Location, upper: Location) -> Vec<MissingRange> {
    ordered_region(Some(last), last, upper, true)
}

/// 最初の entry が End of Non-Existent Range のとき、応答の先頭からその entry までの
/// 範囲を返す (先頭を含み、entry を含む)
pub(super) fn first_end_of_range_missing(anchor: Location, marker: Location) -> Vec<MissingRange> {
    ordered_region(None, anchor, marker, true)
}

/// 直前の entry から End of Range 指示までの範囲を返す (両端の扱いは §11.4.2)
///
/// draft-ietf-moq-transport-23 §11.4.2 (End of Range): "All Objects with Locations between the
/// last serialized Object, if any, and the Location decoded from this indicator, inclusive".
pub(super) fn end_of_range_region(prior: Location, marker: Location) -> Vec<MissingRange> {
    ordered_region(Some(prior), prior, marker, true)
}

/// entry を 1 つも受信していない stream の全範囲を返す (先頭と終端を含む)
pub(super) fn empty_stream_gap(anchor: Location, upper: Location) -> Vec<MissingRange> {
    ordered_region(None, anchor, upper, true)
}

/// 現在の entry 位置から次に進むべき応答順の開始点を返す
///
/// `End of Group` / `End of Track` は marker 自身が ID を運ばないため、Prior Object ID が
/// `2^64 - 1` になる (§11.4.2.2 (Prior Object Fields))。後続 entry の gap 計算では
/// この sentinel をそのまま使う。
pub(super) fn end_marker_location(group_id: u64) -> Location {
    Location {
        group_id,
        object_id: u64::MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::common::Location;
    use alloc::vec;

    fn loc(group_id: u64, object_id: u64) -> Location {
        Location {
            group_id,
            object_id,
        }
    }

    /// 同一 Group 内の gap は Object ID 区間 1 つになる
    #[test]
    fn gap_in_same_group_is_single_range() {
        let ranges = gap_between_entries(loc(0, 2), loc(0, 5));
        assert_eq!(
            ranges,
            vec![MissingRange::Objects {
                group_id: 0,
                start: 3,
                end: 4,
            }]
        );
    }

    /// 隣接する entry の間には gap が無い
    #[test]
    fn adjacent_entries_have_no_gap() {
        assert!(gap_between_entries(loc(0, 2), loc(0, 3)).is_empty());
    }

    /// 昇順で Group をまたぐ gap は先行 Group の残り・経由 Group・後続 Group の先頭に分かれる
    #[test]
    fn gap_across_groups_in_ascending_order() {
        let ranges = gap_between_entries(loc(2, 5), loc(5, 3));
        assert_eq!(
            ranges,
            vec![
                MissingRange::Objects {
                    group_id: 2,
                    start: 6,
                    end: u64::MAX,
                },
                MissingRange::Groups { start: 3, end: 4 },
                MissingRange::Objects {
                    group_id: 5,
                    start: 0,
                    end: 2,
                },
            ]
        );
    }

    /// 降順で Group をまたぐ gap も同じ構造になる (Group ID の数値区間で経由 Group を表す)
    #[test]
    fn gap_across_groups_in_descending_order() {
        let ranges = gap_between_entries(loc(5, 3), loc(2, 5));
        assert_eq!(
            ranges,
            vec![
                MissingRange::Objects {
                    group_id: 5,
                    start: 4,
                    end: u64::MAX,
                },
                MissingRange::Groups { start: 3, end: 4 },
                MissingRange::Objects {
                    group_id: 2,
                    start: 0,
                    end: 4,
                },
            ]
        );
    }

    /// 先頭 gap は基準点を含み、最初の entry を含まない
    #[test]
    fn leading_gap_excludes_first_entry() {
        let ranges = leading_gap(loc(0, 2), loc(0, 5));
        assert_eq!(
            ranges,
            vec![MissingRange::Objects {
                group_id: 0,
                start: 2,
                end: 4,
            }]
        );
    }

    /// 末尾 gap は最後の entry を含まず、終端を含む
    #[test]
    fn trailing_gap_includes_upper_bound() {
        let ranges = trailing_gap(loc(0, 2), loc(0, 5));
        assert_eq!(
            ranges,
            vec![MissingRange::Objects {
                group_id: 0,
                start: 3,
                end: 5,
            }]
        );
    }

    /// End of Range が先頭 entry のときは基準点を含めて marker までを不存在とする
    #[test]
    fn first_end_of_range_includes_anchor_and_marker() {
        let ranges = first_end_of_range_missing(loc(2, 0), loc(2, 2));
        assert_eq!(
            ranges,
            vec![MissingRange::Objects {
                group_id: 2,
                start: 0,
                end: 2,
            }]
        );
    }

    /// 降順で Group をまたぐ End of Range は先行 Group の残りから後続 Group の先頭までになる
    #[test]
    fn end_of_range_region_in_descending_order() {
        let ranges = end_of_range_region(loc(5, 1), loc(3, 2));
        assert_eq!(
            ranges,
            vec![
                MissingRange::Objects {
                    group_id: 5,
                    start: 2,
                    end: u64::MAX,
                },
                MissingRange::Groups { start: 4, end: 4 },
                MissingRange::Objects {
                    group_id: 3,
                    start: 0,
                    end: 2,
                },
            ]
        );
    }

    /// 降順の末尾 gap は Start の Group の下限から Group の最後までになる
    #[test]
    fn trailing_gap_in_descending_multi_group() {
        let ranges = trailing_gap(loc(4, 2), loc(2, u64::MAX));
        assert_eq!(
            ranges,
            vec![
                MissingRange::Objects {
                    group_id: 4,
                    start: 3,
                    end: u64::MAX,
                },
                MissingRange::Groups { start: 3, end: 3 },
                MissingRange::Objects {
                    group_id: 2,
                    start: 0,
                    end: u64::MAX,
                },
            ]
        );
    }

    /// 降順で Group をまたぐ範囲は要求範囲の端で切り詰められる
    ///
    /// 応答の先頭 Group の上限 (End) と末尾 Group の下限 (Start) を超えて不存在を
    /// 記録しないことを確認する。
    #[test]
    fn clamp_uses_request_boundaries_on_both_ends() {
        let start = loc(5, 3);
        let end = loc(8, 2);
        let ranges = clamp_to_request(
            vec![MissingRange::Groups { start: 2, end: 10 }],
            start,
            Some(end),
        );
        assert_eq!(
            ranges,
            vec![
                MissingRange::Objects {
                    group_id: 5,
                    start: 3,
                    end: u64::MAX,
                },
                MissingRange::Objects {
                    group_id: 8,
                    start: 0,
                    end: 2,
                },
                MissingRange::Groups { start: 6, end: 7 },
            ]
        );
    }

    /// 要求範囲外の Object ID 区間は消える
    #[test]
    fn clamp_drops_ranges_outside_request() {
        let start = loc(5, 3);
        let end = loc(8, 2);
        assert!(
            clamp_to_request(
                vec![MissingRange::Objects {
                    group_id: 5,
                    start: 0,
                    end: 2,
                }],
                start,
                Some(end),
            )
            .is_empty()
        );
    }

    /// 応答範囲の終端が未確定なら上限側を切り詰めない
    #[test]
    fn clamp_does_not_apply_unknown_end() {
        let ranges = clamp_to_request(
            vec![MissingRange::Objects {
                group_id: 0,
                start: 1,
                end: 9,
            }],
            loc(0, 0),
            None,
        );
        assert_eq!(
            ranges,
            vec![MissingRange::Objects {
                group_id: 0,
                start: 1,
                end: 9,
            }]
        );
    }

    /// 不存在後の Object は MalformedTrack になる
    #[test]
    fn object_in_missing_range_is_rejected() {
        let mut history = PeerTrackHistory::new();
        history
            .observe_missing(MissingRange::Objects {
                group_id: 0,
                start: 1,
                end: 3,
            })
            .expect("不存在の記録は既出 Object と重ならなければ成功する");
        let err = history
            .observe_object(loc(0, 2), None)
            .expect_err("不存在区間内の Object は拒否される");
        assert!(matches!(err, MessageError::MalformedTrack(_)));
    }

    /// 既出 Object を覆う不存在通知は MalformedTrack になる (逆順の矛盾)
    #[test]
    fn missing_range_covering_existing_object_is_rejected() {
        let mut history = PeerTrackHistory::new();
        history
            .observe_object(loc(1, 4), None)
            .expect("最初の Object は受理される");
        let err = history
            .observe_missing(MissingRange::Groups { start: 0, end: 2 })
            .expect_err("既出 Object を含む Group 全域の不存在は拒否される");
        assert!(matches!(err, MessageError::MalformedTrack(_)));
        let err = history
            .observe_missing(MissingRange::Objects {
                group_id: 1,
                start: 4,
                end: 9,
            })
            .expect_err("同一 Location を含む不存在は拒否される");
        assert!(matches!(err, MessageError::MalformedTrack(_)));
    }

    /// 参照カウントが 0 になったときだけ破棄対象になる
    #[test]
    fn history_is_released_after_last_reference() {
        let mut history = PeerTrackHistory::new();
        history.acquire();
        history.acquire();
        assert!(!history.release());
        assert!(history.release());
    }
}

// ─── Session 連携 ───────────────────────────────────────────────
//
// 履歴の所有者は Session であり、reference (request) と受理済み data stream の数で寿命を
// 管理する。request を 1 件破棄しただけでは他の参照が残る限り履歴を消さない。

use super::core::Session;
use super::types::{DataStreamId, SessionError, TrackRole};
use crate::error::SESSION_PROTOCOL_VIOLATION;
use crate::message_parameter::{
    LocationFilter, LocationFilterContext, LocationFilterUpdate, MessageParameters,
};
use crate::session::types::SubscriptionRangeFilters;

/// FETCH / fill fetch stream の暗黙 gap 判定に使う要求・応答の境界情報
///
/// draft-ietf-moq-transport-23 §3.2.2 (Gaps in a Fetch Stream) は End Location を
/// "the LARGEST_OBJECT in FETCH_OK if present, and the requested End Location otherwise" と
/// 定める。FETCH_OK がデータより後に届く場合、応答の終端は未確定なので、境界に依存する
/// 判定を FETCH_OK の受信まで保留する。
#[derive(Debug, Clone, Copy)]
pub(super) struct FetchGapContext {
    /// 要求された Group Order (1 = Ascending / 2 = Descending)
    pub(super) group_order: u8,
    /// 実効 Range Filters があるか
    ///
    /// §3.2.2: "When one or more Range Filters are present, unmarked gaps indicate objects in
    /// the Unknown state." Range Filters がある場合は暗黙 gap を不存在として記録しない。
    pub(super) has_range_filters: bool,
    /// 要求の Start Location
    pub(super) start: Option<Location>,
    /// 応答で確定した終端 Location
    pub(super) end: Option<Location>,
}

impl Default for FetchGapContext {
    /// 境界が未確定で暗黙 gap の判定を行わない状態
    fn default() -> Self {
        Self {
            group_order: 1,
            has_range_filters: true,
            start: None,
            end: None,
        }
    }
}

impl FetchGapContext {
    /// 暗黙 gap の判定が可能か
    pub(super) fn implicit_enabled(&self) -> bool {
        !self.has_range_filters && self.start.is_some()
    }

    /// 応答順で最初の entry より手前の基準点
    ///
    /// 降順で Start と End の Group が異なる場合、最初に期待される Location は
    /// `{End Location.Group, 0}` である (§3.2.2)。
    pub(super) fn leading_anchor(&self) -> Option<Location> {
        let start = self.start?;
        if self.group_order == FETCH_GROUP_ORDER_DESCENDING
            && let Some(end) = self.end
            && start.group_id != end.group_id
        {
            return Some(Location {
                group_id: end.group_id,
                object_id: 0,
            });
        }
        Some(start)
    }

    /// 応答順で最後の entry より後ろの基準点
    ///
    /// 降順で Start と End の Group が異なる場合、最後に期待される Location は
    /// `{Start Location.Group, 2^64-1}` である (§3.2.2)。
    pub(super) fn trailing_anchor(&self) -> Option<Location> {
        let start = self.start?;
        if self.group_order == FETCH_GROUP_ORDER_DESCENDING
            && let Some(end) = self.end
            && start.group_id != end.group_id
        {
            return Some(Location {
                group_id: start.group_id,
                object_id: u64::MAX,
            });
        }
        self.end
    }

    /// 境界が未確定のために後で解決する必要があるか
    pub(super) fn boundary_pending(&self) -> bool {
        self.group_order == FETCH_GROUP_ORDER_DESCENDING && self.end.is_none()
    }

    /// 範囲を要求範囲へ切り詰める
    pub(super) fn clamp(&self, ranges: Vec<MissingRange>) -> Vec<MissingRange> {
        match self.start {
            Some(start) => clamp_to_request(ranges, start, self.end),
            None => Vec::new(),
        }
    }
}

const FETCH_GROUP_ORDER_DESCENDING: u8 = 0x02;

/// 応答終端の確定を待つ FETCH stream の gap 判定状態
///
/// データや FIN が FETCH_OK より先に届いた場合、[`Session`] はこの状態を保持し、
/// FETCH_OK の受信時に境界依存の判定を行う。
#[derive(Debug, Clone, Default)]
pub(super) struct PendingFetchTail {
    /// leading gap の判定を待つ先頭 Object
    pub(super) leading_object: Option<Location>,
    /// leading gap の判定を待つ先頭 End of Non-Existent Range
    pub(super) leading_marker: Option<Location>,
    /// 最後に観測した entry の Location
    pub(super) last_entry: Option<Location>,
    /// stream の FIN を観測済みか
    pub(super) finished: bool,
    /// 応答順 (降順で終端未確定) の範囲。終端確定後に切り詰めて記録する
    pub(super) pending_ranges: Vec<MissingRange>,
}

/// fill fetch stream の実効フィルタ
///
/// draft-ietf-moq-transport-23 §9.19.15 (FILL PARAMETERS Parameter): FILL_PARAMETERS は
/// 購読状態として保持されないため、fill fetch stream を受信する側が送信時に使った値を
/// 覚えておく必要がある。省略されたパラメータは購読の値を使う (同節)。
#[derive(Debug, Clone)]
pub(super) struct FillContext {
    /// FILL 内側の LOCATION_FILTER
    filter: FillLocationFilter,
    /// FILL 内側の Range Filters (None は購読の Range Filters を継承)
    range_filters: Option<SubscriptionRangeFilters>,
    /// FILL 内側の GROUP_ORDER (None は購読の設定を継承)
    group_order: Option<u8>,
}

/// FILL 内側の LOCATION_FILTER の指定
#[derive(Debug, Clone, Copy)]
enum FillLocationFilter {
    /// 内側で省略されており購読の Location Filter を継承する
    Inherit,
    /// Location Filter Type 0x00 (None) で track 全体
    WholeTrack,
    /// 明示された Location Filter
    Explicit(LocationFilter),
}

impl FillContext {
    /// メッセージの FILL_PARAMETERS から実効フィルタを取り出す
    fn from_parameters(fill: &MessageParameters) -> Option<Self> {
        let filter = match fill.location_filter_update() {
            Ok(LocationFilterUpdate::Unchanged) => FillLocationFilter::Inherit,
            Ok(LocationFilterUpdate::Removed) => FillLocationFilter::WholeTrack,
            Ok(LocationFilterUpdate::Set(filter)) => FillLocationFilter::Explicit(filter),
            // デコード済みメッセージでは到達しない。手組み入力では安全側に倒して
            // 暗黙 gap の判定を行わない (fill の開設自体は publisher の判断)
            Err(_) => return None,
        };
        Some(Self {
            filter,
            range_filters: fill
                .has_range_filters()
                .then(|| crate::session::subscription::validation::extract_range_filters(fill)),
            group_order: fill.group_order(),
        })
    }
}

impl Session {
    /// Track 履歴の参照を 1 つ取得する
    ///
    /// `Subscriber` 役の subscription と FETCH の生成時に呼ぶ。既に同じ request が参照を
    /// 持つ場合は二重に数えない。
    pub(super) fn acquire_peer_track_history(
        &mut self,
        request_id: u64,
        track_namespace: &TrackNamespace,
        track_name: &[u8],
    ) {
        if self.track_history_requests.contains_key(&request_id) {
            return;
        }
        let key = (track_namespace.clone(), track_name.to_vec());
        self.peer_track_history
            .entry(key.clone())
            .or_insert_with(PeerTrackHistory::new)
            .acquire();
        self.track_history_requests.insert(request_id, key);
    }

    /// Track 履歴の参照を 1 つ解放する
    ///
    /// 最後の参照が外れた時点で履歴を破棄する。他の request や受理済み data stream が
    /// 参照している間は保持する。
    pub(super) fn release_peer_track_history(&mut self, request_id: u64) {
        let Some(key) = self.track_history_requests.remove(&request_id) else {
            return;
        };
        self.release_peer_track_history_key(&key);
    }

    /// 受理済み受信 data stream の参照を 1 つ取得する
    ///
    /// request を破棄した後も stream の終端まで履歴を保持するために数える。
    pub(super) fn acquire_peer_track_history_stream(
        &mut self,
        stream_id: DataStreamId,
        request_id: u64,
    ) {
        if self.track_history_streams.contains_key(&stream_id) {
            return;
        }
        let Some(key) = self.track_key_for_received_request(request_id) else {
            return;
        };
        if let Some(history) = self.peer_track_history.get_mut(&key) {
            history.acquire();
        }
        self.track_history_streams.insert(stream_id, key);
    }

    /// 受理済み受信 data stream の参照を 1 つ解放する
    ///
    /// data stream の登録解除経路から呼ぶ。登録されていない id では何もしない。
    pub(super) fn release_peer_track_history_stream(&mut self, stream_id: DataStreamId) {
        let Some(key) = self.track_history_streams.remove(&stream_id) else {
            return;
        };
        self.release_peer_track_history_key(&key);
    }

    /// 参照が 0 になった履歴を破棄する
    fn release_peer_track_history_key(&mut self, key: &TrackKey) {
        let released = self
            .peer_track_history
            .get_mut(key)
            .is_some_and(PeerTrackHistory::release);
        if released {
            self.peer_track_history.remove(key);
        }
    }

    /// 受信側 request に対応する Track キーを返す
    ///
    /// `Subscriber` 役の subscription / FETCH だけを対象にする。peer publisher からデータを
    /// 受けるのはこの役割だけである。
    pub(super) fn track_key_for_received_request(&self, request_id: u64) -> Option<TrackKey> {
        if let Some(subscription) = self.subscriptions.get(&request_id) {
            return (subscription.my_role == TrackRole::Subscriber).then(|| {
                (
                    subscription.track_namespace.clone(),
                    subscription.track_name.clone(),
                )
            });
        }
        let fetch = self.fetches.get(&request_id)?;
        if fetch.my_role != TrackRole::Subscriber {
            return None;
        }
        fetch.track_namespace.clone().zip(fetch.track_name.clone())
    }

    /// 受信した Object の存在を Track 履歴へ記録する
    ///
    /// 不存在区間内の Object と Prior Gap の矛盾を検出したら、同じ Track の request を
    /// malformed として cancel する (§12.1 (Malformed Tracks) 条件 10)。
    ///
    /// # Errors
    ///
    /// Malformed Track を検出した場合。
    pub(super) fn observe_peer_object(
        &mut self,
        request_id: u64,
        stream_id: Option<DataStreamId>,
        location: Location,
        properties: Option<&ObjectProperties>,
    ) -> Result<(), SessionError> {
        let Some(key) = self.track_key_for_received_request(request_id) else {
            return Ok(());
        };
        let result = match self.peer_track_history.get_mut(&key) {
            Some(history) => history.observe_object(location, properties),
            None => return Ok(()),
        };
        match result {
            Ok(()) => Ok(()),
            Err(err) => {
                let reason = err.reason();
                self.terminate_malformed_track(request_id, stream_id, reason);
                Err(SessionError::new(SESSION_PROTOCOL_VIOLATION, reason))
            }
        }
    }

    /// 不存在範囲を Track 履歴へ記録する
    ///
    /// # Errors
    ///
    /// 既出 Object を覆う範囲を検出した場合 (Malformed Track)。
    pub(super) fn observe_peer_missing(
        &mut self,
        request_id: u64,
        stream_id: Option<DataStreamId>,
        ranges: &[MissingRange],
    ) -> Result<(), SessionError> {
        let Some(key) = self.track_key_for_received_request(request_id) else {
            return Ok(());
        };
        for &range in ranges {
            let result = match self.peer_track_history.get_mut(&key) {
                Some(history) => history.observe_missing(range),
                None => return Ok(()),
            };
            if let Err(err) = result {
                let reason = err.reason();
                self.terminate_malformed_track(request_id, stream_id, reason);
                return Err(SessionError::new(SESSION_PROTOCOL_VIOLATION, reason));
            }
        }
        Ok(())
    }

    /// FILL_PARAMETERS 付きメッセージの実効フィルタを記録する
    ///
    /// 自側が送った SUBSCRIBE / REQUEST_UPDATE、または peer から受信した PUBLISH が
    /// fill fetch stream を開かせる。受信した fill stream の暗黙 gap 判定に使うため、
    /// fill の Request ID ごとに保持する。
    pub(super) fn remember_fill_context(
        &mut self,
        fill_request_id: u64,
        fill: Option<&MessageParameters>,
    ) {
        let Some(fill) = fill else {
            return;
        };
        let Some(context) = FillContext::from_parameters(fill) else {
            return;
        };
        self.fill_contexts.insert(fill_request_id, context);
    }

    /// fill fetch stream の必須 Request ID に対応する実効フィルタを破棄する
    pub(super) fn forget_fill_context(&mut self, fill_request_id: u64) {
        self.fill_contexts.remove(&fill_request_id);
    }

    /// fill fetch stream の gap 判定用 context を組み立てる
    ///
    /// fill range は Largest Object を超えない (§3.4 (Fill Semantics)) ため、Largest Object を
    /// 知らない間は暗黙 gap の判定を行わない (誤拒否を避ける)。
    pub(super) fn fill_gap_context(&self, fill_request_id: u64) -> Option<FetchGapContext> {
        let fill = self.fill_contexts.get(&fill_request_id)?;
        let subscription_request_id = self.resolve_fill_subscription(fill_request_id)?;
        let subscription = self.subscriptions.get(&subscription_request_id)?;
        if subscription.my_role != TrackRole::Subscriber {
            return None;
        }
        let largest = subscription.largest_location?;
        let filter = match fill.filter {
            FillLocationFilter::Inherit => subscription.filter,
            FillLocationFilter::WholeTrack => None,
            FillLocationFilter::Explicit(filter) => Some(filter),
        };
        let (start, end) = match filter.as_ref() {
            None => (
                Location {
                    group_id: 0,
                    object_id: 0,
                },
                largest,
            ),
            Some(filter) => (
                filter.effective_start_location(Some(&largest))?,
                filter
                    .effective_end_location(Some(&largest), LocationFilterContext::Fetch)
                    .unwrap_or(largest),
            ),
        };
        if start > end {
            return None;
        }
        let (start, end) = (start, if end > largest { largest } else { end });
        let has_range_filters = match fill.range_filters.as_ref() {
            Some(filters) => !filters.is_empty(),
            None => !subscription.range_filters.is_empty(),
        };
        Some(FetchGapContext {
            group_order: fill.group_order.or(subscription.group_order).unwrap_or(1),
            has_range_filters,
            start: Some(start),
            end: Some(end),
        })
    }
}
