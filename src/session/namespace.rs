//! Namespace 系 / TRACK_STATUS 関連
//!
//! draft-ietf-moq-transport-21 §4 (Namespace Discovery) / §9.13 (TRACK_STATUS) — §9.19 (PUBLISH_SKIPPED) に対応する
//! `impl Session` の送受信メソッドをまとめる。将来 draft 側で変更される可能性がある。

mod publish_namespace;
mod subscribe_namespace;
mod track_status;
mod track_subscription;

use crate::message::common::TrackNamespace;

use super::types::{RequestStreamEnd, TerminationReason};

/// `RequestStreamEnd` を素直な `TerminationReason` に変換する
pub(super) fn terminationreason_from_end(end: RequestStreamEnd) -> TerminationReason {
    match end {
        RequestStreamEnd::Fin => TerminationReason::PeerStreamFin,
        RequestStreamEnd::Reset { error_code, .. } => {
            TerminationReason::PeerStreamReset { error_code }
        }
    }
}

/// 2 つの Track Namespace が prefix overlap するか判定する (draft §9.15 (SUBSCRIBE_NAMESPACE) / §9.18 (SUBSCRIBE_TRACKS))
///
/// 一方が他方の prefix (または完全一致) になっていれば overlap とみなす。
/// 空 prefix (0 フィールド) は common prefix が空のため任意の namespace と overlap する
/// (§9.15 (SUBSCRIBE_NAMESPACE) / §9.18 (SUBSCRIBE_TRACKS) の "shares a common prefix" 判定)。
/// 0 フィールド prefix の意味 (全 namespace 関心) は draft §4.1 (Subscribing to Namespaces) を参照。
pub(super) fn prefix_overlaps(a: &TrackNamespace, b: &TrackNamespace) -> bool {
    let aa = a.fields();
    let bb = b.fields();
    let min = aa.len().min(bb.len());
    aa[..min] == bb[..min]
}

/// `ts_prefix` が `candidate` の prefix かどうかを判定する (片方向)。
/// `prefix_overlaps` は対称関数のため、本ヘルパーで方向を固定する。
pub(super) fn is_prefix_of(ts_prefix: &TrackNamespace, candidate: &TrackNamespace) -> bool {
    ts_prefix.fields().len() <= candidate.fields().len() && prefix_overlaps(ts_prefix, candidate)
}
