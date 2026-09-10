//! SUBSCRIBE / PUBLISH / REQUEST_UPDATE / PUBLISH_DONE / STOP_SENDING 関連
//!
//! draft-ietf-moq-transport-21 §3.1 (Subscriptions), §3.1.1 (Subscription State Management),
//! §9.6 (SUBSCRIBE)-§9.9 (PUBLISH_DONE) に対応する。
//! `impl Session` の送受信メソッドをサブモジュールに分割。将来 draft 側で変更される可能性がある。

pub(super) mod delivery;
pub(super) mod dispatch;
pub(super) mod fill;
pub(super) mod recv;
pub(super) mod send;
pub(super) mod validation;

use super::core::{Session, remove_alias_holder};
use super::types::{
    RequestKind, SessionEvent, Subscription, SubscriptionState, TerminationReason, TrackRole,
};
use crate::message::common::TrackNamespace;
use alloc::vec::Vec;

impl Session {
    // ─── クエリ API ─────────────────────────────────────────

    /// 指定 Request ID の Subscription を参照する
    pub fn subscription(&self, request_id: u64) -> Option<&Subscription> {
        self.subscriptions.get(&request_id)
    }

    /// すべての Subscription をイテレートする
    pub fn subscriptions(&self) -> impl Iterator<Item = &Subscription> {
        self.subscriptions.values()
    }

    /// 購読系索引 (`subscriptions` / `subscriptions_by_track`) への追加を 1 箇所に集約する
    ///
    /// subscription 本体と、Track (namespace, name, role) 単位の索引を同時に登録する。
    /// 削除は [`Session::remove_subscription_track_index`] が `subscriptions_by_track` を担い、
    /// subscription 本体は `forget_subscription` が除去する。alias 索引は別フェーズで登録する
    /// (`my_publisher_aliases` は `insert_alias_holder`、`peer_publisher_aliases` は
    /// `register_peer_alias`（受信 PUBLISH / SUBSCRIBE_OK 確定時）)。
    pub(crate) fn register_subscription(&mut self, request_id: u64, subscription: Subscription) {
        let key = (
            subscription.track_namespace.clone(),
            subscription.track_name.clone(),
            subscription.my_role,
        );
        self.subscriptions.insert(request_id, subscription);
        self.aliases
            .subscriptions_by_track
            .entry(key)
            .or_default()
            .push(request_id);
    }

    /// `subscriptions_by_track` から 1 つの request_id を除去する (空になったら key ごと削除)
    ///
    /// 購読系索引の削除を 1 箇所に集約する。`forget_subscription` / `supersede_pending_subscriber` /
    /// `close_track_subscription_on_stream_end` がこのヘルパを通す。subscription 本体の除去は
    /// `forget_subscription` のみが行う。
    pub(crate) fn remove_subscription_track_index(
        &mut self,
        request_id: u64,
        key: &(TrackNamespace, Vec<u8>, TrackRole),
    ) {
        if let Some(ids) = self.aliases.subscriptions_by_track.get_mut(key) {
            ids.retain(|&id| id != request_id);
            if ids.is_empty() {
                self.aliases.subscriptions_by_track.remove(key);
            }
        }
    }

    /// 指定 Request ID の subscription が cleanup 可能か返す
    pub fn subscription_cleanup_ready(&self, request_id: u64) -> Option<bool> {
        self.subscriptions
            .get(&request_id)
            .map(Subscription::cleanup_ready)
    }

    /// Terminated 状態の subscription をセッションから除去する
    ///
    /// draft §3.1.1 (Subscription State Management): PUBLISH_DONE 後の delivery timeout や open stream drain が残る間は
    /// subscription を保持し、`cleanup_ready()` になった時点で関連索引と合わせて解放する。
    /// cleanup 不可の state の場合は `None` を返し、削除は行わない。
    pub fn forget_subscription(&mut self, request_id: u64) -> Option<Subscription> {
        let entry = self.subscriptions.get(&request_id)?;
        if !entry.cleanup_ready() {
            return None;
        }
        let mut subscription = self.subscriptions.remove(&request_id)?;
        // REQUEST_UPDATE 失敗応答で保留された PUBLISH_DONE (UPDATE_FAILED) は、
        // stream を閉じないまま破棄する場合は push されずに破棄される
        // (draft-ietf-moq-transport-21 §9.9 の MUST NOT により stream が閉じるまで送れず、
        // §9.5.1 の MUST が果たせない場合の帰結。remove でセッションからは除去済みだが、
        // 返り値の subscription が「破棄済み」であることを明示する)
        subscription.pending_publish_done = None;
        self.clear_control_message_deadline(request_id);
        self.request_streams.remove(&request_id);
        let track_key = (
            subscription.track_namespace.clone(),
            subscription.track_name.clone(),
            subscription.my_role,
        );
        // 既に別 entry (新 PUBLISH 等) が track_key を
        // 占有している可能性があるため、自分の request_id を含む場合のみ除去する。
        self.remove_subscription_track_index(request_id, &track_key);
        if let Some(alias) = subscription.track_alias {
            // draft §3.1 (Subscriptions): alias は同一 Track の複数 subscription で共有されうる。
            // 自分の request_id だけを索引から外し、共有相手が残っている場合は
            // subgroup tracker の alias 単位クリーンアップを行わない (残った subscription の
            // data plane 追跡を壊すため)。
            match subscription.my_role {
                TrackRole::Publisher => {
                    if remove_alias_holder(
                        &mut self.aliases.my_publisher_aliases,
                        alias,
                        request_id,
                    ) {
                        self.my_subgroups.remove_track_alias(alias);
                    }
                }
                TrackRole::Subscriber => {
                    if self.release_peer_alias(alias, request_id) {
                        self.peer_subgroups.remove_track_alias(alias);
                    }
                }
            }
        }
        self.remove_incoming_data_streams_for_request(request_id);
        self.remove_outgoing_data_plane_for_request(request_id);
        self.remove_request_update_credit_entries(request_id);
        Some(subscription)
    }

    /// `Pending(Subscriber)` 状態の既存 subscription を `PUBLISH` 受信に伴い
    /// Terminated に遷移させる
    ///
    /// draft-ietf-moq-transport-21 §3.1 (Subscriptions): subscriber が自ら送った
    /// SUBSCRIBE が応答待ち (Pending(Subscriber)) の最中に、同一 Track の PUBLISH を
    /// 受信した場合、既存を Terminated に遷移させてから PUBLISH_OK を送る。
    /// 本メソッドは呼び出し側 (handle_peer_publish) が新 PUBLISH を登録する直前に
    /// 呼ぶことを想定し、既存 entry の追跡期限だけ解除する (subscription 本体は
    /// `state = Terminated` で残り、`request_streams` の索引も保持して後続の
    /// stream close 通知を受理できるようにする)。最終的な回収は呼び出し側が
    /// `forget_subscription` で行う。
    ///
    /// `SessionEvent::RequestTerminated { reason: SupersededByPublish { new_request_id } }`
    /// を発火し、呼び出し側が旧 SUBSCRIBE bidi stream を STOP_SENDING /
    /// RESET_STREAM で閉じられるようにする。将来 draft が変更される可能性がある。
    fn supersede_pending_subscriber(&mut self, existing_id: u64, new_request_id: u64) {
        let existing = self
            .subscriptions
            .get_mut(&existing_id)
            .expect("supersede_pending_subscriber: existing subscription must be present");
        existing.state = SubscriptionState::Terminated;
        let track_alias = existing.track_alias;
        let key = (
            existing.track_namespace.clone(),
            existing.track_name.clone(),
            existing.my_role,
        );
        // 旧 subscription の索引を外す。subscriptions マップ本体は Terminated 状態で
        // 残し、呼び出し側の forget_subscription で最終的に回収する (応答 stream の
        // クリーンアップが必要になる可能性があるため)。
        self.remove_subscription_track_index(existing_id, &key);
        self.clear_control_message_deadline(existing_id);
        if let Some(alias) = track_alias {
            // subscriber 役の subscription は peer_publisher_aliases を経由しないので
            // 通常は None だが、将来仕様変更で alias を持つようになっても追従できるよう
            // チェックを入れておく。共有 alias を壊さないため自分の request_id だけを外す。
            self.release_peer_alias(alias, existing_id);
        }
        self.events.push_back(SessionEvent::RequestTerminated {
            request_id: existing_id,
            kind: RequestKind::Subscribe,
            reason: TerminationReason::SupersededByPublish { new_request_id },
        });
    }
}
