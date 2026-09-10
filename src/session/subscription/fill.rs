//! FILL_PARAMETERS と fill fetch stream (draft-ietf-moq-transport-21 §3.4 (Fill Semantics))
//!
//! Joining FETCH の廃止に伴い、SUBSCRIBE / REQUEST_UPDATE の FILL_PARAMETERS
//! パラメータ (Type 0x23) が fill fetch stream を要求する。publisher は
//! Forward State 1 で FILL_PARAMETERS 付きメッセージを処理したときに
//! fill fetch stream を開く (§3.4.1 (Opening and Closing Fill Fetch Streams))。
//! 将来 draft 側で変更される可能性がある。

use super::super::core::Session;
use super::super::types::{SessionError, SessionEvent, SubscriptionState, TrackRole};
use super::delivery::effective_largest_object;
use crate::error::SESSION_PROTOCOL_VIOLATION;
use crate::message::{common::Location, common::TrackNamespace};
use crate::message_parameter::{
    FILL_PARAMETERS_ALLOWED_PARAMS, LocationFilter, LocationFilterContext, MessageParameters,
};

/// 送信前の FILL_PARAMETERS 事前検証 (Table 6 スコープ + 内側 LOCATION_FILTER)
/// (draft-ietf-moq-transport-21 §9.20.16 (FILL PARAMETERS Parameter))
///
/// 外側 scope と同様、request_id 発行・状態更新より前に検証し、不正 inner の
/// 送出と孤児状態 (登録済みだが I/O 層 encode で失敗する) を防ぐ。
/// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
pub(super) fn validate_outgoing_fill_parameters(
    parameters: &MessageParameters,
) -> Result<(), SessionError> {
    let Some(fill) = parameters.fill_parameters() else {
        return Ok(());
    };
    fill.validate_scope(FILL_PARAMETERS_ALLOWED_PARAMS)
        .map_err(|_| {
            SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "FILL_PARAMETERS parameter not allowed in this context",
            )
        })?;
    if fill.location_filter_update().is_err() {
        return Err(SessionError::new(
            SESSION_PROTOCOL_VIOLATION,
            "invalid fill filter encoding",
        ));
    }
    Ok(())
}

/// FILL_PARAMETERS 付きメッセージの fill fetch stream 開設要否を判定する
///
/// draft-ietf-moq-transport-21 §3.4 (Fill Semantics) / §3.4.1 (Opening and
/// Closing Fill Fetch Streams):
/// - fill range は FILL 内側の LOCATION_FILTER、省略時は `subscription_filter`
///   (subscription の Location filter)、どちらもなければ track 全体
///   (Largest Object まで) とする
/// - FILL 内側の zero-length LOCATION_FILTER は track 全体を指す
/// - fill range が empty、または Largest Object より後に始まる場合は開設しない
/// - Largest Object が未知 (`largest` が `None`) の場合は開設しない
///   (fill range は Largest Object を超えられず、送れる Object が確定しないため)
///
/// `fill` は当該メッセージの FILL_PARAMETERS 内側パラメータ群
/// (`MessageParameters::fill_parameters`)、`subscription_filter` は呼び出し側
/// subscription の typed filter (`Subscription::filter`)、`largest` は publisher が
/// 観測した当該 track の Largest Object を渡す。評価は Fetch の規則
/// (`LocationFilterContext::Fetch`) で行う。
/// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
fn should_open_fill_stream(
    fill: &MessageParameters,
    subscription_filter: Option<&LocationFilter>,
    largest: Option<&Location>,
) -> bool {
    let Some(largest) = largest else {
        return false;
    };
    // 内側 LOCATION_FILTER → subscription filter → track 全体の順で fill range を決める
    let filter: Option<LocationFilter> = match fill.location_filter() {
        // 内側省略時は subscription の filter を使う
        None => subscription_filter.cloned(),
        // 内側 zero-length は track 全体を指す
        Some([]) => None,
        Some(bytes) => match LocationFilter::decode(bytes) {
            Ok(filter) => Some(filter),
            // デコード済みメッセージでは到達しない (codec 層で検証済み)。
            // API 経由の不正入力では安全側に倒して開設しない。
            Err(_) => return false,
        },
    };
    let (start, end) = match filter.as_ref() {
        // track 全体: 先頭から Largest Object まで
        None => (
            Some(Location {
                group_id: 0,
                object_id: 0,
            }),
            Some(*largest),
        ),
        Some(filter) => (
            filter.effective_start_location(Some(largest)),
            filter.effective_end_location(Some(largest), LocationFilterContext::Fetch),
        ),
    };
    let Some(start) = start else {
        // 開始位置が定まらない場合は開設しない (現行の全フィルタ種別は常時 Some を
        // 返すため到達しない防御。将来種別追加時の安全側)
        return false;
    };
    // fill range が Largest Object より後に始まる場合は開設しない
    if start > *largest {
        return false;
    }
    // fill range が empty (Start > End) の場合は開設しない
    if let Some(end) = end
        && start > end
    {
        return false;
    }
    true
}

impl Session {
    /// publisher が観測した track の Largest Object を返す
    ///
    /// 自側 publisher 役の同 track subscription 群の `effective_largest_object` の
    /// 最大値を取る。値は subscription が保持するため、forget 済みの Track では `None` を
    /// 返す (Terminated でも保持中なら寄与する)。用途:
    /// - draft-ietf-moq-transport-21 §3.4 (Fill Semantics): fill range は
    ///   Largest Object を超えない
    /// - draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter):
    ///   TRACK_STATUS_OK に載せる largest
    pub(crate) fn publisher_track_largest(
        &self,
        track_namespace: &TrackNamespace,
        track_name: &[u8],
    ) -> Option<Location> {
        let key = (
            track_namespace.clone(),
            track_name.to_vec(),
            TrackRole::Publisher,
        );
        self.aliases
            .subscriptions_by_track
            .get(&key)?
            .iter()
            .filter_map(|id| self.subscriptions.get(id))
            .filter(|sub| sub.my_role == TrackRole::Publisher)
            .filter_map(effective_largest_object)
            .max()
    }

    /// FILL_PARAMETERS 付きメッセージ処理時の fill fetch stream 開設判定と通知
    ///
    /// draft-ietf-moq-transport-21 §3.4.1 (Opening and Closing Fill Fetch Streams):
    /// Forward State 1 で FILL_PARAMETERS 付き SUBSCRIBE / REQUEST_UPDATE を処理し、
    /// fill range が空でなく Largest Object より後に始まらない場合に
    /// `SessionEvent::OpenFillFetchStream` を発火する。Forward State 0 での運搬や
    /// FILL なし REQUEST_UPDATE では発火しない。
    /// `filter_view` は処理後観点の subscription filter
    /// (SUBSCRIBE なら新規 filter、REQUEST_UPDATE なら累積後の filter) を渡す。
    pub(super) fn maybe_open_fill_stream(
        &mut self,
        subscription_request_id: u64,
        parameters: &MessageParameters,
        filter_view: Option<LocationFilter>,
        forward_state: u8,
    ) {
        if forward_state != 1 {
            return;
        }
        let Some(fill) = parameters.fill_parameters() else {
            return;
        };
        let Some(subscription) = self.subscriptions.get(&subscription_request_id) else {
            return;
        };
        // 自側が publisher の subscription に対してのみ fill stream を開く
        if subscription.my_role != TrackRole::Publisher {
            return;
        }
        if subscription.state == SubscriptionState::Terminated {
            return;
        }
        let track_namespace = subscription.track_namespace.clone();
        let track_name = subscription.track_name.clone();
        let largest = self.publisher_track_largest(&track_namespace, &track_name);
        if should_open_fill_stream(fill, filter_view.as_ref(), largest.as_ref()) {
            self.events.push_back(SessionEvent::OpenFillFetchStream {
                request_id: subscription_request_id,
            });
        }
    }
}
