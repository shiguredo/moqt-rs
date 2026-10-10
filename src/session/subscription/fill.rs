//! FILL_PARAMETERS と fill fetch stream (draft-ietf-moq-transport-22 §3.4 (Fill Semantics))
//!
//! Joining FETCH の廃止に伴い、SUBSCRIBE / REQUEST_UPDATE の FILL_PARAMETERS
//! パラメータ (Type 0x23) が fill fetch stream を要求する。publisher は
//! Forward State 1 で FILL_PARAMETERS 付きメッセージを処理したときに
//! fill fetch stream を開く (§3.4.1 (Opening and Closing Fill Fetch Streams))。
//! 将来 draft 側で変更される可能性がある。

use super::super::core::Session;
use super::super::types::{SessionError, SessionEvent, SubscriptionState, TrackRole};
use crate::error::SESSION_PROTOCOL_VIOLATION;
use crate::message::common::Location;
use crate::message_parameter::{
    FILL_PARAMETERS_ALLOWED_PARAMS, LocationFilter, LocationFilterContext, LocationFilterUpdate,
    MessageParameters,
};

/// 送信前の FILL_PARAMETERS 事前検証 (Table 7 スコープ + 内側 LOCATION_FILTER)
/// (draft-ietf-moq-transport-22 §9.20.15 (FILL PARAMETERS Parameter))
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

/// FILL_PARAMETERS 付きメッセージの fill range を解決する
///
/// 開設できる場合は `(開始 Location, 終端 Location)` を返し、開設できない場合
/// (fill range が空、Largest Object より後に始まる、Largest Object が未知) は `None` を返す。
/// 終端は Largest Object でクランプする (§3.4)。
///
/// draft-ietf-moq-transport-22 §3.4 (Fill Semantics) / §3.4.1 (Opening and
/// Closing Fill Fetch Streams):
/// - fill range は FILL 内側の LOCATION_FILTER、省略時は `subscription_filter`
///   (subscription の Location filter)、どちらもなければ track 全体
///   (Largest Object まで) とする
/// - FILL 内側の Location Filter Type 0x00 (None) は track 全体を指す
///   (§3.4 の「zero-length」は旧符号化の名残であり、新符号化では Type 0x00 が対応する)
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
) -> Option<(Location, Location)> {
    let largest = largest?;
    // 内側 LOCATION_FILTER → subscription filter → track 全体の順で fill range を決める。
    // draft-ietf-moq-transport-22 §3.4 (Fill Semantics): 内側の Location Filter Type 0x00
    // (None) は track 全体 (Largest Object まで) を指し、内側を省略した場合は
    // subscription の Location filter を継承する。両者を区別するため 3 状態で判定する。
    let filter: Option<LocationFilter> = match fill.location_filter_update() {
        // 内側省略時は subscription の filter を使う
        Ok(LocationFilterUpdate::Unchanged) => subscription_filter.copied(),
        // 内側 Type 0x00 は track 全体を指す
        Ok(LocationFilterUpdate::Removed) => None,
        Ok(LocationFilterUpdate::Set(filter)) => Some(filter),
        // 0x21 が Location Filter 形式でない場合はデコード済みメッセージでは到達しない。
        // API 経由の不正入力では安全側に倒して開設しない。
        Err(_) => return None,
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
        return None;
    };
    // fill range が Largest Object より後に始まる場合は開設しない
    if start > *largest {
        return None;
    }
    // 終端の指定が無い filter は Largest Object までを range とする。
    // draft-ietf-moq-transport-22 §3.4 (Fill Semantics): fill range は Largest Object を
    // 超えない。EndObject / EndGroup を明示する filter (Type 0x03 / 0x04) でも
    // Largest Object を超える分は要求できないため、Largest Object でクランプする
    let end = end.unwrap_or(*largest);
    let end = if end > *largest { *largest } else { end };
    // fill range が empty (Start > End) の場合は開設しない
    if start > end {
        return None;
    }
    Some((start, end))
}

impl Session {
    /// FILL_PARAMETERS 付きメッセージ処理時の fill fetch stream 開設判定と通知
    ///
    /// draft-ietf-moq-transport-22 §3.4.1 (Opening and Closing Fill Fetch Streams):
    /// Forward State 1 で FILL_PARAMETERS 付き SUBSCRIBE / REQUEST_UPDATE を処理し、
    /// fill range が空でなく Largest Object より後に始まらない場合に
    /// `SessionEvent::OpenFillFetchStream` を発火する。Forward State 0 での運搬や
    /// FILL なし REQUEST_UPDATE では発火しない。
    /// `fill_request_id` は fill fetch stream の FETCH_HEADER に載せる起因メッセージの
    /// Request ID である (SUBSCRIBE / PUBLISH 起因なら subscription の Request ID、
    /// REQUEST_UPDATE 起因なら REQUEST_UPDATE 自身の Request ID で、§6.4.2.1 により
    /// 両者は一致しない)。`subscription_request_id` は fill 対象 subscription の
    /// Request ID で、開設判定にだけ使う。
    /// `filter_view` は処理後観点の subscription filter
    /// (SUBSCRIBE なら新規 filter、REQUEST_UPDATE なら累積後の filter) を渡す。
    pub(super) fn maybe_open_fill_stream(
        &mut self,
        fill_request_id: u64,
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
        if let Some((start, end)) =
            should_open_fill_stream(fill, filter_view.as_ref(), largest.as_ref())
        {
            self.events.push_back(SessionEvent::OpenFillFetchStream {
                request_id: fill_request_id,
                subscription_request_id,
                start,
                end,
            });
        }
    }

    /// fill fetch stream の Request ID から subscription の Request ID を解決する
    ///
    /// draft-ietf-moq-transport-22 §3.4 (Fill Semantics): fill fetch stream の
    /// FETCH_HEADER は起因メッセージの Request ID を載せる。初回 fill は
    /// SUBSCRIBE / PUBLISH の Request ID (= subscription の Request ID そのもの) で、
    /// REQUEST_UPDATE 起因の fill は REQUEST_UPDATE 自身の Request ID である。
    ///
    /// REQUEST_UPDATE の Request ID の対応を先に引き、無ければ subscription の
    /// Request ID として解決する。この順序により、REQUEST_UPDATE の Request ID が
    /// 別 subscription の Request ID と偶然一致しても、起因メッセージの
    /// subscription へ帰属させる。
    /// 対応先の subscription が既に破棄されている場合は `None` を返す。
    pub(crate) fn resolve_fill_subscription(&self, request_id: u64) -> Option<u64> {
        if let Some(&subscription_request_id) = self.fill_request_subscriptions.get(&request_id)
            && self.subscriptions.contains_key(&subscription_request_id)
        {
            return Some(subscription_request_id);
        }
        if self.subscriptions.contains_key(&request_id) {
            return Some(request_id);
        }
        None
    }

    /// REQUEST_UPDATE 起因の fill fetch stream の Request ID を subscription に対応付ける
    ///
    /// FILL_PARAMETERS を持つ REQUEST_UPDATE だけが fill fetch stream を開きうるため、
    /// 呼び出し側はその場合のみ登録すること。初回 fill は subscription の Request ID
    /// そのものなので登録しない。
    pub(super) fn register_fill_request_subscription(
        &mut self,
        fill_request_id: u64,
        subscription_request_id: u64,
    ) {
        self.fill_request_subscriptions
            .insert(fill_request_id, subscription_request_id);
    }

    /// subscription に紐づく fill fetch stream の Request ID の対応を破棄する
    ///
    /// subscription の破棄時に呼ぶ。残すと破棄済み subscription への対応が
    /// セッション生存中に蓄積する。
    pub(super) fn remove_fill_request_subscriptions(&mut self, subscription_request_id: u64) {
        self.fill_request_subscriptions
            .retain(|_, subscription| *subscription != subscription_request_id);
    }
}
