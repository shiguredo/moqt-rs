//! WebTransport クライアント実装
//!
//! s2n-quic + shiguredo_http3 (Sans I/O) を使い WebTransport セッションを確立する。
//! 節番号は draft-ietf-webtrans-http3-16 を参照する。依存する shiguredo_http3 が
//! 対応する wire バージョンは draft-15 相当 (`:protocol` 値は共通)。
//!
//! publisher (datagram 送信) と subscriber (datagram 受信 + 受信ストリーム accept) の
//! 双方が利用する union API を提供する。
//!
//! 本モジュールはクライアント例であり、サーバ側専用の要件 (非対応リソースへの 405 応答など)
//! や、未実装の任意要件 (応答前の optimistic capsule 送信) は対象外とする。
//! 対象外の詳細は `WtClient::connect` / `WtSession::close` のコメントを参照。

use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};

use bytes::Bytes;
use s2n_quic::client::Connect;
use s2n_quic::stream::{ReceiveStream, SendStream};
use shiguredo_http3::event::WebTransportEvent;
use shiguredo_http3::webtransport::ApplicationErrorCode;
use shiguredo_http3::webtransport::ErrorCode as WtErrorCode;
use shiguredo_http3::webtransport::capsule::Capsule;
use shiguredo_http3::webtransport::connect::ConnectRequest;
use shiguredo_http3::webtransport::stream::{
    ClassifiedUniStream, StreamHeader, StreamHeaderDecodeError, classify_uni_stream_checked,
};
use shiguredo_http3::{ClientConnection, ErrorCode as H3ErrorCode, Event, Settings as H3Settings};
use shiguredo_moqt::session::types::RequestStreamEnd;
use tokio::sync::Notify;
use tokio::sync::mpsc;
use tokio::sync::watch;

use crate::error::{Result, TransportError};

/// WebTransport over HTTP/3 の ALPN プロトコル識別子 (RFC 9114 §3.1)
///
/// s2n-quic の default TLS builder も既定で `h3` を設定するため、証明書検証を
/// スキップする開発用経路でも同じ値を明示する。
const H3_ALPN: &[u8] = b"h3";

// ---------------------------------------------------------------------------
// クライアント設定
// ---------------------------------------------------------------------------

/// WebTransport クライアント設定
pub struct ClientConfig {
    pub remote_addr: SocketAddr,
    /// SNI / 証明書検証に使うサーバー名 (ポートを含まない)
    pub server_name: String,
    /// WebTransport CONNECT の `:authority` に使う値 (URL の authority)
    ///
    /// URL でポートを省略した場合はポートを含まない。
    pub authority: String,
    pub ca_cert_pem: Option<String>,
    pub disable_cert_validation: bool,
    pub h3_settings: H3Settings,
    /// datagram 継続受信のバックグラウンドタスクを起動するか
    ///
    /// subscriber (datagram 受信側) のみ `true` にする。publisher は datagram を
    /// 送信するだけで受信しないため `false` のままとし、未消費の datagram が
    /// HTTP/3 接続状態に蓄積し続けることや、不要なポーリングを避ける。
    pub receive_datagrams: bool,
}

impl ClientConfig {
    pub fn new(remote_addr: SocketAddr, server_name: impl Into<String>) -> Self {
        let server_name = server_name.into();
        Self {
            remote_addr,
            // 既定では SNI と同じ値を `:authority` に使う (ポート無し)
            authority: server_name.clone(),
            server_name,
            ca_cert_pem: None,
            disable_cert_validation: false,
            h3_settings: H3Settings::default(),
            receive_datagrams: false,
        }
    }

    /// WebTransport CONNECT の `:authority` を設定する
    ///
    /// draft-ietf-webtrans-http3-16 §3.2 は拡張 CONNECT で `:authority` と `:path` を
    /// 設定する MUST を定める。SNI 用の `server_name` とは別に、target URI の authority を
    /// URL の表記どおり (ポートの有無も含めて) 指定するために使う。
    pub fn authority(mut self, authority: impl Into<String>) -> Self {
        self.authority = authority.into();
        self
    }

    pub fn ca_cert(mut self, pem: impl Into<String>) -> Self {
        self.ca_cert_pem = Some(pem.into());
        self
    }

    /// 証明書検証を無効化する (開発用)
    ///
    /// 呼び出し側は無効化の旨をログ等で明示すること。
    pub fn insecure(mut self) -> Self {
        self.disable_cert_validation = true;
        self
    }

    pub fn enable_webtransport(mut self, wt: shiguredo_http3::webtransport::Settings) -> Self {
        self.h3_settings = self.h3_settings.enable_webtransport_client(wt);
        self
    }

    /// datagram 継続受信タスクを有効化する (subscriber 側で使用)
    pub fn receive_datagrams(mut self) -> Self {
        self.receive_datagrams = true;
        self
    }
}

// ---------------------------------------------------------------------------
// HTTP/3 接続状態 (Sans I/O ラッパー)
// ---------------------------------------------------------------------------

/// h3 層へ入力を流した結果と、その結果発行されたイベント
///
/// h3 層は入力を処理する途中でイベントをキューへ積んでからエラーを返すことがある
/// (CONNECT stream に `WT_CLOSE_SESSION` の DATA と、終了後の追加 DATA が同じチャンクで
/// 届くと、`SessionClosed` を積んだ後に H3_MESSAGE_ERROR を返す)。
/// そのためエラーとイベントを別々に運び、エラーでもイベントを処理できるようにする。
struct H3FeedOutcome {
    /// h3 層へ入力を流した結果
    result: Result<()>,
    /// h3 層が発行したイベント (drain に失敗した場合は空)
    events: Vec<Event>,
}

struct ClientConnectionState {
    h3_conn: ClientConnection,
}

impl ClientConnectionState {
    fn new(settings: H3Settings) -> Self {
        Self {
            h3_conn: ClientConnection::new(settings),
        }
    }

    /// h3 層へストリームデータを流し、流した結果と drain したイベントを返す
    ///
    /// feed がエラーでも drain する (理由は [`H3FeedOutcome`] を参照)。
    fn feed_stream_and_drain(&mut self, id: u64, data: &[u8], fin: bool) -> H3FeedOutcome {
        let result = self
            .h3_conn
            .feed_stream(id, data, fin)
            .map_err(TransportError::from);
        self.drain_after(result)
    }

    /// QUIC からストリームの RESET_STREAM を受けたことを h3 層へ伝える
    ///
    /// h3 層は Sans I/O のため、I/O 層が通知しないと RESET_STREAM を処理できない。
    /// CONNECT stream の RESET_STREAM はセッション終了であり (draft-ietf-webtrans-http3-16 §6)、
    /// h3 層は `WebTransportEvent::SessionClosed` を発火する。
    /// `stream_reset` の `final_size` (RFC 9000 §19.4 の Final Size) は、s2n-quic の
    /// ストリームエラーが値を運ばないため常に 0 を渡す (セッション終了の判定には使われない)。
    fn process_stream_reset(&mut self, id: u64, error_code: u64) -> H3FeedOutcome {
        let result = self
            .h3_conn
            .stream_reset(id, error_code, 0)
            .map_err(TransportError::from);
        self.drain_after(result)
    }

    /// h3 層へ入力を流した結果を保ったままイベントを取り出す
    ///
    /// 入力がエラーでもイベントは取り出す。drain 自体がエラーになった場合は
    /// イベントを取得できないため、原因をログに残して空を返す (呼び出し側の分岐は
    /// 入力の成否だけを見る)。
    fn drain_after(&mut self, result: Result<()>) -> H3FeedOutcome {
        match self.drain_events() {
            Ok(events) => H3FeedOutcome { result, events },
            Err(e) => {
                tracing::warn!("Failed to drain HTTP/3 events: {e}");
                H3FeedOutcome {
                    result,
                    events: Vec::new(),
                }
            }
        }
    }

    fn drain_events(&mut self) -> Result<Vec<Event>> {
        Ok(self.h3_conn.drain_events()?)
    }

    /// QUIC DATAGRAM のペイロードを h3 層へ流し、流した結果と drain したイベントを返す
    ///
    /// 扱いは `feed_stream_and_drain` と同じである。
    fn feed_datagram(&mut self, data: &[u8]) -> H3FeedOutcome {
        let result = self
            .h3_conn
            .feed_datagram(data)
            .map_err(TransportError::from);
        self.drain_after(result)
    }
}

// ---------------------------------------------------------------------------
// セッション状態
// ---------------------------------------------------------------------------

/// WebTransport セッションの状態 (draft-ietf-webtrans-http3-16 §6 / §4.7)
///
/// `tokio::sync::watch` で各タスクへ配る。ストリームを所有するタスクはこの値の変化を
/// 観測し、終了を検知したら自分のストリームを `WT_SESSION_GONE` で中断する (§6 の MUST)。
/// 状態は `Active` から終了方向にしか進まない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WtSessionState {
    /// セッションが確立し、通常どおり使える
    Active,
    /// peer から drain (WT_DRAIN_SESSION / HTTP/3 GOAWAY) の通知を受けた (§4.7)
    Draining,
    /// peer からの終了通知 (CONNECT stream の close / WT_CLOSE_SESSION) を検知した (§6)
    ClosedByPeer,
    /// 自側が WT_CLOSE_SESSION を送ってセッションを終了した (§6)
    ClosedLocally,
}

impl WtSessionState {
    /// 状態の進み具合。状態はこの値が増える方向にしか遷移しない
    ///
    /// セッションが終了したかどうかの判断は [`session_policy`] の `abort_streams` に
    /// 一本化しており、この型は状態の表現だけを持つ (述語は持たない)。
    fn stage(self) -> u8 {
        match self {
            Self::Active => 0,
            Self::Draining => 1,
            Self::ClosedByPeer | Self::ClosedLocally => 2,
        }
    }
}

/// セッション状態から決まる送信・中断の動作
///
/// `reject_new_streams` と `reject_datagrams` は現状すべての状態で同じ値になる
/// (drain と終了のいずれでも新規ストリームの open と datagram の送信を拒否する)。
/// それでも分けているのは、拒否の根拠が §6 と §4.7 で別の MUST NOT だからである。
/// §6 はセッション終了の検知後に "MUST NOT send any new datagrams or open any new streams" を
/// 定め、§4.7 は drain (GOAWAY / `WT_DRAIN_SESSION`) の後もセッションの利用を MAY としつつ、
/// 本 example は「できるだけ早く終了する」合図として新規の作業を始めない方針を取る。
/// 将来 drain の間だけ datagram を許可する判断があり得るため、判定を 1 つに畳まない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SessionPolicy {
    /// 新しいストリームの open を拒否する
    reject_new_streams: bool,
    /// 新しい datagram の送信を拒否する
    reject_datagrams: bool,
    /// 自セッションの既存ストリームを中断する
    abort_streams: bool,
}

/// セッション状態から動作を決める純関数
///
/// draft-ietf-webtrans-http3-16 §6 (Session Termination) は、セッション終了を検知したら
/// 関連する全 uni / bidi ストリームを `WT_SESSION_GONE` で中断する MUST と、新しい datagram の
/// 送信・新しいストリームの open を禁じる MUST NOT を定める。
///
/// §4.7 (HTTP/3 GOAWAY / WT_DRAIN_SESSION) は終了の開始の合図であり、通知後も
/// "an endpoint MAY continue using the session" である。drain を終了と同一視すると
/// 既存ストリームを不必要に中断してこの MAY を潰すため、drain ではストリームを中断せず、
/// 新規ストリームの open と datagram の送信だけを拒否する。
///
/// セッションが終了したかどうかの判断は `abort_streams` に一本化する。`WtSessionState` は
/// 状態の表現だけを持ち、終了を表す述語を別に持たない (二重表現を作らない)。
fn session_policy(state: WtSessionState) -> SessionPolicy {
    match state {
        WtSessionState::Active => SessionPolicy {
            reject_new_streams: false,
            reject_datagrams: false,
            abort_streams: false,
        },
        WtSessionState::Draining => SessionPolicy {
            reject_new_streams: true,
            reject_datagrams: true,
            abort_streams: false,
        },
        WtSessionState::ClosedByPeer | WtSessionState::ClosedLocally => SessionPolicy {
            reject_new_streams: true,
            reject_datagrams: true,
            abort_streams: true,
        },
    }
}

/// セッション状態を進める
///
/// 状態は `Active` → `Draining` → 終了 の順にしか進まない。終了後に遅れて届いた drain の
/// 通知で状態を戻すと中断済みストリームの扱いが変わるため無視する。同じ状態の再通知でも
/// watch の値を変えない (`send_if_modified` は変更があるときだけ通知する)。
fn update_session_state(session_state: &watch::Sender<WtSessionState>, next: WtSessionState) {
    session_state.send_if_modified(|current| {
        if next.stage() <= current.stage() {
            return false;
        }
        *current = next;
        true
    });
}

/// セッションの終了を待つ
///
/// I/O を待つ `tokio::select!` の分岐として使う。終了かどうかの判断は純関数
/// `session_policy` の `abort_streams` に一本化しており、production でストリームを
/// 中断するかどうかはこの関数の待機が起点になる。drain (§4.7) では中断しないため返らず、
/// 状態が変わるたびに `session_policy` を評価し直す。
///
/// `watch::Receiver::wait_for` は待機に入るときに現在値で述語を評価するため、呼び出し時点で
/// 既に終了していれば待たずに返る。戻り値は待機が終わったことだけを表し、観測した状態は
/// 呼び出し側が `borrow()` で読む (`ClosedByPeer` と `ClosedLocally` を区別できる)。
///
/// 状態を配る sender が drop された場合も `wait_for` が `Err` を返すため、状態を観測できない
/// ものとして待ち続けずに終了として扱う (防御)。この経路は production では通常到達しない。
/// sender の clone を保持するのは `WtSession::session_state`、ストリームのルーティングタスクが
/// 持つ `RouteContext::session_state`、CONNECT stream の受信タスク、datagram 受信タスク
/// (`ClientConfig::receive_datagrams` が true のときだけ起動する) である。
pub(crate) async fn wait_until_terminated(session_state: &mut watch::Receiver<WtSessionState>) {
    let _ = session_state
        .wait_for(|state| session_policy(*state).abort_streams)
        .await;
}

/// h3 層のイベント列をセッション状態へ反映し、受信した datagram の payload を返す
///
/// h3 層のイベントを扱う唯一の経路である。`WebTransportEvent::Datagram` 以外のイベントを
/// 捨てると `SessionClosed` / `SessionDraining` を取りこぼし、セッションが終了しても MOQT 層の
/// 受信ループが待ち続ける。h3 層へ入力を流すすべての経路 (セッション確立時の CONNECT
/// レスポンス待ち / CONNECT stream の受信タスク / 制御・QPACK ストリームの
/// ルーティングタスク / datagram 受信タスク / `take_buffered_datagrams`) がこの関数を通す。
///
/// 本 example は 1 接続 1 セッションであり、イベントの `session_id` は照合しない
/// (h3 層は自セッションに関わるイベントだけを発火する)。`decide_route` の session ID 検証は
/// どのストリームを MOQT 層へ渡すかの判定であり、ここでの照合とは役割が異なる
/// (両方で照合すると同じ前提を二重に管理することになる)。
///
/// datagram の扱いは呼び出し元ごとに異なる (MOQT 層へ渡す / s2n-quic の受信バッファを空に
/// するために捨てる) ため、戻り値として返して状態の反映だけをここに集約する。
fn process_h3_events(
    events: Vec<Event>,
    session_state: &watch::Sender<WtSessionState>,
) -> Vec<Vec<u8>> {
    let mut datagrams = Vec::new();
    for event in events {
        match event {
            Event::WebTransport(WebTransportEvent::Datagram { payload, .. }) => {
                datagrams.push(payload);
            }
            Event::WebTransport(WebTransportEvent::SessionClosed {
                session_id,
                error_code,
                close_error_code,
                close_message,
                ..
            }) => {
                // draft-ietf-webtrans-http3-16 §6: CONNECT stream の close (FIN / RESET_STREAM)
                // または WT_CLOSE_SESSION の送受信でセッションが終了する。
                //
                // `reset_streams` は h3 層が把握しているストリームしか含まず、example が h3 層へ
                // feed していないストリーム (自側が開いた送信ストリーム) は含まれない。そのため
                // 中断対象の台帳には使わず、状態を配って各ストリームを所有するタスクに自分の
                // ストリームを中断させる (`register_local_wt_stream` による登録も行わない)。
                tracing::info!(
                    "WebTransport session closed: session_id={session_id}, error_code={error_code:#x}, close_error_code={close_error_code:#x}, close_message={close_message:?}"
                );
                update_session_state(session_state, WtSessionState::ClosedByPeer);
            }
            Event::WebTransport(WebTransportEvent::SessionDraining { session_id }) => {
                // draft-ietf-webtrans-http3-16 §4.7: drain はセッション終了の開始の合図であり、
                // 受信後もセッションを使い続けてよい (MAY)。既存ストリームは中断しない。
                // h3 層は GOAWAY の goaway_id で影響するセッションにだけ SessionDraining を
                // 発火するため、`Event::GoawayReceived` は使わない。
                tracing::info!("WebTransport session draining: session_id={session_id}");
                update_session_state(session_state, WtSessionState::Draining);
            }
            other => {
                tracing::debug!("HTTP/3 event: {other:?}");
            }
        }
    }
    datagrams
}

/// h3 層へ入力を流した結果をセッション状態へ反映し、流した結果を返す
///
/// h3 層へ入力を流す経路 (セッション確立時の CONNECT レスポンス待ち / CONNECT stream の
/// 受信タスク / 制御・QPACK ストリームのルーティングタスク / datagram 受信タスク) が
/// 共通で使う。feed がエラーでも drain したイベントは必ず `process_h3_events` へ渡す
/// (取りこぼすとセッション終了を検知できず、MOQT 層の受信ループが待ち続ける)。
/// 戻り値の datagram payload はこの経路では使わないため捨てる。
fn process_h3_outcome(
    outcome: H3FeedOutcome,
    session_state: &watch::Sender<WtSessionState>,
) -> Result<()> {
    let _ = process_h3_events(outcome.events, session_state);
    outcome.result
}

/// h3 層へストリームデータを流し、生じたイベントをセッション状態へ反映する
///
/// セッション確立後に h3 層へストリームデータを流す経路 (CONNECT stream の受信タスク /
/// 制御・QPACK ストリームのルーティングタスク) はこの関数を通る。確立前の CONNECT
/// レスポンス待ちループだけは例外で、`feed_stream_and_drain` をロック保持の中で直接呼ぶ
/// (ルーティングタスクはそのロックを取得できないため、確立ループが自分で feed と drain を
/// 行えば CONNECT レスポンスのイベントを取りこぼさない)。
///
/// feed と drain は同じロック保持の中で行うため、あるタスクが feed したイベントを別の
/// タスクが横取りすることはない。`wait_for_peer_settings` は `drain_events` ではなく
/// `peer_settings()` を直接見るため、ルーティングタスクが制御ストリームのイベントを
/// drain しても SETTINGS の到着判定は影響を受けない。
///
/// QPACK のブロック解除 (`retry_blocked_streams`) は drain の中で行われるため、drain する
/// タスクによっては別ストリーム (CONNECT レスポンス) のイベントがここで発火し得る。
/// 本 example は `SETTINGS_QPACK_MAX_TABLE_CAPACITY` を広告しない (既定の
/// `Settings::default()` は `None` = 0) ため peer は動的テーブルを参照できず、ブロック解除
/// 由来のイベントは発生しない。この広告を有効にする場合は、確立前の drain をセッション
/// 確立ループだけが行うように見直すこと。
fn feed_stream_to_h3(
    state: &Arc<StdMutex<ClientConnectionState>>,
    session_state: &watch::Sender<WtSessionState>,
    stream_id: u64,
    data: &[u8],
    fin: bool,
) -> Result<()> {
    let outcome = {
        let mut s = state
            .lock()
            .expect("connection state mutex must not be poisoned");
        s.feed_stream_and_drain(stream_id, data, fin)
    };
    process_h3_outcome(outcome, session_state)
}

// ---------------------------------------------------------------------------
// CONNECT レスポンス判定
// ---------------------------------------------------------------------------

/// CONNECT レスポンスのイベント列を処理した結果
#[derive(Debug, Clone, Copy)]
enum ConnectOutcome {
    /// HeadersEnd 未受信。判定はまだ確定していない
    Pending,
    /// 2xx を受信しセッション確立
    Established,
    /// HeadersEnd を受信したが :status が 2xx でない、または :status 不在
    Failed { status: Option<u16> },
}

/// WT_CLOSE_SESSION の Application Error Message 最大長 (バイト)
///
/// draft-ietf-webtrans-http3-16 §6: length MUST NOT exceed 1024 bytes。
/// 将来 draft 改定で上限が変わる可能性がある。
const CLOSE_SESSION_MESSAGE_MAX_BYTES: usize = 1024;

/// Application Error Message を UTF-8 境界で最大長に切り詰める
///
/// draft-ietf-webtrans-http3-16 §6:
/// Senders that truncate an application-supplied message MUST do so at a UTF-8 character boundary.
fn truncate_close_session_message(message: &str) -> &str {
    if message.len() <= CLOSE_SESSION_MESSAGE_MAX_BYTES {
        return message;
    }
    let mut end = CLOSE_SESSION_MESSAGE_MAX_BYTES;
    while end > 0 && !message.is_char_boundary(end) {
        end -= 1;
    }
    &message[..end]
}

/// CONNECT レスポンスの `:status` 擬似ヘッダー (ASCII 数字列) を u16 にパースする
fn parse_status(value: &[u8]) -> Option<u16> {
    std::str::from_utf8(value).ok()?.parse().ok()
}

/// CONNECT レスポンスのイベント列を走査する
///
/// `:status` ヘッダーを受信したら `status` に蓄積し (HeadersEnd と別バッチで届く場合に備える)、
/// HeadersEnd を受信した時点で蓄積した status が 2xx かどうかで結果を確定する。
/// draft-ietf-webtrans-http3-16 §3.2: クライアントは 2xx 応答受信時のみセッション確立とみなす。
fn connect_outcome(events: &[Event], status: &mut Option<u16>) -> ConnectOutcome {
    for event in events {
        match event {
            Event::Header { name, value, .. } if name == b":status" => {
                *status = parse_status(value);
            }
            Event::HeadersEnd { .. } => {
                return match *status {
                    Some(s) if (200..=299).contains(&s) => ConnectOutcome::Established,
                    other => ConnectOutcome::Failed { status: other },
                };
            }
            _ => {}
        }
    }
    ConnectOutcome::Pending
}

// ---------------------------------------------------------------------------
// WebTransport クライアント
// ---------------------------------------------------------------------------

/// WebTransport セッションを確立するクライアント
pub struct WtClient;

impl WtClient {
    /// WebTransport セッションを確立する
    pub async fn connect(config: ClientConfig, path: &str) -> Result<WtSession> {
        let datagram_endpoint = s2n_quic::provider::datagram::default::Endpoint::builder()
            .with_recv_capacity(64)
            .map_err(|e| TransportError::Internal(format!("datagram endpoint: {e}")))?
            .build()
            .expect("datagram endpoint build must succeed after recv capacity is set");

        let client = if config.disable_cert_validation {
            // 開発用: 証明書検証をスキップする。WebTransport over HTTP/3 の ALPN は
            // `h3` (RFC 9114 §3.1) を使う。s2n-quic の default TLS は feature
            // `provider-tls-rustls` で rustls に解決されるため、cert_store が空の
            // builder では `missing trusted root certificate(s)` で必ず失敗する。
            let tls = crate::quic::build_insecure_tls_client(&[H3_ALPN])?;
            s2n_quic::Client::builder()
                .with_tls(tls)
                .map_err(TransportError::transport)?
                .with_io(crate::local_bind_addr(config.remote_addr))
                .map_err(TransportError::transport)?
                .with_datagram(datagram_endpoint)
                .map_err(TransportError::transport)?
                .start()
                .map_err(TransportError::transport)?
        } else if let Some(ref ca_pem) = config.ca_cert_pem {
            s2n_quic::Client::builder()
                .with_tls(ca_pem.as_str())
                .map_err(TransportError::transport)?
                .with_io(crate::local_bind_addr(config.remote_addr))
                .map_err(TransportError::transport)?
                .with_datagram(datagram_endpoint)
                .map_err(TransportError::transport)?
                .start()
                .map_err(TransportError::transport)?
        } else {
            return Err(TransportError::InvalidState(
                "ca_cert_pem or disable_cert_validation is required".to_string(),
            ));
        };

        let connect = Connect::new(config.remote_addr).with_server_name(&*config.server_name);
        let connection = client.connect(connect).await?;

        let state = Arc::new(StdMutex::new(ClientConnectionState::new(
            config.h3_settings,
        )));

        // QUIC transport parameter レベルの前提条件を注入する
        //
        // draft-ietf-webtrans-http3-16 Section 3.1 / 4.6 は WebTransport CONNECT の前に
        // ピアの transport parameter (max_datagram_frame_size > 0 と RESET_STREAM_AT 対応) を
        // 検証することを求める。I/O 層がその結果を注入しないと CONNECT は拒否される。
        //
        // - max_datagram_frame_size: 本 example は s2n-quic の datagram provider を有効にして
        //   接続しており、ピアが DATAGRAM (RFC 9221) を広告しない場合は以降の WebTransport
        //   datagram 送受信が成立しない。s2n-quic の公開 API にピアの
        //   max_datagram_frame_size を取得する手段が無いため、provider を有効にしていることを
        //   根拠に true を渡す (moqt-example-transport は常に datagram を有効にして接続する)。
        // - reset_stream_at: s2n-quic は RESET_STREAM_AT を送出しないため false を渡す。
        //   本 example が接続する draft (draft-15 相当) では必須ではない。
        {
            let mut s = state
                .lock()
                .expect("connection state mutex must not be poisoned");
            s.h3_conn
                .set_webtransport_transport_verified(true, false)
                .map_err(|e| {
                    TransportError::Internal(format!(
                        "failed to set WebTransport transport parameters: {e}"
                    ))
                })?;
        }

        // 接続を分割し、単方向ストリームの受信タスクを先に起動する
        //
        // draft-ietf-webtrans-http3-16 Section 3.1 / 4.6 は WebTransport CONNECT を
        // 送る前に peer の SETTINGS (wt_enabled / enable_connect_protocol / h3_datagram) を
        // 受信していることを要求する。peer の SETTINGS はサーバーの制御ストリーム
        // (単方向) で届くため、CONNECT を送る前に単方向ストリームを受信して
        // HTTP/3 状態へ流し込む必要がある。
        let (mut handle, stream_acceptor) = connection.split();
        let (mut bidi_acceptor, mut uni_acceptor) = stream_acceptor.split();

        let unblock_notify = Arc::new(Notify::new());
        let (uni_tx, uni_rx) = mpsc::channel::<WtRecvStream>(16);
        let (bi_tx, bi_rx) = mpsc::channel::<(WtSendStream, WtRecvStream)>(16);
        // CONNECT の stream id (= session ID) を単方向ストリームのルーティングタスクへ共有する。
        // 単方向ストリームのタスクは CONNECT より前に spawn されるため、確定値を watch で配る。
        let (session_id_tx, session_id_rx) = watch::channel(None::<u64>);
        // セッション状態 (§6 の終了 / §4.7 の drain) をストリームと受信経路へ配る。
        // 各タスクは spawn 済みで `WtSession` を参照できないため、watch で共有する。
        // 初期値の receiver は受け取らず、必要とするタスクが `subscribe()` で作る。
        let (session_state_tx, _) = watch::channel(WtSessionState::Active);
        // ルーティングタスクが共有するハンドル (ストリームごとのタスクへ clone して渡す)
        let route_context = RouteContext {
            state: Arc::clone(&state),
            notify: Arc::clone(&unblock_notify),
            handle: handle.clone(),
            session_state: session_state_tx.clone(),
        };

        // 単方向ストリーム受信タスク
        //
        // H3 の制御 / QPACK ストリームは HTTP/3 状態へ流し、WebTransport の
        // 単方向ストリームは `uni_tx` へ渡す。
        let context_for_uni = route_context.clone();
        tokio::spawn(async move {
            while let Ok(Some(recv)) = uni_acceptor.accept_receive_stream().await {
                let context = context_for_uni.clone();
                let uni_tx = uni_tx.clone();
                let session_id = session_id_rx.clone();
                let stream_id: u64 = recv.id();
                tokio::spawn(route_uni_stream(
                    stream_id, recv, uni_tx, session_id, context,
                ));
            }
        });

        // H3 ストリーム (制御 + QPACK encoder/decoder) を初期化する
        let mut control_send = handle.open_send_stream().await?;
        let encoder_send = handle.open_send_stream().await?;
        let decoder_send = handle.open_send_stream().await?;

        let init_data = {
            let mut s = state
                .lock()
                .expect("connection state mutex must not be poisoned");
            s.h3_conn
                .init_h3_streams(control_send.id(), encoder_send.id(), decoder_send.id())?
        };

        control_send
            .send(Bytes::from(init_data.control_data))
            .await?;
        // encoder/decoder は初期データだけ送れば良い (ストリーム自体は保持不要)

        // peer の SETTINGS を待ってから CONNECT を送る
        wait_for_peer_settings(&state, &unblock_notify).await?;

        // CONNECT リクエスト (双方向ストリーム) を開く
        let connect_stream = handle.open_bidirectional_stream().await?;
        let connect_stream_id: u64 = connect_stream.id();
        let (mut recv_stream, mut send_stream) = connect_stream.split();
        // 単方向ストリームのルーティングタスクへ session ID を配る (draft-ietf-webtrans-http3-16 §4)
        session_id_tx.send_replace(Some(connect_stream_id));

        // Sans I/O で CONNECT リクエストをエンコードする
        let request_data = {
            let mut s = state
                .lock()
                .expect("connection state mutex must not be poisoned");
            let headers = ConnectRequest::new("https", &config.authority, path)
                // draft-ietf-moq-transport-21 §6.2 / draft-ietf-webtrans-http3-16 §3.3:
                // MOQT のプロトコル識別子を WT-Available-Protocols で通知する
                .available_protocols(vec![crate::MOQT_PROTOCOL.to_string()])
                .to_headers()
                .map_err(|e| {
                    TransportError::Internal(format!("failed to build CONNECT headers: {e}"))
                })?;
            let h3_id = s.h3_conn.send_request(&headers, false)?;
            if h3_id != connect_stream_id {
                return Err(TransportError::Internal(format!(
                    "unexpected CONNECT stream id: expected {connect_stream_id}, got {h3_id}"
                )));
            }
            let mut data = Vec::new();
            while let Some((chunk, _fin)) = s.h3_conn.take_stream_data(h3_id) {
                data.extend_from_slice(&chunk);
            }
            data
        };

        // CONNECT リクエストを送信する (fin=false)
        // draft-ietf-webtrans-http3-16 §3.2: クライアントは応答前に capsule を楽観送信してよい (MAY)。
        // 本実装は 2xx 受信まで capsule を送らない (非対応)。
        send_stream
            .send(Bytes::from(request_data))
            .await
            .map_err(TransportError::transport)?;

        // 双方向ストリーム受信タスク
        //
        // サーバー (MOQT relay) が開始する request stream は双方向ストリームで届く。
        // WT の双方向ストリームヘッダー (0x41 + session_id) を読み捨ててから
        // payload 部分を `WtRecvStream` として公開する。session_id の検証には
        // CONNECT stream の id が要るため、CONNECT を開いた後に起動する。
        let context_for_bi = route_context.clone();
        tokio::spawn(async move {
            while let Ok(Some(stream)) = bidi_acceptor.accept_bidirectional_stream().await {
                let context = context_for_bi.clone();
                let bi_tx = bi_tx.clone();
                let stream_id: u64 = stream.id();
                tokio::spawn(route_bi_stream(
                    connect_stream_id,
                    stream_id,
                    stream,
                    bi_tx,
                    context,
                ));
            }
        });

        // datagram 受信タスク (subscriber 側のみ起動する)
        if config.receive_datagrams {
            let state_for_datagram = Arc::clone(&state);
            let handle_for_datagram = handle.clone();
            let session_state_for_datagram = session_state_tx.clone();
            tokio::spawn(async move {
                loop {
                    let result = handle_for_datagram.datagram_mut(
                        |receiver: &mut s2n_quic::provider::datagram::default::Receiver| {
                            receiver.recv_datagram()
                        },
                    );
                    match result {
                        Ok(Some(bytes)) => {
                            // h3 状態のロックは feed の間だけ保持し、イベント処理はロック外で行う
                            let outcome = {
                                let mut s = state_for_datagram
                                    .lock()
                                    .expect("connection state mutex must not be poisoned");
                                s.feed_datagram(&bytes)
                            };
                            // このタスクは s2n-quic の datagram 受信バッファを空にする
                            // ために起動しており、payload はここでは使わない。セッション
                            // 状態の反映 (`SessionClosed` / `SessionDraining`) だけを行う。
                            // feed がエラーでもイベントは処理済みである。
                            match process_h3_outcome(outcome, &session_state_for_datagram) {
                                Ok(()) => {}
                                Err(e) => {
                                    tracing::warn!("Failed to feed datagram: {e}");
                                    break;
                                }
                            }
                        }
                        Ok(None) => {
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        }
                        Err(e) => {
                            tracing::warn!("Datagram query error: {e}");
                            break;
                        }
                    }
                }
            });
        }

        // 制御ストリームと QPACK ストリームを接続中保持する
        // (クローズすると H3_CLOSED_CRITICAL_STREAM エラーになる)
        tokio::spawn(async move {
            let _control_send = control_send;
            let _encoder_send = encoder_send;
            let _decoder_send = decoder_send;
            std::future::pending::<()>().await;
        });

        // CONNECT レスポンスを待つ
        // draft-ietf-webtrans-http3-16 §3.2: クライアントは 2xx 応答受信時のみセッション確立とみなす。
        // :status を見ず HeadersEnd だけで判定すると 4xx/5xx エラーレスポンスを成功と誤認するため、
        // connect_outcome で :status を確認する。
        // サーバが非対応リソースに返す推奨ステータスは -16 で 404 から 405 に変わったが、
        // クライアントは 2xx 以外を一律失敗として扱うため追加の分岐は不要。
        let mut connect_status: Option<u16> = None;
        let mut session_established = false;
        while !session_established {
            tokio::select! {
                received = recv_stream.receive() => {
                    let (data, fin) = match received {
                        Ok(Some(data)) => (data.to_vec(), false),
                        Ok(None) => (vec![], true),
                        Err(e) => return Err(TransportError::transport(e)),
                    };
                    let outcome = {
                        // feed と drain を同じロック保持で行う。ルーティングタスクも
                        // イベントを drain するため、ロックを跨ぐと CONNECT レスポンスの
                        // イベントを横取りされ得る
                        let mut s = state.lock().expect("connection state mutex must not be poisoned");
                        s.feed_stream_and_drain(connect_stream_id, &data, fin)
                    };
                    let connect_result = connect_outcome(&outcome.events, &mut connect_status);
                    // セッション終了 (§6) が 2xx と同じバッチで届く場合があるため、
                    // イベントは CONNECT の判定だけでなくセッション状態へも反映する。
                    // feed がエラーでもイベントは処理済みである
                    process_h3_outcome(outcome, &session_state_tx)?;
                    match connect_result {
                        ConnectOutcome::Established => session_established = true,
                        ConnectOutcome::Failed { status } => {
                            return Err(TransportError::ConnectFailed { status });
                        }
                        ConnectOutcome::Pending => {}
                    }
                    if fin { break; }
                }
                _ = unblock_notify.notified() => {
                    let events = state.lock().expect("connection state mutex must not be poisoned").drain_events()?;
                    let outcome = connect_outcome(&events, &mut connect_status);
                    let _ = process_h3_events(events, &session_state_tx);
                    match outcome {
                        ConnectOutcome::Established => session_established = true,
                        ConnectOutcome::Failed { status } => {
                            return Err(TransportError::ConnectFailed { status });
                        }
                        ConnectOutcome::Pending => {}
                    }
                }
            }
        }

        if !session_established {
            return Err(TransportError::ConnectionClosed);
        }

        // CONNECT stream の受信半はセッション終了の検知に必要なので保持し、専用のタスクで
        // h3 層へ feed し続ける。
        //
        // draft-ietf-webtrans-http3-16 §6 は CONNECT stream の close (FIN / RESET_STREAM) と
        // WT_CLOSE_SESSION の送受信をセッション終了の条件とする。h3 層 (shiguredo_http3) は
        // CONNECT stream を feed したときに `WebTransportEvent::SessionClosed` を発火するため、
        // 受信半を drop すると終了を検知できない (受信半を drop していた従来の実装では
        // `SessionClosed` が永久に発火しなかった)。
        let state_for_connect = Arc::clone(&state);
        let session_state_for_connect = session_state_tx.clone();
        tokio::spawn(async move {
            // セッション状態の変化も待つ。自発 close (`WtSession::close`) では peer が
            // CONNECT stream を閉じるまで `receive()` が返らないため、状態を観測しないと
            // このタスクが残る。
            let mut state_watch = session_state_for_connect.subscribe();
            loop {
                tokio::select! {
                    received = recv_stream.receive() => match received {
                        Ok(Some(data)) => {
                            if !feed_connect_stream(
                                &state_for_connect,
                                &session_state_for_connect,
                                connect_stream_id,
                                &data,
                                false,
                            ) {
                                // 依存 shiguredo_http3 には `SessionClosed` を発行せずにエラーを
                                // 返す CONNECT stream の経路がある (malformed capsule / 未完成の
                                // capsule を残した FIN)。状態を `Active` のまま break すると
                                // `ensure_new_stream_allowed` / `ensure_datagram_allowed` が
                                // 新しいストリームの open と datagram の送信を許可し続けるため、
                                // 受信エラーの分岐と同じく終了として扱ってから抜ける。
                                // このタスクは `recv_stream` の drop で終わるため、ここで状態を
                                // 移さないと以降 `SessionClosed` は永久に発火しない
                                update_session_state(
                                    &session_state_for_connect,
                                    WtSessionState::ClosedByPeer,
                                );
                                break;
                            }
                        }
                        Ok(None) => {
                            // FIN (CONNECT stream の clean な close)。h3 層がセッション終了として
                            // 扱い `SessionClosed` を発火する (§6)
                            if !feed_connect_stream(
                                &state_for_connect,
                                &session_state_for_connect,
                                connect_stream_id,
                                &[],
                                true,
                            ) {
                                // FIN の feed が失敗した場合も上と同じ理由で終了として扱う
                                update_session_state(
                                    &session_state_for_connect,
                                    WtSessionState::ClosedByPeer,
                                );
                            }
                            break;
                        }
                        Err(s2n_quic::stream::Error::StreamReset { error, .. }) => {
                            // RESET_STREAM (CONNECT stream の abrupt な close) もセッション終了である (§6)。
                            // h3 層は Sans I/O のため、I/O 層からの通知で `SessionClosed` を発火する
                            reset_connect_stream(
                                &state_for_connect,
                                &session_state_for_connect,
                                connect_stream_id,
                                error.into(),
                            );
                            break;
                        }
                        Err(e) => {
                            // 受信の異常終了も §6 の「CONNECT stream が close した場合」に当たるため
                            // セッション終了として扱う。状態を `Active` のまま残すと
                            // `ensure_new_stream_allowed` / `ensure_datagram_allowed` が新しい
                            // ストリームの open と datagram の送信を許可し続ける
                            tracing::warn!("CONNECT stream receive error: {e}");
                            update_session_state(
                                &session_state_for_connect,
                                WtSessionState::ClosedByPeer,
                            );
                            break;
                        }
                    },
                    _ = wait_until_terminated(&mut state_watch) => {
                        // セッション終了を観測したら、CONNECT stream 自身の受信半も
                        // `WT_SESSION_GONE` で中断する。§6 はセッション終了を検知したときに
                        // "abort reading on the receive side of all unidirectional and
                        // bidirectional streams" を MUST とし、`WT_CLOSE_SESSION` を送った側にも
                        // 同じ中断を定める。中断しないと `ReceiveStream` の drop で s2n-quic が
                        // `stop_sending(0x0)` を送り、プロトコルコードが peer へ伝わらない。
                        //
                        // `wait_until_terminated` は `abort_streams` が真になったときのほか、
                        // 状態を配る sender が drop されたときにも返る。後者は状態を観測できない
                        // 場合であり、送信しても失敗する (production では通常到達しない)。
                        // 観測した状態は問わず、受信半の中断だけを行う
                        abort_connect_stream_read(&mut recv_stream);
                        break;
                    }
                }
            }
        });

        Ok(WtSession {
            session_id: connect_stream_id,
            handle,
            connect_send: send_stream,
            uni_rx: Some(uni_rx),
            bi_rx: Some(bi_rx),
            state,
            session_state: session_state_tx,
        })
    }
}

/// CONNECT stream から読んだデータを h3 層へ流し、イベントをセッション状態へ反映する
///
/// 戻り値は CONNECT stream を読み続けるかどうか (h3 層がエラーを返した場合は false)。
/// feed がエラーでも `feed_stream_to_h3` が drain したイベントを処理してから返るため、
/// 同じチャンクで届いたセッション終了 (§6) を取りこぼさない。
/// datagram は CONNECT stream を流れないため、`process_h3_events` の戻り値は捨てる。
fn feed_connect_stream(
    state: &Arc<StdMutex<ClientConnectionState>>,
    session_state: &watch::Sender<WtSessionState>,
    stream_id: u64,
    data: &[u8],
    fin: bool,
) -> bool {
    match feed_stream_to_h3(state, session_state, stream_id, data, fin) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!("Failed to feed CONNECT stream: {e}");
            false
        }
    }
}

/// CONNECT stream の RESET_STREAM を h3 層へ伝え、イベントをセッション状態へ反映する
///
/// s2n-quic のストリームエラーは RFC 9000 §19.4 の Final Size を運ばないため、h3 層へは
/// 0 を渡す (`ClientConnectionState::process_stream_reset`)。
fn reset_connect_stream(
    state: &Arc<StdMutex<ClientConnectionState>>,
    session_state: &watch::Sender<WtSessionState>,
    stream_id: u64,
    error_code: u64,
) {
    let outcome = {
        let mut s = state
            .lock()
            .expect("connection state mutex must not be poisoned");
        s.process_stream_reset(stream_id, error_code)
    };
    if let Err(e) = process_h3_outcome(outcome, session_state) {
        tracing::warn!("Failed to reset CONNECT stream: {e}");
    }
}

/// CONNECT stream の受信半を `WT_SESSION_GONE` で中断する (draft-ietf-webtrans-http3-16 §6)
///
/// §6 はセッション終了を検知したら "abort reading on the receive side of all unidirectional
/// and bidirectional streams associated with the session ... using the WT_SESSION_GONE error
/// code" を MUST とする。CONNECT stream 自身も対象に含まれるため、peer 主導の終了を観測した
/// ときに受信半へ STOP_SENDING を送る。`WT_SESSION_GONE` はプロトコルコードであり §4.4 の
/// remap を通さない (`StreamErrorCode::Protocol`)。
fn abort_connect_stream_read(recv_stream: &mut ReceiveStream) {
    // `WT_SESSION_GONE` (0x170d7b68) は QUIC の application error code の範囲に収まるため失敗しない
    let error = stream_application_error(session_gone_code())
        .expect("WT_SESSION_GONE fits in the QUIC application error code range");
    match recv_stream.stop_sending(error) {
        Ok(()) => {
            tracing::debug!("Aborted reading the WebTransport CONNECT stream with WT_SESSION_GONE");
        }
        Err(e) => {
            tracing::warn!(
                "Failed to abort reading the WebTransport CONNECT stream with WT_SESSION_GONE: {e}"
            );
        }
    }
}

/// peer の SETTINGS 受信を待つ
///
/// draft-ietf-webtrans-http3-16 Section 3.1 / 4.6 は WebTransport CONNECT の送信前に
/// peer の SETTINGS を受信していることを要求する。待たずに送ると
/// `WtSetupError::PeerSettingsNotReceived` で失敗する。
/// peer の SETTINGS はサーバーの制御ストリーム (単方向) で届き、受信タスクが
/// HTTP/3 状態へ流し込むたびに `notify` で通知する。
async fn wait_for_peer_settings(
    state: &Arc<StdMutex<ClientConnectionState>>,
    notify: &Notify,
) -> Result<()> {
    /// peer SETTINGS を待つ上限 (ms)
    ///
    /// サーバーは接続直後に制御ストリームを開くため通常は 1 RTT 以内に届く。
    /// 届かない場合は接続が壊れているため、待ち続けずに失敗させる。
    const PEER_SETTINGS_TIMEOUT_MS: u64 = 10_000;

    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_millis(PEER_SETTINGS_TIMEOUT_MS);
    loop {
        // 条件確認より先に通知の受信登録を行う (取りこぼしを避ける)
        let notified = notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        {
            let s = state
                .lock()
                .expect("connection state mutex must not be poisoned");
            if s.h3_conn.peer_settings().is_some() {
                return Ok(());
            }
        }
        tokio::select! {
            _ = &mut notified => {}
            _ = tokio::time::sleep_until(deadline) => {
                return Err(TransportError::Internal(
                    "peer SETTINGS not received before WebTransport CONNECT".to_string(),
                ));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// WebTransport セッション
// ---------------------------------------------------------------------------

/// WebTransport セッション
///
/// `state` は `ClientConnectionState` (HTTP/3 の Sans I/O 状態) を `Arc<StdMutex<...>>` で
/// 共有する。datagram 受信タスクと各ストリーム操作が並行して状態を更新するため
/// `Mutex` で保護するが、各操作は await をまたがず短時間で完結する。
///
/// `session_state` は §6 の終了 / §4.7 の drain を各タスクへ配る `watch` である。
/// ストリームを所有するタスクは自分では状態を更新せず、この watch の変化を観測して
/// 自分のストリームを中断する (`wait_until_terminated` を参照)。
pub struct WtSession {
    session_id: u64,
    handle: s2n_quic::connection::Handle,
    connect_send: SendStream,
    /// 単方向受信ストリームの receiver。`take_uni_receiver` で取り出すと None になる
    uni_rx: Option<mpsc::Receiver<WtRecvStream>>,
    /// 双方向受信ストリームの receiver。`take_bi_receiver` で取り出すと None になる
    bi_rx: Option<mpsc::Receiver<(WtSendStream, WtRecvStream)>>,
    state: Arc<StdMutex<ClientConnectionState>>,
    /// セッション状態 (§6 の終了 / §4.7 の drain) を配る sender
    session_state: watch::Sender<WtSessionState>,
}

impl WtSession {
    /// 現在のセッション状態を返す
    ///
    /// 状態を配る sender から直接読む。読み取りロックはこの式の中だけで解放されるため、
    /// 待機中のタスクに影響しない。
    fn session_state(&self) -> WtSessionState {
        *self.session_state.borrow()
    }

    /// セッション状態の変更を観測する receiver を作る
    ///
    /// 受信ループのように `WtSession` のロックを保持せずに待つタスクが、セッション終了を
    /// 観測するために使う。既に終了している場合は `wait_until_terminated` がすぐに返る。
    pub(crate) fn session_state_receiver(&self) -> watch::Receiver<WtSessionState> {
        self.session_state.subscribe()
    }

    /// 新しいストリームを開ける状態かを確認する
    ///
    /// draft-ietf-webtrans-http3-16 §6 はセッション終了の検知後に新しいストリームを開くことを
    /// 禁じ (MUST NOT)、§4.7 の drain の通知後もセッションの利用は許す (MAY) が、本 example は
    /// drain を「できるだけ早く終了する」合図として扱い、新規ストリームの open を拒否する。
    ///
    /// どちらの状態でも MOQT 層へは `ConnectionClosed` を返す。MOQT 層に必要な情報は
    /// 「このセッションでは新しい作業を始められない」ことで、drain と終了で呼び出し側の
    /// 動作が変わらないためである。
    fn ensure_new_stream_allowed(&self) -> Result<()> {
        let state = self.session_state();
        if session_policy(state).reject_new_streams {
            // 呼び出し側 (MOQT 層 / transport の受信ループ) はこのエラーをセッション終了と
            // して扱い、info (受信ループの終了) または warn (後始末の送信失敗) でログする。
            // ここでは debug に留めて二重にログを出さない
            tracing::debug!(
                "Refusing to open a new WebTransport stream: session state is {state:?}"
            );
            return Err(TransportError::ConnectionClosed);
        }
        Ok(())
    }

    /// datagram を送れる状態かを確認する
    ///
    /// 判定の理由は `ensure_new_stream_allowed` を参照する。
    fn ensure_datagram_allowed(&self) -> Result<()> {
        let state = self.session_state();
        if session_policy(state).reject_datagrams {
            // 拒否の理由は `ensure_new_stream_allowed` と同じである
            tracing::debug!("Refusing to send a WebTransport datagram: session state is {state:?}");
            return Err(TransportError::ConnectionClosed);
        }
        Ok(())
    }

    /// 単方向送信ストリームを開く
    pub async fn open_uni_stream(&mut self) -> Result<WtSendStream> {
        self.ensure_new_stream_allowed()?;
        let stream = self
            .handle
            .open_send_stream()
            .await
            .map_err(TransportError::transport)?;
        let stream_id: u64 = stream.id();
        let mut send = stream;

        // WT 単方向ストリームヘッダー (0x54 + session_id) を送信する
        let mut header = Vec::new();
        // session_id は CONNECT stream ID である (client 起動の双方向、session_id % 4 == 0)
        StreamHeader::new(self.session_id)
            .expect("session_id must be a client-initiated bidirectional stream ID")
            .encode_unidirectional(&mut header);
        send.send(Bytes::from(header))
            .await
            .map_err(TransportError::transport)?;

        Ok(WtSendStream {
            stream_id,
            send,
            session_state: self.session_state_receiver(),
        })
    }

    /// 単方向受信ストリームを受け付ける
    ///
    /// `take_uni_receiver` で receiver を取り出した後は `StreamClosed` を返す。
    pub async fn accept_uni_stream(&mut self) -> Result<WtRecvStream> {
        let rx = self.uni_rx.as_mut().ok_or(TransportError::StreamClosed)?;
        rx.recv().await.ok_or(TransportError::StreamClosed)
    }

    /// サーバーが開始した双方向ストリームを受け付ける
    ///
    /// WT の双方向ストリームヘッダーは受信タスクが読み捨て済みである。
    /// `take_bi_receiver` で receiver を取り出した後は `StreamClosed` を返す。
    pub async fn accept_bi_stream(&mut self) -> Result<(WtSendStream, WtRecvStream)> {
        let rx = self.bi_rx.as_mut().ok_or(TransportError::StreamClosed)?;
        rx.recv().await.ok_or(TransportError::StreamClosed)
    }

    /// 単方向受信ストリームの receiver を取り出す
    ///
    /// `accept_uni_stream` は `&mut self` の await であり、`Arc<Mutex<WtSession>>` を
    /// 共有していると待機中ロックを保持してしまう。受信ループのように長く待つ呼び出しは
    /// receiver を取り出してロック外で `recv()` する。
    /// 取り出しは 1 回だけで、2 回目以降は None を返す。
    pub fn take_uni_receiver(&mut self) -> Option<mpsc::Receiver<WtRecvStream>> {
        self.uni_rx.take()
    }

    /// 双方向受信ストリームの receiver を取り出す
    ///
    /// 詳細は `take_uni_receiver` を参照する。
    pub fn take_bi_receiver(&mut self) -> Option<mpsc::Receiver<(WtSendStream, WtRecvStream)>> {
        self.bi_rx.take()
    }

    /// 双方向ストリームを開く (draft-ietf-webtrans-http3-16 Section 4.3)
    pub async fn open_bi_stream(&mut self) -> Result<WtBiStream> {
        self.ensure_new_stream_allowed()?;
        let stream = self
            .handle
            .open_bidirectional_stream()
            .await
            .map_err(TransportError::transport)?;
        let stream_id: u64 = stream.id();
        let (recv, mut send) = stream.split();

        let mut header = Vec::new();
        // session_id は CONNECT stream ID である (client 起動の双方向、session_id % 4 == 0)
        StreamHeader::new(self.session_id)
            .expect("session_id must be a client-initiated bidirectional stream ID")
            .encode_bidirectional(&mut header);
        send.send(Bytes::from(header))
            .await
            .map_err(TransportError::transport)?;

        Ok(WtBiStream {
            stream_id,
            recv,
            send,
            pending: Vec::new(),
            session_state: self.session_state_receiver(),
        })
    }

    /// セッションをクローズする (draft-ietf-webtrans-http3-16 §6)
    ///
    /// `WT_CLOSE_SESSION` を送り、直後に CONNECT stream へ FIN を送る (MUST)。
    /// Application Error Message が 1024 バイトを超える場合は UTF-8 境界で truncate する (MUST)。
    /// `code` は 32 ビットの Application Error Code である (`moqt_close_code` で変換する)。
    ///
    /// 送信の前にセッション状態を終了へ移す。§6 はセッション終了を検知したら関連する
    /// 全 uni / bidi ストリームを `WT_SESSION_GONE` で中断する MUST を定めるが、中断は
    /// ストリームを所有するタスクが状態変化を観測して行う (この関数はストリームを保持しない)。
    ///
    /// 非対応:
    /// - CONNECT stream の受信半への `STOP_SENDING` with `WT_SESSION_GONE` (§6 MAY)
    /// - `WT_CLOSE_SESSION` 受信後に追加データを受信した場合の `H3_MESSAGE_ERROR` reset (§6)
    pub async fn close(&mut self, code: u32, reason: &str) -> Result<()> {
        update_session_state(&self.session_state, WtSessionState::ClosedLocally);
        let capsule = Capsule::CloseSession {
            error_code: code,
            message: truncate_close_session_message(reason).to_string(),
        };
        let mut buf = Vec::new();
        capsule.encode(&mut buf);
        self.connect_send
            .send(Bytes::from(buf))
            .await
            .map_err(TransportError::transport)?;
        // draft-ietf-webtrans-http3-16 §6 (Session Termination):
        // WT_CLOSE_SESSION 送信後は CONNECT stream に即座に FIN を送る (MUST)
        self.connect_send
            .finish()
            .map_err(TransportError::transport)
    }

    /// WebTransport セッションにバッファリングされた datagram を取り出す (subscriber 側で使用)
    ///
    /// h3 層のイベントを `process_h3_events` に通すため、イベントに `SessionClosed` /
    /// `SessionDraining` が含まれていても取りこぼさない。セッション終了を検知している場合は
    /// MOQT 層の受信ループを待たせないよう `ConnectionClosed` を返すが、同じバッチで
    /// 取り出した datagram は捨てない (`resolve_buffered_datagrams` を参照)。
    pub fn take_buffered_datagrams(&self) -> Result<Vec<Vec<u8>>> {
        let events = {
            let mut s = self
                .state
                .lock()
                .expect("connection state mutex must not be poisoned");
            s.drain_events()?
        };
        let datagrams = process_h3_events(events, &self.session_state);
        resolve_buffered_datagrams(datagrams, self.session_state())
    }

    /// datagram を送信する (publisher 側で使用)
    ///
    /// HTTP Datagram フォーマットでエンコードし、s2n-quic の datagram sender で送信する。
    /// `state` のロックは HTTP Datagram 生成後に解放してから `handle.datagram_mut` を呼び、
    /// デッドロックを回避する。
    /// セッション終了・drain の検知後は新しい datagram を送らない (§6 の MUST NOT / §4.7)。
    pub async fn send_datagram(&self, payload: &[u8]) -> Result<()> {
        self.ensure_datagram_allowed()?;
        let datagram_bytes = {
            let s = self
                .state
                .lock()
                .expect("connection state mutex must not be poisoned");
            s.h3_conn.send_datagram(self.session_id, payload)?
        };
        let bytes = Bytes::from(datagram_bytes);
        self.handle
            .datagram_mut(
                |sender: &mut s2n_quic::provider::datagram::default::Sender| {
                    sender.send_datagram(bytes)
                },
            )
            .map_err(TransportError::transport)?
            .map_err(|e| TransportError::Internal(format!("datagram send: {e}")))?;
        Ok(())
    }
}

/// 取り出した datagram とセッション状態から `take_buffered_datagrams` の戻り値を決める純関数
///
/// セッション終了 (§6) を検知しても、同じバッチで取り出した datagram は捨てない
/// (終了直前の datagram を落とすと、peer が送った最後の object が届かない)。
/// 次回の呼び出しでは取り出せるイベントが無いため、datagram が空で終了を検知している
/// 場合だけ `ConnectionClosed` を返し、MOQT 層の受信ループを待たせない。
fn resolve_buffered_datagrams(
    datagrams: Vec<Vec<u8>>,
    state: WtSessionState,
) -> Result<Vec<Vec<u8>>> {
    // セッションが終了したかどうかは `session_policy` の `abort_streams` で判断する
    if datagrams.is_empty() && session_policy(state).abort_streams {
        return Err(TransportError::ConnectionClosed);
    }
    Ok(datagrams)
}

// ---------------------------------------------------------------------------
// WebTransport のエラーコード変換
// ---------------------------------------------------------------------------

/// MOQT のエラーコードを WebTransport の HTTP/3 エラーコードへ remap する
///
/// draft-ietf-webtrans-http3-16 §4.4 (Resetting Data Streams) は、WebTransport の
/// アプリケーションエラーコード (0x00000000-0xffffffff) を WT_APPLICATION_ERROR の範囲へ
/// remap する MUST を定める。0x00000000 が 0x52e4a40fa8db、0xffffffff が 0x52e5ac983162 に
/// 対応し、予約コードポイント (0x1f * N + 0x21) はスキップする。
/// 変換自体は `shiguredo_http3::webtransport::ApplicationErrorCode` に実装済みのものを使う。
///
/// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
///
/// MOQT のエラーコードは §12.5 (Stream Reset Error Codes) の登録値だけでなく、
/// §13 (Grease) の greasing 値 (`0x7f * N + 0x9D`。`0x9D, 0x11C, ..., 0x3fffffffffffffde` を取り得る) も含む。
/// §16.11.4 の Stream Reset Error Codes registry も同じ greasing 予約を持つ。
/// §4.4 は WebTransport のアプリケーションエラーコードを 32 ビットに限るため、
/// 32 ビットを超える greasing 値は WebTransport 経路では remap できずリセットを送れない。
/// 黙って切り捨てずエラーにする (2^32 以上 2^62 未満は従来は remap せず wire に載っていたため、
/// この防御は挙動変更になる)。公開 API (`Session::reset_outgoing_data_stream_with_code` など) は
/// 任意の `u64` を受け付けるため、32 ビットを超える値が到達し得る。
/// この制限は §4.4 の u32 制限に由来し、`RequestStreamEnd` の型や greasing の扱いとは別に
/// WebTransport 経路の既知の制限として扱う。
fn moqt_to_wt_code(code: u64) -> Result<u64> {
    let app_code = u32::try_from(code).map_err(|_| {
        TransportError::Internal(format!(
            "MOQT error code {code:#x} does not fit in a WebTransport application error code (0x00000000-0xffffffff)"
        ))
    })?;
    Ok(ApplicationErrorCode::to_http3_code(app_code))
}

/// HTTP/3 のエラーコードを WebTransport のアプリケーションエラーコードへ戻す
///
/// WT_APPLICATION_ERROR の範囲外のコード (WT_SESSION_GONE = 0x170d7b68 などのプロトコルコード) と
/// 予約コードポイント (0x1f * N + 0x21) は `None` を返す (draft-ietf-webtrans-http3-16 §4.4)。
///
/// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
fn wt_to_moqt_code(http3_code: u64) -> Option<u32> {
    ApplicationErrorCode::from_http3_code(http3_code)
}

/// RESET_STREAM で受信した HTTP/3 のエラーコードを `RequestStreamEnd::Reset` へ入れる値に変換する
///
/// remap できれば MOQT のエラーコードを返す。remap できない場合について §4.4 は
/// "the stream is still considered reset, but the error code is not mapped to a WebTransport
/// application error code." と定めるが、`RequestStreamEnd::Reset` の `error_code` は必須の `u64` で
/// 「アプリケーションエラーコード無し」を表す値を持たない (`Option<u64>` への型変更は
/// `SessionEvent` / `TerminationReason` の公開 API とその構築サイトに波及するため行わない)。
/// そのため wire の HTTP/3 コードをそのまま返し、生値を `tracing::warn!` でログに残す。
/// 呼び出し元では「WT_APPLICATION_ERROR の範囲だったものを remap した MOQT コード」と
/// 「範囲外の HTTP/3 コードをそのまま入れた値」の 2 種が区別されずに渡る。MOQT §12.5 のコードは
/// 小さな値 (現行は 0x0-0x12) なので前者とは区別できるが、後者には MOQT のコードと
/// 区別できない値もある。この扱いは `RequestStreamEnd::Reset` の型を変更するまでの暫定である。
///
/// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
fn wt_reset_error_code(http3_code: u64) -> u64 {
    match wt_to_moqt_code(http3_code) {
        Some(code) => u64::from(code),
        None => {
            if ApplicationErrorCode::is_application_error(http3_code) {
                // 数値上は WT_APPLICATION_ERROR の範囲内だが予約コードポイント (0x1f * N + 0x21)。
                // 依存に予約コードポイントの判定 API が無いため、`from_http3_code` が None を返す条件と
                // `is_application_error` が範囲だけを見ることの組み合わせで判定している。
                // shiguredo_http3 を更新したらこの 2 つの条件が変わっていないか確認すること
                tracing::warn!(
                    "RESET_STREAM received with a reserved HTTP/3 error code point: {http3_code:#x}"
                );
            } else {
                // 範囲外 (WT_SESSION_GONE などのプロトコルコード)
                tracing::warn!(
                    "RESET_STREAM received with an HTTP/3 error code outside the WebTransport application error range: {http3_code:#x}"
                );
            }
            http3_code
        }
    }
}

/// MOQT の close code を `WT_CLOSE_SESSION` の Application Error Code (`u32`) へ変換する
///
/// draft-ietf-webtrans-http3-16 §6 (Session Termination) の `WT_CLOSE_SESSION` capsule の
/// Application Error Code は 32 ビットである。§4.4 の remap は RESET_STREAM / STOP_SENDING の
/// アプリケーションエラーコードに対する規則であり、capsule が運ぶ値には適用しない
/// (capsule は 32 ビットのアプリケーションコードをそのまま運ぶ)。
///
/// MOQT のコードは `u64` で、§13 (Grease) の greasing 値は 32 ビットを超える
/// (`0x7f * N + 0x9D`。`0x9D, 0x11C, ..., 0x3fffffffffffffde` を取り得る)。
/// `as u32` で切り捨てると別のコードに化けるため、収まらない場合はエラーにする。
/// `Session` の公開 API は任意の `u64` を受け付けるため、この値が到達し得る。
pub(crate) fn moqt_close_code(code: u64) -> Result<u32> {
    u32::try_from(code).map_err(|_| {
        TransportError::Internal(format!(
            "MOQT close code {code:#x} does not fit in the WT_CLOSE_SESSION application error code (0x00000000-0xffffffff)"
        ))
    })
}

/// ストリームの中断・送信停止に載せるエラーコード
///
/// WebTransport のストリーム操作には 2 種類のコードが載る。型で分けることで、プロトコル
/// コードが §4.4 の remap 経路へ入る事故を防ぐ。
///
/// - `Application`: MOQT のアプリケーションエラーコード。§4.4 の remap を通して
///   WT_APPLICATION_ERROR の範囲へ写す
/// - `Protocol`: WebTransport / HTTP/3 のプロトコルコード (`WT_SESSION_GONE` など)。
///   §4.4 の remap はアプリケーションエラーコード専用であり、プロトコルコードを通しては
///   ならない (通すと `WT_SESSION_GONE` = 0x170d7b68 が WT_APPLICATION_ERROR の別のコードに
///   化ける)。wire の値のまま渡す
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamErrorCode {
    /// MOQT のアプリケーションエラーコード
    Application(u64),
    /// WebTransport / HTTP/3 のプロトコルコード
    Protocol(u64),
}

/// ストリーム操作のエラーコードを s2n-quic のアプリケーションエラーへ変換する
///
/// remap した値 / wire の値だけを `s2n_quic::application::Error::new` に渡す経路を
/// この関数に閉じ、`WtSendStream` / `WtRecvStream` の reset / stop_sending から呼ぶ。
fn stream_application_error(code: StreamErrorCode) -> Result<s2n_quic::application::Error> {
    let wire_code = match code {
        StreamErrorCode::Application(code) => moqt_to_wt_code(code)?,
        // プロトコルコードは §4.4 の remap を通さない (`moqt_to_wt_code` は MOQT の
        // アプリケーションエラーコード専用)。そのまま wire に載せる
        StreamErrorCode::Protocol(code) => code,
    };
    s2n_quic::application::Error::new(wire_code).map_err(TransportError::transport)
}

/// セッション終了時にストリームを中断するエラーコード (draft-ietf-webtrans-http3-16 §6)
///
/// "the endpoint MUST reset the send side and abort reading on the receive side of all
/// unidirectional and bidirectional streams associated with the session ... using the
/// WT_SESSION_GONE error code" と定める。`WT_SESSION_GONE` は
/// "a protocol-level error code rather than an application error code" であるため、
/// MOQT のアプリケーションエラーコードとして扱わない (`StreamErrorCode::Protocol`)。
/// 数値は example 側で再定義せず依存 crate の定義を使う。
fn session_gone_code() -> StreamErrorCode {
    StreamErrorCode::Protocol(WtErrorCode::SessionGone as u64)
}

/// wire の HTTP/3 エラーコードから `RequestStreamEnd::Reset` を組み立てる
///
/// `reliable_size` は `RESET_STREAM_AT` を受信したときに埋まる値だが、s2n-quic は
/// `RESET_STREAM_AT` に対応していないため常に `None` にする。
fn wt_reset_stream_end(http3_code: u64) -> RequestStreamEnd {
    RequestStreamEnd::Reset {
        error_code: wt_reset_error_code(http3_code),
        reliable_size: None,
    }
}

/// `WtRecvStream::recv_chunk` のエラー分岐を `RecvChunk` へ変換する
///
/// `RESET_STREAM` を受信したときは wire の HTTP/3 コードを MOQT のコードへ戻して
/// `RequestStreamEnd::Reset` にし、それ以外のストリームエラーはそのままエラーにする。
/// I/O ハンドルを持たないため `recv_chunk` から切り出して単体テストできるようにしている。
fn wt_recv_end(error: s2n_quic::stream::Error) -> Result<RecvChunk> {
    match error {
        s2n_quic::stream::Error::StreamReset { error, .. } => {
            Ok(RecvChunk::End(wt_reset_stream_end(error.into())))
        }
        e => Err(TransportError::transport(e)),
    }
}

// ---------------------------------------------------------------------------
// ストリーム型
// ---------------------------------------------------------------------------

/// WebTransport の単方向送信ストリーム
///
/// `session_state` はセッション状態 (§6 の終了 / §4.7 の drain) の観測用である。
/// 送信待ちの間に終了を観測したら、送信方向を `WT_SESSION_GONE` で中断する (§6 の MUST)。
pub struct WtSendStream {
    stream_id: u64,
    send: SendStream,
    session_state: watch::Receiver<WtSessionState>,
}

impl WtSendStream {
    /// データを送信する
    ///
    /// セッション終了 (§6) を観測した場合は送信を待たずに送信方向を `WT_SESSION_GONE` で
    /// 中断し、MOQT 層へ `ConnectionClosed` を返す (受信経路と同じ扱い)。
    pub async fn send(&mut self, data: &[u8]) -> Result<()> {
        tokio::select! {
            sent = self.send.send(Bytes::copy_from_slice(data)) => {
                sent.map_err(TransportError::transport)
            }
            _ = wait_until_terminated(&mut self.session_state) => {
                self.abort_session_gone();
                Err(TransportError::ConnectionClosed)
            }
        }
    }

    /// ストリームの送信方向を終了する (FIN)
    pub fn finish(&mut self) -> Result<()> {
        self.send.finish().map_err(TransportError::transport)
    }

    /// ストリームの送信方向を reset する (QUIC RESET_STREAM)
    ///
    /// MOQT のエラーコードは WebTransport のアプリケーションエラーコードとして
    /// WT_APPLICATION_ERROR の範囲へ remap してから送る (draft-ietf-webtrans-http3-16 §4.4)。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub fn reset(&mut self, error_code: u64) -> Result<()> {
        self.reset_with(StreamErrorCode::Application(error_code))
    }

    /// セッション終了時の中断 (draft-ietf-webtrans-http3-16 §6)
    ///
    /// エラーコードは `WT_SESSION_GONE` (プロトコルコード) であり、MOQT のアプリケーション
    /// エラーコードではない。§4.4 の remap はアプリケーションエラーコード専用のため通さない
    /// (`StreamErrorCode::Protocol` で渡す)。
    ///
    /// `RESET_STREAM_AT` が無い現状の QUIC 実装では `RESET_STREAM` にフォールバックする
    /// (`SendStream::reset` が `RESET_STREAM` を送出する)。
    fn abort_session_gone(&mut self) {
        let stream_id = self.stream_id;
        match self.reset_with(session_gone_code()) {
            Ok(()) => {
                tracing::debug!("Reset WebTransport send stream {stream_id} with WT_SESSION_GONE");
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to reset WebTransport send stream {stream_id} with WT_SESSION_GONE: {e}"
                );
            }
        }
    }

    /// エラーコードの種別を指定してストリームの送信方向を reset する
    fn reset_with(&mut self, code: StreamErrorCode) -> Result<()> {
        self.send
            .reset(stream_application_error(code)?)
            .map_err(TransportError::transport)
    }

    /// ストリーム ID を返す (publisher 側で使用)
    pub fn stream_id(&self) -> u64 {
        self.stream_id
    }
}

/// WebTransport の受信ストリーム
///
/// ストリームタイプ判定で読みすぎたバイトを `pending` に保持し、
/// `recv_chunk` の初回呼び出しで返す。
///
/// `session_state` はセッション状態 (§6 の終了 / §4.7 の drain) の観測用である。
/// 受信待ちの間に終了を観測したら、受信方向を `WT_SESSION_GONE` で中断する (§6 の MUST)。
pub struct WtRecvStream {
    stream_id: u64,
    recv: ReceiveStream,
    pending: Vec<u8>,
    session_state: watch::Receiver<WtSessionState>,
}

/// WebTransport ストリームから取り出した 1 要素
#[derive(PartialEq, Eq)]
pub enum RecvChunk {
    /// 受信データ
    Data(Vec<u8>),
    /// ストリーム終端
    End(RequestStreamEnd),
}

impl std::fmt::Debug for RecvChunk {
    // 受信データは長さだけを出す。`#[derive(Debug)]` だとメディアの payload 全体が
    // ログに載るため手書きする。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Data(data) => write!(f, "Data({} bytes)", data.len()),
            Self::End(end) => write!(f, "End({end:?})"),
        }
    }
}

impl WtRecvStream {
    fn new(
        stream_id: u64,
        recv: ReceiveStream,
        pending: Vec<u8>,
        session_state: watch::Receiver<WtSessionState>,
    ) -> Self {
        Self {
            stream_id,
            recv,
            pending,
            session_state,
        }
    }

    /// データまたは終端を 1 つ受信する
    ///
    /// セッション終了 (§6) を観測した場合は受信を待たずに受信方向を `WT_SESSION_GONE` で
    /// 中断し、MOQT 層へ `ConnectionClosed` を返す (受信ループを永久に待たせない)。
    pub async fn recv_chunk(&mut self) -> Result<RecvChunk> {
        if !self.pending.is_empty() {
            return Ok(RecvChunk::Data(std::mem::take(&mut self.pending)));
        }
        tokio::select! {
            received = self.recv.receive() => match received {
                Ok(Some(data)) => Ok(RecvChunk::Data(data.to_vec())),
                Ok(None) => Ok(RecvChunk::End(RequestStreamEnd::Fin)),
                Err(e) => wt_recv_end(e),
            },
            _ = wait_until_terminated(&mut self.session_state) => {
                self.abort_session_gone();
                Err(TransportError::ConnectionClosed)
            }
        }
    }

    /// 受信方向へ STOP_SENDING を送出する (QUIC STOP_SENDING)
    ///
    /// draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection): 受信方向の
    /// cancel は STOP_SENDING で行う。error code は §12.5 (Stream Reset Error Codes) から選ぶ。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub fn stop_sending(&mut self, error_code: u64) -> Result<()> {
        self.stop_sending_with(StreamErrorCode::Application(error_code))
    }

    /// セッション終了時の中断 (draft-ietf-webtrans-http3-16 §6)
    ///
    /// エラーコードの扱いは `WtSendStream::abort_session_gone` を参照する。
    fn abort_session_gone(&mut self) {
        let stream_id = self.stream_id;
        match self.stop_sending_with(session_gone_code()) {
            Ok(()) => {
                tracing::debug!(
                    "Aborted WebTransport receive stream {stream_id} with WT_SESSION_GONE"
                );
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to abort WebTransport receive stream {stream_id} with WT_SESSION_GONE: {e}"
                );
            }
        }
    }

    /// エラーコードの種別を指定して受信方向へ STOP_SENDING を送出する
    fn stop_sending_with(&mut self, code: StreamErrorCode) -> Result<()> {
        self.recv
            .stop_sending(stream_application_error(code)?)
            .map_err(TransportError::transport)
    }

    /// ストリーム ID を返す (subscriber 側で使用)
    pub fn stream_id(&self) -> u64 {
        self.stream_id
    }
}

/// WebTransport の双方向ストリーム
pub struct WtBiStream {
    stream_id: u64,
    recv: ReceiveStream,
    send: SendStream,
    pending: Vec<u8>,
    session_state: watch::Receiver<WtSessionState>,
}

impl WtBiStream {
    /// 双方向ストリームを送信側と受信側に分解する
    ///
    /// 分解後の双方がセッション終了を観測できるよう、状態の receiver は clone して渡す。
    pub fn into_parts(self) -> (WtSendStream, WtRecvStream) {
        (
            WtSendStream {
                stream_id: self.stream_id,
                send: self.send,
                session_state: self.session_state.clone(),
            },
            WtRecvStream::new(self.stream_id, self.recv, self.pending, self.session_state),
        )
    }
}

// ---------------------------------------------------------------------------
// ストリームルーティングの判定
// ---------------------------------------------------------------------------

/// ストリームの方向
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamDirection {
    /// 単方向ストリーム
    Uni,
    /// 双方向ストリーム
    Bi,
}

/// デコード結果を方向に依存しない形へ正規化した判定入力
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteInput {
    /// WebTransport のストリーム。`own_session` は自セッションの session ID と一致するか
    WebTransport { own_session: bool },
    /// HTTP/3 のストリーム (制御 / QPACK など、WebTransport 以外)
    Http3,
    /// ヘッダーのデコードにバッファが足りない
    BufferTooShort,
    /// session ID が client-initiated bidirectional stream ID ではない
    InvalidSessionId,
    /// session ID が QUIC のストリーム ID の範囲外
    SessionIdOutOfRange,
    /// 形式が不正 (WebTransport 以外のタイプ / 双方向の signal 値が所定の値でない)
    InvalidFormat,
}

/// ストリームをどこへ流すかの判定結果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteAction {
    /// MOQT 層へ渡す (単方向は `uni_tx`、双方向は `bi_tx`)
    ForwardToMoqt,
    /// HTTP/3 層へ流す (制御 / QPACK ストリームなど)
    ForwardToH3,
    /// 読み捨てる
    Discard,
    /// 追加の受信を待つ
    Continue,
    /// 接続を HTTP/3 のエラーコードで閉じる
    CloseConnection(H3ErrorCode),
}

/// デコード結果から次の動作を決める純関数
///
/// draft-ietf-webtrans-http3-16 §4 (WebTransport Features): session ID は CONNECT ストリームの
/// stream ID 由来であり、常に client-initiated bidirectional stream に対応しなければならない。
/// "If an endpoint receives a session ID on a unidirectional stream, bidirectional stream, or
/// datagram that does not correspond to a client-initiated bidirectional stream ID, the endpoint
/// MUST close the connection with an H3_ID_ERROR error code." に従い、`InvalidSessionId` と
/// `SessionIdOutOfRange` は接続クローズにする (`SessionIdOutOfRange` は受信経路では
/// `StreamHeader::new` からのみ生成されるため到達しないが、網羅性のために扱いを固定する)。
///
/// 他セッションの session ID を持つストリームは §4 の "Session IDs that correspond to closed
/// sessions are not considered invalid for the purposes of this check" により接続を閉じない。
/// 単方向は MOQT 層へ渡さず読み捨て、双方向は HTTP/3 層へ流す (従来動作)。
///
/// datagram 経路は対象外である。依存する shiguredo_http3 は datagram の session ID 不正に
/// H3_DATAGRAM_ERROR を返すため、H3_ID_ERROR にするには crate 側の変更が要る (別 issue)。
fn decide_route(direction: StreamDirection, input: RouteInput) -> RouteAction {
    match input {
        RouteInput::WebTransport { own_session: true } => RouteAction::ForwardToMoqt,
        // 他セッション向けのストリームは MOQT の data stream ではない
        RouteInput::WebTransport { own_session: false } => match direction {
            StreamDirection::Uni => RouteAction::Discard,
            StreamDirection::Bi => RouteAction::ForwardToH3,
        },
        RouteInput::Http3 => RouteAction::ForwardToH3,
        RouteInput::BufferTooShort => RouteAction::Continue,
        RouteInput::InvalidSessionId | RouteInput::SessionIdOutOfRange => {
            RouteAction::CloseConnection(H3ErrorCode::IdError)
        }
        RouteInput::InvalidFormat => RouteAction::Discard,
    }
}

// ---------------------------------------------------------------------------
// 単方向ストリームルーティング
// ---------------------------------------------------------------------------

/// ストリームのルーティングタスクが共有するハンドル
///
/// ルーティングタスクは接続時に spawn され、ストリームごとのタスクへ clone して渡す。
/// `WtSession` はタスクの spawn より後に作られるため、HTTP/3 の状態・通知・接続ハンドル・
/// セッション状態は `WtSession` を経由せず個別に共有する。
#[derive(Clone)]
struct RouteContext {
    /// HTTP/3 の Sans I/O 状態
    state: Arc<StdMutex<ClientConnectionState>>,
    /// h3 層へデータを流したことの通知
    notify: Arc<Notify>,
    /// 接続を HTTP/3 のエラーコードで閉じるためのハンドル
    handle: s2n_quic::connection::Handle,
    /// セッション状態 (§6 の終了 / §4.7 の drain) を配る sender
    ///
    /// ルーティングタスクは自分が h3 層へ流したイベントを `process_h3_events` で処理し、
    /// MOQT 層へ渡す `WtSendStream` / `WtRecvStream` には `subscribe()` で作った receiver を
    /// 渡す。ストリームを所有する側がセッション終了を観測して中断できるようにするためである (§6)。
    session_state: watch::Sender<WtSessionState>,
}

/// 単方向ストリームをタイプで判別してルーティングする
///
/// 自セッション以外の session ID を持つ WebTransport ストリームは MOQT 層へ渡さず読み捨てる
/// (`decide_route`)。CONNECT の stream id が確定するまでは session ID を照合できないため、
/// `session_id` が設定されるまで待ってから判定する。
///
/// 制御 / QPACK ストリーム (`ForwardToH3`) は h3 層へ流し、そのつど `feed_stream_to_h3` で
/// イベントを処理する。drain しないと h3 層がキューへ積んだ `SessionDraining` (HTTP/3 GOAWAY
/// 由来) や `SessionClosed` を誰も取り出さず、publisher 側ではセッション終了を検知できない
/// (publisher は datagram を受信しないため `take_buffered_datagrams` も通らない)。
async fn route_uni_stream(
    stream_id: u64,
    mut recv_stream: ReceiveStream,
    uni_tx: mpsc::Sender<WtRecvStream>,
    session_id: watch::Receiver<Option<u64>>,
    context: RouteContext,
) {
    let mut type_buf: Vec<u8> = Vec::new();
    let mut data_offset: Option<usize> = None;

    let mut action: Option<RouteAction> = None;
    while action.is_none() {
        match recv_stream.receive().await {
            Ok(Some(data)) => {
                type_buf.extend_from_slice(&data);
                let input = match classify_uni_stream_checked(&type_buf) {
                    Ok(ClassifiedUniStream::WebTransport {
                        session_id: stream_session_id,
                        data_offset: offset,
                    }) => {
                        data_offset = Some(offset);
                        let Some(own_session_id) = wait_for_session_id(session_id.clone()).await
                        else {
                            return;
                        };
                        RouteInput::WebTransport {
                            own_session: stream_session_id == own_session_id,
                        }
                    }
                    Ok(ClassifiedUniStream::Http3 { .. }) => RouteInput::Http3,
                    Err(StreamHeaderDecodeError::BufferTooShort) => RouteInput::BufferTooShort,
                    Err(StreamHeaderDecodeError::InvalidSessionId) => RouteInput::InvalidSessionId,
                    Err(StreamHeaderDecodeError::SessionIdOutOfRange) => {
                        RouteInput::SessionIdOutOfRange
                    }
                    // 単方向では WebTransport 以外の stream type がここに来る
                    // (`classify_uni_stream_checked` は `InvalidFormat` を返さない)
                    Err(StreamHeaderDecodeError::InvalidFormat) => RouteInput::InvalidFormat,
                };
                match decide_route(StreamDirection::Uni, input) {
                    RouteAction::Continue => continue,
                    decided => action = Some(decided),
                }
            }
            Ok(None) => {
                // 種別判定の途中で FIN したストリーム (制御 / QPACK など) も h3 層へ伝える。
                // h3 層は FIN を処理してイベントを発行することがある
                let _ =
                    feed_stream_to_h3(&context.state, &context.session_state, stream_id, &[], true);
                context.notify.notify_one();
                return;
            }
            Err(_) => return,
        }
    }

    match action.expect("the classification loop always yields an action") {
        RouteAction::ForwardToMoqt => {
            let offset = data_offset.expect("WebTransport classification records a data offset");
            let pending = type_buf[offset..].to_vec();
            let wt_recv = WtRecvStream::new(
                stream_id,
                recv_stream,
                pending,
                context.session_state.subscribe(),
            );
            let _ = uni_tx.send(wt_recv).await;
        }
        RouteAction::ForwardToH3 => {
            // 制御 / QPACK ストリームは h3 層の管轄である。feed のあとは必ず drain して
            // イベントを処理する (drain しないと GOAWAY 由来の SessionDraining が
            // キューに残り、publisher では誰も取り出さない)
            let _ = feed_stream_to_h3(
                &context.state,
                &context.session_state,
                stream_id,
                &type_buf,
                false,
            );
            context.notify.notify_one();
            while let Ok(Some(data)) = recv_stream.receive().await {
                let _ = feed_stream_to_h3(
                    &context.state,
                    &context.session_state,
                    stream_id,
                    &data,
                    false,
                );
                context.notify.notify_one();
            }
            let _ = feed_stream_to_h3(&context.state, &context.session_state, stream_id, &[], true);
            context.notify.notify_one();
        }
        RouteAction::Discard => {
            // 他セッション向けのストリームは MOQT 層へ渡さず読み捨てる。読み続けないと
            // 受信バッファが滞留してフロー制御で peer が止まるため、FIN まで読み切る
            while let Ok(Some(_)) = recv_stream.receive().await {}
        }
        RouteAction::CloseConnection(code) => {
            close_connection_with_h3_error(&context.handle, code, "unidirectional stream");
        }
        // `decide_route` が `Continue` を返すのはデコード途中のみであり、この時点では返らない
        RouteAction::Continue => {}
    }
}

/// 双方向ストリームをルーティングする
///
/// サーバーが開始する双方向ストリームには WT の双方向ストリームヘッダー
/// (0x41 + session_id) が先頭に付く (draft-ietf-webtrans-http3-16 Section 4.3)。
/// ヘッダーを読み捨てた残りを payload として公開する。
/// 自 session 以外の session_id を持つストリームは MOQT の request stream ではないため
/// HTTP/3 層へ渡す。
async fn route_bi_stream(
    session_id: u64,
    stream_id: u64,
    stream: s2n_quic::stream::BidirectionalStream,
    bi_tx: mpsc::Sender<(WtSendStream, WtRecvStream)>,
    context: RouteContext,
) {
    let (mut recv_stream, send_stream) = stream.split();
    let mut header_buf: Vec<u8> = Vec::new();
    let mut data_offset: Option<usize> = None;

    let mut action: Option<RouteAction> = None;
    while action.is_none() {
        match recv_stream.receive().await {
            Ok(Some(data)) => {
                header_buf.extend_from_slice(&data);
                let input = match StreamHeader::decode_bidirectional_checked(&header_buf) {
                    Ok((header, offset)) => {
                        data_offset = Some(offset);
                        RouteInput::WebTransport {
                            own_session: header.session_id() == session_id,
                        }
                    }
                    Err(StreamHeaderDecodeError::BufferTooShort) => RouteInput::BufferTooShort,
                    Err(StreamHeaderDecodeError::InvalidSessionId) => RouteInput::InvalidSessionId,
                    Err(StreamHeaderDecodeError::SessionIdOutOfRange) => {
                        RouteInput::SessionIdOutOfRange
                    }
                    // 双方向では signal 値が所定の値でない場合にここに来る
                    Err(StreamHeaderDecodeError::InvalidFormat) => RouteInput::InvalidFormat,
                };
                match decide_route(StreamDirection::Bi, input) {
                    RouteAction::Continue => continue,
                    decided => action = Some(decided),
                }
            }
            Ok(None) => return,
            Err(_) => return,
        }
    }

    match action.expect("the classification loop always yields an action") {
        RouteAction::ForwardToMoqt => {
            let offset = data_offset.expect("WebTransport classification records a data offset");
            let pending = header_buf[offset..].to_vec();
            let wt_send = WtSendStream {
                stream_id,
                send: send_stream,
                session_state: context.session_state.subscribe(),
            };
            let wt_recv = WtRecvStream::new(
                stream_id,
                recv_stream,
                pending,
                context.session_state.subscribe(),
            );
            let _ = bi_tx.send((wt_send, wt_recv)).await;
        }
        RouteAction::ForwardToH3 => {
            // 他 session の双方向ストリームは HTTP/3 層の管轄である。feed のあとは必ず
            // drain してイベントを処理する (`route_uni_stream` と同じ理由)
            let _ = feed_stream_to_h3(
                &context.state,
                &context.session_state,
                stream_id,
                &header_buf,
                false,
            );
            context.notify.notify_one();
        }
        RouteAction::CloseConnection(code) => {
            close_connection_with_h3_error(&context.handle, code, "bidirectional stream");
        }
        // 双方向の形式不正は MOQT 層へ渡さず drop する (従来動作)。追加受信待ちはループ内で
        // 消化されるためこの時点では残らない
        RouteAction::Discard | RouteAction::Continue => {}
    }
}

/// CONNECT の stream id (= session ID) が確定するまで待つ
///
/// 単方向ストリームのルーティングタスクは CONNECT より前に spawn されるため、session ID を
/// 共有セルで受け取り、確定するまで照合しない (確定前のストリームを他セッション扱いしないため)。
/// 送信側が drop された場合 (CONNECT の失敗) は `None` を返し、呼び出し側はストリームを捨てる。
async fn wait_for_session_id(mut session_id: watch::Receiver<Option<u64>>) -> Option<u64> {
    let id = session_id.wait_for(|id| id.is_some()).await.ok()?;
    *id
}

/// 接続を HTTP/3 のエラーコードで閉じる
///
/// h3 層は Sans I/O のため CONNECTION_CLOSE の送出は I/O 層 (s2n-quic) が行う。application
/// error code に HTTP/3 のエラーコードを渡す。draft-ietf-webtrans-http3-16 §4 の H3_ID_ERROR は
/// `ErrorCode::IdError` であり、数値は example 側で再定義しない。
fn close_connection_with_h3_error(
    handle: &s2n_quic::connection::Handle,
    code: H3ErrorCode,
    stream_kind: &str,
) {
    tracing::warn!(
        code = code.code(),
        stream_kind,
        "closing connection with HTTP/3 error code"
    );
    // HTTP/3 のエラーコードは QUIC の application error code の範囲に収まるため失敗しない。
    // 失敗時に別のコードで閉じると MUST の通知が変わってしまうため、ここで止める
    let error = s2n_quic::application::Error::new(code.code())
        .expect("HTTP/3 error code fits in the QUIC application error code range");
    handle.close(error);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// WT_APPLICATION_ERROR 範囲外のプロトコルエラーコード (draft-ietf-webtrans-http3-16 §9.5)
    const WT_SESSION_GONE: u64 = shiguredo_http3::webtransport::ErrorCode::SessionGone as u64;

    /// 予約コードポイント (0x1f * N + 0x21) のうち WT_APPLICATION_ERROR 範囲に入る最初の値
    ///
    /// draft-ietf-webtrans-http3-16 §4.4 は予約コードポイントをアプリケーションエラーコードの
    /// remap 対象外とする。`ApplicationErrorCode::to_http3_code` は `n / 0x1e` の加算で
    /// 予約コードポイントを飛ばすため、変換結果がこの値になることはない。
    fn first_reserved_code_point_in_range() -> u64 {
        let base = ApplicationErrorCode::FIRST - 0x21;
        base.next_multiple_of(0x1f) + 0x21
    }

    /// `tracing::warn!` の回数とメッセージを記録するテスト用サブスクライバ
    ///
    /// 観測専用であり、変換関数の処理や戻り値には関与しない (テスト対象を置き換えない)。
    struct WarnRecorder {
        count: Arc<AtomicUsize>,
        messages: Arc<StdMutex<Vec<String>>>,
    }

    impl tracing::Subscriber for WarnRecorder {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            if *event.metadata().level() == tracing::Level::WARN {
                self.count.fetch_add(1, Ordering::SeqCst);
                let mut collector = MessageCollector::default();
                event.record(&mut collector);
                self.messages
                    .lock()
                    .expect("warn のメッセージを記録できること")
                    .push(collector.message);
            }
        }

        fn enter(&self, _span: &tracing::span::Id) {}

        fn exit(&self, _span: &tracing::span::Id) {}
    }

    /// `tracing` のイベントから `message` フィールドを取り出す
    #[derive(Default)]
    struct MessageCollector {
        message: String,
    }

    impl tracing::field::Visit for MessageCollector {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.message = format!("{value:?}");
            }
        }
    }

    /// warn を記録するサブスクライバを設定して関数を実行し、結果と記録内容を返す
    fn with_warn_recorder<T>(f: impl FnOnce() -> T) -> (T, usize, Vec<String>) {
        let count = Arc::new(AtomicUsize::new(0));
        let messages = Arc::new(StdMutex::new(Vec::new()));
        let recorder = WarnRecorder {
            count: Arc::clone(&count),
            messages: Arc::clone(&messages),
        };
        let returned = tracing::subscriber::with_default(recorder, f);
        let recorded = messages
            .lock()
            .expect("warn のメッセージを取得できること")
            .clone();
        (returned, count.load(Ordering::SeqCst), recorded)
    }

    /// 1024 バイト以下はそのまま返す
    #[test]
    fn truncate_close_session_message_keeps_short_message() {
        assert_eq!(truncate_close_session_message("ok"), "ok");
        assert_eq!(
            truncate_close_session_message(&"a".repeat(1024)).len(),
            1024
        );
    }

    /// ASCII のみで 1024 を超える場合はちょうど 1024 バイトに切る
    #[test]
    fn truncate_close_session_message_cuts_ascii_at_limit() {
        let message = "a".repeat(1025);
        let truncated = truncate_close_session_message(&message);
        assert_eq!(truncated.len(), 1024);
        assert!(truncated.is_char_boundary(truncated.len()));
    }

    /// 1024 バイト境界がマルチバイト文字の途中になる場合は文字境界まで戻す
    ///
    /// draft-ietf-webtrans-http3-16 §6: truncate は UTF-8 character boundary で行う MUST。
    #[test]
    fn truncate_close_session_message_respects_utf8_boundary() {
        // 'あ' は 3 バイト。1022 バイトの ASCII の直後に置くと 1024 が文字の途中になる
        let mut message = "a".repeat(1022);
        message.push('あ'); // bytes 1022..1025
        message.push('あ');
        let truncated = truncate_close_session_message(&message);
        assert_eq!(truncated.len(), 1022);
        assert!(truncated.is_char_boundary(truncated.len()));
        assert!(!truncated.contains('あ'));
    }

    /// MOQT の登録コードを remap すると往復して元の値に戻る (draft-ietf-webtrans-http3-16 §4.4)
    #[test]
    fn moqt_to_wt_code_round_trips_registered_codes() {
        for moqt_code in [0x0, 0x1, 0x12] {
            let http3_code = moqt_to_wt_code(moqt_code).expect("remap に成功すること");
            assert!(
                ApplicationErrorCode::is_application_error(http3_code),
                "{moqt_code:#x} の remap 結果 {http3_code:#x} が WT_APPLICATION_ERROR の範囲に入ること"
            );
            assert_eq!(
                wt_to_moqt_code(http3_code),
                Some(moqt_code as u32),
                "{moqt_code:#x} が往復して戻ること"
            );
        }
    }

    /// §4.4 のアンカー (0x00000000 / 0xffffffff の対応) をリテラルで固定する
    #[test]
    fn code_conversion_matches_specification_anchors() {
        assert_eq!(
            moqt_to_wt_code(0x00000000).expect("remap に成功すること"),
            0x52e4a40fa8db,
            "0x00000000 が WT_APPLICATION_ERROR の先頭に対応すること"
        );
        assert_eq!(
            moqt_to_wt_code(0xffffffff).expect("remap に成功すること"),
            0x52e5ac983162,
            "0xffffffff が WT_APPLICATION_ERROR の末尾に対応すること"
        );
        assert_eq!(
            wt_to_moqt_code(0x52e4a40fa8db),
            Some(0x00000000),
            "WT_APPLICATION_ERROR の先頭が 0x00000000 に戻ること"
        );
        assert_eq!(
            wt_to_moqt_code(0x52e5ac983162),
            Some(0xffffffff),
            "WT_APPLICATION_ERROR の末尾が 0xffffffff に戻ること"
        );
    }

    /// §4.4 の変換の境界値を remap しても往復して元の値に戻る
    #[test]
    fn moqt_to_wt_code_round_trips_boundary_codes() {
        for moqt_code in [0x1e_u64, 0x1f, u64::from(u32::MAX)] {
            let http3_code = moqt_to_wt_code(moqt_code).expect("remap に成功すること");
            assert!(
                ApplicationErrorCode::is_application_error(http3_code),
                "{moqt_code:#x} の remap 結果 {http3_code:#x} が WT_APPLICATION_ERROR の範囲に入ること"
            );
            assert_ne!(
                (http3_code - 0x21) % 0x1f,
                0,
                "{moqt_code:#x} の remap 結果が予約コードポイントにならないこと"
            );
            assert_eq!(
                wt_to_moqt_code(http3_code),
                Some(moqt_code as u32),
                "{moqt_code:#x} が往復して戻ること"
            );
        }
    }

    /// remap の結果は予約コードポイントにならない
    #[test]
    fn moqt_to_wt_code_never_produces_reserved_code_points() {
        let reserved = first_reserved_code_point_in_range();
        assert_eq!(
            (reserved - 0x21) % 0x1f,
            0,
            "{reserved:#x} が予約コードポイントの式 (0x1f * N + 0x21) を満たすこと"
        );
        assert_eq!(
            wt_to_moqt_code(reserved),
            None,
            "予約コードポイント {reserved:#x} は remap できないこと"
        );
        // 予約コードポイントは 0x1f 個ごとに 1 点現れるため、写像の 1 周期より広い範囲を全数確認する
        for moqt_code in 0..=0x1000_u64 {
            let http3_code = moqt_to_wt_code(moqt_code).expect("remap に成功すること");
            let nearest_reserved = ((http3_code - 0x21) / 0x1f) * 0x1f + 0x21;
            assert_ne!(
                http3_code, nearest_reserved,
                "{moqt_code:#x} の remap 結果 {http3_code:#x} が予約コードポイントでないこと"
            );
            assert!(
                ApplicationErrorCode::is_application_error(http3_code),
                "{moqt_code:#x} の remap 結果 {http3_code:#x} が範囲内であること"
            );
        }
        assert!(
            reserved < moqt_to_wt_code(0x1000).expect("remap に成功すること"),
            "確認範囲が最初の予約コードポイント {reserved:#x} を跨いでいること"
        );
    }

    /// WT_APPLICATION_ERROR の範囲外のコードは remap できない (draft-ietf-webtrans-http3-16 §4.4)
    #[test]
    fn wt_to_moqt_code_rejects_codes_outside_application_range() {
        for http3_code in [
            // セッション終了などのプロトコルエラーコード
            WT_SESSION_GONE,
            // WebTransport 拡張ではない任意の HTTP/3 コード
            0x12345678,
            // 範囲の直前と直後
            ApplicationErrorCode::FIRST - 1,
            ApplicationErrorCode::LAST + 1,
        ] {
            assert_eq!(
                wt_to_moqt_code(http3_code),
                None,
                "{http3_code:#x} は remap できないこと"
            );
        }
    }

    /// 範囲内のコードは MOQT のコードに戻り、範囲外は warn を出して生値のまま返る
    #[test]
    fn wt_reset_error_code_returns_moqt_code_or_passes_through() {
        let http3_code = moqt_to_wt_code(0x1).expect("remap に成功すること");
        let (returned, count, messages) = with_warn_recorder(|| {
            [
                wt_reset_error_code(http3_code),
                wt_reset_error_code(WT_SESSION_GONE),
                wt_reset_error_code(0x12345678),
            ]
        });
        assert_eq!(
            returned,
            [0x1, WT_SESSION_GONE, 0x12345678],
            "remap できたコードは MOQT のコードになり、できないコードは wire の値のまま返ること"
        );
        assert_eq!(
            count, 2,
            "remap できないコードごとに warn が 1 回だけ出ること (remap できたコードでは出ないこと)"
        );
        assert_eq!(
            messages,
            [WT_SESSION_GONE, 0x12345678].map(|http3_code| format!(
                "RESET_STREAM received with an HTTP/3 error code outside the WebTransport application error range: {http3_code:#x}"
            )),
            "範囲外の warn が生値を含み、予約コードポイントの文言と異なること"
        );
    }

    /// 予約コードポイントは範囲外と区別できる warn になる
    #[test]
    fn wt_reset_error_code_warns_reserved_code_points_separately() {
        let reserved = first_reserved_code_point_in_range();
        let (returned, count, messages) = with_warn_recorder(|| wt_reset_error_code(reserved));
        assert_eq!(returned, reserved, "予約コードポイントが生値のまま返ること");
        assert_eq!(count, 1, "予約コードポイントで warn が 1 回出ること");
        assert_eq!(
            messages,
            [format!(
                "RESET_STREAM received with a reserved HTTP/3 error code point: {reserved:#x}"
            )],
            "予約コードポイントを範囲外と表現しないこと"
        );
    }

    /// `recv_chunk` が組み立てる `RequestStreamEnd::Reset` に remap 後の値が入る
    #[test]
    fn wt_reset_stream_end_uses_remapped_code() {
        let http3_code = moqt_to_wt_code(0x12).expect("remap に成功すること");
        assert_eq!(
            wt_reset_stream_end(http3_code),
            RequestStreamEnd::Reset {
                error_code: 0x12,
                reliable_size: None,
            },
            "remap できたコードが MOQT のコードとして入ること"
        );
        assert_eq!(
            wt_reset_stream_end(WT_SESSION_GONE),
            RequestStreamEnd::Reset {
                error_code: WT_SESSION_GONE,
                reliable_size: None,
            },
            "remap できないコードが生値のまま入ること"
        );
    }

    /// remap した値で s2n-quic のアプリケーションエラーを構築する
    #[test]
    fn stream_application_error_remaps_moqt_codes_before_creating_s2n_error() {
        for (moqt_code, expected) in [
            (0x0_u64, 0x52e4a40fa8db_u64),
            (0x1, 0x52e4a40fa8dc),
            (0x12, 0x52e4a40fa8ed),
            (u64::from(u32::MAX), 0x52e5ac983162),
        ] {
            let error = stream_application_error(StreamErrorCode::Application(moqt_code))
                .expect("remap に成功すること");
            assert_eq!(
                u64::from(error),
                expected,
                "{moqt_code:#x} の wire の値が remap 後の値になること"
            );
        }
        assert!(
            stream_application_error(StreamErrorCode::Application(u64::MAX)).is_err(),
            "32 ビットを超える値は wire に載せないこと"
        );
    }

    /// プロトコルコードは §4.4 の remap を通さず wire の値のまま渡る
    ///
    /// `WT_SESSION_GONE` は WebTransport / HTTP/3 のプロトコルコードであり、MOQT の
    /// アプリケーションエラーコードではない (draft-ietf-webtrans-http3-16 §6)。
    /// `StreamErrorCode::Application` として渡すと WT_APPLICATION_ERROR の範囲へ remap され、
    /// 別のコードに化ける。
    #[test]
    fn stream_application_error_keeps_protocol_codes_as_is() {
        let protocol = stream_application_error(session_gone_code()).expect("varint に収まること");
        assert_eq!(
            u64::from(protocol),
            WT_SESSION_GONE,
            "WT_SESSION_GONE が remap されずに wire へ載ること"
        );

        let application = stream_application_error(StreamErrorCode::Application(WT_SESSION_GONE))
            .expect("remap に成功すること");
        assert_ne!(
            u64::from(application),
            WT_SESSION_GONE,
            "アプリケーションエラーコードとして渡すと remap されて別の値になること"
        );
    }

    /// `RESET_STREAM` の受信エラーを remap した `RequestStreamEnd::Reset` に変換する
    ///
    /// `StreamError` の variant は `#[non_exhaustive]` で、接続なしに `RESET_STREAM` の値を組み立てるには
    /// s2n-quic が公開している `stream_reset` を使うしかない (ドキュメント非公開の補助 API)。
    #[test]
    fn wt_recv_end_maps_stream_reset_to_remapped_request_stream_end() {
        for (http3_code, expected) in [
            (
                moqt_to_wt_code(0x12).expect("remap に成功すること"),
                0x12_u64,
            ),
            (WT_SESSION_GONE, WT_SESSION_GONE),
        ] {
            let application_error =
                s2n_quic::application::Error::new(http3_code).expect("varint に収まること");
            let end = wt_recv_end(s2n_quic::stream::Error::stream_reset(application_error))
                .expect("End に変換できること");
            assert_eq!(
                end,
                RecvChunk::End(RequestStreamEnd::Reset {
                    error_code: expected,
                    reliable_size: None,
                }),
                "wire の {http3_code:#x} が {expected:#x} として入ること"
            );
        }
    }

    /// `RESET_STREAM` 以外のストリームエラーはエラーのまま返る
    #[test]
    fn wt_recv_end_keeps_other_stream_errors() {
        let error = wt_recv_end(s2n_quic::stream::Error::send_after_finish())
            .expect_err("エラーになること");
        assert!(
            matches!(error, TransportError::Transport(_)),
            "ストリームエラーとして返ること: {error:?}"
        );
    }

    /// 32 ビットに収まらない MOQT のコードは切り捨てずエラーにする
    ///
    /// エラーメッセージの全文は実装の写しになるため固定せず、種別と元のコードだけを見る。
    #[test]
    fn moqt_to_wt_code_rejects_values_beyond_32_bits() {
        for moqt_code in [u64::from(u32::MAX) + 1, u64::MAX] {
            let error = moqt_to_wt_code(moqt_code).expect_err("エラーになること");
            assert!(
                matches!(error, TransportError::Internal(_)),
                "内部エラーとして返ること: {error:?}"
            );
            assert!(
                error.to_string().contains(&format!("{moqt_code:#x}")),
                "{moqt_code:#x} のエラーメッセージに元のコードが含まれること: {error}"
            );
        }
    }

    /// ストリームのルーティング判定 (draft-ietf-webtrans-http3-16 §4)
    ///
    /// session ID は CONNECT ストリームの ID 由来であり、client-initiated bidirectional stream
    /// に対応しない session ID を受けたエンドポイントは H3_ID_ERROR で接続を閉じなければならない。
    #[test]
    fn decide_route_closes_connection_for_invalid_session_id() {
        for input in [
            RouteInput::InvalidSessionId,
            RouteInput::SessionIdOutOfRange,
        ] {
            for direction in [StreamDirection::Uni, StreamDirection::Bi] {
                assert_eq!(
                    decide_route(direction, input),
                    RouteAction::CloseConnection(H3ErrorCode::IdError),
                    "{direction:?} の {input:?} は H3_ID_ERROR で接続を閉じること"
                );
            }
        }
        // 数値は example 側で再定義しない (spec 側の値を固定する)
        assert_eq!(H3ErrorCode::IdError.code(), 0x108);
    }

    /// WebTransport 以外とデコード途中の入力の扱い
    #[test]
    fn decide_route_continues_and_discards() {
        for direction in [StreamDirection::Uni, StreamDirection::Bi] {
            assert_eq!(
                decide_route(direction, RouteInput::BufferTooShort),
                RouteAction::Continue,
                "{direction:?} は追加受信を待つこと"
            );
            assert_eq!(
                decide_route(direction, RouteInput::InvalidFormat),
                RouteAction::Discard,
                "{direction:?} の形式不正は読み捨てること"
            );
            assert_eq!(
                decide_route(direction, RouteInput::Http3),
                RouteAction::ForwardToH3,
                "{direction:?} の HTTP/3 ストリームは HTTP/3 層へ渡すこと"
            );
        }
    }

    /// 自セッションのストリームは MOQT 層へ、他セッションのストリームは MOQT 層へ渡さない
    #[test]
    fn decide_route_handles_own_and_other_session() {
        for direction in [StreamDirection::Uni, StreamDirection::Bi] {
            assert_eq!(
                decide_route(direction, RouteInput::WebTransport { own_session: true }),
                RouteAction::ForwardToMoqt,
                "{direction:?} の自セッションのストリームは MOQT 層へ渡すこと"
            );
        }
        // 他セッション向けの単方向ストリームは MOQT 層へ渡さず読み捨てる
        assert_eq!(
            decide_route(
                StreamDirection::Uni,
                RouteInput::WebTransport { own_session: false }
            ),
            RouteAction::Discard,
            "他セッション向けの単方向ストリームを MOQT 層へ渡さないこと"
        );
        // 他セッション向けの双方向ストリームは HTTP/3 層の管轄である (従来動作)
        assert_eq!(
            decide_route(
                StreamDirection::Bi,
                RouteInput::WebTransport { own_session: false }
            ),
            RouteAction::ForwardToH3,
            "他セッション向けの双方向ストリームは HTTP/3 層へ渡すこと"
        );
    }

    /// セッション状態ごとの動作 (draft-ietf-webtrans-http3-16 §6 / §4.7)
    ///
    /// §6 の終了 (peer からの終了通知 / 自発 close) は全ストリームの中断と新規拒否、
    /// §4.7 の drain は新規拒否のみである。drain でストリームを中断すると
    /// "an endpoint MAY continue using the session" を潰す。
    #[test]
    fn session_policy_separates_termination_from_draining() {
        assert_eq!(
            session_policy(WtSessionState::Active),
            SessionPolicy {
                reject_new_streams: false,
                reject_datagrams: false,
                abort_streams: false,
            },
            "確立済みのセッションはすべての操作を許すこと"
        );
        assert_eq!(
            session_policy(WtSessionState::Draining),
            SessionPolicy {
                reject_new_streams: true,
                reject_datagrams: true,
                abort_streams: false,
            },
            "drain では新規ストリームと datagram だけを拒否し、既存ストリームは中断しないこと"
        );
        for state in [WtSessionState::ClosedByPeer, WtSessionState::ClosedLocally] {
            assert_eq!(
                session_policy(state),
                SessionPolicy {
                    reject_new_streams: true,
                    reject_datagrams: true,
                    abort_streams: true,
                },
                "{state:?} (終了) では新規拒否と全ストリームの中断を行うこと"
            );
        }
    }

    /// 状態は終了方向にしか進まず、終了後に届いた drain で戻らない
    #[test]
    fn update_session_state_never_goes_backwards() {
        let (state_tx, state_rx) = watch::channel(WtSessionState::Active);
        assert!(
            !state_rx.has_changed().expect("sender が生きていること"),
            "初期状態は変更として通知されないこと"
        );

        update_session_state(&state_tx, WtSessionState::Draining);
        assert_eq!(*state_rx.borrow(), WtSessionState::Draining);
        assert!(
            state_rx.has_changed().expect("sender が生きていること"),
            "drain への遷移が通知されること"
        );

        update_session_state(&state_tx, WtSessionState::ClosedByPeer);
        assert_eq!(*state_rx.borrow(), WtSessionState::ClosedByPeer);

        // 終了後に遅れて届いた drain や自発 close では状態を戻さない
        update_session_state(&state_tx, WtSessionState::Draining);
        assert_eq!(
            *state_rx.borrow(),
            WtSessionState::ClosedByPeer,
            "終了後に届いた drain で戻らないこと"
        );
        update_session_state(&state_tx, WtSessionState::ClosedLocally);
        assert_eq!(
            *state_rx.borrow(),
            WtSessionState::ClosedByPeer,
            "同じ段階の終了で上書きしないこと"
        );
    }

    /// セッション状態を終了へ遷移させると watch で観測でき、中断には WT_SESSION_GONE を渡す
    ///
    /// ストリームを所有するタスクは watch の変化を観測して自分のストリームを中断する (§6)。
    /// ストリーム自体は s2n-quic の I/O ハンドルを持つため、I/O を持たない範囲
    /// (状態の遷移と観測、中断に使うエラーコード) を固定する。
    ///
    /// このテストが保証するのは「終了済みの状態を観測できる」ことである。`watch::Receiver::wait_for`
    /// は待機に入るときに現在値で述語を評価し、既に真であれば待たずに返るため、「遷移で待機タスクが
    /// 起こされる」ことはここでは保証しない (待機タスクの登録と遷移の順序は実行に依存する)。
    /// drain (§4.7) では中断しないことと、終了の種別ごとに観測できることを合わせて確認する。
    #[tokio::test]
    async fn session_termination_is_observable_and_aborts_with_session_gone() {
        // drain の通知だけでは中断しない。待機は終わらず、状態も drain のままである
        let (drain_tx, mut drain_rx) = watch::channel(WtSessionState::Active);
        update_session_state(&drain_tx, WtSessionState::Draining);
        let waited = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            wait_until_terminated(&mut drain_rx),
        )
        .await;
        assert!(
            waited.is_err(),
            "drain の通知では待機が終わらないこと (中断しないこと)"
        );
        assert_eq!(
            *drain_rx.borrow(),
            WtSessionState::Draining,
            "待機中に状態が変わらないこと"
        );

        // 終了へ遷移したセッションを、別タスクの待機が観測し、その状態でストリームを中断する。
        // 待機タスクの登録と遷移の順序は実行に依存するが、`wait_for` は待機に入るときに現在値で
        // 述語を評価するため、どちらの順序でも終了を観測できる
        let (state_tx, state_rx) = watch::channel(WtSessionState::Active);
        let mut waiting = state_rx.clone();
        let observer = tokio::spawn(async move {
            wait_until_terminated(&mut waiting).await;
            *waiting.borrow()
        });
        update_session_state(&state_tx, WtSessionState::ClosedByPeer);
        let observed = tokio::time::timeout(std::time::Duration::from_secs(1), observer)
            .await
            .expect("終了へ遷移したセッションの観測が終わること")
            .expect("観測タスクが panic しないこと");
        assert_eq!(
            observed,
            WtSessionState::ClosedByPeer,
            "終了へ遷移したことを待機中のタスクが観測できること"
        );
        assert!(
            session_policy(observed).abort_streams,
            "終了を観測したらストリームを中断すること"
        );

        // drain の通知を受け取った後でも、終了 (peer からの終了通知 / 自発 close) へ遷移すれば
        // 待機が終わり、その状態を観測できる
        for state in [WtSessionState::ClosedByPeer, WtSessionState::ClosedLocally] {
            let (state_tx, state_rx) = watch::channel(WtSessionState::Active);
            update_session_state(&state_tx, WtSessionState::Draining);
            update_session_state(&state_tx, state);

            // 終了してから待機を始めても、待たずに観測できる
            let mut waiting = state_rx.clone();
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                wait_until_terminated(&mut waiting),
            )
            .await
            .unwrap_or_else(|_| panic!("終了した {state:?} の観測が終わること"));
            assert_eq!(
                *waiting.borrow(),
                state,
                "観測した状態が {state:?} であること"
            );
            assert!(
                session_policy(*waiting.borrow()).abort_streams,
                "{state:?} を観測したらストリームを中断すること"
            );
        }

        // 中断に渡すコードは WT_SESSION_GONE であり、プロトコルコードの値は仕様側で固定する
        assert_eq!(
            session_gone_code(),
            StreamErrorCode::Protocol(0x170d7b68),
            "中断には WT_SESSION_GONE (0x170d7b68) を渡すこと"
        );
        let error = stream_application_error(session_gone_code()).expect("varint に収まること");
        assert_eq!(
            u64::from(error),
            WT_SESSION_GONE,
            "WT_SESSION_GONE が remap されずに中断のコードになること"
        );
    }

    /// 終了を配る sender が drop されても待機中のタスクが終了として扱う
    ///
    /// セッションを保持する側が消えた場合は状態を観測できないため、待ち続けない。
    #[tokio::test]
    async fn wait_until_terminated_returns_when_sender_is_dropped() {
        let (state_tx, mut state_rx) = watch::channel(WtSessionState::Active);
        drop(state_tx);
        // 終了していなくても sender が drop されていれば返る (タイムアウトしない)
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            wait_until_terminated(&mut state_rx),
        )
        .await
        .expect("sender の drop で待機が終わること");
    }

    /// CONNECT stream の終了イベントがセッション状態へ反映され、datagram は取り出される
    ///
    /// h3 層は CONNECT stream の close (FIN / RESET_STREAM) で `SessionClosed` を、
    /// WT_DRAIN_SESSION / GOAWAY で `SessionDraining` を発火する。どちらも捨てない。
    #[test]
    fn process_h3_events_terminates_session_and_keeps_datagrams() {
        let (state_tx, state_rx) = watch::channel(WtSessionState::Active);
        let datagrams = process_h3_events(
            vec![
                Event::WebTransport(WebTransportEvent::Datagram {
                    session_id: 0,
                    payload: vec![0xaa, 0xbb],
                }),
                Event::WebTransport(WebTransportEvent::SessionClosed {
                    session_id: 0,
                    reset_streams: Vec::new(),
                    error_code: WT_SESSION_GONE,
                    close_error_code: 0,
                    close_message: String::new(),
                }),
                // 終了後に遅れて届いた drain で状態を戻さない
                Event::WebTransport(WebTransportEvent::SessionDraining { session_id: 0 }),
            ],
            &state_tx,
        );
        assert_eq!(
            datagrams,
            vec![vec![0xaa, 0xbb]],
            "datagram の payload が取り出されること"
        );
        assert_eq!(
            *state_rx.borrow(),
            WtSessionState::ClosedByPeer,
            "SessionClosed で終了へ遷移すること"
        );
    }

    /// drain の通知ではセッションを終了させない (§4.7)
    #[test]
    fn process_h3_events_marks_draining_without_terminating() {
        let (state_tx, state_rx) = watch::channel(WtSessionState::Active);
        let datagrams = process_h3_events(
            vec![Event::WebTransport(WebTransportEvent::SessionDraining {
                session_id: 3,
            })],
            &state_tx,
        );
        assert!(
            datagrams.is_empty(),
            "datagram が無いバッチでは空を返すこと"
        );

        let state = *state_rx.borrow();
        assert_eq!(state, WtSessionState::Draining);
        assert!(
            !session_policy(state).abort_streams,
            "drain は終了ではないため既存ストリームを中断しないこと"
        );
    }

    /// 32 ビットに収まる MOQT の close code は切り捨てずにそのまま通る
    ///
    /// 期待値は実装 (`u32::try_from`) の写しにせず、リテラルで固定する。
    #[test]
    fn moqt_close_code_accepts_values_within_32_bits() {
        for (code, expected) in [
            (0_u64, 0_u32),
            (0x1, 0x1),
            (0x12, 0x12),
            (u64::from(u32::MAX), u32::MAX),
        ] {
            assert_eq!(
                moqt_close_code(code).expect("変換に成功すること"),
                expected,
                "{code:#x} が {expected:#x} のまま渡ること"
            );
        }
    }

    /// セッション終了を検知しても、同じバッチで取り出した datagram は捨てない
    ///
    /// 終了直前の datagram を落とすと peer が送った最後の object が届かない。次回の
    /// 呼び出しでは取り出せるイベントが無いため、そこで `ConnectionClosed` を返す。
    #[test]
    fn resolve_buffered_datagrams_keeps_datagrams_received_before_termination() {
        let datagrams = vec![vec![0xaa, 0xbb], vec![0xcc]];
        for state in [
            WtSessionState::Active,
            WtSessionState::Draining,
            WtSessionState::ClosedByPeer,
            WtSessionState::ClosedLocally,
        ] {
            assert_eq!(
                resolve_buffered_datagrams(datagrams.clone(), state).expect("datagram を返すこと"),
                datagrams,
                "{state:?} では取り出した datagram を捨てないこと"
            );
        }

        // 取り出せる datagram が無い場合は、セッション終了を検知していれば受信ループを
        // 待たせない (drain は終了ではないため返さない。§4.7 は datagram の送受信を許す)
        for state in [WtSessionState::ClosedByPeer, WtSessionState::ClosedLocally] {
            assert!(
                matches!(
                    resolve_buffered_datagrams(Vec::new(), state),
                    Err(TransportError::ConnectionClosed)
                ),
                "{state:?} では空の場合に ConnectionClosed を返すこと"
            );
        }
        // 確立済み / drain 中は空でもエラーにしない (datagram がまだ届いていないだけ)
        for state in [WtSessionState::Active, WtSessionState::Draining] {
            assert!(
                resolve_buffered_datagrams(Vec::new(), state)
                    .expect("空を返すこと")
                    .is_empty(),
                "{state:?} では空のまま返すこと"
            );
        }
    }

    /// 32 ビットに収まらない MOQT の close code は切り捨てずエラーにする
    ///
    /// `WT_CLOSE_SESSION` capsule の Application Error Code は 32 ビットである (§6)。
    /// `as u32` で切り捨てると別のコードに化けるため、収まらない場合はエラーにする。
    /// エラーメッセージの全文は実装の写しになるため固定せず、種別と元のコードだけを見る。
    #[test]
    fn moqt_close_code_rejects_values_beyond_32_bits() {
        for code in [u64::from(u32::MAX) + 1, u64::MAX] {
            let error = moqt_close_code(code).expect_err("エラーになること");
            assert!(
                matches!(error, TransportError::Internal(_)),
                "内部エラーとして返ること: {error:?}"
            );
            assert!(
                error.to_string().contains(&format!("{code:#x}")),
                "{code:#x} のエラーメッセージに元のコードが含まれること: {error}"
            );
        }
    }
}
