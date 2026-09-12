//! GOAWAY / Migration / Tick 関連
//!
//! draft-ietf-moq-transport-21 §6.6 (Termination), §6.6.1 (Graceful Session Migration), §9.2 (GOAWAY) に対応する
//! `impl Session` のメソッドをまとめる。sans-I/O 制約のため、session 自体はタイマーを
//! 持たず、外部時計から渡される `tick(now_ms)` だけが時刻源となる。
//! draft 由来の実装のため将来変更される可能性がある。

use crate::error::{
    SESSION_CONTROL_MESSAGE_TIMEOUT, SESSION_DATA_STREAM_TIMEOUT, SESSION_GOAWAY_TIMEOUT,
    SESSION_PROTOCOL_VIOLATION, STREAM_GOING_AWAY,
};
use crate::message::{ControlMessage, Goaway};
use alloc::vec::Vec;

use super::core::Session;
use super::types::{
    DeadlineTimer, GoawayDrainSnapshot, MAX_NEW_SESSION_URI_LENGTH, PeerGoawayInfo, Role,
    SessionError, SessionEvent, SessionState,
};

/// GOAWAY URI の検証ヘルパー (送信側)
///
/// draft-ietf-moq-transport-21 §9.2 (GOAWAY) の URI 長制限と Client の非空 URI 制限を検証する。
fn validate_outgoing_goaway_uri(role: Role, uri: &[u8]) -> Result<(), SessionError> {
    if uri.len() > MAX_NEW_SESSION_URI_LENGTH {
        return Err(SessionError::new(
            SESSION_PROTOCOL_VIOLATION,
            "new_session_uri exceeds 8192 bytes",
        ));
    }
    if role == Role::Client && !uri.is_empty() {
        return Err(SessionError::new(
            SESSION_PROTOCOL_VIOLATION,
            "client MUST send zero-length new_session_uri",
        ));
    }
    Ok(())
}

/// GOAWAY URI の検証ヘルパー (受信側)
///
/// draft-ietf-moq-transport-21 §9.2 (GOAWAY) の URI 長制限と Client の非空 URI 制限を検証する。
/// 受信側では Role を反転して使う (自側 Server → peer Client)。
fn validate_incoming_goaway_uri(role: Role, uri: &[u8]) -> Result<(), SessionError> {
    if uri.len() > MAX_NEW_SESSION_URI_LENGTH {
        return Err(SessionError::new(
            SESSION_PROTOCOL_VIOLATION,
            "received GOAWAY new_session_uri exceeds 8192 bytes",
        ));
    }
    // draft-ietf-moq-transport-21 §9.2 (GOAWAY): server (自側) は client (peer) からの non-zero URI を拒否
    if role == Role::Server && !uri.is_empty() {
        return Err(SessionError::new(
            SESSION_PROTOCOL_VIOLATION,
            "client sent non-zero new_session_uri",
        ));
    }
    Ok(())
}

impl Session {
    // ─── クエリ API ─────────────────────────────────────────

    /// GOAWAY sender 側の drain blocker snapshot
    pub fn goaway_drain_snapshot(&self) -> GoawayDrainSnapshot {
        let mut snapshot = GoawayDrainSnapshot::default();
        snapshot
            .blocking_subscription_request_ids
            .extend(
                self.subscriptions
                    .iter()
                    .filter_map(|(&request_id, subscription)| {
                        (!subscription.cleanup_ready()).then_some(request_id)
                    }),
            );
        snapshot.blocking_fetch_request_ids.extend(
            self.fetches
                .keys()
                .copied()
                .filter(|request_id| self.fetch_cleanup_ready(*request_id) == Some(false)),
        );
        snapshot.blocking_track_subscription_request_ids.extend(
            self.track_subscriptions
                .iter()
                .filter_map(|(&request_id, ts)| {
                    use super::types::TrackSubscriptionState;
                    (ts.state != TrackSubscriptionState::Terminated).then_some(request_id)
                }),
        );
        snapshot.blocking_namespace_subscription_request_ids.extend(
            self.namespaces
                .subscriptions
                .iter()
                .filter_map(|(&request_id, ns)| {
                    use super::types::NamespaceSubscriptionState;
                    (ns.state != NamespaceSubscriptionState::Terminated).then_some(request_id)
                }),
        );
        snapshot.blocking_namespace_publication_request_ids.extend(
            self.namespaces
                .publications
                .iter()
                .filter_map(|(&request_id, np)| {
                    use super::types::NamespacePublicationState;
                    (np.state != NamespacePublicationState::Terminated).then_some(request_id)
                }),
        );
        snapshot.blocking_track_status_request_ids.extend(
            self.track_status_requests
                .iter()
                .filter_map(|(&request_id, ts)| ts.response.is_none().then_some(request_id)),
        );
        snapshot.blocking_subscription_request_ids.sort_unstable();
        snapshot.blocking_fetch_request_ids.sort_unstable();
        snapshot
            .blocking_track_subscription_request_ids
            .sort_unstable();
        snapshot
            .blocking_namespace_subscription_request_ids
            .sort_unstable();
        snapshot
            .blocking_namespace_publication_request_ids
            .sort_unstable();
        snapshot.blocking_track_status_request_ids.sort_unstable();
        snapshot
    }

    /// GOAWAY sender 側の drain が完了しているか
    pub fn goaway_drain_ready(&self) -> bool {
        self.goaway_drain_snapshot().ready()
    }

    // ─── 送信 API ──────────────────────────────────────────

    /// GOAWAY を送信する (draft-ietf-moq-transport-21 §9.2 (GOAWAY))
    ///
    /// 制約:
    /// - `Role::Client` は `new_session_uri` を空にする必要がある
    /// - `new_session_uri` の長さは 8192 バイト以下
    /// - 各エンドポイントから 1 回のみ
    ///
    /// draft-ietf-moq-transport-21 §6.6.1 (Graceful Session Migration): GOAWAY は新規セッションへの
    /// 移行合図 (drain) であり、Established subscription の自動終端は行わない。
    /// 移行する subscription への PUBLISH_DONE 送出はアプリケーションの責務であり、
    /// 送らずに GOAWAY_TIMEOUT の期限切れに任せる選択もある。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub fn send_goaway(
        &mut self,
        new_session_uri: Vec<u8>,
        timeout: u64,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        if self.goaway.local_sent {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "GOAWAY already sent",
            ));
        }
        validate_outgoing_goaway_uri(self.role, &new_session_uri)?;
        // draft-ietf-moq-transport-21 Appendix A.2 (Since draft-ietf-moq-transport-18) #1623:
        // GOAWAY から Request ID は削除された。Message Body は URI + Timeout のみ。
        let msg = ControlMessage::Goaway(Goaway {
            new_session_uri,
            timeout,
        });
        self.events.push_back(SessionEvent::SendControl(msg));
        self.goaway.local_sent = true;
        // draft-ietf-moq-transport-21 §9.2 (GOAWAY): timeout==0 は specific timeout なし。自動判定しない。
        if timeout > 0 {
            if let Some(now) = self.timing.last_tick_ms {
                self.goaway.local_deadline_ms = Some(now.saturating_add(timeout));
            } else {
                // tick 未経験。最初の tick で deadline を確定する
                self.goaway.local_pending_timeout_ms = Some(timeout);
            }
        }
        Ok(())
    }

    /// 外部時計からの時刻更新を受け取る (sans-I/O、session はタイマーを持たない)
    ///
    /// 呼び出し側は単調増加ミリ秒時刻を渡し、その時点で deadline 超過かつ
    /// drain blocker が残っている場合は
    /// session 内の `events` キューに `CloseSession` が積まれる。session 自体が
    /// 時刻取得やタイマー発火を行うことはない。
    ///
    /// また request stream 上の GOAWAY の timeout が満了した場合は、セッションを閉じずに
    /// 当該 request の `ResetRequestStream(STREAM_GOING_AWAY)` を積む。
    ///
    /// 既に Closing / Closed の場合は時刻の記録のみで何もしない。
    pub fn tick(&mut self, now_ms: u64) {
        self.timing.last_tick_ms = Some(now_ms);
        // 保留 timeout があればここで deadline を確定
        if self.goaway.local_deadline_ms.is_none()
            && let Some(t) = self.goaway.local_pending_timeout_ms.take()
        {
            self.goaway.local_deadline_ms = Some(now_ms.saturating_add(t));
        }
        if matches!(self.state, SessionState::Closing | SessionState::Closed) {
            return;
        }
        self.tick_subscription_timeouts(now_ms);
        // draft §3.1.2 (Track Alias): discard 用 tombstone の保持期間を進め、期限切れを片付ける
        self.tick_peer_alias_tombstones(now_ms);
        // draft §3.1.2 (Track Alias): 破棄対象 stream id の保持期間を進め、期限切れを片付ける
        // (alias tombstone とは独立に管理する。理由は `DataStreamState::discarded` の doc 参照)
        self.tick_discarded_data_stream_ids(now_ms);

        // 以下、3 種の session close timeout を固定順で評価する。
        // draft-ietf-moq-transport-21 §6.6 (Termination) は各エラーコードを定義するのみで、
        // 複数 timeout の優先順位を規定していないため、実装で
        // control → data_stream → goaway の順に固定する。
        // 同一 tick に複数の deadline が満了しても、評価順で決定的に 1 つのエラーコードが
        // 選ばれる。各 HashMap ループ内の反復順は出力に影響しない。
        // (将来の draft で変更される可能性がある)

        // draft-ietf-moq-transport-21 §6.6 (Termination) で定義される CONTROL_MESSAGE_TIMEOUT
        for deadline in self.timing.control_message_deadlines.values_mut() {
            deadline.tick(now_ms);
            if deadline.expired {
                self.fail(SessionError::new(
                    SESSION_CONTROL_MESSAGE_TIMEOUT,
                    "control message timeout expired",
                ));
                return;
            }
        }
        // draft-ietf-moq-transport-21 §6.6 (Termination) で定義される DATA_STREAM_TIMEOUT
        if let Some(timeout_ms) = self.timing.data_stream_timeout_ms {
            for last_activity_ms in self.timing.data_stream_last_activity_ms.values() {
                if now_ms.saturating_sub(*last_activity_ms) >= timeout_ms {
                    self.fail(SessionError::new(
                        SESSION_DATA_STREAM_TIMEOUT,
                        "data stream timeout expired",
                    ));
                    return;
                }
            }
        }
        // draft-ietf-moq-transport-21 §6.6 (Termination) で定義される GOAWAY_TIMEOUT
        // (発生条件は §6.6.1 (Graceful Session Migration) / §9.2 (GOAWAY) を参照)
        if let Some(deadline) = self.goaway.local_deadline_ms
            && now_ms >= deadline
            && !self.goaway_drain_ready()
        {
            self.fail(SessionError::new(
                SESSION_GOAWAY_TIMEOUT,
                "goaway timeout expired",
            ));
            return;
        }
        // draft-ietf-moq-transport-21 §9.2 (GOAWAY): "When sent on a request stream, the sender
        // SHOULD reset the stream with GOING_AWAY after the indicated timeout." セッションは
        // 閉じず、当該 request stream の reset イベントを 1 回だけ発行する。
        let mut expired = Vec::new();
        for (request_id, deadline) in self.goaway.request_stream_deadlines.iter_mut() {
            deadline.tick(now_ms);
            if deadline.expired {
                expired.push(*request_id);
            }
        }
        // 同一 tick で複数が期限到達してもイベント順を決定的にする
        expired.sort_unstable();
        for request_id in expired {
            self.goaway.request_stream_deadlines.remove(&request_id);
            // request stream を reset するとローカル送信方向も閉じるため、reset 後に flush されると
            // 矛盾する保留 PUBLISH_DONE を破棄する (PUBLISH_DONE の FIN より reset が先に確定した場合)
            if let Some(subscription) = self.subscriptions.get_mut(&request_id) {
                subscription.pending_publish_done = None;
            }
            self.events.push_back(SessionEvent::ResetRequestStream {
                request_id,
                error_code: STREAM_GOING_AWAY,
            });
        }
    }

    /// request stream 上の GOAWAY の reset deadline を解除する
    ///
    /// peer の FIN / RESET_STREAM 受信、`forget_*`、ローカル送信方向を FIN または RESET_STREAM で
    /// 閉じた後に呼ぶ。以後 reset を送る意味がなくなるため deadline を破棄する。
    pub(super) fn clear_request_stream_goaway_deadline(&mut self, request_id: u64) {
        self.goaway.request_stream_deadlines.remove(&request_id);
    }

    /// リクエストストリーム上で GOAWAY を送信する (draft-ietf-moq-transport-21 §9.2 (GOAWAY))
    ///
    /// 個別リクエストストリームに GOAWAY を送信し、そのリクエストのマイグレーションを開始する。
    /// `timeout > 0` の場合は request 単位の reset deadline を設定し、期限到達時に
    /// `ResetRequestStream(STREAM_GOING_AWAY)` を 1 回だけ発行する (control stream の GOAWAY と
    /// 異なりセッションは閉じない)。timeout == 0 は deadline を設定しない。
    /// peer の stream 終端やローカル FIN / RESET_STREAM で request が閉じた場合は解除され、
    /// reset は発行されない。
    ///
    /// 制約:
    /// - `Role::Client` は `new_session_uri` を空にする必要がある
    /// - `new_session_uri` の長さは 8192 バイト以下
    /// - 各リクエストストリームから 1 回のみ
    pub fn send_goaway_on_request_stream(
        &mut self,
        request_id: u64,
        new_session_uri: Vec<u8>,
        timeout: u64,
    ) -> Result<(), SessionError> {
        self.require_established()?;
        if self.goaway.request_stream_sent.contains(&request_id) {
            return Err(SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "GOAWAY already sent on this request stream",
            ));
        }
        validate_outgoing_goaway_uri(self.role, &new_session_uri)?;
        let msg = ControlMessage::Goaway(Goaway {
            new_session_uri,
            timeout,
        });
        self.events.push_back(SessionEvent::SendOnStream {
            request_id,
            message: msg,
            fin: false,
        });
        self.goaway.request_stream_sent.insert(request_id);
        // draft-ietf-moq-transport-21 §9.2 (GOAWAY): "When sent on a request stream, the sender
        // SHOULD reset the stream with GOING_AWAY after the indicated timeout. A value of 0
        // indicates the sender has no specific timeout, but the recipient SHOULD migrate as
        // quickly as possible." timeout == 0 は deadline を登録しない。
        if timeout > 0 {
            self.goaway.request_stream_deadlines.insert(
                request_id,
                DeadlineTimer::new(timeout, self.timing.last_tick_ms),
            );
        }
        Ok(())
    }

    // ─── 受信ハンドラ ──────────────────────────────────────

    pub(super) fn handle_peer_goaway(&mut self, msg: Goaway) -> Result<(), SessionError> {
        if self.state != SessionState::Established {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "GOAWAY received before session established",
            );
            self.fail(err.clone());
            return Err(err);
        }
        if self.goaway.peer.is_some() {
            let err = SessionError::new(SESSION_PROTOCOL_VIOLATION, "duplicate GOAWAY received");
            self.fail(err.clone());
            return Err(err);
        }
        if let Err(err) = validate_incoming_goaway_uri(self.role, &msg.new_session_uri) {
            self.fail(err.clone());
            return Err(err);
        }
        self.goaway.peer = Some(PeerGoawayInfo {
            new_session_uri: msg.new_session_uri.clone(),
            timeout: msg.timeout,
        });
        self.events.push_back(SessionEvent::GoawayReceived {
            new_session_uri: msg.new_session_uri,
            timeout: msg.timeout,
            on_request_stream: None,
        });
        Ok(())
    }

    /// リクエストストリーム上で GOAWAY を受信する (draft-ietf-moq-transport-21 §9.2 (GOAWAY))
    ///
    /// 個別リクエストストリーム上の GOAWAY はそのリクエストのマイグレーションを開始する。
    /// control stream の GOAWAY と異なり session 全体には影響しない。
    pub(super) fn handle_peer_goaway_on_request_stream(
        &mut self,
        request_id: u64,
        msg: Goaway,
    ) -> Result<(), SessionError> {
        if self.state != SessionState::Established {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "GOAWAY received before session established",
            );
            self.fail(err.clone());
            return Err(err);
        }
        if self.goaway.request_stream_received.contains(&request_id) {
            let err = SessionError::new(
                SESSION_PROTOCOL_VIOLATION,
                "duplicate GOAWAY received on request stream",
            );
            self.fail(err.clone());
            return Err(err);
        }
        if let Err(err) = validate_incoming_goaway_uri(self.role, &msg.new_session_uri) {
            self.fail(err.clone());
            return Err(err);
        }
        self.goaway.request_stream_received.insert(request_id);
        self.events.push_back(SessionEvent::GoawayReceived {
            new_session_uri: msg.new_session_uri,
            timeout: msg.timeout,
            on_request_stream: Some(request_id),
        });
        Ok(())
    }
}
