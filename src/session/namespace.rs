//! Namespace 系 / TRACK_STATUS 関連
//!
//! draft-ietf-moq-transport-21 §4 (Namespace Discovery) / §9.13 (TRACK_STATUS) — §9.19 (PUBLISH_SKIPPED) に対応する
//! `impl Session` の送受信メソッドをまとめる。将来 draft 側で変更される可能性がある。

mod publish_namespace;
mod subscribe_namespace;
mod track_status;
mod track_subscription;

use alloc::collections::VecDeque;
use hashbrown::HashMap;

use crate::error::SESSION_PROTOCOL_VIOLATION;
use crate::message::common::TrackNamespace;

use super::types::{RequestStreamEnd, SessionError, TerminationReason};

/// 空 prefix の購読で suffix が予約名前空間を広告するかを判定する
///
/// draft-ietf-moq-transport-21 §6.5 (Session-Level Tracks and Namespaces) の
/// "The Application MUST NOT publish tracks or namespaces whose first field is .session." と
/// §2.4.2 (Reserved Namespaces) の single period `.` の MUST NOT を満たすために使う。
/// publisher 役の購読の prefix を対象とする。full namespace は購読の prefix と suffix の
/// 連結なので、prefix が空のときだけ suffix の
/// 先頭フィールドが full namespace の先頭になる。prefix が非空の場合は購読の確立時と
/// REQUEST_UPDATE の prefix 更新時に予約名前空間が拒否されているため、suffix 側の追加検証は
/// 不要である。
pub(super) fn advertises_reserved_namespace(
    prefix: &TrackNamespace,
    suffix: &TrackNamespace,
) -> bool {
    prefix.fields().is_empty() && (suffix.is_single_period() || suffix.is_session_level())
}

/// 予約名前空間の広告をローカル拒否する
///
/// `send_namespace` / `send_namespace_done` / `send_publish_skipped` の共通検証。
/// publisher 役の購読の prefix と送信する suffix を受け取り、予約名前空間の広告になる場合は
/// `SESSION_PROTOCOL_VIOLATION` を返す。
pub(super) fn require_advertisable_suffix(
    prefix: &TrackNamespace,
    suffix: &TrackNamespace,
) -> Result<(), SessionError> {
    if advertises_reserved_namespace(prefix, suffix) {
        return Err(SessionError::new(
            SESSION_PROTOCOL_VIOLATION,
            "application cannot advertise reserved namespace with empty subscription prefix",
        ));
    }
    Ok(())
}

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

/// 確定待ち prefix を踏まえた実効 prefix を返す
///
/// 確定待ちキューの最後の prefix 指定を反映した値 (指定が無ければ適用済み prefix)。
/// REQUEST_UPDATE と REQUEST_OK の対応は送信順のため、途中の OK までに複数の
/// 確定待ちが積まれていても「実効値」は最後の指定で決まる。
pub(super) fn effective_prefix(
    pending_prefix_updates: &HashMap<u64, VecDeque<Option<TrackNamespace>>>,
    request_id: u64,
    applied: &TrackNamespace,
) -> TrackNamespace {
    pending_prefix_updates
        .get(&request_id)
        .and_then(|queue| queue.iter().rev().flatten().next())
        .cloned()
        .unwrap_or_else(|| applied.clone())
}

/// 確定待ち prefix を 1 件積む
///
/// prefix 変更を含まない REQUEST_UPDATE も `None` として 1 件積み、送信順に届く
/// REQUEST_OK との対応を保つ。
pub(super) fn push_pending_prefix_update(
    pending_prefix_updates: &mut HashMap<u64, VecDeque<Option<TrackNamespace>>>,
    request_id: u64,
    pending: Option<TrackNamespace>,
) {
    pending_prefix_updates
        .entry(request_id)
        .or_default()
        .push_back(pending);
}

/// 確定待ち prefix を先頭から 1 件取り出す
///
/// 空になったキューはエントリごと除去する。返り値 `None` は確定待ちが無い
/// (対応する REQUEST_UPDATE を送っていない REQUEST_OK) ことを表す。
pub(super) fn pop_pending_prefix_update(
    pending_prefix_updates: &mut HashMap<u64, VecDeque<Option<TrackNamespace>>>,
    request_id: u64,
) -> Option<Option<TrackNamespace>> {
    let queue = pending_prefix_updates.get_mut(&request_id)?;
    let pending = queue.pop_front();
    if queue.is_empty() {
        pending_prefix_updates.remove(&request_id);
    }
    pending
}
