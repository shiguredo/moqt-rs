//! 配信タイムアウト計算、EXPIRES の設定、PUBLISH_DONE 記録、LARGEST_OBJECT の集約
//! (draft-ietf-moq-transport-21 §3.1, §5.2, §9.5, §9.9, §9.20.4, §9.20.5, §9.20.17, §9.20.18)
//!
//! 将来 draft 側で変更される可能性がある。

use crate::message::{PublishDone, common::Location, common::TrackNamespace};
use crate::message_parameter::MessageParameters;

use super::super::core::{PUBLISH_DONE_STREAM_COUNT_UNKNOWN, Session};
use super::super::types::{DeadlineTimer, Subscription, SubscriptionPublishDone, TrackRole};

/// delivery timeout (ms) を「タイムアウトなし」の表現に正規化する
/// (draft-ietf-moq-transport-21 §5.2 (Delivery Timeouts and Data Reliability))
///
/// §5.2 は OBJECT_DELIVERY_TIMEOUT / SUBGROUP_DELIVERY_TIMEOUT の値 0 を
/// 「timeout を設定しない」と規定するため、`Some(0)` は `None` に正規化する。
/// `None` はそのまま `None`、`Some(ms)` (ms > 0) はそのまま返す。
pub fn normalize_delivery_timeout_ms(delivery_timeout_ms: Option<u64>) -> Option<u64> {
    match delivery_timeout_ms {
        Some(0) | None => None,
        Some(delivery_timeout_ms) => Some(delivery_timeout_ms),
    }
}

/// subscriber 申告と publisher 申告から effective な delivery timeout (ms) を計算する
/// (draft-ietf-moq-transport-21 §5.2 (Delivery Timeouts and Data Reliability))
///
/// §5.2 の「publisher の値と subscriber の値がともに non-zero なら小さい方を使う」に従い、
/// 双方を [`normalize_delivery_timeout_ms`] で正規化したうえで、両方 `Some` なら小さい方、
/// 片方のみ `Some` ならその値、両方 `None` なら `None` を返す。
/// 引数は subscriber 側 (Message Parameter) / publisher 側 (Track Property、subgroup 先頭
/// Object の Object Property がある場合は呼び出し側がそれで置き換えた値) の順に渡す。
pub fn compute_effective_delivery_timeout_ms(
    subscriber_delivery_timeout_ms: Option<u64>,
    publisher_delivery_timeout_ms: Option<u64>,
) -> Option<u64> {
    match (
        normalize_delivery_timeout_ms(subscriber_delivery_timeout_ms),
        normalize_delivery_timeout_ms(publisher_delivery_timeout_ms),
    ) {
        (Some(subscriber_delivery_timeout_ms), Some(publisher_delivery_timeout_ms)) => {
            Some(subscriber_delivery_timeout_ms.min(publisher_delivery_timeout_ms))
        }
        (Some(subscriber_delivery_timeout_ms), None) => Some(subscriber_delivery_timeout_ms),
        (None, Some(publisher_delivery_timeout_ms)) => Some(publisher_delivery_timeout_ms),
        (None, None) => None,
    }
}

/// OBJECT_DELIVERY_TIMEOUT の effective 値を subscriber / publisher の現在値から再計算する
///
/// `DeliveryTimeoutState::effective_object_ms` の更新を 1 箇所に集約するための内部関数。
/// 計算規則は [`compute_effective_delivery_timeout_ms`] (draft-ietf-moq-transport-21 §5.2) に従う。
fn refresh_effective_object_delivery_timeout(subscription: &mut Subscription) {
    subscription.delivery_timeouts.effective_object_ms = compute_effective_delivery_timeout_ms(
        subscription.delivery_timeouts.subscriber_object_ms,
        subscription.delivery_timeouts.publisher_object_ms,
    );
}

/// SUBGROUP_DELIVERY_TIMEOUT の effective 値を subscriber / publisher の現在値から再計算する
///
/// `DeliveryTimeoutState::effective_subgroup_ms` の更新を 1 箇所に集約するための内部関数。
/// 計算規則は [`compute_effective_delivery_timeout_ms`] (draft-ietf-moq-transport-21 §5.2) に従う。
fn refresh_effective_subgroup_delivery_timeout(subscription: &mut Subscription) {
    subscription.delivery_timeouts.effective_subgroup_ms = compute_effective_delivery_timeout_ms(
        subscription.delivery_timeouts.subscriber_subgroup_ms,
        subscription.delivery_timeouts.publisher_subgroup_ms,
    );
}

/// subscription の EXPIRES 期限を設定する
/// (draft-ietf-moq-transport-21 §9.20.17 (EXPIRES Parameter))
///
/// §9.20.17 は「EXPIRES が 0 または不在なら subscription は expire しないか、不明な時刻に
/// expire する」と規定するため、`None` に加えて `Some(0)` も「期限なし」(`None`) に正規化する。
/// 呼び出し側が正規化済みとは限らない場面でも安全に扱えるよう、ここでも防御的に正規化する。
/// `None` を渡すと既存の期限はクリアされる。`now_ms` は `DeadlineTimer` の起点であり、
/// `None` の場合は最初の `tick()` まで起点を保持しない。
pub fn set_subscription_expires(
    subscription: &mut Subscription,
    expires_ms: Option<u64>,
    now_ms: Option<u64>,
) {
    subscription.expires = expires_ms
        .filter(|&expires_ms| expires_ms != 0)
        .map(|expires_ms| DeadlineTimer::new(expires_ms, now_ms));
}

/// parameters に EXPIRES が存在する場合のみ subscription の期限を更新する
/// (draft-ietf-moq-transport-21 §9.5 (REQUEST_UPDATE), §9.5.1 (Updating Subscriptions),
/// §9.20.17 (EXPIRES Parameter))
///
/// EXPIRES が不在の場合は §9.5 の「要求に含まれないパラメータの値は変更されない」に従い
/// 前回値を維持する (累積パラメータ。§9.5.1 は複数の REQUEST_UPDATE を合体して適用することを
/// 許し、後続の値が優先される)。存在する場合は [`set_subscription_expires`] に委譲し、
/// 0 は「期限なし」として既存の期限をクリアする ([`MessageParameters::expires`] が 0 を
/// `None` に正規化する)。そのため出現有無の判定には生の値を見る
/// [`MessageParameters::has_expires`] を使う。
pub fn update_subscription_expires_if_present(
    subscription: &mut Subscription,
    parameters: &MessageParameters,
    now_ms: Option<u64>,
) {
    if parameters.has_expires() {
        set_subscription_expires(subscription, parameters.expires(), now_ms);
    }
}

/// subscriber 申告の OBJECT_DELIVERY_TIMEOUT を設定し effective 値を再計算する
///
/// [`update_subscription_subscriber_delivery_timeouts_if_present`] からのみ呼ばれる内部関数で、
/// REQUEST_UPDATE の OBJECT_DELIVERY_TIMEOUT parameter (draft-ietf-moq-transport-21 §9.20.5) を
/// 反映する。SUBSCRIBE の送受信時の初期値は `Subscription` の構築時に直接設定する。
/// `None` は「タイムアウトなし」を表す。値 0 の正規化は effective 値の計算時に行われる
/// ([`compute_effective_delivery_timeout_ms`])。
fn set_subscription_subscriber_object_delivery_timeout(
    subscription: &mut Subscription,
    timeout_ms: Option<u64>,
) {
    subscription.delivery_timeouts.subscriber_object_ms = timeout_ms;
    refresh_effective_object_delivery_timeout(subscription);
}

/// subscriber 申告の SUBGROUP_DELIVERY_TIMEOUT を設定し effective 値を再計算する
///
/// [`update_subscription_subscriber_delivery_timeouts_if_present`] からのみ呼ばれる内部関数で、
/// REQUEST_UPDATE の SUBGROUP_DELIVERY_TIMEOUT parameter (draft-ietf-moq-transport-21 §9.20.4)
/// を反映する。SUBSCRIBE の送受信時の初期値は `Subscription` の構築時に直接設定する。
/// `None` は「タイムアウトなし」を表す。値 0 の正規化は effective 値の計算時に行われる
/// ([`compute_effective_delivery_timeout_ms`])。
fn set_subscription_subscriber_subgroup_delivery_timeout(
    subscription: &mut Subscription,
    timeout_ms: Option<u64>,
) {
    subscription.delivery_timeouts.subscriber_subgroup_ms = timeout_ms;
    refresh_effective_subgroup_delivery_timeout(subscription);
}

/// publisher 申告の OBJECT_DELIVERY_TIMEOUT を設定し effective 値を再計算する
/// (draft-ietf-moq-transport-21 §10.2 (OBJECT_DELIVERY_TIMEOUT), §5.2)
///
/// publisher 側の値は Track Property として通知されるため、subscriber 役では SUBSCRIBE_OK の
/// Track Properties から、publisher 役では自側が `send_subscribe_ok` に渡した Track Properties
/// から設定する。PUBLISH の送受信時の初期値は `Subscription` の構築時に直接設定する。
/// `None` は「タイムアウトなし」を表し、この場合の effective 値は subscriber 申告のみで決まる。
/// 値 0 の正規化は effective 値の計算時に行われる ([`compute_effective_delivery_timeout_ms`])。
pub fn set_subscription_publisher_object_delivery_timeout(
    subscription: &mut Subscription,
    timeout_ms: Option<u64>,
) {
    subscription.delivery_timeouts.publisher_object_ms = timeout_ms;
    refresh_effective_object_delivery_timeout(subscription);
}

/// publisher 申告の SUBGROUP_DELIVERY_TIMEOUT を設定し effective 値を再計算する
/// (draft-ietf-moq-transport-21 §10.1 (SUBGROUP_DELIVERY_TIMEOUT), §5.2)
///
/// publisher 側の値は Track Property として通知されるため、subscriber 役では SUBSCRIBE_OK の
/// Track Properties から、publisher 役では自側が `send_subscribe_ok` に渡した Track Properties
/// から設定する。PUBLISH の送受信時の初期値は `Subscription` の構築時に直接設定する。
/// `None` は「タイムアウトなし」を表し、この場合の effective 値は subscriber 申告のみで決まる。
/// 値 0 の正規化は effective 値の計算時に行われる ([`compute_effective_delivery_timeout_ms`])。
pub fn set_subscription_publisher_subgroup_delivery_timeout(
    subscription: &mut Subscription,
    timeout_ms: Option<u64>,
) {
    subscription.delivery_timeouts.publisher_subgroup_ms = timeout_ms;
    refresh_effective_subgroup_delivery_timeout(subscription);
}

/// parameters に含まれる subscriber 申告の delivery timeout を subscription に反映する
/// (draft-ietf-moq-transport-21 §9.20.4 (SUBGROUP_DELIVERY_TIMEOUT Parameter),
/// §9.20.5 (OBJECT_DELIVERY_TIMEOUT Parameter), §9.5 (REQUEST_UPDATE),
/// §9.5.1 (Updating Subscriptions), §5.2)
///
/// OBJECT_DELIVERY_TIMEOUT / SUBGROUP_DELIVERY_TIMEOUT が存在する項目だけを
/// [`set_subscription_subscriber_object_delivery_timeout`] /
/// [`set_subscription_subscriber_subgroup_delivery_timeout`] で更新し、不在の項目は §9.5
/// の「含まれないパラメータの値は変更されない」に従い前回値を維持する (累積パラメータ)。
/// 値 0 は §5.2 の「timeout を設定しない」を意味するため、effective 値の再計算で `None` に
/// 正規化される。subscriber 側の値は Message Parameter として通知される。
pub fn update_subscription_subscriber_delivery_timeouts_if_present(
    subscription: &mut Subscription,
    parameters: &MessageParameters,
) {
    if let Some(ms) = parameters.object_delivery_timeout() {
        set_subscription_subscriber_object_delivery_timeout(subscription, Some(ms));
    }
    if let Some(ms) = parameters.subgroup_delivery_timeout() {
        set_subscription_subscriber_subgroup_delivery_timeout(subscription, Some(ms));
    }
}

/// PUBLISH_DONE 受信時の状態を subscription に記録する
/// (draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE))
///
/// drain timer は §9.9 の「subscriber は late-arriving object に備えて
/// SUBGROUP_DELIVERY_TIMEOUT と OBJECT_DELIVERY_TIMEOUT の大きい方以上の timer を設定する
/// (SHOULD)」に従い、両 effective 値の大きい方で作る (両方 `None` なら 0 ms)。
/// `now_ms` は drain timer の起点であり、`None` の場合は最初の `tick()` まで起点を保持しない。
///
/// Stream Count が sentinel ([`PUBLISH_DONE_STREAM_COUNT_UNKNOWN`] = 2^64 - 1) の場合は
/// publisher が正確な本数を表明していないため比較対象が無く、`stream_count_overrun` は
/// 常に false とする。それ以外は受信 data stream 数 (`incoming_subgroup_count`) が
/// Stream Count を超えた場合に true とし、`cleanup_ready()` が drain timer の満了を待たずに
/// subscription state を回収できるようにする。
pub fn record_publish_done(
    subscription: &mut Subscription,
    done: &PublishDone,
    now_ms: Option<u64>,
) {
    let object = subscription
        .delivery_timeouts
        .effective_object_ms
        .unwrap_or(0);
    let subgroup = subscription
        .delivery_timeouts
        .effective_subgroup_ms
        .unwrap_or(0);
    let drain_timeout_ms = object.max(subgroup);
    let stream_count_overrun = done.stream_count != PUBLISH_DONE_STREAM_COUNT_UNKNOWN
        && subscription.stream_counts.incoming_subgroup_count > done.stream_count;
    subscription.publish_done = Some(SubscriptionPublishDone {
        status_code: done.status_code,
        stream_count: done.stream_count,
        reason: done.reason.clone(),
        drain: DeadlineTimer::new(drain_timeout_ms, now_ms),
        stream_count_overrun,
    });
}

/// 報告すべき最大 Location を計算する (draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter))
///
/// subscriber 側で保存した広告値 (`largest_location`) と実際の受信最大値
/// (`largest_received_location`) の大きい方を返す。publisher 側では
/// `largest_location` が常に `None` のため、実効値は送信観測値のみになる。
pub fn effective_largest_object(subscription: &Subscription) -> Option<Location> {
    match (
        subscription.largest_location.as_ref(),
        subscription.largest_received_location.as_ref(),
    ) {
        (Some(a), Some(b)) => Some(*a.max(b)),
        (Some(a), None) => Some(*a),
        (None, Some(b)) => Some(*b),
        (None, None) => None,
    }
}

/// parameters の LARGEST_OBJECT が location 以上であることを保証する
///
/// 既存の LARGEST_OBJECT が location 以上であればそのまま。そうでなければ置き換える。
pub fn update_largest_object_in_parameters(
    parameters: &mut MessageParameters,
    location: &Location,
) {
    if let Some((existing_group, existing_object)) = parameters.largest_object() {
        let existing = Location {
            group_id: existing_group,
            object_id: existing_object,
        };
        if &existing >= location {
            return;
        }
    }
    parameters.set_largest_object(location.group_id, location.object_id);
}

impl Session {
    /// publisher が観測した Track の Largest Object を返す
    ///
    /// 同一 Track の全 publisher 役 subscription の `effective_largest_object` の
    /// 最大値を返す Track 単位の集約である (draft-ietf-moq-transport-21
    /// §3.1.3 (Largest Object))。値は subscription が保持するため、forget 済みの
    /// Track では `None` を返す (Terminated でも保持中なら寄与する)。用途:
    /// - draft-ietf-moq-transport-21 §3.4 (Fill Semantics): fill range は
    ///   Largest Object を超えない
    /// - draft-ietf-moq-transport-21 §9.11 (FETCH): 要求 Start が Largest Object を
    ///   上回る場合の INVALID_RANGE 判定
    /// - draft-ietf-moq-transport-21 §3.3.1 (Location Filters): 受信した SUBSCRIBE の
    ///   Largest Object 相対フィルタを解決する基準位置
    /// - draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter):
    ///   SUBSCRIBE_OK / REQUEST_UPDATE_OK / PUBLISH_STATE_NOTIFY / PUBLISH に載せる
    ///   largest
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
            // 索引キーが publisher 役を含むため現状では恒真だが、索引と本体の
            // 不整合があった場合に subscriber 役の値を混ぜないための防御
            .filter(|sub| sub.my_role == TrackRole::Publisher)
            .filter_map(effective_largest_object)
            .max()
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::{
        DeadlineTimer, MessageParameters, Subscription, set_subscription_expires,
        update_subscription_expires_if_present,
    };
    use crate::message::common::TrackNamespace;
    use crate::message_parameter::{MessageParameter, MessageParameterValue, PARAM_EXPIRES};
    use crate::session::types::{
        DeliveryTimeoutState, StreamCountState, SubscriptionInitiator, SubscriptionRangeFilters,
        SubscriptionState, TrackRole,
    };

    fn empty_subscription() -> Subscription {
        Subscription {
            request_id: 0,
            initiator: SubscriptionInitiator::Subscriber,
            my_role: TrackRole::Subscriber,
            track_namespace: TrackNamespace::new(Vec::new()).expect("空のトラック名前空間は有効"),
            track_name: Vec::new(),
            track_alias: None,
            state: SubscriptionState::Pending,
            forward_state: 0,
            largest_location: None,
            largest_received_location: None,
            delivery_timeouts: DeliveryTimeoutState {
                subscriber_object_ms: None,
                subscriber_subgroup_ms: None,
                publisher_object_ms: None,
                publisher_subgroup_ms: None,
                effective_object_ms: None,
                effective_subgroup_ms: None,
                subgroup_overrides: hashbrown::HashMap::new(),
            },
            expires: None,
            dynamic_groups: false,
            default_publisher_priority: None,
            default_publisher_group_order: None,
            stream_counts: StreamCountState {
                published_count: 0,
                incoming_subgroup_count: 0,
                open_incoming_subgroup_count: 0,
            },
            publish_done: None,
            pending_publish_done: None,
            filter: None,
            subscriber_priority: None,
            group_order: None,
            // PUBLISH に INCLUDE_PROPERTIES は出現しない (draft-ietf-moq-transport-21 §9.20.22)
            include_properties: None,
            filter_start: None,
            filter_end: None,
            range_filters: SubscriptionRangeFilters::default(),
            ended_groups: hashbrown::HashMap::new(),
            end_of_track: None,
            pending_update_params: None,
        }
    }

    #[test]
    fn set_subscription_expires_with_none_clears_expires() {
        // set_subscription_expires に None を渡すと subscription.expires がクリアされる。
        let mut subscription = empty_subscription();
        subscription.expires = Some(DeadlineTimer::new(300, Some(1000)));
        set_subscription_expires(&mut subscription, None, Some(2000));
        assert!(subscription.expires.is_none());
    }

    #[test]
    fn set_subscription_expires_with_zero_clears_expires() {
        // set_subscription_expires に Some(0) を渡すと、防御的に None に正規化されて
        // subscription.expires がクリアされる。
        let mut subscription = empty_subscription();
        subscription.expires = Some(DeadlineTimer::new(300, Some(1000)));
        set_subscription_expires(&mut subscription, Some(0), Some(2000));
        assert!(subscription.expires.is_none());
    }

    #[test]
    fn expires_zero_clears_subscription_expires() {
        // EXPIRES=0 は「期限なし」を意味し、既存の期限値も None にクリアする。
        // draft-ietf-moq-transport-21 §9.20.17
        let mut subscription = empty_subscription();
        subscription.expires = Some(DeadlineTimer::new(300, Some(1000)));
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_EXPIRES,
            value: MessageParameterValue::VarInt(0),
        });
        update_subscription_expires_if_present(&mut subscription, &params, Some(1000));
        assert!(subscription.expires.is_none());
    }

    #[test]
    fn expires_non_zero_sets_subscription_expires() {
        // EXPIRES=n（n > 0）では subscription.expires に DeadlineTimer が設定される。
        // draft-ietf-moq-transport-21 §9.20.17
        let mut subscription = empty_subscription();
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_EXPIRES,
            value: MessageParameterValue::VarInt(300),
        });
        update_subscription_expires_if_present(&mut subscription, &params, Some(1000));
        let expires = subscription
            .expires
            .as_ref()
            .expect("expires が設定されていなければならない");
        assert_eq!(expires.duration_ms, 300);
        assert_eq!(expires.since_ms, Some(1000));
        assert!(!expires.expired);
    }

    #[test]
    fn missing_expires_keeps_existing_subscription_expires() {
        // EXPIRES 不在の場合は累積パラメータとして前回値を維持する。
        // draft-ietf-moq-transport-21 §9.5, §9.5.1 (Updating Subscriptions)
        let mut subscription = empty_subscription();
        subscription.expires = Some(DeadlineTimer::new(300, Some(1000)));
        let params = MessageParameters::new();
        update_subscription_expires_if_present(&mut subscription, &params, Some(2000));
        let expires = subscription
            .expires
            .as_ref()
            .expect("expires が維持されていなければならない");
        assert_eq!(expires.duration_ms, 300);
        assert_eq!(expires.since_ms, Some(1000));
    }
}
