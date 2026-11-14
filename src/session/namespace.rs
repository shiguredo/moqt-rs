//! namespace 発見・告知 (draft-ietf-moq-transport-23 §4 (Namespace Discovery))
//!
//! PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS と、その応答 stream に置く
//! NAMESPACE / NAMESPACE_DONE / PUBLISH_SKIPPED を扱う。本ライブラリは client / server の
//! endpoint 専用であり、relay の forwarding (§7) は実装しない。したがってここで扱うのは
//! 自側の publisher / subscriber と 1 対 1 の peer の間の状態だけである。
//!
//! 役割は [`TrackRole`] で表す。
//!
//! - PUBLISH_NAMESPACE: 送る側 (requester) が Publisher、受けて応答する側 (responder) が
//!   Subscriber
//! - SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS: 送る側 (requester) が Subscriber、受けて応答する
//!   側 (responder) が Publisher
//!
//! 節番号・規則は draft 由来であり将来 draft 改版で変わる可能性がある。

use alloc::vec::Vec;

use crate::error::{
    REQUEST_DOES_NOT_EXIST, SESSION_INVALID_PARAMETER, SESSION_INVALID_TRACK_NAMESPACE,
    SESSION_PROTOCOL_VIOLATION, SESSION_UNEXPECTED_MESSAGE,
};
use crate::message::common::TrackNamespace;
use crate::message::{
    ControlMessage, Namespace, NamespaceDone, PublishNamespace, PublishSkipped, SubscribeNamespace,
    SubscribeTracks,
};
use crate::message_parameter::MessageParameters;
use crate::session::core::Session;
use crate::session::subscription::validation::{
    validate_group_order, validate_include_properties, validate_paused,
};
use crate::session::types::{
    NamespaceRequestState, NamespaceSubscription, PublishNamespaceRequest, RequestKind,
    RequestStreamEnd, SendRequestError, SessionEvent, TerminationReason, TrackRole,
    TrackSubscription, terminationreason_from_end,
};

/// prefix と suffix を結合して完全な Track Namespace を返す
///
/// draft-ietf-moq-transport-23 §9.15 (NAMESPACE) / §9.16 (NAMESPACE_DONE) / §9.18
/// (PUBLISH_SKIPPED) の Suffix は、対応する要求の Track Namespace Prefix の後ろに付く。
/// 結合結果が §8.7 (Track Namespace Structure) の上限 (32 フィールド / 4,096 バイト) を
/// 超える場合は INVALID_TRACK_NAMESPACE でセッションを閉じる。
fn combine_namespace(
    prefix: &TrackNamespace,
    suffix: &TrackNamespace,
) -> Result<TrackNamespace, crate::session::types::SessionError> {
    let mut fields = prefix.fields().to_vec();
    fields.extend_from_slice(suffix.fields());
    TrackNamespace::new(fields).map_err(|_| {
        crate::session::types::SessionError::new(
            SESSION_INVALID_TRACK_NAMESPACE,
            "namespace suffix does not form a valid Track Namespace",
        )
    })
}

/// 送信する namespace prefix の予約名前空間を検証する
///
/// draft-ietf-moq-transport-23 §2.4.3 (Reserved Namespaces) / §6.5 (Session-Level Tracks and
/// Namespaces): `.` 単独と `.session` は Application が使えない予約名前空間である。
fn validate_outgoing_namespace_prefix(
    prefix: &TrackNamespace,
    what: &'static str,
) -> Result<(), crate::session::types::SessionError> {
    if prefix.is_single_period() {
        return Err(crate::session::types::SessionError::new(
            SESSION_PROTOCOL_VIOLATION,
            what,
        ));
    }
    if prefix.is_session_level() {
        return Err(crate::session::types::SessionError::new(
            SESSION_PROTOCOL_VIOLATION,
            what,
        ));
    }
    Ok(())
}

/// namespace 要求の受信前に共通で行う検証
///
/// draft-ietf-moq-transport-23 §2.4.3 (Reserved Namespaces) / §6.5 (Session-Level Tracks and
/// Namespaces) の MUST に従い、`.` 単独と `.session` を DOES_NOT_EXIST で拒否する。
/// request ID の parity / 重複とパラメータスコープの検証は呼び出し側が
/// [`Session::accept_peer_request`] で行う。
fn reject_reserved_namespace(
    session: &mut Session,
    request_id: u64,
    prefix: &TrackNamespace,
) -> Result<bool, crate::session::types::SessionError> {
    if prefix.is_single_period() {
        session.emit_request_error(
            request_id,
            REQUEST_DOES_NOT_EXIST,
            "reserved single-period namespace",
        );
        return Ok(true);
    }
    if prefix.is_session_level() {
        session.emit_request_error(
            request_id,
            REQUEST_DOES_NOT_EXIST,
            "session-level namespace does not exist",
        );
        return Ok(true);
    }
    Ok(false)
}

impl Session {
    // ─── 送信 API (requester 側) ────────────────────────────

    /// PUBLISH_NAMESPACE を送信する (自側が publisher)
    ///
    /// draft-ietf-moq-transport-23 §9.13 (PUBLISH_NAMESPACE): publisher は subscriber へ、
    /// Track Namespace Prefix に一致する namespace の track を配信できることを告知する。
    /// [`SessionEvent::PublishNamespaceReceived`] の Pending 状態から REQUEST_OK で
    /// `Established` へ遷移する。
    pub fn send_publish_namespace(
        &mut self,
        track_namespace: TrackNamespace,
        parameters: MessageParameters,
    ) -> Result<u64, SendRequestError> {
        self.require_established()?;
        self.check_peer_goaway()?;
        validate_outgoing_namespace_prefix(
            &track_namespace,
            "application cannot publish a reserved namespace",
        )?;
        // draft-ietf-moq-transport-23 §9.19.1 (Parameter Scope): 許可されないパラメータの
        // 送信は request_id 発行前に拒否する
        if parameters
            .validate_scope(crate::message::PUBLISH_NAMESPACE_ALLOWED_PARAMS)
            .is_err()
        {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "PUBLISH_NAMESPACE parameter not allowed in this context",
            )
            .into());
        }
        let request_id = self.request_ids.local_generator.next_id();
        self.publish_namespaces.insert(
            request_id,
            PublishNamespaceRequest {
                request_id,
                my_role: TrackRole::Publisher,
                track_namespace: track_namespace.clone(),
                parameters: parameters.clone(),
                state: NamespaceRequestState::Pending,
            },
        );
        self.request_streams
            .insert(request_id, RequestKind::PublishNamespace);
        self.start_control_message_deadline(request_id);
        self.events.push_back(SessionEvent::SendRequest {
            request_id,
            message: ControlMessage::PublishNamespace(PublishNamespace {
                request_id,
                track_namespace,
                parameters,
            }),
        });
        Ok(request_id)
    }

    /// SUBSCRIBE_NAMESPACE を送信する (自側が subscriber)
    ///
    /// draft-ietf-moq-transport-23 §9.14 (SUBSCRIBE_NAMESPACE): subscriber は publisher へ、
    /// Track Namespace Prefix に一致する namespace の発見を要求する。REQUEST_OK のあと、
    /// 同じ response stream に [`SessionEvent::NamespaceReceived`] /
    /// [`SessionEvent::NamespaceDoneReceived`] が届く。
    pub fn send_subscribe_namespace(
        &mut self,
        track_namespace: TrackNamespace,
        parameters: MessageParameters,
    ) -> Result<u64, SendRequestError> {
        self.require_established()?;
        self.check_peer_goaway()?;
        validate_outgoing_namespace_prefix(
            &track_namespace,
            "application cannot subscribe to a reserved namespace",
        )?;
        if parameters
            .validate_scope(crate::message::SUBSCRIBE_NAMESPACE_ALLOWED_PARAMS)
            .is_err()
        {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "SUBSCRIBE_NAMESPACE parameter not allowed in this context",
            )
            .into());
        }
        let request_id = self.request_ids.local_generator.next_id();
        self.namespace_subscriptions.insert(
            request_id,
            NamespaceSubscription {
                request_id,
                my_role: TrackRole::Subscriber,
                track_namespace: track_namespace.clone(),
                parameters: parameters.clone(),
                state: NamespaceRequestState::Pending,
                received_namespaces: alloc::collections::BTreeSet::new(),
                sent_suffixes: alloc::collections::BTreeSet::new(),
            },
        );
        self.request_streams
            .insert(request_id, RequestKind::SubscribeNamespace);
        self.start_control_message_deadline(request_id);
        self.events.push_back(SessionEvent::SendRequest {
            request_id,
            message: ControlMessage::SubscribeNamespace(SubscribeNamespace {
                request_id,
                track_namespace,
                parameters,
            }),
        });
        Ok(request_id)
    }

    /// SUBSCRIBE_TRACKS を送信する (自側が subscriber)
    ///
    /// draft-ietf-moq-transport-23 §9.17 (SUBSCRIBE_TRACKS): subscriber は publisher へ、
    /// Track Namespace Prefix に一致する track の購読を要求する。REQUEST_OK のあと、
    /// publisher は一致する track ごとに新しい request stream で PUBLISH を送る。
    /// 購読を作れない track は [`SessionEvent::PublishSkippedReceived`] で届く。
    ///
    /// パラメータは resulting PUBLISH の initial subscription parameters として伝播する
    /// (§3.6.2 (Parameters on SUBSCRIBE_TRACKS))。値域の検証は SUBSCRIBE と同じ規則を適用する。
    pub fn send_subscribe_tracks(
        &mut self,
        track_namespace: TrackNamespace,
        parameters: MessageParameters,
    ) -> Result<u64, SendRequestError> {
        self.require_established()?;
        self.check_peer_goaway()?;
        validate_outgoing_namespace_prefix(
            &track_namespace,
            "application cannot subscribe to a reserved namespace",
        )?;
        if parameters
            .validate_scope(crate::message::SUBSCRIBE_TRACKS_ALLOWED_PARAMS)
            .is_err()
        {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "SUBSCRIBE_TRACKS parameter not allowed in this context",
            )
            .into());
        }
        // draft-ietf-moq-transport-23 §9.19.18 (PAUSED Parameter) / §9.19.8 (GROUP ORDER) /
        // §9.19.22 (INCLUDE_PROPERTIES): 値域外の送信を request_id 発行前に拒否する
        if let Some(value) = parameters.paused() {
            validate_paused(value)?;
        }
        if let Some(order) = parameters.group_order() {
            validate_group_order(order)?;
        }
        if let Some(value) = parameters.include_properties() {
            validate_include_properties(value)?;
        }
        // draft-ietf-moq-transport-23 §9.19.9 (LOCATION FILTER Parameter): 符号化の構造を
        // request_id 発行前に検証する
        if parameters.location_filter_update().is_err() {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "invalid subscription filter encoding",
            )
            .into());
        }
        // draft-ietf-moq-transport-23 §9.19.15 (FILL PARAMETERS Parameter): 内側スコープと
        // LOCATION_FILTER を送信前に検証する
        crate::session::subscription::fill::validate_outgoing_fill_parameters(&parameters)?;
        // draft-ietf-moq-transport-23 §3.3.2 (Range Filters) / §9.1.6 (MAX FILTER RANGES):
        // peer の上限超過は送信不可
        self.validate_outgoing_range_filters(&parameters)?;
        let request_id = self.request_ids.local_generator.next_id();
        self.track_subscriptions.insert(
            request_id,
            TrackSubscription {
                request_id,
                my_role: TrackRole::Subscriber,
                track_namespace: track_namespace.clone(),
                parameters: parameters.clone(),
                state: NamespaceRequestState::Pending,
                received_published: alloc::collections::BTreeSet::new(),
                received_skipped: alloc::collections::BTreeSet::new(),
                sent_published: alloc::collections::BTreeSet::new(),
                sent_skipped: alloc::collections::BTreeSet::new(),
            },
        );
        self.request_streams
            .insert(request_id, RequestKind::SubscribeTracks);
        self.start_control_message_deadline(request_id);
        self.events.push_back(SessionEvent::SendRequest {
            request_id,
            message: ControlMessage::SubscribeTracks(SubscribeTracks {
                request_id,
                track_namespace,
                parameters,
            }),
        });
        Ok(request_id)
    }

    // ─── 送信 API (responder 側) ────────────────────────────

    /// SUBSCRIBE_NAMESPACE の response stream に NAMESPACE を送信する (自側が publisher)
    ///
    /// draft-ietf-moq-transport-23 §9.15 (NAMESPACE): 発見した namespace の suffix を送る。
    /// `track_namespace` は要求の prefix に結合できる 0 フィールド以上の Track Namespace で
    /// なければならない。
    pub fn send_namespace(
        &mut self,
        request_id: u64,
        track_namespace: TrackNamespace,
        parameters: MessageParameters,
    ) -> Result<(), crate::session::types::SessionError> {
        self.require_established()?;
        let prefix = {
            let entry = self
                .namespace_subscriptions
                .get(&request_id)
                .ok_or_else(|| {
                    crate::session::types::SessionError::new(
                        SESSION_PROTOCOL_VIOLATION,
                        "SUBSCRIBE_NAMESPACE not found for send_namespace",
                    )
                })?;
            if entry.my_role != TrackRole::Publisher {
                return Err(crate::session::types::SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "NAMESPACE can only be sent by the responder side",
                ));
            }
            if entry.state != NamespaceRequestState::Established {
                return Err(crate::session::types::SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "NAMESPACE requires an established SUBSCRIBE_NAMESPACE",
                ));
            }
            entry.track_namespace.clone()
        };
        // draft-ietf-moq-transport-23 §9.19.1 (Parameter Scope): NAMESPACE が許可する
        // パラメータは無い
        if parameters
            .validate_scope(crate::message::NAMESPACE_ALLOWED_PARAMS)
            .is_err()
        {
            return Err(crate::session::types::SessionError::new(
                SESSION_INVALID_PARAMETER,
                "NAMESPACE parameter not allowed in this context",
            ));
        }
        // 結合結果が Track Namespace の上限に収まることを送信前に検証する
        combine_namespace(&prefix, &track_namespace)?;
        self.namespace_subscriptions
            .get_mut(&request_id)
            .expect("checked above")
            .sent_suffixes
            .insert(track_namespace.fields().to_vec());
        self.events.push_back(SessionEvent::SendOnStream {
            request_id,
            message: ControlMessage::Namespace(Namespace {
                track_namespace_suffix: track_namespace,
                parameters,
            }),
            fin: false,
        });
        Ok(())
    }

    /// SUBSCRIBE_NAMESPACE の response stream に NAMESPACE_DONE を送信する (自側が publisher)
    ///
    /// draft-ietf-moq-transport-23 §9.16 (NAMESPACE_DONE): §4.2 の MUST NOT に従い、
    /// 同じ suffix の NAMESPACE を送る前の NAMESPACE_DONE は拒否する。
    pub fn send_namespace_done(
        &mut self,
        request_id: u64,
        track_namespace: TrackNamespace,
    ) -> Result<(), crate::session::types::SessionError> {
        self.require_established()?;
        let prefix = {
            let entry = self
                .namespace_subscriptions
                .get(&request_id)
                .ok_or_else(|| {
                    crate::session::types::SessionError::new(
                        SESSION_PROTOCOL_VIOLATION,
                        "SUBSCRIBE_NAMESPACE not found for send_namespace_done",
                    )
                })?;
            if entry.my_role != TrackRole::Publisher {
                return Err(crate::session::types::SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "NAMESPACE_DONE can only be sent by the responder side",
                ));
            }
            if entry.state != NamespaceRequestState::Established {
                return Err(crate::session::types::SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "NAMESPACE_DONE requires an established SUBSCRIBE_NAMESPACE",
                ));
            }
            // draft-ietf-moq-transport-23 §4.2: "The publisher MUST NOT send NAMESPACE_DONE
            // for a namespace suffix before the corresponding NAMESPACE."
            if !entry.sent_suffixes.contains(track_namespace.fields()) {
                return Err(crate::session::types::SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "NAMESPACE_DONE requires the corresponding NAMESPACE",
                ));
            }
            entry.track_namespace.clone()
        };
        combine_namespace(&prefix, &track_namespace)?;
        self.namespace_subscriptions
            .get_mut(&request_id)
            .expect("checked above")
            .sent_suffixes
            .remove(track_namespace.fields());
        self.events.push_back(SessionEvent::SendOnStream {
            request_id,
            message: ControlMessage::NamespaceDone(NamespaceDone {
                track_namespace_suffix: track_namespace,
            }),
            fin: false,
        });
        Ok(())
    }

    /// SUBSCRIBE_TRACKS の response stream に PUBLISH_SKIPPED を送信する (自側が publisher)
    ///
    /// draft-ietf-moq-transport-23 §9.18 (PUBLISH_SKIPPED) / §3.6.3 (Skipped Tracks):
    /// PUBLISH を送らずに購読を作らなかった track を通知する。PUBLISH_SKIPPED を送った
    /// track には、その SUBSCRIBE_TRACKS に対して以後 PUBLISH を送ってはならない (MUST NOT)。
    pub fn send_publish_skipped(
        &mut self,
        request_id: u64,
        track_namespace: TrackNamespace,
        track_name: Vec<u8>,
    ) -> Result<(), crate::session::types::SessionError> {
        self.require_established()?;
        Self::validate_outgoing_track_name(&track_name)?;
        let prefix = {
            let entry = self.track_subscriptions.get(&request_id).ok_or_else(|| {
                crate::session::types::SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "SUBSCRIBE_TRACKS not found for send_publish_skipped",
                )
            })?;
            if entry.my_role != TrackRole::Publisher {
                return Err(crate::session::types::SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "PUBLISH_SKIPPED can only be sent by the responder side",
                ));
            }
            if entry.state != NamespaceRequestState::Established {
                return Err(crate::session::types::SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "PUBLISH_SKIPPED requires an established SUBSCRIBE_TRACKS",
                ));
            }
            entry.track_namespace.clone()
        };
        let full = combine_namespace(&prefix, &track_namespace)?;
        let key = (full.fields().to_vec(), track_name.clone());
        {
            let entry = self
                .track_subscriptions
                .get_mut(&request_id)
                .expect("checked above");
            // 同じ track の PUBLISH を送った後に PUBLISH_SKIPPED を送るのも、
            // 同じ track の PUBLISH_SKIPPED を 2 回送るのも §3.6.3 の目的に反する
            if entry.sent_published.contains(&key) || !entry.sent_skipped.insert(key) {
                return Err(crate::session::types::SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "PUBLISH_SKIPPED conflicts with the state of the SUBSCRIBE_TRACKS",
                ));
            }
        }
        self.events.push_back(SessionEvent::SendOnStream {
            request_id,
            message: ControlMessage::PublishSkipped(PublishSkipped {
                track_namespace_suffix: track_namespace,
                track_name,
            }),
            fin: false,
        });
        Ok(())
    }

    /// SUBSCRIBE_NAMESPACE の REQUEST_UPDATE を送信する (requester 側)
    ///
    /// draft-ietf-moq-transport-23 §9.5.2 (Updating Namespace Subscriptions): prefix を
    /// 置き換える。REQUEST_OK を受ける前でも、以後の suffix 解決に使う prefix は新しい値へ
    /// 切り替える (PAUSED と同じ楽観的な反映である)。新しい prefix に一致しなくなった
    /// 広告済み namespace は、§9.5.2 が「REQUEST_OK は NAMESPACE_DONE を意味する」と定める
    /// ため、`NamespaceDoneReceived` を発行して受信済み集合から除く。
    pub(crate) fn send_update_for_namespace_subscription(
        &mut self,
        request_id: u64,
        parameters: &MessageParameters,
    ) -> Result<(), crate::session::types::SessionError> {
        let new_prefix = parameters.track_namespace_prefix().cloned();
        if let Some(prefix) = &new_prefix
            && (prefix.is_single_period() || prefix.is_session_level())
        {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "application cannot update to a reserved namespace",
            ));
        }
        let mut ended = Vec::new();
        {
            let entry = self
                .namespace_subscriptions
                .get_mut(&request_id)
                .ok_or_else(|| {
                    crate::session::types::SessionError::new(
                        SESSION_PROTOCOL_VIOLATION,
                        "SUBSCRIBE_NAMESPACE not found for send_request_update",
                    )
                })?;
            if entry.my_role != TrackRole::Subscriber {
                return Err(crate::session::types::SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "REQUEST_UPDATE for SUBSCRIBE_NAMESPACE can only be sent by the requester side",
                ));
            }
            if entry.state != NamespaceRequestState::Established {
                return Err(crate::session::types::SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "REQUEST_UPDATE requires an established SUBSCRIBE_NAMESPACE",
                ));
            }
            entry.parameters.merge_from(parameters);
            if let Some(prefix) = new_prefix {
                entry.track_namespace = prefix.clone();
                let mut still_advertised = alloc::collections::BTreeSet::new();
                for fields in core::mem::take(&mut entry.received_namespaces) {
                    if fields.starts_with(prefix.fields()) {
                        still_advertised.insert(fields);
                    } else {
                        ended.push(fields);
                    }
                }
                entry.received_namespaces = still_advertised;
            }
        }
        for fields in ended {
            let track_namespace = TrackNamespace::new(fields).map_err(|_| {
                crate::session::types::SessionError::new(
                    SESSION_INVALID_TRACK_NAMESPACE,
                    "advertised namespace does not form a valid Track Namespace",
                )
            })?;
            self.events.push_back(SessionEvent::NamespaceDoneReceived {
                request_id,
                track_namespace,
            });
        }
        Ok(())
    }

    /// SUBSCRIBE_TRACKS の REQUEST_UPDATE を送信する (requester 側)
    ///
    /// draft-ietf-moq-transport-23 §9.5.2: prefix の更新は既存の subscription に影響しない。
    /// 送受信済み track の記録は完全な Track Namespace で保持しているため、prefix を変えても
    /// 重複 PUBLISH / PUBLISH_SKIPPED の判定は維持される。
    pub(crate) fn send_update_for_track_subscription(
        &mut self,
        request_id: u64,
        parameters: &MessageParameters,
    ) -> Result<(), crate::session::types::SessionError> {
        let new_prefix = parameters.track_namespace_prefix().cloned();
        if let Some(prefix) = &new_prefix
            && (prefix.is_single_period() || prefix.is_session_level())
        {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "application cannot update to a reserved namespace",
            ));
        }
        let entry = self
            .track_subscriptions
            .get_mut(&request_id)
            .ok_or_else(|| {
                crate::session::types::SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "SUBSCRIBE_TRACKS not found for send_request_update",
                )
            })?;
        if entry.my_role != TrackRole::Subscriber {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_UPDATE for SUBSCRIBE_TRACKS can only be sent by the requester side",
            ));
        }
        if entry.state != NamespaceRequestState::Established {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_UPDATE requires an established SUBSCRIBE_TRACKS",
            ));
        }
        entry.parameters.merge_from(parameters);
        if let Some(prefix) = new_prefix {
            entry.track_namespace = prefix;
        }
        Ok(())
    }

    // ─── REQUEST_UPDATE (prefix 更新) ─────────────────────

    /// SUBSCRIBE_NAMESPACE の REQUEST_UPDATE を受信できる状態かを検証する
    ///
    /// draft-ietf-moq-transport-23 §9.5 (REQUEST_UPDATE): 更新できるのは要求を開始した側
    /// (subscriber) であり、受信するのは responder (publisher) である。state は Established
    /// でなければならない。
    pub(crate) fn validate_namespace_request_update(
        &self,
        request_id: u64,
    ) -> Result<(), crate::session::types::SessionError> {
        let entry = self
            .namespace_subscriptions
            .get(&request_id)
            .ok_or_else(|| {
                crate::session::types::SessionError::new(
                    SESSION_UNEXPECTED_MESSAGE,
                    "REQUEST_UPDATE received for unknown SUBSCRIBE_NAMESPACE",
                )
            })?;
        if entry.my_role != TrackRole::Publisher {
            return Err(crate::session::types::SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "REQUEST_UPDATE for SUBSCRIBE_NAMESPACE received by the requester side",
            ));
        }
        if entry.state != NamespaceRequestState::Established {
            return Err(crate::session::types::SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "REQUEST_UPDATE requires an established SUBSCRIBE_NAMESPACE",
            ));
        }
        Ok(())
    }

    /// SUBSCRIBE_TRACKS の REQUEST_UPDATE を受信できる状態かを検証する
    pub(crate) fn validate_track_subscription_update(
        &self,
        request_id: u64,
    ) -> Result<(), crate::session::types::SessionError> {
        let entry = self.track_subscriptions.get(&request_id).ok_or_else(|| {
            crate::session::types::SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "REQUEST_UPDATE received for unknown SUBSCRIBE_TRACKS",
            )
        })?;
        if entry.my_role != TrackRole::Publisher {
            return Err(crate::session::types::SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "REQUEST_UPDATE for SUBSCRIBE_TRACKS received by the requester side",
            ));
        }
        if entry.state != NamespaceRequestState::Established {
            return Err(crate::session::types::SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "REQUEST_UPDATE requires an established SUBSCRIBE_TRACKS",
            ));
        }
        Ok(())
    }

    /// SUBSCRIBE_NAMESPACE の REQUEST_UPDATE を反映する (responder 側)
    ///
    /// draft-ietf-moq-transport-23 §9.5.2 (Updating Namespace Subscriptions): TRACK_NAMESPACE_PREFIX
    /// は prefix を置き換える。以後の NAMESPACE / NAMESPACE_DONE の suffix は新しい prefix を
    /// 基準にするため、旧 prefix 基準で記録した送信済み suffix を破棄する。
    pub(crate) fn handle_update_for_namespace_subscription(
        &mut self,
        request_id: u64,
        parameters: MessageParameters,
        locally_terminated: bool,
    ) -> Result<(), crate::session::types::SessionError> {
        if locally_terminated {
            return Ok(());
        }
        let new_prefix = parameters.track_namespace_prefix().cloned();
        if let Some(prefix) = &new_prefix
            && (prefix.is_single_period() || prefix.is_session_level())
        {
            let err = crate::session::types::SessionError::new(
                SESSION_INVALID_TRACK_NAMESPACE,
                "reserved namespace in REQUEST_UPDATE",
            );
            self.fail(err.clone());
            return Err(err);
        }
        let entry = self
            .namespace_subscriptions
            .get_mut(&request_id)
            .expect("validate_namespace_request_update guarantees key presence");
        entry.parameters.merge_from(&parameters);
        if let Some(prefix) = new_prefix {
            entry.track_namespace = prefix;
            entry.sent_suffixes.clear();
        }
        Ok(())
    }

    /// SUBSCRIBE_TRACKS の REQUEST_UPDATE を反映する (responder 側)
    ///
    /// draft-ietf-moq-transport-23 §9.5.2: prefix の更新は既存の subscription に影響しない。
    /// 送信済み track の記録は完全な Track Namespace で保持しているため、prefix を変えても
    /// 重複 PUBLISH の判定は維持される。
    pub(crate) fn handle_update_for_track_subscription(
        &mut self,
        request_id: u64,
        parameters: MessageParameters,
        locally_terminated: bool,
    ) -> Result<(), crate::session::types::SessionError> {
        if locally_terminated {
            return Ok(());
        }
        let new_prefix = parameters.track_namespace_prefix().cloned();
        if let Some(prefix) = &new_prefix
            && (prefix.is_single_period() || prefix.is_session_level())
        {
            let err = crate::session::types::SessionError::new(
                SESSION_INVALID_TRACK_NAMESPACE,
                "reserved namespace in REQUEST_UPDATE",
            );
            self.fail(err.clone());
            return Err(err);
        }
        let entry = self
            .track_subscriptions
            .get_mut(&request_id)
            .expect("validate_track_subscription_update guarantees key presence");
        entry.parameters.merge_from(&parameters);
        if let Some(prefix) = new_prefix {
            entry.track_namespace = prefix;
        }
        Ok(())
    }

    // ─── 応答送信 (responder 側) ───────────────────────────

    /// namespace 要求の REQUEST_OK 送信時の状態遷移 (responder 側)
    ///
    /// draft-ietf-moq-transport-23 §9.3 (REQUEST_OK): PUBLISH_NAMESPACE_OK /
    /// SUBSCRIBE_NAMESPACE_OK / SUBSCRIBE_TRACKS_OK は REQUEST_OK の別名であり、応答するのは
    /// 要求を受けた側 (responder) である。
    pub(crate) fn send_ok_for_namespace_request(
        &mut self,
        request_id: u64,
    ) -> Result<(), crate::session::types::SessionError> {
        if let Some(entry) = self.publish_namespaces.get_mut(&request_id) {
            if entry.my_role != TrackRole::Subscriber {
                return Err(crate::session::types::SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "REQUEST_OK (publish_namespace) can only be sent by the responder side",
                ));
            }
            if entry.state != NamespaceRequestState::Pending {
                return Err(crate::session::types::SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "REQUEST_OK on a PUBLISH_NAMESPACE that is not pending",
                ));
            }
            entry.state = NamespaceRequestState::Established;
            return Ok(());
        }
        if let Some(entry) = self.namespace_subscriptions.get_mut(&request_id) {
            if entry.my_role != TrackRole::Publisher {
                return Err(crate::session::types::SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "REQUEST_OK (subscribe_namespace) can only be sent by the responder side",
                ));
            }
            if entry.state == NamespaceRequestState::Terminated {
                return Err(crate::session::types::SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "REQUEST_OK on a terminated SUBSCRIBE_NAMESPACE",
                ));
            }
            entry.state = NamespaceRequestState::Established;
            return Ok(());
        }
        if let Some(entry) = self.track_subscriptions.get_mut(&request_id) {
            if entry.my_role != TrackRole::Publisher {
                return Err(crate::session::types::SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "REQUEST_OK (subscribe_tracks) can only be sent by the responder side",
                ));
            }
            if entry.state == NamespaceRequestState::Terminated {
                return Err(crate::session::types::SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "REQUEST_OK on a terminated SUBSCRIBE_TRACKS",
                ));
            }
            entry.state = NamespaceRequestState::Established;
            return Ok(());
        }
        Err(crate::session::types::SessionError::new(
            SESSION_PROTOCOL_VIOLATION,
            "namespace request not found for send_request_ok",
        ))
    }

    /// PUBLISH_NAMESPACE の REQUEST_ERROR 送信時の状態遷移 (responder 側)
    pub(crate) fn send_err_for_publish_namespace(
        &mut self,
        request_id: u64,
    ) -> Result<(), crate::session::types::SessionError> {
        let entry = self
            .publish_namespaces
            .get_mut(&request_id)
            .ok_or_else(|| {
                crate::session::types::SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "PUBLISH_NAMESPACE not found for send_request_error",
                )
            })?;
        if entry.my_role != TrackRole::Subscriber {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_ERROR (publish_namespace) can only be sent by the responder side",
            ));
        }
        if entry.state == NamespaceRequestState::Terminated {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_ERROR on a terminated PUBLISH_NAMESPACE",
            ));
        }
        entry.state = NamespaceRequestState::Terminated;
        Ok(())
    }

    /// SUBSCRIBE_NAMESPACE の REQUEST_ERROR 送信時の状態遷移 (responder 側)
    pub(crate) fn send_err_for_namespace_subscription(
        &mut self,
        request_id: u64,
    ) -> Result<(), crate::session::types::SessionError> {
        let entry = self
            .namespace_subscriptions
            .get_mut(&request_id)
            .ok_or_else(|| {
                crate::session::types::SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "SUBSCRIBE_NAMESPACE not found for send_request_error",
                )
            })?;
        if entry.my_role != TrackRole::Publisher {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_ERROR (subscribe_namespace) can only be sent by the responder side",
            ));
        }
        if entry.state == NamespaceRequestState::Terminated {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_ERROR on a terminated SUBSCRIBE_NAMESPACE",
            ));
        }
        entry.state = NamespaceRequestState::Terminated;
        Ok(())
    }

    /// SUBSCRIBE_TRACKS の REQUEST_ERROR 送信時の状態遷移 (responder 側)
    pub(crate) fn send_err_for_subscribe_tracks(
        &mut self,
        request_id: u64,
    ) -> Result<(), crate::session::types::SessionError> {
        let entry = self
            .track_subscriptions
            .get_mut(&request_id)
            .ok_or_else(|| {
                crate::session::types::SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "SUBSCRIBE_TRACKS not found for send_request_error",
                )
            })?;
        if entry.my_role != TrackRole::Publisher {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_ERROR (subscribe_tracks) can only be sent by the responder side",
            ));
        }
        if entry.state == NamespaceRequestState::Terminated {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "REQUEST_ERROR on a terminated SUBSCRIBE_TRACKS",
            ));
        }
        entry.state = NamespaceRequestState::Terminated;
        Ok(())
    }

    // ─── 受信ハンドラ (peer request の開始メッセージ) ──────

    /// peer から PUBLISH_NAMESPACE を受信した (自側が subscriber)
    ///
    /// draft-ietf-moq-transport-23 §9.13 (PUBLISH_NAMESPACE): 応答は 1 通の REQUEST_OK または
    /// REQUEST_ERROR でなければならない。アプリは
    /// [`SessionEvent::PublishNamespaceReceived`] を受けて応答する。
    pub(crate) fn handle_peer_publish_namespace(
        &mut self,
        msg: PublishNamespace,
    ) -> Result<(), crate::session::types::SessionError> {
        let request_id = msg.request_id;
        if !self.accept_peer_request(
            request_id,
            &msg.parameters,
            true,
            crate::message::PUBLISH_NAMESPACE_ALLOWED_PARAMS,
            "PUBLISH_NAMESPACE parameter not allowed in this context",
        )? {
            return Ok(());
        }
        self.apply_peer_message_auth_tokens(&msg.parameters)?;
        if reject_reserved_namespace(self, request_id, &msg.track_namespace)? {
            return Ok(());
        }
        self.publish_namespaces.insert(
            request_id,
            PublishNamespaceRequest {
                request_id,
                my_role: TrackRole::Subscriber,
                track_namespace: msg.track_namespace.clone(),
                parameters: msg.parameters.clone(),
                state: NamespaceRequestState::Pending,
            },
        );
        self.request_streams
            .insert(request_id, RequestKind::PublishNamespace);
        self.events
            .push_back(SessionEvent::PublishNamespaceReceived {
                request_id,
                track_namespace: msg.track_namespace,
                parameters: msg.parameters,
            });
        Ok(())
    }

    /// peer から SUBSCRIBE_NAMESPACE を受信した (自側が publisher)
    ///
    /// draft-ietf-moq-transport-23 §9.14 (SUBSCRIBE_NAMESPACE): アプリは
    /// [`SessionEvent::SubscribeNamespaceReceived`] を受けて REQUEST_OK / REQUEST_ERROR で
    /// 応答し、以後 `send_namespace` / `send_namespace_done` で発見結果を返す。
    pub(crate) fn handle_peer_subscribe_namespace(
        &mut self,
        msg: SubscribeNamespace,
    ) -> Result<(), crate::session::types::SessionError> {
        let request_id = msg.request_id;
        if !self.accept_peer_request(
            request_id,
            &msg.parameters,
            false,
            crate::message::SUBSCRIBE_NAMESPACE_ALLOWED_PARAMS,
            "SUBSCRIBE_NAMESPACE parameter not allowed in this context",
        )? {
            return Ok(());
        }
        self.apply_peer_message_auth_tokens(&msg.parameters)?;
        if reject_reserved_namespace(self, request_id, &msg.track_namespace)? {
            return Ok(());
        }
        self.namespace_subscriptions.insert(
            request_id,
            NamespaceSubscription {
                request_id,
                my_role: TrackRole::Publisher,
                track_namespace: msg.track_namespace.clone(),
                parameters: msg.parameters.clone(),
                state: NamespaceRequestState::Pending,
                received_namespaces: alloc::collections::BTreeSet::new(),
                sent_suffixes: alloc::collections::BTreeSet::new(),
            },
        );
        self.request_streams
            .insert(request_id, RequestKind::SubscribeNamespace);
        self.events
            .push_back(SessionEvent::SubscribeNamespaceReceived {
                request_id,
                track_namespace: msg.track_namespace,
                parameters: msg.parameters,
            });
        Ok(())
    }

    /// peer から SUBSCRIBE_TRACKS を受信した (自側が publisher)
    ///
    /// draft-ietf-moq-transport-23 §9.17 (SUBSCRIBE_TRACKS): アプリは
    /// [`SessionEvent::SubscribeTracksReceived`] を受けて REQUEST_OK / REQUEST_ERROR で応答し、
    /// 一致する track ごとに `send_publish` (SUBSCRIBE_TRACKS_ID 付き) または
    /// `send_publish_skipped` を呼ぶ。
    pub(crate) fn handle_peer_subscribe_tracks(
        &mut self,
        msg: SubscribeTracks,
    ) -> Result<(), crate::session::types::SessionError> {
        let request_id = msg.request_id;
        if !self.accept_peer_request(
            request_id,
            &msg.parameters,
            false,
            crate::message::SUBSCRIBE_TRACKS_ALLOWED_PARAMS,
            "SUBSCRIBE_TRACKS parameter not allowed in this context",
        )? {
            return Ok(());
        }
        self.apply_peer_message_auth_tokens(&msg.parameters)?;
        // draft-ietf-moq-transport-23 §3.3.2 (Range Filters) / §9.1.6 (MAX FILTER RANGES)
        if let Err(reason) = self.check_incoming_range_filters(&msg.parameters) {
            self.emit_request_error(request_id, crate::error::REQUEST_INVALID_FILTER, reason);
            return Ok(());
        }
        if reject_reserved_namespace(self, request_id, &msg.track_namespace)? {
            return Ok(());
        }
        // draft-ietf-moq-transport-23 §9.19.18 / §9.19.8 / §9.19.22: 値域外の受信は
        // INVALID_PARAMETER でセッションを閉じる (MUST)。wire 経路では decode 層が先に
        // 拒否するため、ここへ届くのは API 経由で手組みしたメッセージだけである。
        if let Some(value) = msg.parameters.paused()
            && let Err(err) = validate_paused(value)
        {
            self.fail(err.clone());
            return Err(err);
        }
        if let Some(order) = msg.parameters.group_order()
            && let Err(err) = validate_group_order(order)
        {
            self.fail(err.clone());
            return Err(err);
        }
        if let Some(value) = msg.parameters.include_properties()
            && let Err(err) = validate_include_properties(value)
        {
            self.fail(err.clone());
            return Err(err);
        }
        self.track_subscriptions.insert(
            request_id,
            TrackSubscription {
                request_id,
                my_role: TrackRole::Publisher,
                track_namespace: msg.track_namespace.clone(),
                parameters: msg.parameters.clone(),
                state: NamespaceRequestState::Pending,
                received_published: alloc::collections::BTreeSet::new(),
                received_skipped: alloc::collections::BTreeSet::new(),
                sent_published: alloc::collections::BTreeSet::new(),
                sent_skipped: alloc::collections::BTreeSet::new(),
            },
        );
        self.request_streams
            .insert(request_id, RequestKind::SubscribeTracks);
        self.events
            .push_back(SessionEvent::SubscribeTracksReceived {
                request_id,
                track_namespace: msg.track_namespace,
                parameters: msg.parameters,
            });
        Ok(())
    }

    // ─── 応答ハンドラ (requester 側) ────────────────────────

    /// PUBLISH_NAMESPACE の REQUEST_OK を反映する
    pub(crate) fn handle_ok_for_publish_namespace(
        &mut self,
        request_id: u64,
    ) -> Result<(), crate::session::types::SessionError> {
        let entry = self
            .publish_namespaces
            .get_mut(&request_id)
            .ok_or_else(|| {
                crate::session::types::SessionError::new(
                    SESSION_UNEXPECTED_MESSAGE,
                    "REQUEST_OK received for unknown PUBLISH_NAMESPACE",
                )
            })?;
        if entry.my_role != TrackRole::Publisher {
            return Err(crate::session::types::SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "REQUEST_OK for PUBLISH_NAMESPACE received by the responder side",
            ));
        }
        if entry.state != NamespaceRequestState::Pending {
            return Err(crate::session::types::SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "REQUEST_OK received for a PUBLISH_NAMESPACE that is not pending",
            ));
        }
        entry.state = NamespaceRequestState::Established;
        Ok(())
    }

    /// SUBSCRIBE_NAMESPACE の REQUEST_OK を反映する
    pub(crate) fn handle_ok_for_subscribe_namespace(
        &mut self,
        request_id: u64,
    ) -> Result<(), crate::session::types::SessionError> {
        let entry = self
            .namespace_subscriptions
            .get_mut(&request_id)
            .ok_or_else(|| {
                crate::session::types::SessionError::new(
                    SESSION_UNEXPECTED_MESSAGE,
                    "REQUEST_OK received for unknown SUBSCRIBE_NAMESPACE",
                )
            })?;
        if entry.my_role != TrackRole::Subscriber {
            return Err(crate::session::types::SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "REQUEST_OK for SUBSCRIBE_NAMESPACE received by the responder side",
            ));
        }
        // 初回応答 (Pending → Established) と REQUEST_UPDATE_OK (Established のまま) の
        // 両方を受け付ける。応答の context は self 側の state でしか区別できない。
        if entry.state == NamespaceRequestState::Terminated {
            return Err(crate::session::types::SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "REQUEST_OK received for a terminated SUBSCRIBE_NAMESPACE",
            ));
        }
        entry.state = NamespaceRequestState::Established;
        Ok(())
    }

    /// SUBSCRIBE_TRACKS の REQUEST_OK を反映する
    pub(crate) fn handle_ok_for_subscribe_tracks(
        &mut self,
        request_id: u64,
    ) -> Result<(), crate::session::types::SessionError> {
        let entry = self
            .track_subscriptions
            .get_mut(&request_id)
            .ok_or_else(|| {
                crate::session::types::SessionError::new(
                    SESSION_UNEXPECTED_MESSAGE,
                    "REQUEST_OK received for unknown SUBSCRIBE_TRACKS",
                )
            })?;
        if entry.my_role != TrackRole::Subscriber {
            return Err(crate::session::types::SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "REQUEST_OK for SUBSCRIBE_TRACKS received by the responder side",
            ));
        }
        // 初回応答と REQUEST_UPDATE_OK の両方を受け付ける
        if entry.state == NamespaceRequestState::Terminated {
            return Err(crate::session::types::SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "REQUEST_OK received for a terminated SUBSCRIBE_TRACKS",
            ));
        }
        entry.state = NamespaceRequestState::Established;
        Ok(())
    }

    /// PUBLISH_NAMESPACE の REQUEST_ERROR を反映する
    pub(crate) fn handle_err_for_publish_namespace(
        &mut self,
        request_id: u64,
    ) -> Result<(), crate::session::types::SessionError> {
        let entry = self
            .publish_namespaces
            .get_mut(&request_id)
            .ok_or_else(|| {
                crate::session::types::SessionError::new(
                    SESSION_UNEXPECTED_MESSAGE,
                    "REQUEST_ERROR received for unknown PUBLISH_NAMESPACE",
                )
            })?;
        entry.state = NamespaceRequestState::Terminated;
        Ok(())
    }

    /// SUBSCRIBE_NAMESPACE の REQUEST_ERROR を反映する
    pub(crate) fn handle_err_for_subscribe_namespace(
        &mut self,
        request_id: u64,
    ) -> Result<(), crate::session::types::SessionError> {
        let entry = self
            .namespace_subscriptions
            .get_mut(&request_id)
            .ok_or_else(|| {
                crate::session::types::SessionError::new(
                    SESSION_UNEXPECTED_MESSAGE,
                    "REQUEST_ERROR received for unknown SUBSCRIBE_NAMESPACE",
                )
            })?;
        entry.state = NamespaceRequestState::Terminated;
        Ok(())
    }

    /// SUBSCRIBE_TRACKS の REQUEST_ERROR を反映する
    pub(crate) fn handle_err_for_subscribe_tracks(
        &mut self,
        request_id: u64,
    ) -> Result<(), crate::session::types::SessionError> {
        let entry = self
            .track_subscriptions
            .get_mut(&request_id)
            .ok_or_else(|| {
                crate::session::types::SessionError::new(
                    SESSION_UNEXPECTED_MESSAGE,
                    "REQUEST_ERROR received for unknown SUBSCRIBE_TRACKS",
                )
            })?;
        entry.state = NamespaceRequestState::Terminated;
        Ok(())
    }

    // ─── 応答 stream 上のメッセージ (requester 側) ─────────

    /// SUBSCRIBE_NAMESPACE の response stream で NAMESPACE を受信した
    ///
    /// draft-ietf-moq-transport-23 §9.15 (NAMESPACE): 要求の prefix と受信した suffix を
    /// 結合した完全な namespace をアプリへ通知する。
    pub(crate) fn handle_peer_namespace(
        &mut self,
        request_id: u64,
        msg: Namespace,
    ) -> Result<(), crate::session::types::SessionError> {
        let prefix = {
            let entry = self
                .namespace_subscriptions
                .get(&request_id)
                .ok_or_else(|| {
                    crate::session::types::SessionError::new(
                        SESSION_UNEXPECTED_MESSAGE,
                        "NAMESPACE received for unknown SUBSCRIBE_NAMESPACE",
                    )
                })?;
            if entry.my_role != TrackRole::Subscriber {
                return Err(crate::session::types::SessionError::new(
                    SESSION_UNEXPECTED_MESSAGE,
                    "NAMESPACE received by the responder side",
                ));
            }
            if entry.state != NamespaceRequestState::Established {
                return Err(crate::session::types::SessionError::new(
                    SESSION_UNEXPECTED_MESSAGE,
                    "NAMESPACE received before SUBSCRIBE_NAMESPACE_OK",
                ));
            }
            entry.track_namespace.clone()
        };
        let full = combine_namespace(&prefix, &msg.track_namespace_suffix)?;
        let entry = self
            .namespace_subscriptions
            .get_mut(&request_id)
            .expect("checked above");
        entry.received_namespaces.insert(full.fields().to_vec());
        self.events.push_back(SessionEvent::NamespaceReceived {
            request_id,
            track_namespace: full,
            parameters: msg.parameters,
        });
        Ok(())
    }

    /// SUBSCRIBE_NAMESPACE の response stream で NAMESPACE_DONE を受信した
    ///
    /// draft-ietf-moq-transport-23 §4.2: "If a subscriber receives a NAMESPACE_DONE before
    /// the corresponding NAMESPACE, it MUST close the session with UNEXPECTED_MESSAGE."
    pub(crate) fn handle_peer_namespace_done(
        &mut self,
        request_id: u64,
        msg: NamespaceDone,
    ) -> Result<(), crate::session::types::SessionError> {
        let prefix = {
            let entry = self
                .namespace_subscriptions
                .get(&request_id)
                .ok_or_else(|| {
                    crate::session::types::SessionError::new(
                        SESSION_UNEXPECTED_MESSAGE,
                        "NAMESPACE_DONE received for unknown SUBSCRIBE_NAMESPACE",
                    )
                })?;
            if entry.my_role != TrackRole::Subscriber {
                return Err(crate::session::types::SessionError::new(
                    SESSION_UNEXPECTED_MESSAGE,
                    "NAMESPACE_DONE received by the responder side",
                ));
            }
            if entry.state != NamespaceRequestState::Established {
                return Err(crate::session::types::SessionError::new(
                    SESSION_UNEXPECTED_MESSAGE,
                    "NAMESPACE_DONE received before SUBSCRIBE_NAMESPACE_OK",
                ));
            }
            entry.track_namespace.clone()
        };
        let full = combine_namespace(&prefix, &msg.track_namespace_suffix)?;
        let entry = self
            .namespace_subscriptions
            .get_mut(&request_id)
            .expect("checked above");
        if !entry.received_namespaces.remove(full.fields()) {
            return Err(crate::session::types::SessionError::new(
                SESSION_UNEXPECTED_MESSAGE,
                "NAMESPACE_DONE received before the corresponding NAMESPACE",
            ));
        }
        self.events.push_back(SessionEvent::NamespaceDoneReceived {
            request_id,
            track_namespace: full,
        });
        Ok(())
    }

    /// SUBSCRIBE_TRACKS の response stream で PUBLISH_SKIPPED を受信した
    ///
    /// draft-ietf-moq-transport-23 §9.18 (PUBLISH_SKIPPED) / §3.6.3 (Skipped Tracks):
    /// 購読を作らなかった track をアプリへ通知する。同じ track の PUBLISH 受信済み、
    /// または PUBLISH_SKIPPED の重複は PROTOCOL_VIOLATION でセッションを閉じる。
    pub(crate) fn handle_peer_publish_skipped(
        &mut self,
        request_id: u64,
        msg: PublishSkipped,
    ) -> Result<(), crate::session::types::SessionError> {
        let prefix = {
            let entry = self.track_subscriptions.get(&request_id).ok_or_else(|| {
                crate::session::types::SessionError::new(
                    SESSION_UNEXPECTED_MESSAGE,
                    "PUBLISH_SKIPPED received for unknown SUBSCRIBE_TRACKS",
                )
            })?;
            if entry.my_role != TrackRole::Subscriber {
                return Err(crate::session::types::SessionError::new(
                    SESSION_UNEXPECTED_MESSAGE,
                    "PUBLISH_SKIPPED received by the responder side",
                ));
            }
            if entry.state != NamespaceRequestState::Established {
                return Err(crate::session::types::SessionError::new(
                    SESSION_UNEXPECTED_MESSAGE,
                    "PUBLISH_SKIPPED received before SUBSCRIBE_TRACKS_OK",
                ));
            }
            entry.track_namespace.clone()
        };
        let full = combine_namespace(&prefix, &msg.track_namespace_suffix)?;
        let key = (full.fields().to_vec(), msg.track_name.clone());
        let entry = self
            .track_subscriptions
            .get_mut(&request_id)
            .expect("checked above");
        // PUBLISH_SKIPPED を送った後に同じ track の PUBLISH を送るのは §3.6.3 (Skipped
        // Tracks) の MUST NOT に反する。PUBLISH を受けた後の PUBLISH_SKIPPED も同じ
        // 矛盾であり、PROTOCOL_VIOLATION でセッションを閉じる。
        if entry.received_published.contains(&key) {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "PUBLISH_SKIPPED conflicts with a received PUBLISH",
            ));
        }
        // 同じ track の PUBLISH_SKIPPED の重複は仕様違反ではない。状態は冪等に保ち、
        // 2 回目以降のイベントは発行しない。
        if !entry.received_skipped.insert(key) {
            return Ok(());
        }
        self.events.push_back(SessionEvent::PublishSkippedReceived {
            request_id,
            track_namespace: full,
            track_name: msg.track_name,
        });
        Ok(())
    }

    /// SUBSCRIBE_TRACKS の結果として届く PUBLISH を検証する
    ///
    /// draft-ietf-moq-transport-23 §3.6 (Subscribing to Tracks by Prefix) / §9.5.2
    /// (Updating Namespace Subscriptions) / §9.19.21 (SUBSCRIBE_TRACKS_ID Parameter):
    /// publisher は同じ SUBSCRIBE_TRACKS に対して同じ track の PUBLISH を 2 回送ってはならず、
    /// PUBLISH_SKIPPED を送った track の PUBLISH も送ってはならない。
    /// 対応する要求が見つからない場合は、従来どおり関連付け情報としてだけ扱う。
    pub(crate) fn validate_incoming_subscribe_tracks_publish(
        &mut self,
        parameters: &MessageParameters,
        track_namespace: &TrackNamespace,
        track_name: &[u8],
    ) -> Result<(), crate::session::types::SessionError> {
        let Some(request_id) = parameters.subscribe_tracks_id() else {
            return Ok(());
        };
        let Some(entry) = self.track_subscriptions.get_mut(&request_id) else {
            // 要求を回収済み、または要求を送っていない PUBLISH は受理する
            return Ok(());
        };
        if entry.my_role != TrackRole::Subscriber {
            return Ok(());
        }
        // 要求の prefix で始まらない namespace の PUBLISH は、その SUBSCRIBE_TRACKS の
        // 結果ではない (§3.6)
        let prefix = entry.track_namespace.fields();
        let fields = track_namespace.fields();
        if !fields.starts_with(prefix) {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "PUBLISH namespace does not match the SUBSCRIBE_TRACKS prefix",
            ));
        }
        let key = (fields.to_vec(), track_name.to_vec());
        if entry.received_skipped.contains(&key) {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "PUBLISH received after PUBLISH_SKIPPED for the same track",
            ));
        }
        if !entry.received_published.insert(key) {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "duplicate PUBLISH for the same SUBSCRIBE_TRACKS track",
            ));
        }
        Ok(())
    }

    /// SUBSCRIBE_TRACKS の結果として PUBLISH を送る前の検証と記録
    ///
    /// draft-ietf-moq-transport-23 §9.18 (PUBLISH_SKIPPED) / §9.5.2: PUBLISH_SKIPPED を送った
    /// track への PUBLISH と、同じ track の重複 PUBLISH を拒否する。SUBSCRIBE_TRACKS_ID が
    /// 対応する要求を指していない場合は、§9.19.21 の契約をアプリの責務として受理する
    /// (要求を回収済みの場合も含む)。
    pub(crate) fn record_outgoing_subscribe_tracks_publish(
        &mut self,
        parameters: &MessageParameters,
        track_namespace: &TrackNamespace,
        track_name: &[u8],
    ) -> Result<(), crate::session::types::SessionError> {
        let Some(request_id) = parameters.subscribe_tracks_id() else {
            return Ok(());
        };
        let Some(entry) = self.track_subscriptions.get(&request_id) else {
            return Ok(());
        };
        if entry.my_role != TrackRole::Publisher {
            return Ok(());
        }
        let prefix = entry.track_namespace.fields();
        let fields = track_namespace.fields();
        if !fields.starts_with(prefix) {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "PUBLISH namespace does not match the SUBSCRIBE_TRACKS prefix",
            ));
        }
        let key = (fields.to_vec(), track_name.to_vec());
        let entry = self
            .track_subscriptions
            .get_mut(&request_id)
            .expect("checked above");
        if entry.sent_skipped.contains(&key) {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "PUBLISH cannot follow PUBLISH_SKIPPED for the same track",
            ));
        }
        if !entry.sent_published.insert(key) {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "duplicate PUBLISH for the same SUBSCRIBE_TRACKS track",
            ));
        }
        Ok(())
    }

    // ─── 終端処理 ──────────────────────────────────────────

    /// PUBLISH_NAMESPACE の request stream が終端した
    pub(crate) fn close_publish_namespace_on_stream_end(
        &mut self,
        request_id: u64,
        end: RequestStreamEnd,
    ) -> Result<TerminationReason, crate::session::types::SessionError> {
        let entry = self
            .publish_namespaces
            .get_mut(&request_id)
            .ok_or_else(|| {
                crate::session::types::SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "request stream close for unknown PUBLISH_NAMESPACE",
                )
            })?;
        entry.state = NamespaceRequestState::Terminated;
        Ok(terminationreason_from_end(end))
    }

    /// SUBSCRIBE_NAMESPACE の request stream が終端した
    ///
    /// draft-ietf-moq-transport-23 §4.2.2 (Namespace Subscription State Management): 応答
    /// stream の reset / FIN を受けた subscriber は、広告中の各 namespace に NAMESPACE_DONE を
    /// 受けたものとして扱う (SHOULD)。広告済みの完全な namespace を
    /// [`SessionEvent::NamespaceDoneReceived`] としてアプリへ通知する。
    pub(crate) fn close_namespace_subscription_on_stream_end(
        &mut self,
        request_id: u64,
        end: RequestStreamEnd,
    ) -> Result<TerminationReason, crate::session::types::SessionError> {
        let ended = {
            let entry = self
                .namespace_subscriptions
                .get_mut(&request_id)
                .ok_or_else(|| {
                    crate::session::types::SessionError::new(
                        SESSION_PROTOCOL_VIOLATION,
                        "request stream close for unknown SUBSCRIBE_NAMESPACE",
                    )
                })?;
            entry.state = NamespaceRequestState::Terminated;
            if entry.my_role == TrackRole::Subscriber {
                core::mem::take(&mut entry.received_namespaces)
            } else {
                alloc::collections::BTreeSet::new()
            }
        };
        for fields in ended {
            let track_namespace = TrackNamespace::new(fields).map_err(|_| {
                crate::session::types::SessionError::new(
                    SESSION_INVALID_TRACK_NAMESPACE,
                    "advertised namespace does not form a valid Track Namespace",
                )
            })?;
            self.events.push_back(SessionEvent::NamespaceDoneReceived {
                request_id,
                track_namespace,
            });
        }
        Ok(terminationreason_from_end(end))
    }

    /// SUBSCRIBE_TRACKS の request stream が終端した
    pub(crate) fn close_track_subscription_on_stream_end(
        &mut self,
        request_id: u64,
        end: RequestStreamEnd,
    ) -> Result<TerminationReason, crate::session::types::SessionError> {
        let entry = self
            .track_subscriptions
            .get_mut(&request_id)
            .ok_or_else(|| {
                crate::session::types::SessionError::new(
                    SESSION_PROTOCOL_VIOLATION,
                    "request stream close for unknown SUBSCRIBE_TRACKS",
                )
            })?;
        entry.state = NamespaceRequestState::Terminated;
        Ok(terminationreason_from_end(end))
    }

    // ─── 参照と回収 ────────────────────────────────────────

    /// PUBLISH_NAMESPACE の状態を返す
    pub fn publish_namespace_request(&self, request_id: u64) -> Option<&PublishNamespaceRequest> {
        self.publish_namespaces.get(&request_id)
    }

    /// SUBSCRIBE_NAMESPACE の状態を返す
    pub fn namespace_subscription(&self, request_id: u64) -> Option<&NamespaceSubscription> {
        self.namespace_subscriptions.get(&request_id)
    }

    /// SUBSCRIBE_TRACKS の状態を返す
    pub fn track_subscription(&self, request_id: u64) -> Option<&TrackSubscription> {
        self.track_subscriptions.get(&request_id)
    }

    /// PUBLISH_NAMESPACE の状態を破棄する
    ///
    /// REQUEST_ERROR の送信後や、peer の stream 終端を観測した後にアプリが呼ぶ。
    pub fn forget_publish_namespace(
        &mut self,
        request_id: u64,
    ) -> Result<(), crate::session::types::SessionError> {
        if self.publish_namespaces.remove(&request_id).is_none() {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "PUBLISH_NAMESPACE not found for forget_publish_namespace",
            ));
        }
        self.request_streams.remove(&request_id);
        self.clear_control_message_deadline(request_id);
        Ok(())
    }

    /// SUBSCRIBE_NAMESPACE の状態を破棄する
    pub fn forget_namespace_subscription(
        &mut self,
        request_id: u64,
    ) -> Result<(), crate::session::types::SessionError> {
        if self.namespace_subscriptions.remove(&request_id).is_none() {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "SUBSCRIBE_NAMESPACE not found for forget_namespace_subscription",
            ));
        }
        self.request_streams.remove(&request_id);
        self.clear_control_message_deadline(request_id);
        Ok(())
    }

    /// SUBSCRIBE_TRACKS の状態を破棄する
    pub fn forget_track_subscription(
        &mut self,
        request_id: u64,
    ) -> Result<(), crate::session::types::SessionError> {
        if self.track_subscriptions.remove(&request_id).is_none() {
            return Err(crate::session::types::SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "SUBSCRIBE_TRACKS not found for forget_track_subscription",
            ));
        }
        self.request_streams.remove(&request_id);
        self.clear_control_message_deadline(request_id);
        Ok(())
    }
}
