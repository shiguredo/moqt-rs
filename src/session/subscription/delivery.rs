//! 配信タイムアウト計算と PUBLISH_DONE 記録 (draft-ietf-moq-transport-21 §3.1, §9.9)
//!
//! 将来 draft 側で変更される可能性がある。

use crate::message::{PublishDone, common::Location};
use crate::message_parameter::MessageParameters;

use super::super::core::PUBLISH_DONE_STREAM_COUNT_UNKNOWN;
use super::super::types::{DeadlineTimer, Subscription, SubscriptionPublishDone};

pub fn normalize_delivery_timeout_ms(delivery_timeout_ms: Option<u64>) -> Option<u64> {
    match delivery_timeout_ms {
        Some(0) | None => None,
        Some(delivery_timeout_ms) => Some(delivery_timeout_ms),
    }
}

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

pub fn refresh_effective_object_delivery_timeout(subscription: &mut Subscription) {
    subscription.delivery_timeouts.effective_object_ms = compute_effective_delivery_timeout_ms(
        subscription.delivery_timeouts.subscriber_object_ms,
        subscription.delivery_timeouts.publisher_object_ms,
    );
}

pub fn refresh_effective_subgroup_delivery_timeout(subscription: &mut Subscription) {
    subscription.delivery_timeouts.effective_subgroup_ms = compute_effective_delivery_timeout_ms(
        subscription.delivery_timeouts.subscriber_subgroup_ms,
        subscription.delivery_timeouts.publisher_subgroup_ms,
    );
}

pub fn set_subscription_expires(
    subscription: &mut Subscription,
    expires_ms: Option<u64>,
    now_ms: Option<u64>,
) {
    // EXPIRES=0 は「期限なし」を意味するため、呼び出し側が正規化済みとは限らない
    // 場面でも安全に扱えるよう防御的に None に正規化する。
    // draft-ietf-moq-transport-21 §9.20.17（将来の draft で変更される可能性がある）
    subscription.expires = expires_ms
        .filter(|&expires_ms| expires_ms != 0)
        .map(|expires_ms| DeadlineTimer::new(expires_ms, now_ms));
}

pub fn update_subscription_expires_if_present(
    subscription: &mut Subscription,
    parameters: &MessageParameters,
    now_ms: Option<u64>,
) {
    // EXPIRES パラメータが存在する場合のみ更新する。
    // EXPIRES=0 は `expires()` で `None` に正規化され、「期限なし」として
    // クリアされる。EXPIRES 不在の場合は累積パラメータとして前回値を維持する。
    // draft-ietf-moq-transport-21 §9.5, §9.5.1 (Updating Subscriptions)
    //（将来の draft で変更される可能性がある）
    if parameters.has_expires() {
        set_subscription_expires(subscription, parameters.expires(), now_ms);
    }
}

pub fn set_subscription_subscriber_object_delivery_timeout(
    subscription: &mut Subscription,
    timeout_ms: Option<u64>,
) {
    subscription.delivery_timeouts.subscriber_object_ms = timeout_ms;
    refresh_effective_object_delivery_timeout(subscription);
}

pub fn set_subscription_subscriber_subgroup_delivery_timeout(
    subscription: &mut Subscription,
    timeout_ms: Option<u64>,
) {
    subscription.delivery_timeouts.subscriber_subgroup_ms = timeout_ms;
    refresh_effective_subgroup_delivery_timeout(subscription);
}

pub fn set_subscription_publisher_object_delivery_timeout(
    subscription: &mut Subscription,
    timeout_ms: Option<u64>,
) {
    subscription.delivery_timeouts.publisher_object_ms = timeout_ms;
    refresh_effective_object_delivery_timeout(subscription);
}

pub fn set_subscription_publisher_subgroup_delivery_timeout(
    subscription: &mut Subscription,
    timeout_ms: Option<u64>,
) {
    subscription.delivery_timeouts.publisher_subgroup_ms = timeout_ms;
    refresh_effective_subgroup_delivery_timeout(subscription);
}

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
    // draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE):
    // sentinel (`PUBLISH_DONE_STREAM_COUNT_UNKNOWN`) を受信した場合、publisher は exact 数を
    // 表明していないので比較対象が無い。overrun フラグは常に false とする。
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
            subscriber_rendezvous_timeout_ms: None,
            expires: None,
            dynamic_groups: false,
            publisher_priority: None,
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
