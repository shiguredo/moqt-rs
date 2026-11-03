//! MOQT セッション状態機械の型定義
//!
//! 1 本の `MOQT Transport Session` に閉じた [`crate::session::core::Session`] 構造体以外の
//! 全 public 型をまとめる。draft-ietf-moq-transport-22 由来の実装のため将来変更される
//! 可能性がある。

use crate::message::{
    ControlMessage, ErrorDetails, ReasonPhrase, common::Location, common::TrackNamespace,
};
use crate::message_parameter::{LocationFilter, MessageParameters, RangeFilterSet};
use crate::track_properties::TrackProperties;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use hashbrown::HashMap;

/// `Transport Session` における endpoint の役割 (draft-ietf-moq-transport-22 §6.2 (Session establishment))
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// セッションを開始する側
    Client,
    /// セッションを受け付ける側
    Server,
}

/// 下位トランスポート種別 (draft-ietf-moq-transport-22 §6.2 (Session establishment))
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// QUIC connection 直接 (moqt:// URI)
    Quic,
    /// WebTransport over HTTP/3
    WebTransport,
    /// QMux (draft-ietf-quic-qmux-02) 経由
    ///
    /// 順序保証された 1 本の双方向バイトストリーム (TLS over TCP) の上で、QUIC version 1 の
    /// ストリーム操作と DATAGRAM 操作を多重化する。draft-ietf-moq-transport-22 §6.2 は
    /// 「TCP など他のトランスポート上で直接動かす方法は定義しない」としており、QMux 上の
    /// MOQT を扱うのは individual draft (`refs/moq/draft-nandakumar-moq-qmux-moqt-00.txt`) で
    /// ある。同 draft は MOQT draft-16 を前提としているため、本実装は QUIC version 1 との
    /// 1 対 1 の対応を採る。
    ///
    /// §9.1.1 (AUTHORITY) / §9.1.2 (PATH) は AUTHORITY / PATH を native QUIC に限るため、
    /// QMux がこれらを送ることは同節の範囲外である (QMux は WebTransport ではない)。
    Qmux,
}

/// この endpoint から見た 1 本の `Transport Session` の状態
/// (draft-ietf-moq-transport-22 §6.3 (Session initialization))
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// 自側の SETUP をイベントとして発行済み、相手側 SETUP 未受信
    LocalSetupSent,
    /// 両側 SETUP 完了
    Established,
    /// セッション終了処理中 (close() 後、CloseSession イベント発行済み、ただしまだ poll されていない)
    Closing,
    /// 完全終了 (CloseSession イベントが poll された後)
    Closed,
}

/// peer が広告した `MAX_CONTROL_MESSAGE_SIZE` の状態
///
/// 自側が制御メッセージを送るときの上限 (送信上限) を表す。
/// draft-ietf-moq-transport-23 §9.1.8 (MAX_CONTROL_MESSAGE_SIZE): 既定値は 65535 で、
/// "Until an endpoint receives the peer's SETUP, it uses the default value, including for
/// SETUP itself."
///
/// 省略 (peer が option を送らなかった) と交渉前 (peer SETUP 未受信) は同じ 65535 だが、
/// 意味が異なるため別の variant として区別する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerControlMessageSizeLimit {
    /// peer の SETUP を受信していない。既定値 [`crate::parameter::DEFAULT_MAX_CONTROL_MESSAGE_SIZE`] を適用する
    BeforeSetup,
    /// peer の SETUP を受信し、MAX_CONTROL_MESSAGE_SIZE を省略していた。既定値
    /// [`crate::parameter::DEFAULT_MAX_CONTROL_MESSAGE_SIZE`] を適用する
    Omitted,
    /// peer の SETUP が MAX_CONTROL_MESSAGE_SIZE で広告した値
    Advertised(u64),
}

impl PeerControlMessageSizeLimit {
    /// 送信に適用する上限値 (バイト) を返す
    pub fn bytes(self) -> u64 {
        match self {
            Self::BeforeSetup | Self::Omitted => crate::parameter::DEFAULT_MAX_CONTROL_MESSAGE_SIZE,
            Self::Advertised(value) => value,
        }
    }
}

/// セッション終了エラー (draft-ietf-moq-transport-22 §16.11.1 (Session Termination Error Codes))
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionError {
    /// Session Termination Error Code (draft-ietf-moq-transport-22 §16.11.1 (Session Termination Error Codes))
    pub code: u64,
    /// 人間が読むための説明 (英語、静的文字列)
    pub reason: &'static str,
}

impl SessionError {
    /// セッション終了エラーを作る
    pub const fn new(code: u64, reason: &'static str) -> Self {
        Self { code, reason }
    }
}

impl core::fmt::Display for SessionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "session error {:#x}: {}", self.code, self.reason)
    }
}

impl core::error::Error for SessionError {}

/// 送信側 API (`send_*`) が拒否される理由を表す型。
///
/// `SessionError` (draft-ietf-moq-transport-22 §16.11.1 由来) とは分離し、wire への流出を防ぐ。
/// アプリは本エラーが持つ値を wire コードとして公開 API に渡してはならない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendRequestError {
    /// peer GOAWAY 受信後の新規リクエスト送信抑制 (draft §9.2 SHOULD NOT)。
    ///
    /// wire コードを持たない。公開 API に渡す必要はない。
    PeerGoawayReceived,
    /// 送信 Object / Track Property が購読フィルタを通らないローカル拒否 (draft §3.3.2 / §3.3.3)。
    ///
    /// wire コードを持たない。公開 API に渡す必要はない (渡しても各レジストリの
    /// `*_INTERNAL_ERROR` に置換される)。
    LocalFilterMismatch,
    /// datagram の delivery timeout 超過によるローカルドロップ (draft §5.2)。
    ///
    /// wire コードを持たない。公開 API に渡す必要はない (渡しても各レジストリの
    /// `*_INTERNAL_ERROR` に置換される)。
    LocalDatagramTimeout,
    /// セッション層由来のエラー (既存の `require_established()` 失敗等)。
    Session(SessionError),
}

impl From<SessionError> for SendRequestError {
    fn from(e: SessionError) -> Self {
        Self::Session(e)
    }
}

impl SendRequestError {
    /// 内部の `SessionError` を参照する (`PeerGoawayReceived` 等のローカルエラーの場合は `None`)
    pub fn as_session_error(&self) -> Option<&SessionError> {
        match self {
            Self::PeerGoawayReceived | Self::LocalFilterMismatch | Self::LocalDatagramTimeout => {
                None
            }
            Self::Session(e) => Some(e),
        }
    }
}

impl core::fmt::Display for SendRequestError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PeerGoawayReceived => {
                write!(
                    f,
                    "new request suppressed: peer GOAWAY received (draft §9.2 SHOULD NOT)"
                )
            }
            Self::LocalFilterMismatch => {
                write!(
                    f,
                    "outgoing object or track properties do not pass subscription filters"
                )
            }
            Self::LocalDatagramTimeout => {
                write!(f, "datagram delivery timeout exceeded, dropping datagram")
            }
            Self::Session(e) => write!(f, "{e}"),
        }
    }
}

impl core::error::Error for SendRequestError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::PeerGoawayReceived | Self::LocalFilterMismatch | Self::LocalDatagramTimeout => {
                None
            }
            Self::Session(e) => Some(e),
        }
    }
}

/// セッション層が生成するイベント
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    /// 制御ストリーム (SETUP / GOAWAY) に書くべきメッセージ
    SendControl(ControlMessage),
    /// 新規 bidi request stream を開いて書くべきメッセージ (Request ID 含む)
    ///
    /// I/O 層はこの Request ID と stream を対応付けて記憶する必要がある
    /// (応答メッセージは wire format に Request ID を含まないため)。
    SendRequest {
        /// 対象 request の Request ID
        request_id: u64,
        /// 書き込む制御メッセージ
        message: ControlMessage,
    },
    /// 既存 bidi request stream (request_id で引く) に書くべきメッセージ
    ///
    /// 応答メッセージ (SUBSCRIBE_OK / PUBLISH_OK / REQUEST_ERROR / PUBLISH_DONE /
    /// REQUEST_OK)、requester 側フォローアップ (REQUEST_UPDATE)、publisher 側通知
    /// (PUBLISH_STATE_NOTIFY) のいずれも含む。
    /// `fin` が true の場合はメッセージ送信後に FIN で閉じる
    /// (draft-ietf-moq-transport-22 §6.4.2.3 (Request Cancellation and Rejection):
    /// "it SHOULD send a REQUEST_ERROR and FIN the stream" / §9.9 (PUBLISH_DONE):
    /// "A publisher sends a PUBLISH_DONE message as the final message before closing
    /// the subscription's bidi stream")。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    SendOnStream {
        /// 対象 request の Request ID
        request_id: u64,
        /// 書き込む制御メッセージ
        message: ControlMessage,
        /// true の場合は送信後に FIN で閉じる
        fin: bool,
    },
    /// セッションが確立した (両側の SETUP 完了)
    Established,
    /// セッションをクローズすべき (I/O 層は接続を閉じる)
    CloseSession(SessionError),
    /// peer からの REQUEST_UPDATE を受信した (アプリは send_request_ok / send_request_error で応答する)
    ///
    /// draft-ietf-moq-transport-23 §9.5 (REQUEST_UPDATE): 複数の更新の処理を合体できるため、
    /// `parameters` は受信した 1 通のパラメータではなく、未応答の先行更新と今回の更新を
    /// 受信順に合体した累積表示である ([`Subscription::pending_update_view`])。
    /// 受信時に拒否を決めた更新についてはイベントを発行しない (応答は Session が自動送出する)。
    RequestUpdateReceived {
        /// 対象 request の Request ID
        request_id: u64,
        /// 受信順に合体した累積パラメータ (FILL_PARAMETERS を含む)
        parameters: MessageParameters,
    },
    /// peer からの PUBLISH_STATE_NOTIFY を受信した
    /// (draft-ietf-moq-transport-22 §9.10 (PUBLISH_STATE_NOTIFY))
    ///
    /// 応答は不要であり、MAX_REQUEST_UPDATES のクレジットも消費しない。
    /// PAUSED / LOCATION_FILTER / LARGEST_OBJECT は session 層で
    /// `Subscription` の状態に反映済み。仕様上は informative
    /// (no action required) だが、本実装は受信側の状態追跡のため反映する。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    PublishStateNotifyReceived {
        /// 対象 request の Request ID
        request_id: u64,
        /// 受信したパラメータ
        parameters: MessageParameters,
    },
    /// peer からの REQUEST_OK を受信した (draft §9.3 (REQUEST_OK): PUBLISH_OK は REQUEST_OK に統一)
    ///
    /// `request_kind` は購読を開始したメッセージ種別 (SUBSCRIBE 起点なら `Subscribe`、
    /// PUBLISH 起点なら `Publish`) を表す。初回応答 (SUBSCRIBE_OK / PUBLISH_OK) と
    /// REQUEST_UPDATE_OK はどちらも同じ `request_kind` になるため (SUBSCRIBE 起点なら
    /// ともに `Subscribe`、PUBLISH 起点ならともに `Publish`)、両者は `request_kind` では
    /// 区別できない。初回応答はその request で最初に受信する REQUEST_OK である。
    ///
    /// `parameters` に載りうるのは、応答の種別ごとの許可集合 (`src/message.rs` の
    /// `SUBSCRIBE_OK_ALLOWED_PARAMS` / `PUBLISH_OK_ALLOWED_PARAMS` /
    /// `REQUEST_UPDATE_OK_ALLOWED_PARAMS` / `TRACK_STATUS_OK_ALLOWED_PARAMS`) にある型だけである。
    /// 現状は EXPIRES (§9.20.16) と LARGEST_OBJECT (§9.20.17) の 2 種で、
    /// REQUEST_OK にその他の型が載ると codec / session 層が PROTOCOL_VIOLATION で閉じる。
    ///
    /// PAUSED (draft-ietf-moq-transport-23 §9.19.18 (PAUSED Parameter)) は REQUEST_OK には
    /// 出現しない。購読の paused 状態は REQUEST_UPDATE の送信時に楽観的に
    /// `Subscription::paused` へ反映され、応答のパラメータから読み取る値ではない。
    ///
    /// TRACK_STATUS_OK が未知の Mandatory Track Property を含む場合は成功として扱わず、
    /// 本イベントの代わりに [`SessionEvent::TrackStatusUnsupported`] を発行する。
    RequestOkReceived {
        /// 対象 request の Request ID
        request_id: u64,
        /// 応答対象の request 種別 (subscription では購読を開始したメッセージ種別)
        request_kind: RequestKind,
        /// 受信したパラメータ
        parameters: MessageParameters,
    },
    /// peer からの TRACK_STATUS_OK が未知の Mandatory Track Property を含んでいた
    ///
    /// draft-ietf-moq-transport-23 §3.7 (Mandatory to Understand Track Properties):
    /// 0x4000-0x7FFF の Mandatory Track Property のうち意味を理解しない型を含む
    /// TRACK_STATUS_OK を受信した endpoint は、その Track を処理してはならない (MUST)。
    /// 本ライブラリはリレーではないため、同節がリレーに求める downstream の TRACK_STATUS への
    /// TRACK_STATUS_ERROR 伝播は行わない。代わりに、応答を受領済みのローカル未対応結果として
    /// [`TrackStatusResponse::Unsupported`] を記録し、本イベントでアプリへ通知する。
    ///
    /// `unsupported_property_types` は §3.7 の 0x4000-0x7FFF のうち、§16.8 (Properties) の
    /// 既知型と §13 (Grease) の GREASE 値を除いた型の一覧である。§10.7 (Immutable
    /// Properties) の "MUST search both" に従い IMMUTABLE_PROPERTIES の内側も探索し、
    /// 昇順・重複なしで並べる。
    ///
    /// 本イベントを発行するとき [`SessionEvent::RequestOkReceived`] は発行しない。
    /// request の応答タイマーは解除され、FIN と
    /// [`Session::forget_track_status`](crate::session::core::Session::forget_track_status) は
    /// TRACK_STATUS_OK を受信した単発要求と同じ条件に従う。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    TrackStatusUnsupported {
        /// 対象 request の Request ID
        request_id: u64,
        /// 未知の Mandatory Track Property の型 (昇順・重複なし)
        unsupported_property_types: Vec<u64>,
    },
    /// peer から FETCH の初回成功応答 (REQUEST_OK、別名 FETCH_OK) を受信した
    ///
    /// draft-ietf-moq-transport-23 §3.2 (Fetch) / §9.3 (REQUEST_OK): FETCH への応答は
    /// REQUEST_OK / REQUEST_ERROR のみ。データストリームの FIN 前後どちらの到着でも
    /// 受理時に発火する。アプリは `Session::fetch` のポーリングなしに再生終了の確定などを
    /// イベント駆動で書ける。
    ///
    /// `end_location` は応答範囲の終端であり、FETCH_OK の LARGEST_OBJECT が要求範囲内なら
    /// その値、そうでなければ要求で明示した End Location になる (§3.2 / §3.2.2
    /// (Gaps in a Fetch Stream))。Track の終端は FETCH_OK では運ばず、FETCH 応答
    /// ストリームの End of Group (0x80) / End of Track (0x81) で受け取る (§11.4.2.2
    /// (Prior Object Fields))。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    FetchOkReceived {
        /// 対象 request の Request ID
        request_id: u64,
        /// 応答範囲の終端 Location
        end_location: Location,
    },
    /// peer からの PUBLISH_DONE を受信した (subscription は Terminated に遷移済み)
    PublishDoneReceived {
        /// 対象 request の Request ID
        request_id: u64,
        /// 終了ステータスコード
        status_code: u64,
        /// publisher が開いた stream 数
        stream_count: u64,
        /// 理由
        reason: ReasonPhrase,
    },
    /// peer からの REQUEST_ERROR を受信した
    ///
    /// draft-ietf-moq-transport-22 §9.4 (REQUEST_ERROR): REQUEST_ERROR は任意の request への失敗応答。
    /// - `Pending*` 状態からの受信: 初回 SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS への
    ///   失敗応答で、対象 request は Terminated に遷移済み。
    /// - `Established` 状態からの受信: REQUEST_UPDATE への失敗応答。
    ///   subscription / fetch は Terminated に遷移する (draft §3.1.2: REQUEST_ERROR の
    ///   受信で subscription state を終える。fetch は draft §3.2.4 の破棄許可を行使する)。
    ///   subscription の publisher role は `send_request_error` 内で Terminated に遷移し、
    ///   PUBLISH_DONE(UPDATE_FAILED) を自動送信する (ワイヤ順序は
    ///   REQUEST_ERROR → PUBLISH_DONE、open 中の outgoing data stream (subgroup / fill fetch)
    ///   がある場合は §9.9 の MUST NOT に従い全 stream 終端後に送信、
    ///   draft-ietf-moq-transport-22 §9.5.1 (Updating Subscriptions))。
    ///   残存 outgoing data stream の FIN / RESET の実行 (Session が自動発行した fill fetch
    ///   stream の reset を含む) はアプリケーション層の責務である。
    RequestErrorReceived {
        /// 対象 request の Request ID
        request_id: u64,
        /// エラーコード
        error_code: u64,
        /// 再送までの最小待ち時間 (ms) に 1 を足した値 (plus one)。
        /// 値 1 は即時リトライ可を表す。
        /// 値 0 は再送すべきでない (SHOULD NOT retry) ことを表す。REDIRECT 応答では
        /// 元のリクエストを as sent で再送すべきでないが、提供された URI への接続または
        /// Redirect target を使った再送 (follow) を妨げない
        /// (draft-ietf-moq-transport-23 §9.4.1 (REQUEST_ERROR Message Format))。
        /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
        retry_interval: u64,
        /// 理由
        reason: ReasonPhrase,
        /// 構造化されたエラー詳細 (draft-ietf-moq-transport-23 §9.4.2 (Error Details))
        ///
        /// Error Code = REDIRECT の場合は REDIRECT Detail を必ず 1 つ含む
        /// (draft-ietf-moq-transport-23 §9.4.2.1 (REDIRECT))。Error Code =
        /// UNSUPPORTED_EXTENSION の場合は未対応の Mandatory Track Property の型ごとに
        /// UNSUPPORTED_PROPERTY Detail を含みうる
        /// (draft-ietf-moq-transport-23 §9.4.2.2 (UNSUPPORTED_PROPERTY))。
        /// 未知の Detail と、適用されない Error Code の Detail は受信時に無視されるため
        /// 含まれない (§9.4.2)。REDIRECT の内容は
        /// [`ErrorDetails::redirect`](crate::message::ErrorDetails::redirect)、未対応の型は
        /// [`ErrorDetails::unsupported_properties`](crate::message::ErrorDetails::unsupported_properties)
        /// で取得する。
        error_details: ErrorDetails,
    },
    /// bidi request stream の終端 (または REQUEST_ERROR 等による) で request が
    /// Terminated 状態に遷移した (draft-ietf-moq-transport-22 §6.4.2.3 (Request Cancellation and Rejection))
    ///
    /// `reason` に応じて:
    /// - `PeerStreamFin` / `PeerStreamReset`: 相手側が bidi request stream を閉じた
    /// - `LocalCancel`: 自側から cancel した
    /// - `SupersededByPublish`: PUBLISH 受信で既存 Pending(Subscriber) が置き換えられた (draft §3.1 (Subscriptions))
    /// - `MalformedTrack`: Malformed Track 検出で該当 request を cancel した (draft §12.1 (Malformed Tracks))
    /// - `GoawayTimeout`: request stream 上の GOAWAY の timeout 満了で自側が送信方向を
    ///   reset し、request を終端した (draft §9.2 (GOAWAY))。既に終端済みの request には
    ///   発行されない (reset イベントのみが発行される)
    ///
    /// `PeerStreamFin` は相手側の FIN の到着と同時に発行されるとは限らない。bidi request
    /// stream は方向ごとに独立に閉じるため (draft §6.4.2.2 (Graceful Request Stream
    /// Closure))、自側が SUBSCRIBE / FETCH / TRACK_STATUS の responder、または PUBLISH を
    /// 送った側 (publisher 役) で `Established` の場合は peer の FIN と自側が最終メッセージと
    /// ともに送る FIN の両方が揃った時点で発行される (FIN の到着順に依存しない)。
    /// `GoawayTimeout` で終端した後に peer が GOAWAY / reset を観測する前に送ったメッセージ
    /// (REQUEST_UPDATE / PUBLISH_DONE / PUBLISH_STATE_NOTIFY 等) は Session が no-op で吸収する
    /// ため、本イベントが再度発行されることはない。
    RequestTerminated {
        /// 対象 request の Request ID
        request_id: u64,
        /// request の種別
        kind: RequestKind,
        /// 理由
        reason: TerminationReason,
    },
    /// peer からの GOAWAY を受信した (draft-ietf-moq-transport-22 §9.2 (GOAWAY))
    GoawayReceived {
        /// 移行先セッション URI
        new_session_uri: Vec<u8>,
        /// 猶予時間 (ms)
        timeout: u64,
        /// GOAWAY を受信したリクエストストリームの request_id
        /// `None` = control stream, `Some(id)` = request stream
        /// (ワイヤ上の Request ID ではなく、セッション層が受信ストリームを識別する値)
        on_request_stream: Option<u64>,
    },
    /// 自側が送信した GOAWAY の drain が完了した
    ///
    /// draft-ietf-moq-transport-22 §6.6.1 (Graceful Session Migration): GOAWAY を送った側は、
    /// open request が無くなった時点で NO_ERROR でセッションを閉じるのが RECOMMENDED である。
    /// [`Session::goaway_drain_ready`](crate::session::core::Session::goaway_drain_ready) を
    /// ポーリングする代わりに、このイベントで早期 close の契機を得る。同じセッションで
    /// 1 回だけ発行する。
    GoawayDrainReady,
    /// パディングストリーム (全 0x00 ペイロード) を送信する (draft §11.5.1 (Padding Streams))
    ///
    /// I/O 層は stream type `PADDING_STREAM_TYPE` の単方向ストリームを開き、
    /// `length` バイトの 0x00 を書き込んで閉じる。
    SendPaddingStream {
        /// パディングのバイト長
        length: u64,
    },
    /// パディングデータグラム (全 0x00 ペイロード) を送信する (draft §11.5.2 (Padding Datagrams))
    ///
    /// I/O 層は datagram type `PADDING_DATAGRAM_TYPE` を先頭 varint として、
    /// `length` バイトの 0x00 をペイロードとするデータグラムを送信する。
    SendPaddingDatagram {
        /// パディングのバイト長
        length: u64,
    },
    /// データストリームをデリバリータイムアウトでリセットする (draft §5.2 (Delivery Timeouts and Data Reliability))
    ///
    /// I/O 層は `stream_id` に相当する uni data stream を RESET_STREAM で閉じる。
    /// `error_code` は [`STREAM_DELIVERY_TIMEOUT`](crate::error::STREAM_DELIVERY_TIMEOUT) (0x2) を使用する。
    /// `reliable_size` が `Some(n)` の場合は RESET_STREAM_AT を `n` で発行し、
    /// `None` の場合は従来の RESET_STREAM を発行する
    /// (draft-ietf-moq-transport-22 §11.3.2 (Closing Subgroup Streams))。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    ResetDataStream {
        /// 対象 data stream の ID
        stream_id: DataStreamId,
        /// エラーコード
        error_code: u64,
        /// RESET_STREAM_AT の reliable size (`None` は RESET_STREAM)
        reliable_size: Option<u64>,
    },
    /// 受信 data stream に STOP_SENDING を送る (draft-ietf-moq-transport-22 §12.2 (Session Termination Codes))
    ///
    /// I/O 層は `stream_id` に相当する uni data stream の受信方向に `error_code` で
    /// STOP_SENDING を送る。`error_code` は §12.5 (Stream Reset Error Codes) のコードを使う。
    /// Session が本イベントを発行するのは、open な subgroup stream で新しい object header を
    /// 待って DATA_STREAM_TIMEOUT (0x12) の期限に達した場合と、subscriber 役 subscription への
    /// REQUEST_ERROR 送信で当該 subscription を終端した場合 (draft §9.8 (PUBLISH) の
    /// "abandon reading any publisher initiated streams associated with that subscription using
    /// a STOP_SENDING frame") である。前者の `error_code` は `STREAM_CANCELLED` を使い、§12.2 は
    /// この場合の措置を
    /// "it MAY send a STOP_SENDING on that stream or terminate the subscription" と定め、
    /// セッション全体のクローズを挙げていない (§6.6 (Termination) は単一購読の条件を
    /// セッションエラーへ昇格させることを MAY で認めているため、クローズも仕様違反ではない。
    /// 他の購読への影響を避けるため本実装は STOP_SENDING を選ぶ)。
    /// 購読は生かしたまま timeout した stream だけを止め、以後の Group の stream は通常どおり
    /// 受信する (§11.3.2 (Closing Subgroup Streams): "Resets and STOP_SENDING on SUBSCRIBE data
    /// streams have no impact on other Subgroups in the Group or the subscription")。
    /// timeout 後に同じ stream へ届く Object / FIN / RESET_STREAM / 遅延 SUBGROUP_HEADER は
    /// 破棄対象として no-op で吸収する (吸収は `peer_alias_retention_ms` の保持期間の間であり、
    /// 期限切れ後は unknown stream id の `SESSION_PROTOCOL_VIOLATION` になる)。FETCH stream は
    /// §12.2 に代替措置が無いため現行どおり 0x12 でセッションを閉じる。
    /// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    StopSendingDataStream {
        /// 対象 data stream の ID
        stream_id: DataStreamId,
        /// Stream Reset Error Code (§12.5)
        error_code: u64,
    },
    /// fill fetch stream を開く (draft-ietf-moq-transport-22 §3.4 (Fill Semantics))
    ///
    /// publisher が FILL_PARAMETERS 付き SUBSCRIBE / REQUEST_UPDATE を unpaused で
    /// 処理したとき、または FILL_PARAMETERS 付き PUBLISH を unpaused で送るとき
    /// (draft-ietf-moq-transport-23 §3.4.1 (Opening and Closing Fill Fetch Streams)) に、
    /// fill range が空でなく Largest Object より後に始まらない場合に発火する。
    /// 開設するのは publisher だけであり、PUBLISH を受けて PUBLISH_OK を返す subscriber は
    /// 発火しない (自側は publisher が開いた fill fetch stream を
    /// [`Session::recv_fetch_header`](crate::session::core::Session::recv_fetch_header) で
    /// 受理する側である)。
    /// I/O 層は uni stream を開き、先頭に `request_id` を載せた FETCH_HEADER を書いて
    /// fill range の Object 送出を開始し、送り終わったら FIN で閉じる
    /// (失敗時は FETCH_HEADER の直後で即 reset する)。
    /// アプリは開いた stream を [`Session::send_fill_fetch_header`](crate::session::core::Session::send_fill_fetch_header)
    /// にそのまま `request_id` を渡して登録すること。
    /// `request_id` は起因メッセージの Request ID である。SUBSCRIBE / PUBLISH 起因なら
    /// subscription の Request ID、REQUEST_UPDATE 起因なら REQUEST_UPDATE 自身の
    /// Request ID (draft-ietf-moq-transport-22 §6.4.2.1 (Request ID)) であり、
    /// 同じ subscription でも両者は一致しない。
    /// 1 つの subscription に複数本の fill stream が
    /// 同時に開くことがあり、いずれも同じ `request_id` (起因メッセージのもの) を載せる。
    /// fill range が empty または Largest Object より後に始まる場合は発火しない。
    /// subscription のキャンセル時は開いた fill stream を Session が
    /// `ResetDataStream` で自動 reset する。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    OpenFillFetchStream {
        /// fill 起因の request の Request ID (FETCH_HEADER に載せる値)
        request_id: u64,
        /// fill 対象 subscription の Request ID
        ///
        /// 起因メッセージが REQUEST_UPDATE の場合は [`Self::OpenFillFetchStream::request_id`]
        /// と一致しない (§6.4.2.1 (Request ID))。対象 track の識別に使う。
        subscription_request_id: u64,
        /// fill range の開始 Location
        start: Location,
        /// fill range の終端 Location
        ///
        /// 開いた範囲 (終端の指定が無い filter) は開設時点で self 側が知る Largest Object で
        /// 解決した値。PUBLISH 起源では PUBLISH の LARGEST_OBJECT (アプリ指定値と観測値の
        /// 大きい方) である (draft-ietf-moq-transport-23 §3.4 (Fill Semantics))。
        end: Location,
    },
    /// bidi request stream の送信方向を RESET_STREAM で打ち切る
    ///
    /// draft-ietf-moq-transport-22 §6.4.2.3 (Request Cancellation and Rejection):
    /// "Implementations cancel a request by abruptly terminating any directions of the
    /// stream that are still open, using RESET_STREAM for a direction they are sending
    /// and STOP_SENDING for a direction they are receiving."
    /// I/O 層は `request_id` に対応する bidi stream の送信方向を `error_code` で reset する。
    /// `error_code` は §12.5 (Stream Reset Error Codes) のコードを使う。
    /// Session が本イベントを発行するのは、Malformed Track 検出時 (§12.1 (Malformed Tracks)、
    /// [`STREAM_MALFORMED_TRACK`](crate::error::STREAM_MALFORMED_TRACK)) と、未知の
    /// Mandatory Track Property を含む SUBSCRIBE_OK / FETCH_OK の受信時
    /// (§3.7 (Mandatory to Understand Track Properties) の cancel、
    /// [`STREAM_CANCELLED`](crate::error::STREAM_CANCELLED)) である。どちらも
    /// `StopSendingRequestStream` と対で発行する。加えて、request stream 上の GOAWAY の
    /// timeout 満了時 (§9.2 (GOAWAY)、[`STREAM_GOING_AWAY`](crate::error::STREAM_GOING_AWAY)) は
    /// `StopSendingRequestStream` を伴わずに単独で発行する。
    /// 送信方向が既に FIN / RESET 済みの request に対する本イベントは I/O 層で無視する
    /// (送信方向を再度 reset しない。Session は送信方向の閉塞を追跡しないため、
    /// GOING_AWAY timeout reset と malformed cancel が重複して届きうる)。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    ResetRequestStream {
        /// 対象 request の Request ID
        request_id: u64,
        /// Stream Reset Error Code (§12.5)
        error_code: u64,
    },
    /// bidi request stream の受信方向を STOP_SENDING で打ち切る
    ///
    /// draft-ietf-moq-transport-22 §6.4.2.3 (Request Cancellation and Rejection) の
    /// 受信方向の打ち切り。I/O 層は `request_id` に対応する bidi stream の受信方向に
    /// `error_code` で STOP_SENDING を送る。`error_code` は §12.5 のコードを使う。
    /// Session が本イベントを発行するのは、新規に `Terminated` へ遷移させる次の 2 経路である
    /// (いずれも続けて送信方向の cancel として `ResetRequestStream` を発行する)。
    ///
    /// - Malformed Track 検出 (§12.1 (Malformed Tracks))。
    ///   `error_code` は [`STREAM_MALFORMED_TRACK`](crate::error::STREAM_MALFORMED_TRACK)
    /// - 未知の Mandatory Track Property を含む SUBSCRIBE_OK / FETCH_OK の受信
    ///   (§3.7 (Mandatory to Understand Track Properties) の cancel)。`error_code` は
    ///   [`STREAM_CANCELLED`](crate::error::STREAM_CANCELLED)
    ///
    /// request stream 上の GOAWAY の deadline 満了による終端 (§9.2 (GOAWAY)) では本イベントを
    /// 発行しない。§9.2 が求めるのは送信方向の reset であり、受信方向は peer が閉じるためである
    /// ([`SessionEvent::ResetRequestStream`] の doc 参照)。この経路で終端後に届く応答は
    /// `Session::recv_stream_message` が no-op で吸収する。
    ///
    /// 既に `Terminated` の経路 (アプリが先に cancel した / PUBLISH_DONE 受信済み) では
    /// 発行しない。節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    StopSendingRequestStream {
        /// 対象 request の Request ID
        request_id: u64,
        /// Stream Reset Error Code (§12.5)
        error_code: u64,
    },
    /// bidi request stream の送信方向を FIN で閉じる
    ///
    /// draft-ietf-moq-transport-22 §6.4.2.2 (Graceful Request Stream Closure):
    /// "A FIN sent by the responder after its response and any subsequent messages for the
    /// request signals that the request is complete; if it has not already done so, the
    /// requester SHOULD then send a FIN on its direction, gracefully closing the stream."
    ///
    /// 発行契機は 2 つある。
    ///
    /// - requester 側: responder の FIN を受けて送信方向を閉じる
    ///   ([`Session::recv_request_stream_closed`](crate::session::core::Session::recv_request_stream_closed))
    /// - responder 側: PUBLISH 起点の subscription で PUBLISH_DONE を受信し、自側が送るべき
    ///   メッセージが無くなった時点で閉じる。
    ///   §6.4.2.2 "An endpoint MUST NOT send a FIN on a direction of a request stream until it
    ///   has sent all required messages on that direction for its request type." より、
    ///   必須応答を送り終えた `Established` からの遷移に限る
    ///
    /// I/O 層は `request_id` に対応する bidi stream の送信方向を FIN で閉じる。既に FIN 済みの
    /// request への本イベントは I/O 層で無視する (Session は送信方向の閉塞を追跡しないため、
    /// アプリが独自に FIN した後に本イベントが届きうる)。
    /// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    FinishRequestStream {
        /// 対象 request の Request ID
        request_id: u64,
    },
}

/// data stream を reset する理由 (draft-ietf-moq-transport-22 §12.5 (Stream Reset Error Codes))
///
/// §12.5: "The application SHOULD use a relevant error code when resetting or sending
/// STOP_SENDING on any stream."
///
/// 理由からエラーコードへの対応を型で固定し、アプリケーションが生の数値を選ばなくても
/// 適切なコードが載るようにする。生のコードを指定したい場合は
/// [`Session::reset_outgoing_data_stream_with_code`](crate::session::core::Session::reset_outgoing_data_stream_with_code)
/// を使う。
/// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataStreamResetReason {
    /// 実装固有のエラー → `INTERNAL_ERROR` (0x0)
    InternalError,
    /// いずれかの端点による cancel → `CANCELLED` (0x1)
    ///
    /// §12.5: "The stream was cancelled by either endpoint. For Subscriptions, PUBLISH_DONE
    /// (Section 9.9) may have a more detailed status code."
    Cancelled,
    /// delivery timeout 超過 → `DELIVERY_TIMEOUT` (0x2)
    ///
    /// Session が自動で発行する経路が既にあるので、通常アプリケーションが指定する必要はない。
    DeliveryTimeout,
    /// セッション終了中 → `SESSION_CLOSED` (0x3)
    ///
    /// §12.5: "The session is being closed."
    SessionClosed,
    /// GOAWAY 送受信による拒否 → `GOING_AWAY` (0x4)
    ///
    /// §12.5: "The endpoint is rejecting this request because it has sent or
    /// received a GOAWAY."
    GoingAway,
    /// publisher のリソース上限超過 → `TOO_FAR_BEHIND` (0x5)
    ///
    /// §12.5: "The corresponding subscription has exceeded the publisher's resource limits and
    /// is being terminated (see Section 5.2)."
    TooFarBehind,
    /// FETCH 応答で次の Object の status を判定できない → `UNKNOWN_OBJECT_STATUS` (0x6)
    ///
    /// §12.5: "In response to a FETCH, the publisher is unable to determine the status of the
    /// next Object in the requested range."
    UnknownObjectStatus,
    /// 認証トークンの有効期限切れ → `EXPIRED_AUTH_TOKEN` (0x7)
    ///
    /// §12.5: "The authorization token for the request has expired."
    ExpiredAuthToken,
    /// 過負荷 → `EXCESSIVE_LOAD` (0x9)
    ExcessiveLoad,
    /// Malformed Track の検出 → `MALFORMED_TRACK` (0x12)
    ///
    /// draft-ietf-moq-transport-22 §12.1 (Malformed Tracks) の検出経路 (Data Stream 側のヘッダ検証・
    /// unique-key 違反・ relay の Object 変更検出など) で Session が自動発行する。
    /// 具体的な発行は `data.rs::terminate_malformed_track` を経由する。
    MalformedTrack,
}

impl DataStreamResetReason {
    /// 対応する Stream Reset Error Code を返す (draft §12.5 (Stream Reset Error Codes))
    pub fn error_code(self) -> u64 {
        match self {
            Self::InternalError => crate::error::STREAM_INTERNAL_ERROR,
            Self::Cancelled => crate::error::STREAM_CANCELLED,
            Self::DeliveryTimeout => crate::error::STREAM_DELIVERY_TIMEOUT,
            Self::SessionClosed => crate::error::STREAM_SESSION_CLOSED,
            Self::GoingAway => crate::error::STREAM_GOING_AWAY,
            Self::TooFarBehind => crate::error::STREAM_TOO_FAR_BEHIND,
            Self::UnknownObjectStatus => crate::error::STREAM_UNKNOWN_OBJECT_STATUS,
            Self::ExpiredAuthToken => crate::error::STREAM_EXPIRED_AUTH_TOKEN,
            Self::ExcessiveLoad => crate::error::STREAM_EXCESSIVE_LOAD,
            Self::MalformedTrack => crate::error::STREAM_MALFORMED_TRACK,
        }
    }
}

/// caller が `Session` に渡す受信 uni data stream の識別子
///
/// QUIC / WebTransport / QMux の stream ID をそのまま使ってもよいし、caller 独自の
/// stable な整数 ID を使ってもよい。`Session` は値の意味を解釈せず、
/// 同一 stream を同一値で通知されることだけを前提にする。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DataStreamId(pub u64);

/// data plane の track alias 判定結果
///
/// datagram や subgroup stream は、draft が unknown Track Alias に対して
/// session close を要求していない。caller はこの結果を見て drop / abandon /
/// short buffer を選べる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackDataAcceptance {
    /// 受け入れた
    Accepted,
    /// 未知の Track Alias のため受け入れられない
    UnknownTrackAlias,
    /// キャンセル済み subscription の alias に届いた不要 Object として破棄した
    ///
    /// draft-ietf-moq-transport-22 §3.1.3 (Track Alias): "Objects can arrive after a
    /// subscription has been cancelled. Subscribers SHOULD retain sufficient state to quickly
    /// discard these unwanted Objects, rather than treating them as belonging to an unknown
    /// Track Alias."
    ///
    /// `UnknownTrackAlias` と区別することで、caller は「知らない alias なので buffer するか
    /// 判断が必要」と「元は知っていた alias なので確実に捨てて良い」を切り分けられる。
    Discarded,
    /// 候補 subscription は存在するが、フィルタ再適用の結果どの subscription にも属さなかった
    ///
    /// draft-ietf-moq-transport-22 §3.1 (Subscriptions): "the subscriber re-applies each
    /// subscription's filter to determine which subscription a received Object belongs to."
    /// どのフィルタも通らない Object はセッションを閉じずに破棄する (§3.1 / §11.2 は
    /// 受信側の破棄を要求していないが、紐づけ先がないため受け入れられない)。
    FilteredOut,
}

/// `Session::recv_datagram` の判定結果 (draft-ietf-moq-transport-22 §11 (Data Streams and Datagrams))
///
/// datagram 先頭の type varint で分岐した結果を表す。unknown type は §11 の
/// "An endpoint that receives an unknown datagram type MUST close the session." により
/// `Err` になるので、この enum には現れない。
/// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatagramAcceptance {
    /// OBJECT_DATAGRAM (§11.2.1) として処理した
    Object(TrackDataAcceptance),
    /// Padding Datagram (§11.5.2) として破棄した
    Padding,
}

/// `Session::recv_fetch_header` の受理結果 (draft-ietf-moq-transport-23 §11.4.1 (Fetch Header))
///
/// §11.4.1 は FETCH_HEADER の Request ID が未知の場合の扱いを MUST で定めていない。一方
/// draft-ietf-moq-transport-23 §3.4 (Fill Semantics) / §3.4.1 (Opening and Closing Fill Fetch
/// Streams) は、publisher が PUBLISH を送るときに FILL_PARAMETERS が載っていれば初回 fill の
/// fill fetch stream を開くと定める。PUBLISH は bidi stream、fill fetch stream は uni stream
/// であり両者の到着順序は保証されないため、PUBLISH の処理前に初回 fill が届きうる。
/// したがって未知の Request ID はセッション終了ではなく再試行可能な結果として返し、
/// 保留の上限・期限を設けたうえで caller に再通知させる。
/// この節番号・規則は draft 由来であり将来 draft 改版で変わる可能性がある。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchHeaderAcceptance {
    /// 受理し、stream を帰属先の fetch または subscription に登録した
    Accepted,
    /// 対応する要求が未登録のため受理しなかった。状態は一切変更していない
    ///
    /// caller は stream と decode 済みヘッダを有限の上限・期限で保留し、対応する PUBLISH を
    /// 受理した後に [`Session::recv_fetch_header`](crate::session::core::Session::recv_fetch_header)
    /// を再通知する。期限超過時は stream を破棄して Session の会計を解放する。
    RequestNotRegistered,
}

// ─── bidi request stream のライフサイクル型 ─────────────────
//
// draft-ietf-moq-transport-22 §6.3 (Session initialization) / §6.4.2.3 (Request Cancellation and Rejection)
// bidi request stream のライフサイクルを追跡するための基礎型。
// 将来 draft が変更される可能性がある。

/// 本実装が扱う bidi request stream の開始メッセージ種別
/// (draft-ietf-moq-transport-22 §6.3 (Session initialization))
///
/// §6.3 は bidi request stream の先頭メッセージを 7 種類 (TRACK_STATUS / SUBSCRIBE / PUBLISH /
/// FETCH / PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS) に限る。本実装が
/// 受理するのはこのうち以下の 4 種類である (残り 3 種類は `Session::recv_request` が
/// `REQUEST_ERROR` (NOT_SUPPORTED、control GOAWAY を送信済みなら GOING_AWAY、`.` 単独または
/// `.session` の Track Namespace Prefix なら DOES_NOT_EXIST) で拒否する。
/// `SUBSCRIBE_TRACKS` は Range Filter を載せられるため、§3.3.2 に違反する場合は
/// `INVALID_FILTER` で拒否する。`PUBLISH_NAMESPACE` / `SUBSCRIBE_NAMESPACE` は Range Filter を
/// 許可しないため、載っていた場合は §9.20.1 (Parameter Scope) 違反でセッションを閉じる)。
/// `request_id` からどの種別で開始されたかを逆引きする用途に使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequestKind {
    /// SUBSCRIBE で開始
    Subscribe,
    /// PUBLISH で開始
    Publish,
    /// FETCH で開始
    Fetch,
    /// TRACK_STATUS で開始
    TrackStatus,
}

/// bidi request stream の終端原因 (draft-ietf-moq-transport-22 §6.4.2.3 (Request Cancellation and Rejection))
///
/// `Session::recv_request_stream_closed` / `Session::recv_control_stream_closed`
/// の引数として使う。FIN と RESET_STREAM を区別する。FETCH データ stream (uni)
/// の終端にも同じ型を使い回す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestStreamEnd {
    /// FIN による正常終端
    Fin,
    /// RESET_STREAM による打ち切り
    Reset {
        /// Stream Reset Error Code (§12.5)
        ///
        /// `None` は「アプリケーションエラーコード無し」を表す。
        /// draft-ietf-webtrans-http3-16 §4.4 (Resetting Data Streams) は
        /// WT_APPLICATION_ERROR の範囲外のコードで RESET_STREAM / STOP_SENDING を受信した場合に
        /// "The WebTransport implementation SHOULD deliver this to the application as a stream
        /// reset with no application error code." と定めるため、WebTransport 経路で
        /// HTTP/3 のコードを MOQT のコードへ戻せない場合は `None` になる。
        /// QUIC 経路 (`moqt://`) は QUIC のコード空間であり常にコードがあるため `Some` になる。
        /// この節番号・規則は draft 由来であり将来 draft 改版で変わる可能性がある。
        error_code: Option<u64>,
        /// RESET_STREAM_AT の reliable size (`None` は RESET_STREAM)
        reliable_size: Option<u64>,
    },
}

/// `RequestStreamEnd` を素直な `TerminationReason` に変換する
///
/// request stream / fetch data stream の終端通知を、アプリへ通知する
/// [`TerminationReason`] に正規化する。
pub(crate) fn terminationreason_from_end(end: RequestStreamEnd) -> TerminationReason {
    match end {
        RequestStreamEnd::Fin => TerminationReason::PeerStreamFin,
        RequestStreamEnd::Reset { error_code, .. } => {
            TerminationReason::PeerStreamReset { error_code }
        }
    }
}

/// request が終端した原因 (draft-ietf-moq-transport-22 §3.1 (Subscriptions) / §6.4.2.3 (Request Cancellation and Rejection) / §9.2 (GOAWAY) / §12.1 (Malformed Tracks))
///
/// `SessionEvent::RequestTerminated` の payload として使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminationReason {
    /// peer が FIN で bidi request stream を閉じた
    PeerStreamFin,
    /// peer が RESET_STREAM で打ち切った
    PeerStreamReset {
        /// peer が載せた Stream Reset Error Code
        ///
        /// `None` は peer がアプリケーションエラーコードを載せなかったことを表す
        /// ([`RequestStreamEnd::Reset`] と同じ表現)。
        error_code: Option<u64>,
    },
    /// 自側が cancel / abort / close で打ち切った
    LocalCancel,
    /// draft §3.1 (Subscriptions): PUBLISH 受信により既存 Pending(Subscriber) が
    /// 自動的に Terminated に遷移した
    SupersededByPublish {
        /// 置き換え後の PUBLISH の Request ID
        new_request_id: u64,
    },
    /// Malformed Track を検出して該当 request をキャンセルした (draft §12.1 (Malformed Tracks))
    ///
    /// §12.1: "When a subscriber detects a Malformed Track, it MUST cancel any corresponding
    /// subscription or fetches for that Track from that publisher (see Section 6.4.2.3), and
    /// SHOULD deliver an error to the application."
    ///
    /// 本イベントが SHOULD の「アプリケーションへのエラー通知」に相当する。Session は
    /// §6.4.2.3 に従い `StopSendingRequestStream` → `ResetRequestStream` を
    /// [`STREAM_MALFORMED_TRACK`](crate::error::STREAM_MALFORMED_TRACK) で発行して
    /// 当該 bidi request stream の両方向を cancel する
    /// (I/O 層が実際の STOP_SENDING / RESET_STREAM を送出する)。
    ///
    /// relay として動作する場合、§12.1 は downstream の subscription を
    /// PUBLISH_DONE で終端し fetch stream を MALFORMED_TRACK で reset することを MUST とする。
    /// downstream は別セッションなので本 `Session` からは操作できない。アプリケーションが
    /// downstream 側の `Session` で
    /// `send_publish_done(rid, PUBLISH_DONE_MALFORMED_TRACK, ...)` を呼ぶこと。
    ///
    /// §12.1 の "Object(s) triggering Malformed Track status MUST NOT be cached." は
    /// キャッシュを持つアプリケーション層の責務である (`Session` は Object を保持しない)。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    MalformedTrack {
        /// 検出内容の説明 (英語、静的文字列)
        reason: &'static str,
    },
    /// request stream 上の GOAWAY の timeout 満了で自側が送信方向を reset し、request を終端した
    /// (draft-ietf-moq-transport-22 §9.2 (GOAWAY))
    ///
    /// §9.2: "When sent on a request stream, the sender SHOULD reset the stream with
    /// GOING_AWAY after the indicated timeout." の reset 時点でまだ終端していなかった
    /// request を Session がローカルで終端したことを表す。この終端は仕様が要求するものでは
    /// なく、reset 後に最終メッセージ (PUBLISH_DONE 等) を送れないことから Session が
    /// 終端を確定させる実装判断である。reset するのは送信方向だけなので peer の送信方向は
    /// 開いたままであり、peer が GOAWAY / reset を観測する前に送ったメッセージは Session が
    /// no-op で吸収する
    /// ([`Session::recv_stream_message`](crate::session::core::Session::recv_stream_message)
    /// の doc 参照)。1 回目の peer FIN / RESET_STREAM も同じく no-op で吸収される。
    /// GOAWAY を送れるのは request の受信側 (responder) だけなので (§9.2)、終端されるのは
    /// 自側が応答側だった entry であり、応答を送れないまま応答を持たずに残る
    /// (`Session::forget_*` で回収できる)。publisher 役 fetch の open 中の
    /// outgoing FETCH data stream は Session が reset しないため、アプリが
    /// `Session::send_fetch_data_stream_closed` で閉じること。publisher 役 subscription の
    /// open 中の outgoing subgroup stream も同様に Session が reset しないため、
    /// アプリが `Session::send_data_stream_closed` で閉じること (subscription 側で Session が
    /// reset するのは、購読の終端に伴い §3.4.1 の MUST が課る fill fetch stream だけである)。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    GoawayTimeout,
}

/// `Session::recv_request` が返すエラー
///
/// - `BeforeSessionEstablished`: SETUP 完了前に request が到着した
///   (draft §6.3 (Session initialization))。session state には影響しない。呼び出し側は
///   buffer して後で再投入するか、その bidi stream だけを RESET_STREAM で
///   閉じるかを選べる。
/// - `Session`: 致命的なプロトコル違反。session state は `Closing` に遷移済み。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecvRequestError {
    /// SETUP 完了前の到着 (session state に影響しない)
    BeforeSessionEstablished,
    /// 致命的なプロトコル違反 (session state は `Closing` に遷移済み)
    Session(SessionError),
}

impl RecvRequestError {
    /// `Session` variant の内部 `SessionError` を参照する (診断用)
    ///
    /// `BeforeSessionEstablished` は session state に影響しないため `None` を返す。
    pub fn as_session_error(&self) -> Option<&SessionError> {
        match self {
            Self::BeforeSessionEstablished => None,
            Self::Session(err) => Some(err),
        }
    }
}

impl From<SessionError> for RecvRequestError {
    fn from(err: SessionError) -> Self {
        Self::Session(err)
    }
}

impl core::fmt::Display for RecvRequestError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BeforeSessionEstablished => {
                write!(f, "request received before session established")
            }
            Self::Session(err) => write!(f, "{}", err),
        }
    }
}

impl core::error::Error for RecvRequestError {}

/// `Session::recv_data_stream_type` が返すエラー
///
/// - `BeforeSessionEstablished`: SETUP 完了前に uni data stream が到着した
///   (draft §6.3 (Session initialization))。caller は buffer して後で再投入するか、
///   その stream だけを打ち切るかを選べる。
/// - `InvalidInput`: caller が同じ `DataStreamId` を二重登録するなど、`Session`
///   の API 契約違反。session state は変更しない。
/// - `Session`: 致命的なプロトコル違反。session state は `Closing` に遷移済み。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecvDataStreamError {
    /// SETUP 完了前の到着 (session state に影響しない)
    BeforeSessionEstablished,
    /// API 契約違反 (session state は変更しない)
    InvalidInput(SessionError),
    /// 致命的なプロトコル違反 (session state は `Closing` に遷移済み)
    Session(SessionError),
}

impl RecvDataStreamError {
    /// `Session` variant の内部 `SessionError` を参照する (診断用)
    ///
    /// `BeforeSessionEstablished` / `InvalidInput(_)` は session state を変更しないため
    /// `None` を返す。
    pub fn as_session_error(&self) -> Option<&SessionError> {
        match self {
            Self::Session(err) => Some(err),
            Self::BeforeSessionEstablished | Self::InvalidInput(_) => None,
        }
    }
}

impl core::fmt::Display for RecvDataStreamError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BeforeSessionEstablished => {
                write!(f, "data stream received before session established")
            }
            Self::InvalidInput(err) | Self::Session(err) => write!(f, "{}", err),
        }
    }
}

impl core::error::Error for RecvDataStreamError {}

// ─── Subscription 状態管理 ────────────────────────────────────
//
// draft-ietf-moq-transport-22 §3.1 (Subscriptions), §3.1.2 (Subscription State Management)

/// Subscription を開始した側 (draft §3.1 (Subscriptions))
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubscriptionInitiator {
    /// SUBSCRIBE を送信した Subscriber 主導
    Subscriber,
    /// PUBLISH を送信した Publisher 主導
    Publisher,
}

/// 各 request / track において自エンドポイントが担う protocol role
///
/// [`Role`] (`Client` / `Server`) とは別軸であり、1 本の
/// [`crate::session::core::Session`] の中で `Publisher` / `Subscriber` はどちらも取りうる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrackRole {
    /// 自側が Publisher (相手へ Object を送る側)
    Publisher,
    /// 自側が Subscriber (相手から Object を受ける側)
    Subscriber,
}

/// Subscription 状態機械 (draft §3.1 (Subscriptions) の state machine)
///
/// draft の state machine は Idle → Pending → Established → Terminated の 3 状態
/// (Idle は entry 未作成に対応)。`Pending` の subject (Subscriber / Publisher) は
/// `Subscription.initiator` から導出する (`is_pending_subscriber` / `is_pending_publisher`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionState {
    /// 初回要求を送信 / 受信した直後、相手側の OK / REQUEST_ERROR 待ち
    ///
    /// initiator == Subscriber なら SUBSCRIBE_OK 待ち、Publisher なら PUBLISH_OK 待ち。
    Pending,
    /// 応答 OK 受信済み、Object 転送可能
    Established,
    /// 終了 (REQUEST_ERROR / PUBLISH_DONE / STOP_SENDING / supersede / request stream 上の
    /// GOAWAY の deadline 満了 ([`TerminationReason::GoawayTimeout`]) で遷移)
    Terminated,
}

/// ミリ秒ベースの相対タイマー (Sans I/O)
///
/// `Session` は時刻源を持たないため、`tick(now_ms)` までは `since_ms` / `deadline_ms`
/// が `None` のまま保持されうる。`expired` は `tick()` により更新される。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeadlineTimer {
    /// タイマー長 (ms)
    pub duration_ms: u64,
    /// タイマー開始時刻 (ms)。`tick()` 前は `None`
    pub since_ms: Option<u64>,
    /// 期限の絶対時刻 (ms)。`tick()` 前は `None`
    pub deadline_ms: Option<u64>,
    /// 期限到達済みか
    pub expired: bool,
}

impl DeadlineTimer {
    /// 新しい `DeadlineTimer` を作成する。
    ///
    /// `duration_ms == 0` は即期限切れとして扱う。例えば draft-ietf-moq-transport-22
    /// §9.20.16（将来の draft で変更される可能性がある）の EXPIRES=0 を「期限なし」
    /// と扱いたい場合は、呼び出し側で `None` に正規化してから渡すこと。
    pub(crate) fn new(duration_ms: u64, now_ms: Option<u64>) -> Self {
        match now_ms {
            Some(now_ms) => Self {
                duration_ms,
                since_ms: Some(now_ms),
                deadline_ms: Some(now_ms.saturating_add(duration_ms)),
                expired: duration_ms == 0,
            },
            None => Self {
                duration_ms,
                since_ms: None,
                deadline_ms: None,
                expired: duration_ms == 0,
            },
        }
    }

    pub(crate) fn tick(&mut self, now_ms: u64) {
        if self.expired {
            return;
        }
        if self.deadline_ms.is_none() {
            self.since_ms = Some(now_ms);
            self.deadline_ms = Some(now_ms.saturating_add(self.duration_ms));
        }
        if let Some(deadline_ms) = self.deadline_ms
            && now_ms >= deadline_ms
        {
            self.expired = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DeadlineTimer, DeliveryTimeoutState, DynamicGroupsSupport, HashMap, StreamCountState,
        Subscription, SubscriptionInitiator, SubscriptionPublishDone, SubscriptionRangeFilters,
        SubscriptionState, TrackNamespace, TrackRole,
    };
    use crate::message::ReasonPhrase;
    use alloc::collections::VecDeque;
    use alloc::vec;

    /// 最小限のフィールドだけ埋めた Subscription を作る (cleanup_ready 専用)
    fn terminated_subscription_with_publish_done(
        drain_expired: bool,
        stream_count_overrun: bool,
        open_incoming_subgroup_stream_count: u64,
    ) -> Subscription {
        let mut sub = Subscription {
            request_id: 1,
            initiator: SubscriptionInitiator::Subscriber,
            my_role: TrackRole::Subscriber,
            track_namespace: TrackNamespace::new(vec![b"live".to_vec()])
                .expect("正当な namespace である"),
            track_name: b"cam".to_vec(),
            track_alias: Some(1),
            state: SubscriptionState::Terminated,
            paused: false,
            largest_location: None,
            largest_received_location: None,
            delivery_timeouts: DeliveryTimeoutState {
                subscriber_object_ms: None,
                subscriber_subgroup_ms: None,
                publisher_object_ms: None,
                publisher_subgroup_ms: None,
                effective_object_ms: None,
                effective_subgroup_ms: None,
                subgroup_overrides: HashMap::new(),
            },
            expires: None,
            dynamic_groups: DynamicGroupsSupport::Unknown,
            default_publisher_priority: None,
            default_publisher_group_order: None,
            stream_counts: StreamCountState {
                published_count: 0,
                incoming_subgroup_count: 0,
                open_incoming_subgroup_count: open_incoming_subgroup_stream_count,
            },
            publish_done: Some(SubscriptionPublishDone {
                status_code: 0x2,
                stream_count: 0,
                reason: ReasonPhrase::new("ended").expect("正当な reason phrase である"),
                drain: DeadlineTimer::new(100, Some(0)),
                stream_count_overrun,
            }),
            filter: None,
            subscriber_priority: None,
            group_order: None,
            subscribe_tracks_id: None,
            include_properties: None,
            filter_start: None,
            filter_end: None,
            range_filters: SubscriptionRangeFilters::default(),
            ended_groups: HashMap::new(),
            end_of_track: None,
            pending_update_params: VecDeque::new(),
            pending_publish_done: None,
        };
        sub.publish_done
            .as_mut()
            .expect("terminated_subscription_with_publish_done must set publish_done")
            .drain
            .expired = drain_expired;
        sub
    }

    #[test]
    fn cleanup_ready_true_when_overrun_and_no_open_streams() {
        let sub = terminated_subscription_with_publish_done(false, true, 0);
        assert!(
            sub.cleanup_ready(),
            "overrun 確定時に open stream がなければ drain timer を待たずに回収できる"
        );
    }

    #[test]
    fn cleanup_ready_false_when_overrun_but_open_streams_remain() {
        let sub = terminated_subscription_with_publish_done(false, true, 1);
        assert!(
            !sub.cleanup_ready(),
            "overrun 確定時でも open stream が残っていれば回収しない"
        );
    }

    #[test]
    fn cleanup_ready_true_when_drain_expired_and_no_open_streams() {
        let sub = terminated_subscription_with_publish_done(true, false, 0);
        assert!(
            sub.cleanup_ready(),
            "drain timer 満了後に open stream がなければ回収できる"
        );
    }

    #[test]
    fn cleanup_ready_false_when_drain_not_expired_nor_overrun() {
        let sub = terminated_subscription_with_publish_done(false, false, 0);
        assert!(
            !sub.cleanup_ready(),
            "drain timer 未満かつ overrun でもない場合は回収しない"
        );
    }

    #[test]
    fn cleanup_ready_true_when_terminated_without_publish_done() {
        let mut sub = terminated_subscription_with_publish_done(false, false, 0);
        sub.publish_done = None;
        assert!(
            sub.cleanup_ready(),
            "PUBLISH_DONE を受信していない Terminated subscription は即座に回収できる"
        );
    }

    #[test]
    fn cleanup_ready_false_when_not_terminated() {
        let mut sub = terminated_subscription_with_publish_done(true, true, 0);
        sub.state = SubscriptionState::Established;
        assert!(
            !sub.cleanup_ready(),
            "Terminated 以外の subscription は回収できない"
        );
    }

    #[test]
    fn zero_duration_is_immediately_expired() {
        // duration_ms == 0 は即期限切れとして扱う。
        // EXPIRES=0 は呼び出し側で正規化して渡すため、ここでは 0 のまま検証する。
        let timer = DeadlineTimer::new(0, Some(1000));
        assert_eq!(timer.duration_ms, 0);
        assert_eq!(timer.since_ms, Some(1000));
        assert_eq!(timer.deadline_ms, Some(1000));
        assert!(timer.expired);
    }

    #[test]
    fn non_zero_duration_is_not_expired_until_tick() {
        // duration_ms > 0 の DeadlineTimer は tick() までは期限切れにならない。
        let timer = DeadlineTimer::new(100, Some(1000));
        assert_eq!(timer.duration_ms, 100);
        assert_eq!(timer.since_ms, Some(1000));
        assert_eq!(timer.deadline_ms, Some(1100));
        assert!(!timer.expired);
    }

    #[test]
    fn zero_duration_with_no_now_is_immediately_expired() {
        // now_ms = None の場合も duration_ms == 0 は即期限切れとして扱う。
        let timer = DeadlineTimer::new(0, None);
        assert_eq!(timer.duration_ms, 0);
        assert_eq!(timer.since_ms, None);
        assert_eq!(timer.deadline_ms, None);
        assert!(timer.expired);
    }

    #[test]
    fn zero_duration_stays_expired_after_tick() {
        // duration_ms == 0 の DeadlineTimer は tick() 後も期限切れのまま。
        let mut timer = DeadlineTimer::new(0, None);
        timer.tick(2_000);
        assert!(timer.expired);
        assert_eq!(timer.since_ms, None);
        assert_eq!(timer.deadline_ms, None);
    }

    #[test]
    fn non_zero_duration_with_no_now_starts_unticked() {
        // now_ms = None の場合、tick() までは since_ms / deadline_ms が None のままとなる。
        let timer = DeadlineTimer::new(100, None);
        assert_eq!(timer.duration_ms, 100);
        assert_eq!(timer.since_ms, None);
        assert_eq!(timer.deadline_ms, None);
        assert!(!timer.expired);
    }
}

/// PUBLISH_DONE 受信後の subscriber-side 補助状態
///
/// draft-ietf-moq-transport-22 §9.9 (PUBLISH_DONE):
/// "Once the timer has expired, the receiver destroys subscription state once all open streams
/// for the subscription have closed." drain timer 満了後、かつ全 open stream が閉じれば
/// subscriber は subscription state を破棄する。
/// "A subscriber MAY discard subscription state earlier, at the cost of potentially not
/// delivering some late objects to the application." 遅延オブジェクトを諦める代償で早期破棄してもよい (MAY)。
/// "If a subscriber receives more streams for a subscription than specified in Stream Count,
/// it MAY close the session with a PROTOCOL_VIOLATION." stream count を超過した場合、
/// PROTOCOL_VIOLATION で session close する MAY もある。本実装では既存方針通り session close は
/// 行わず、open stream 消失後の subscription state 早期回収を許容する。
/// (将来の draft で変更される可能性がある)
#[derive(Debug, Clone, PartialEq)]
pub struct SubscriptionPublishDone {
    /// PUBLISH_DONE.status_code
    pub status_code: u64,
    /// PUBLISH_DONE.stream_count
    pub stream_count: u64,
    /// PUBLISH_DONE.reason
    pub reason: ReasonPhrase,
    /// late-arriving stream を待つ drain timer
    pub drain: DeadlineTimer,
    /// `stream_count` を超える data stream (subgroup / fill fetch) を受信済みか
    pub stream_count_overrun: bool,
}

/// draft-ietf-moq-transport-22 §5.2 (Delivery Timeouts and Data Reliability) の
/// OBJECT_DELIVERY_TIMEOUT / SUBGROUP_DELIVERY_TIMEOUT の subscriber 申告 /
/// publisher 申告 / effective 値の 3 つ組と、§10.1 / §10.2 の per-subgroup オーバーライドを保持する。
#[derive(Debug, Clone)]
pub struct DeliveryTimeoutState {
    /// subscriber 側が指定した OBJECT_DELIVERY_TIMEOUT parameter (ms)
    ///
    /// `my_role == Subscriber` では自側が指定した値、`my_role == Publisher` では peer
    /// subscriber が指定した値を保持する。
    pub subscriber_object_ms: Option<u64>,
    /// subscriber 側が指定した SUBGROUP_DELIVERY_TIMEOUT parameter (ms)
    pub subscriber_subgroup_ms: Option<u64>,
    /// publisher 側が Track Property として指定した OBJECT_DELIVERY_TIMEOUT (ms)
    pub publisher_object_ms: Option<u64>,
    /// publisher 側が Track Property として指定した SUBGROUP_DELIVERY_TIMEOUT (ms)
    pub publisher_subgroup_ms: Option<u64>,
    /// OBJECT_DELIVERY_TIMEOUT の effective 値 (ms)
    ///
    /// subscriber + publisher の両側情報から計算。`None` は timeout 無し
    pub effective_object_ms: Option<u64>,
    /// SUBGROUP_DELIVERY_TIMEOUT の effective 値 (ms)
    pub effective_subgroup_ms: Option<u64>,
    /// subgroup 固有の delivery timeout オーバーライド (draft-ietf-moq-transport-22 §10.1 / §10.2)
    ///
    /// キー: (group_id, subgroup_id)、値: (subgroup_timeout, object_timeout)。
    /// subgroup の先頭 object に付与された Object Property から抽出した raw 値を保持する。
    /// `None` フィールドは Object Property 不在を意味し、Track Property にフォールバックする。
    pub subgroup_overrides: HashMap<(u64, u64), (Option<u64>, Option<u64>)>,
}

/// draft-ietf-moq-transport-22 §9.9 (PUBLISH_DONE) の Stream Count 監視と
/// cleanup_ready() の回収判定で連携する 3 フィールドを保持する。
#[derive(Debug, Clone)]
pub struct StreamCountState {
    /// この subscription で自端点が publisher として開いた data stream 数 (subgroup と fill fetch)
    ///
    /// draft §9.9 (PUBLISH_DONE) の `Stream Count` と対応する。`send_subgroup_header` と
    /// `send_fill_fetch_header` で加算し、close では減算しない。`my_role == Publisher` 以外では 0。
    pub published_count: u64,
    /// peer publisher から受信した data stream 数 (subgroup と fill fetch)
    ///
    /// draft §9.9 (PUBLISH_DONE) の `Stream Count` との比較に使う。fill fetch stream は
    /// `recv_fetch_header` の fill 分岐で加算し、close (FIN / RESET / STOP_SENDING) では
    /// 減算しない (open 中の本数は `open_incoming_subgroup_count` が別途保持する)。
    pub incoming_subgroup_count: u64,
    /// 現在 open 中の受信 data stream 数 (subgroup と fill fetch)
    ///
    /// `cleanup_ready()` の open stream 判定に使う。FIN / RESET / STOP_SENDING による
    /// 終端と破棄対象化 (`register_discarded_stream`) で 1 本ずつ減算する。
    pub open_incoming_subgroup_count: u64,
}

/// この subscription で判明している Track の `DYNAMIC_GROUPS` Property の対応状況
///
/// draft-ietf-moq-transport-23 §9.19.19 (NEW GROUP REQUEST Parameter):
/// "A subscriber MUST NOT send this parameter in REQUEST_UPDATE if it has received the
/// Track's Properties and they did not include the DYNAMIC_GROUPS Property with value 1."
/// Track Properties をまだ受け取っていない間はこの MUST NOT の条件を満たさないため、
/// 「未判明」と「既知の非対応」を区別しないと `REQUEST_UPDATE` に `NEW_GROUP_REQUEST` を
/// 載せてよいかを正しく判定できない。同節は、dynamic Groups に対応しない original
/// publisher が `NEW_GROUP_REQUEST` を無視することも定める。
/// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynamicGroupsSupport {
    /// Track Properties をまだ受け取っておらず、対応しているか判明していない
    Unknown,
    /// Track Properties (自側の発行値または peer の受信値) の `DYNAMIC_GROUPS` が `1`
    Supported,
    /// Track Properties を受け取ったが `DYNAMIC_GROUPS` が `1` ではなかった (`0` または欠落)
    Unsupported,
}

impl DynamicGroupsSupport {
    /// 自側が発行する Track Properties から対応状況を求める (publisher 側)
    ///
    /// peer へ提示する `SUBSCRIBE_OK` の Track Properties を `INCLUDE_PROPERTIES=0` で
    /// 空にする場合も、判定材料は空にする前の発行値である。空にした値を渡すと、対応して
    /// いる Track を非対応と誤認する。
    pub fn from_local_properties(properties: &TrackProperties) -> Self {
        Self::from_dynamic_groups_value(properties.dynamic_groups())
    }

    /// peer から受け取った Track Properties から対応状況を求める (subscriber 側)
    ///
    /// `requested_include_properties` は `SUBSCRIBE` で自側が指定した `INCLUDE_PROPERTIES`。
    /// draft-ietf-moq-transport-23 §9.19.22 (INCLUDE_PROPERTIES Parameter): `0` を指定すると
    /// 応答の Track Properties は「存在するが空」になる (SHOULD)。空で届いた場合は peer が
    /// `DYNAMIC_GROUPS` を告知していないだけで、対応しているかは判明しないため
    /// [`Unknown`](Self::Unknown) を保つ。`0` を指定していない場合と、`0` を指定しても空で
    /// なかった場合は受け取った値をそのまま反映する (`1` 以外は非対応)。
    ///
    /// `PUBLISH` は `INCLUDE_PROPERTIES` を運ばないため、呼び出し側は `None` を渡す。
    pub fn from_peer_properties(
        properties: &TrackProperties,
        requested_include_properties: Option<u8>,
    ) -> Self {
        if requested_include_properties == Some(0) && properties.is_empty() {
            return Self::Unknown;
        }
        Self::from_dynamic_groups_value(properties.dynamic_groups())
    }

    /// `DYNAMIC_GROUPS` の値 (未告知は `None`) を対応状況へ写す
    ///
    /// draft-ietf-moq-transport-23 §10.6 (DYNAMIC GROUPS): 値 `1` だけが動的 Group の
    /// 対応を表し、`0` と省略 (省略時の値は 0) は非対応である。
    fn from_dynamic_groups_value(value: Option<u64>) -> Self {
        if value == Some(1) {
            Self::Supported
        } else {
            Self::Unsupported
        }
    }

    /// `REQUEST_UPDATE` に `NEW_GROUP_REQUEST` を載せてよいか
    ///
    /// draft-ietf-moq-transport-23 §9.19.19 (NEW GROUP REQUEST Parameter): Track Properties を
    /// 受け取っていない間は MUST NOT の条件 ("has received the Track's Properties and they did
    /// not include the DYNAMIC_GROUPS Property with value 1") を満たさないため送信できる。
    /// 既知の非対応と判明した後だけ送信してはならない。
    pub const fn allows_new_group_request(self) -> bool {
        !matches!(self, Self::Unsupported)
    }
}

/// Subscription 単位の状態
#[derive(Debug, Clone)]
pub struct Subscription {
    /// この Subscription の Request ID
    pub request_id: u64,
    /// Subscription を開始した側
    pub initiator: SubscriptionInitiator,
    /// 自端点の役割 (initiator と自側の role から決まる)
    pub my_role: TrackRole,
    /// Full Track Name の namespace
    pub track_namespace: TrackNamespace,
    /// Full Track Name の name
    pub track_name: Vec<u8>,
    /// Track Alias (PUBLISH では送信時点、SUBSCRIBE では SUBSCRIBE_OK 受信時に確定)
    pub track_alias: Option<u64>,
    /// 現在の状態
    pub state: SubscriptionState,
    /// PAUSED により停止中か (draft-ietf-moq-transport-23 §9.19.18 (PAUSED Parameter))
    ///
    /// `true` の間、publisher はこの subscription で Object を送らない
    /// (draft-ietf-moq-transport-23 §3.1.1 (Pausing Subscriptions))。初期値は
    /// SUBSCRIBE / PUBLISH の PAUSED パラメータで定まり、省略時は unpaused (`false`)。
    /// REQUEST_UPDATE / PUBLISH_STATE_NOTIFY で PAUSED を省略した場合は現在値を維持する。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub paused: bool,
    /// 広告された Largest Object (draft-ietf-moq-transport-22 §9.20.17 (LARGEST OBJECT Parameter))
    ///
    /// peer が SUBSCRIBE_OK / PUBLISH / REQUEST_UPDATE_OK / PUBLISH_STATE_NOTIFY の
    /// LARGEST_OBJECT parameter で広告した値を subscriber 側で保存する。
    /// publisher 側は保存しない (自側の観測値は `largest_received_location` で追跡する)。
    /// `effective_largest_object` の max 導出に使う。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub largest_location: Option<Location>,
    /// 購読に紐づく object の最大 Location (draft-ietf-moq-transport-22 §9.20.17 (LARGEST OBJECT Parameter))
    ///
    /// draft §3.3.1 (Location Filters) は「Largest Object updates when the first byte
    /// of an Object with a Location larger than the previous value is published or received
    /// through a subscription」と規定する。`my_role` (Subscriber / Publisher は排他) に
    /// 応じて以下のいずれか一方が入る:
    /// - Subscriber: peer publisher から受信した subgroup object (`recv_subgroup_object`) /
    ///   datagram object (`recv_object_datagram`) の (group_id, object_id) 最大値。
    /// - Publisher: 自端点が送信した subgroup object (`send_subgroup_object`) /
    ///   datagram object (`send_object_datagram`) の (group_id, object_id) 最大値。
    ///
    /// draft-ietf-moq-transport-22 §9.20.17 は LARGEST_OBJECT parameter が SUBSCRIBE_OK / PUBLISH /
    /// REQUEST_UPDATE_OK / TRACK_STATUS_OK / PUBLISH_STATE_NOTIFY に乗ると規定する。
    /// 自端点が送信する LARGEST_OBJECT 計算に `largest_location` との max を取るために使う。
    pub largest_received_location: Option<Location>,
    /// delivery timeout 関連の状態 (subscriber / publisher 申告値、effective 値、per-subgroup オーバーライド)
    pub delivery_timeouts: DeliveryTimeoutState,
    /// 直近に観測した EXPIRES parameter
    ///
    /// sender がこの subscription を終了しうる時刻の advisory deadline を表す。
    pub expires: Option<DeadlineTimer>,
    /// Track の DYNAMIC_GROUPS Property (draft §10.6 (DYNAMIC GROUPS)) の対応状況
    ///
    /// 自側が Publisher として送信した PUBLISH / SUBSCRIBE_OK、または peer から受信した
    /// PUBLISH / SUBSCRIBE_OK の `TrackProperties` から求める。Track Properties をまだ
    /// 受け取っていない subscriber 側は [`DynamicGroupsSupport::Unknown`] であり、
    /// `SUBSCRIBE` に `NEW_GROUP_REQUEST` を載せられる (draft-ietf-moq-transport-23
    /// §9.19.19 (NEW GROUP REQUEST Parameter) は foreknowledge 無しの `SUBSCRIBE` を認める)。
    /// Publisher 側は peer が `INCLUDE_PROPERTIES=0` を指定して wire 上を空化しても、空に
    /// する前の発行値で対応状況を保持する。Subscriber 側は `INCLUDE_PROPERTIES=0` で空の
    /// Track Properties を受けた場合も未判明のまま保ち、値 `1`・値 `0`・欠落を受けた場合に
    /// 既知の対応 / 既知の非対応へ遷移する。
    pub dynamic_groups: DynamicGroupsSupport,
    /// Track Property の DEFAULT_PUBLISHER_PRIORITY (draft §10.4 (DEFAULT PUBLISHER PRIORITY))
    ///
    /// PUBLISH / SUBSCRIBE_OK / REQUEST_OK の `TrackProperties` から取り込む。
    /// 宣言されていなければ `None` で、その場合の実効値は §10.4 のデフォルト 128 になる。
    /// DEFAULT_PRIORITY bit が立った Subgroup / Datagram は「購読を確立した制御メッセージで
    /// 指定された Publisher Priority」としてこの値を継承するため、解決は
    /// [`resolve_header_publisher_priority`](Self::resolve_header_publisher_priority) を使う。
    pub default_publisher_priority: Option<u8>,
    /// Track Property の DEFAULT_PUBLISHER_GROUP_ORDER (draft §10.5 (DEFAULT PUBLISHER GROUP ORDER))
    ///
    /// publisher の選好であり、§9.20.8 の GROUP ORDER Parameter が運ぶ subscriber からの
    /// 要求 (`group_order`) とは別の値なので、上書きせず別フィールドで保持する。
    /// 宣言されていなければ `None` で、その場合の実効値は §10.5 のデフォルト Ascending (0x1)。
    /// 実効値の解決は [`effective_publisher_group_order`](Self::effective_publisher_group_order)。
    pub default_publisher_group_order: Option<u8>,
    /// stream count 関連の状態 (publisher 送信数、受信数、open 中の受信数)
    pub stream_counts: StreamCountState,
    /// peer から PUBLISH_DONE を受信した後の drain 状態
    pub publish_done: Option<SubscriptionPublishDone>,
    /// REQUEST_UPDATE 失敗応答 (`send_request_error`) で保留された PUBLISH_DONE
    /// (UPDATE_FAILED) の stream_count
    ///
    /// draft-ietf-moq-transport-22 §9.9 (PUBLISH_DONE): "A sender MUST NOT send
    /// PUBLISH_DONE until it has closed all streams it will ever open..." の MUST NOT と
    /// §9.5.1 の MUST (REQUEST_UPDATE 失敗時、publisher は PUBLISH_DONE (UPDATE_FAILED) を
    /// 送る) を両立するため、open 中の outgoing data stream (subgroup / fill fetch) が
    /// 残っている間は push を保留し、全 stream 終端後 (`send_data_stream_closed` /
    /// `reset_outgoing_data_stream` / `recv_data_stream_stop_sending` で
    /// `has_open_outgoing_data_streams_for_request` が false になった時点) に自動送信する。
    /// 既存の `publish_done` フィールド (受信側 drain timer 用。送信側 PUBLISH_DONE とは
    /// 無関係) とは別フィールドであり、`cleanup_ready()` の判定に影響しない。
    /// push と同時に `None` に戻り (二重 push を防ぐ)、`forget_subscription` で除去される。
    /// request stream 上の GOAWAY の deadline 満了で送信方向を reset した場合も破棄される
    /// (reset 後に bidi request stream へ書き込めないため。既に `Terminated` の subscription
    /// でも破棄だけは行う)。
    pub pending_publish_done: Option<u64>,
    /// Subscription Filter (draft-ietf-moq-transport-22 §3.3.1 (Location Filters), §9.20.9 (LOCATION FILTER Parameter))
    ///
    /// SUBSCRIBE / REQUEST_UPDATE に含まれる。
    /// `None` は unfiltered subscription に相当する (draft-ietf-moq-transport-22 §3.3.1 (Location Filters))。
    pub filter: Option<LocationFilter>,
    /// Subscriber Priority (draft-ietf-moq-transport-22 §9.20.7 (SUBSCRIBER PRIORITY Parameter))
    ///
    /// SUBSCRIBE / FETCH / PUBLISH_OK / REQUEST_UPDATE に含まれ、動的に変更可能。
    /// 省略時は `None` で、application 層が default (128) を適用する。
    pub subscriber_priority: Option<u8>,
    /// Group Order (draft-ietf-moq-transport-22 §9.20.8 (GROUP ORDER Parameter))
    ///
    /// SUBSCRIBE / SUBSCRIBE_TRACKS / FETCH に含まれる。draft-ietf-moq-transport-22 §5.1.1 (Definitions) により
    /// 確定後は変更不可。
    pub group_order: Option<u8>,
    /// PUBLISH を生じさせた SUBSCRIBE_TRACKS の Request ID
    /// (draft-ietf-moq-transport-23 §9.19.21 (SUBSCRIBE_TRACKS_ID Parameter))
    ///
    /// PUBLISH の SUBSCRIBE_TRACKS_ID パラメータの値をそのまま保持する。
    /// draft-ietf-moq-transport-23 §3.6 (Subscribing to Tracks by Prefix) は
    /// SUBSCRIBE_TRACKS の結果として送る PUBLISH にこの値を載せると定める。
    /// 本ライブラリは SUBSCRIBE_TRACKS 自体を実装せず、対応する namespace 要求との照合状態も
    /// 持たないため、値はアプリケーションが相関を取るための情報に限られる。対応する要求が
    /// 無い PUBLISH でも値は保持し、受理・拒否は通常の PUBLISH の判断として行う。
    /// 自側が publisher の場合は `send_publish` に渡した値が入る。省略時は `None`。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub subscribe_tracks_id: Option<u64>,
    /// INCLUDE_PROPERTIES (draft-ietf-moq-transport-23 §9.19.22 (INCLUDE_PROPERTIES Parameter))
    ///
    /// この subscription の OK 応答 (SUBSCRIBE_OK) の Track Properties を支配する値。
    /// 自側が subscriber なら SUBSCRIBE で自側が指定した値、自側が publisher なら peer の
    /// SUBSCRIBE が指定した値が入る。`Some(0)` のとき OK 応答の Track Properties は存在
    /// するが空になる (SHOULD)。`None` (省略時) は default 1 として扱う。
    /// subscriber 側では、空の Track Properties を受けたときに DYNAMIC_GROUPS の対応状況を
    /// 未判明のまま保つ判定にも使う ([`DynamicGroupsSupport::from_peer_properties`])。
    pub include_properties: Option<u8>,
    /// 解決済みの Location Filter の Start Location (draft §3.3.1 (Location Filters))
    ///
    /// RelativeGroup / NextObject は相対フィルタなので、送信のたびに現在の
    /// `largest_received_location` で再計算すると Start が前方にずれ続ける。確立時と
    /// REQUEST_UPDATE 適用時に `LocationFilter::effective_start_location` で解決した値を
    /// ここに固定し、Pass 評価はこの値を使う。
    pub filter_start: Option<Location>,
    /// 解決済みの Location Filter の End Location (draft §3.3.1 (Location Filters))
    ///
    /// AbsoluteRange / AbsoluteRangeWithEnd のときだけ `Some`。Location 単位の上限で inclusive。
    /// End フィールド省略時は subscription open-ended のため `None`。
    pub filter_end: Option<Location>,
    /// 適用されている Object 系 Range Filter (draft §3.3.2 (Range Filters))
    pub range_filters: SubscriptionRangeFilters,
    /// Group ごとの終端位置 (draft §11.1.1 (Object Status) / §11.3.1 (Subgroup Header))
    ///
    /// 値は「その Group に存在しない最小の Object ID」。この値以上の Object ID を持つ
    /// Object を受信したら §12.1 (Malformed Tracks) の条件 4 に該当する。
    ///
    /// 登録契機は 3 つあり、いずれも同じ表現に正規化する。終端を宣言した Object 自身は
    /// §12.1 条件 4 の "the final Object in the Group" として存在するものとして扱うため、
    /// その位置は「存在しない」に含めない (同一位置の重複は条件 6 の内容比較に委ねる)。
    ///
    /// - Object Status 0x3 (End of Group) を位置 (G, N) で受信 → `N + 1`
    ///   (§11.1.1 は "Object ID that is greater than or equal to the one specified" と述べるが、
    ///   §12.1 条件 4 は宣言した Object を final Object と定義し "larger than" だけを Malformed と
    ///   するため、宣言位置の 1 つ先を「存在しない最小」とする)
    /// - datagram の END_OF_GROUP bit を位置 (G, N) で受信 → `N + 1`
    ///   (§11.2.1 は "an Object ID greater than the Object ID in this datagram" と
    ///   その位置より大きいものが存在しないと述べる)
    /// - ヘッダの END_OF_GROUP bit が立った subgroup stream が FIN で終わった → 最終 Object ID + 1
    ///   (§12.1 の "the last Object before a FIN in a Subgroup which has the END_OF_GROUP bit set")
    ///
    /// 上記のうち Object ID から 1 つ先を計算する経路は `saturating_add` を使うため、
    /// Object ID が `u64::MAX` の場合は境界を進められず、終端を宣言した Object 自身の再受信も
    /// Malformed のままになる (有効な Object ID 1 点だけの既知の限界)。
    /// Object 0 個の Subgroup は `record_group_end_after` を通らず 0 を直接記録するため、
    /// この限界の対象外である。
    pub ended_groups: HashMap<u64, u64>,
    /// Track の終端位置 (draft §11.1.1 (Object Status))
    ///
    /// 値は終端を宣言した Object 自身の Location (存在しない最小の Location ではない)。
    /// Object Status 0x4 (End of Track) を位置 L で受信したら `L` を格納し、`L` より大きい
    /// Location の Object を §12.1 (Malformed Tracks) 条件 5 として拒否する。
    /// §12.1 条件 5 は "whose Group and Object ID are larger than the final Object in the Track"
    /// と定め、final Object (End of Track を宣言した Object 自身) は Malformed にしない
    /// (§11.1.1 の "Location that is equal to or greater than the one specified" との差は、
    /// 宣言した Object 自身を存在するものとして扱うことで解消する)。
    pub end_of_track: Option<Location>,
    /// 未応答の REQUEST_UPDATE の適用待ちグループ (受信順)
    ///
    /// draft-ietf-moq-transport-23 §9.5 (REQUEST_UPDATE): 受信側は REQUEST_UPDATE を
    /// 受信順に処理し、更新ごとに 1 つの REQUEST_UPDATE_OK / REQUEST_UPDATE_ERROR を返す。
    /// 処理を合体 (coalesce) して累積結果のみを適用することは許されるが、FILL_PARAMETERS を
    /// 含む更新は合体できない (§9.19.15 (FILL PARAMETERS Parameter))。そのため通常の更新は
    /// 末尾のグループへ累積し、FILL_PARAMETERS を含む更新の前後を境界としてグループを
    /// 分割する。
    ///
    /// 先頭が最も古い未応答グループであり、`Session::send_request_ok` が先頭から 1 グループを
    /// 適用して取り除く。これにより応答の返却順と適用順が受信順に一致する。
    /// FILL_PARAMETERS は運んできたメッセージにのみ適用され subscription 状態として保持されない
    /// (sticky 対象外) ため、グループのパラメータからは除去する。
    ///
    /// 受信時に REQUEST_ERROR で拒否することが決まった更新も、先行グループの応答との順序を
    /// 守るため、拒否を表すグループとして同じキューへ積む
    /// (draft §9.5: "REQUEST_UPDATE_OKs MUST be returned in the order the REQUEST_UPDATEs
    /// were received.")。先頭が拒否グループになった時点で `Session::send_request_ok` が
    /// REQUEST_ERROR を送出する。
    /// 拒否グループより後に受信した更新は受理グループとして積まれることがあるが、拒否の
    /// REQUEST_ERROR 送出で購読が終端するため、それらのグループは適用されずに解放される。
    ///
    /// 既定構成では自側 MAX_REQUEST_UPDATES が 0 (無制限) のため、peer は未応答の更新を
    /// いくつでも送れる。FILL_PARAMETERS を含む更新は合体できず更新ごとに 1 グループ増える
    /// ため、[`MAX_PENDING_UPDATE_GROUPS`] を実装上限として設け、超過時はセッションを閉じる。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub pending_update_params: VecDeque<PendingUpdateGroup>,
}

/// 未応答の REQUEST_UPDATE グループ数の実装上限
///
/// draft-ietf-moq-transport-23 §9.1.7 (MAX_REQUEST_UPDATES): 既定値 0 は
/// "the endpoint does not limit REQUEST_UPDATE concurrency" を意味する。しかし
/// FILL_PARAMETERS を含む更新は合体できない (§9.19.15 (FILL PARAMETERS Parameter)) ため、
/// 更新ごとに [`PendingUpdateGroup`] が 1 つ増える。アプリの応答が滞留した状態で peer が
/// 更新を送り続けると、未応答グループ数に上限が無くなる。
///
/// そこで自側 MAX_REQUEST_UPDATES が 0 のときだけ本値を実装上限として適用する
/// (宣言済みなら peer はその数を超えられず、§9.1.7 のとおり超過時にセッションを閉じる)。
/// 1 グループのサイズは制御メッセージ長で抑えられるが、その数百倍は無視できないため、
/// 正常なアプリの応答遅延では到達しない 256 とする。
pub const MAX_PENDING_UPDATE_GROUPS: usize = 256;

/// 受信時に REQUEST_ERROR で拒否することを決めた REQUEST_UPDATE の応答内容
///
/// 拒否は受信順の応答を守るため [`PendingUpdateGroup`] としてキューに積み、
/// 先行グループの応答 (REQUEST_UPDATE_OK) が送出された後に REQUEST_ERROR を送出する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingUpdateRejection {
    /// REQUEST_ERROR の Error Code (draft-ietf-moq-transport-23 §12.3 (Request Error Codes))
    pub error_code: u64,
    /// REQUEST_ERROR の Reason Phrase
    ///
    /// 拒否理由は静的な実装メッセージであり 1024 バイト上限
    /// (draft §8.5 (Reason Phrase Structure)) を超えない。
    pub reason: &'static str,
}

/// 未応答の REQUEST_UPDATE 1 グループ分の適用待ちパラメータ
///
/// draft-ietf-moq-transport-23 §9.5 (REQUEST_UPDATE) は、受信側が複数の REQUEST_UPDATE を
/// 合体して累積結果のみを適用することを許す。合体の単位が本構造体であり、通常の更新は
/// 1 グループへ累積する。FILL_PARAMETERS を含む更新は合体できない (§9.19.15) ため、
/// その更新だけを含むグループとして前後の更新から分離する。
///
/// 拒否する更新は [`Self::rejection`] が `Some` のグループとして表し、`parameters` は空、
/// `has_fill_parameters` は `false` になる。拒否グループはパラメータを適用しないため、
/// 合体の境界としても扱う (後続の更新を拒否グループへ累積しない)。
///
/// TRACK_NAMESPACE_PREFIX (0x34) も合体を禁止されるパラメータだが
/// (§9.19.20 (TRACK_NAMESPACE_PREFIX Parameter))、SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS は
/// 本 Session が request として受理しないため本キューには現れない。将来これらの request を
/// 扱う場合も、同じ境界規則で他の更新と分離する必要がある。
#[derive(Debug, Clone)]
pub struct PendingUpdateGroup {
    /// このグループに属する REQUEST_UPDATE の累積パラメータ
    ///
    /// FILL_PARAMETERS は含まない。`send_request_ok` が応答時に subscription 状態へ
    /// 適用する値そのものである。拒否グループでは空になる。
    pub parameters: MessageParameters,
    /// このグループが FILL_PARAMETERS を含む更新かどうか
    ///
    /// グループ境界の判定だけに使う。受信した更新が合体できない境界かどうかを表し、
    /// fill fetch stream の開設は受信時点のメッセージに対して行うため、この値を
    /// 開設判定には使わない。拒否グループでは `false` になる。
    pub has_fill_parameters: bool,
    /// REQUEST_ERROR で拒否する更新のとき `Some`
    ///
    /// `Some` のグループは `parameters` を適用せず、`send_request_ok` が拒否応答を送出する。
    pub rejection: Option<PendingUpdateRejection>,
}

impl PendingUpdateGroup {
    /// REQUEST_ERROR で拒否する更新のグループか
    pub fn is_rejection(&self) -> bool {
        self.rejection.is_some()
    }

    /// 受信時に拒否を決めた更新のグループを作る
    ///
    /// 拒否グループはパラメータを適用しないため `parameters` は空、
    /// `has_fill_parameters` は `false` にする。
    pub fn rejected(rejection: PendingUpdateRejection) -> Self {
        Self {
            parameters: MessageParameters::new(),
            has_fill_parameters: false,
            rejection: Some(rejection),
        }
    }
}

/// subscription に適用されている Range Filter 群 (draft-ietf-moq-transport-22 §3.3.2 (Range Filters))
///
/// Object 系 Range Filter (0x25-0x28) を型付きで保持する。TRACK_PROPERTY_FILTER (0x29) は
/// PUBLISH 選別に使うもので subscription の Object 判定には使わないため含めない。
///
/// §3.3.2: 同じ SetID を持つフィルタ同士を AND し、SetID 間を OR する。
/// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubscriptionRangeFilters {
    /// SUBGROUP_FILTER (0x25)。Subgroup ID を対象とする
    pub subgroup: Vec<RangeFilterSet>,
    /// OBJECTID_FILTER (0x26)。Object ID を対象とする
    pub object_id: Vec<RangeFilterSet>,
    /// PRIORITY_FILTER (0x27)。Publisher Priority を対象とする
    pub priority: Vec<RangeFilterSet>,
    /// OBJECT_PROPERTY_FILTER (0x28)。Object Property の値を対象とする
    pub object_property: Vec<RangeFilterSet>,
}

impl SubscriptionRangeFilters {
    /// フィルタが 1 つも設定されていないか
    pub fn is_empty(&self) -> bool {
        self.subgroup.is_empty()
            && self.object_id.is_empty()
            && self.priority.is_empty()
            && self.object_property.is_empty()
    }

    /// 出現する SetID を昇順・重複なしで返す
    ///
    /// §3.3.2 の "The final result is SetID=0 OR SetID=1 OR ... SetID=255" を評価するため、
    /// OR の対象となる SetID を列挙する。
    pub fn set_ids(&self) -> Vec<u8> {
        let mut ids: Vec<u8> = self
            .subgroup
            .iter()
            .chain(self.object_id.iter())
            .chain(self.priority.iter())
            .chain(self.object_property.iter())
            .map(|set| set.set_id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}

/// DEFAULT_PUBLISHER_PRIORITY の既定値 (draft-ietf-moq-transport-22 §10.4 (DEFAULT PUBLISHER PRIORITY))
///
/// §10.4: "If omitted, the Default Publisher Priority is 128."
pub const PUBLISHER_PRIORITY_DEFAULT: u8 = 128;

/// DEFAULT_PUBLISHER_GROUP_ORDER の既定値 Ascending (draft-ietf-moq-transport-22 §10.5 (DEFAULT PUBLISHER GROUP ORDER))
///
/// §10.5: "If omitted, the publisher's preference is Ascending (0x1)."
pub const DEFAULT_PUBLISHER_GROUP_ORDER_ASCENDING: u8 = 0x1;

impl Subscription {
    /// header 明示値 → Track Property → 既定値 128 の順で解決する (draft §10.4 (DEFAULT PUBLISHER PRIORITY))
    ///
    /// SUBGROUP_HEADER の DEFAULT_PRIORITY bit が立っている場合は `header_priority` に
    /// `None` を渡す。DEFAULT_PRIORITY bit が立った Object Datagram も同じ解決を使う
    /// (draft-ietf-moq-transport-22 §11.2.1 (Object Datagram) の「購読を確立した制御メッセージで
    /// 指定された Publisher Priority」がこれにあたる)。
    pub fn resolve_header_publisher_priority(&self, header_priority: Option<u8>) -> u8 {
        header_priority
            .or(self.default_publisher_priority)
            .unwrap_or(PUBLISHER_PRIORITY_DEFAULT)
    }

    /// 観測最大位置を単調に更新する
    pub fn record_largest_received_location(&mut self, location: Location) {
        self.largest_received_location = Some(
            self.largest_received_location
                .as_ref()
                .map_or(location, |existing| *existing.max(&location)),
        );
    }

    /// publisher の実効 Group Order 選好を返す (draft §10.5 (DEFAULT PUBLISHER GROUP ORDER))
    ///
    /// 宣言が無ければ既定値 Ascending (0x1)。subscriber からの要求である
    /// `group_order` (§9.20.8) とは別の値である。
    pub fn effective_publisher_group_order(&self) -> u8 {
        self.default_publisher_group_order
            .unwrap_or(DEFAULT_PUBLISHER_GROUP_ORDER_ASCENDING)
    }

    /// この subscription の initiator が自端点かどうか判定する (draft §3.1 (Subscriptions))
    ///
    /// - initiator == Subscriber で my_role == Subscriber → 自分が initiator
    /// - initiator == Publisher  で my_role == Publisher  → 自分が initiator
    /// - それ以外は peer が initiator (自分は responder)
    pub fn is_initiator_self(&self) -> bool {
        match self.initiator {
            SubscriptionInitiator::Subscriber => self.my_role == TrackRole::Subscriber,
            SubscriptionInitiator::Publisher => self.my_role == TrackRole::Publisher,
        }
    }

    /// Pending かつ Subscriber が initiator (旧 `PendingSubscriber` と等価)
    ///
    /// draft §3.1 (Subscriptions): SUBSCRIBE 送信後 SUBSCRIBE_OK 待ち、または SUBSCRIBE 受信後
    /// SUBSCRIBE_OK 応答前の状態を表す。
    pub fn is_pending_subscriber(&self) -> bool {
        self.state == SubscriptionState::Pending
            && self.initiator == SubscriptionInitiator::Subscriber
    }

    /// Pending かつ Publisher が initiator (旧 `PendingPublisher` と等価)
    ///
    /// draft §3.1 (Subscriptions): PUBLISH 送信後 PUBLISH_OK 待ち、または PUBLISH 受信後
    /// PUBLISH_OK 応答前の状態を表す。
    pub fn is_pending_publisher(&self) -> bool {
        self.state == SubscriptionState::Pending
            && self.initiator == SubscriptionInitiator::Publisher
    }

    /// この subscription をセッションから除去できるか判定する
    ///
    /// PUBLISH_DONE 受信後は通常 drain timer 満了を待つが、`stream_count_overrun` が確定し、
    /// かつ open 中の受信 stream が残っていない場合は早期回収できる。
    /// open stream 判定には subgroup と fill fetch の両方を含む
    /// (`open_incoming_subgroup_count` が両方を会計する)。
    /// 詳細は [`SubscriptionPublishDone`] の doc コメントを参照。
    /// (将来の draft で変更される可能性がある)
    pub fn cleanup_ready(&self) -> bool {
        if self.state != SubscriptionState::Terminated {
            return false;
        }
        match self.publish_done.as_ref() {
            Some(publish_done) => {
                (publish_done.drain.expired || publish_done.stream_count_overrun)
                    && self.stream_counts.open_incoming_subgroup_count == 0
            }
            None => true,
        }
    }

    /// EXPIRES deadline を超過済みか
    pub fn expires_elapsed(&self) -> bool {
        self.expires.as_ref().is_some_and(|expires| expires.expired)
    }

    /// 未応答の REQUEST_UPDATE のうち最も古いグループの累積パラメータを返す
    ///
    /// draft-ietf-moq-transport-23 §9.5 (REQUEST_UPDATE): 未応答の更新が無ければ `None`。
    /// `Some` の場合、次に `Session::send_request_ok` を呼ぶとこの値が subscription 状態へ
    /// 適用される (FILL_PARAMETERS は含まない)。更新の受信順のグループ一覧は
    /// `pending_update_params` で参照できる。
    ///
    /// 先頭が拒否待ちグループ (`PendingUpdateGroup::is_rejection`) の場合、その
    /// `parameters` は空であり、次に `send_request_ok` を呼ぶと REQUEST_ERROR が送出される
    /// (拒否した更新のパラメータは保持しない)。
    pub fn pending_update_parameters(&self) -> Option<&MessageParameters> {
        self.pending_update_params
            .front()
            .map(|group| &group.parameters)
    }

    /// 未応答の更新と `current` を先頭から合体した累積表示を返す
    ///
    /// draft-ietf-moq-transport-23 §9.5 (REQUEST_UPDATE): 受信側は複数の更新を合体して
    /// 累積結果のみを適用できるため、「受信順に処理した場合の累積パラメータ」が必要になる。
    /// `current` はこれから処理する REQUEST_UPDATE のパラメータであり、受信時に
    /// 「今回のメッセージまで含めた表示」を必要とする検証とアプリへの通知で使う。
    /// 既に受信済みのグループだけを見たい場合は空の [`MessageParameters`] を渡す。
    ///
    /// 各グループの `parameters` は FILL_PARAMETERS を除去済みである。`current` に
    /// FILL_PARAMETERS が含まれる場合はそのまま表示に残る (アプリへの通知用)。
    /// 拒否待ちグループはパラメータを持たないため合体に寄与しない。
    pub fn pending_update_view(&self, current: &MessageParameters) -> MessageParameters {
        let mut view = MessageParameters::new();
        for group in &self.pending_update_params {
            view.merge_from(&group.parameters);
        }
        view.merge_from(current);
        view
    }
}

// ─── Fetch 状態管理 ───────────────────────────────────────────
//
// draft-ietf-moq-transport-22 §3.2.4 (Fetch State Management) / §9.11 (FETCH) / §9.12 (FETCH_OK)

/// Fetch の状態機械 (draft §3.2.4 (Fetch State Management))
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchState {
    /// FETCH 送信直後 (自側=subscriber) / FETCH 受信直後 (自側=publisher)
    /// FETCH_OK / REQUEST_ERROR 未処理
    Pending,
    /// FETCH_OK 送信 or 受信後
    ///
    /// subscriber 側はデータストリームの FIN 後に FETCH_OK を受信した場合も state が
    /// `Terminated` のまま維持されるため、`Established` は FIN 前に FETCH_OK を受信した
    /// 場合のみ。publisher 側は `send_fetch_ok` で `Established` になり、データストリームの
    /// FIN 送信後も維持される。
    Established,
    /// REQUEST_ERROR / STOP_SENDING / FIN / RESET_STREAM、または request stream 上の
    /// GOAWAY の deadline 満了 ([`TerminationReason::GoawayTimeout`]) で終了
    ///
    /// draft-ietf-moq-transport-22 §3.2 (Fetch) により、FIN 後に
    /// FETCH_OK / REQUEST_ERROR が届いても `Terminated` のまま受理する
    /// (FETCH_OK 受理時は終端情報を保存する。REQUEST_ERROR 受理時はフラグ設定と
    /// イベント発行のみ)。`Established` に戻すと fetch が回収不能になるため。
    Terminated,
}

/// Fetch 単位の状態
#[derive(Debug, Clone)]
pub struct Fetch {
    /// 対象 request の Request ID
    pub request_id: u64,
    /// 自端点の役割
    pub my_role: TrackRole,
    /// 要求 track の namespace (常に Some)
    pub track_namespace: Option<TrackNamespace>,
    /// 要求 track の name (常に Some)
    pub track_name: Option<Vec<u8>>,
    /// 要求範囲の Start Location
    ///
    /// draft-ietf-moq-transport-23 §3.2 (Fetch) / §9.19.9 (LOCATION FILTER Parameter):
    /// range は LOCATION_FILTER パラメータで指定する。送信時 (subscriber) / 受信時
    /// (publisher) に解決した値を保持し、応答範囲の終端解決と送信 Object の範囲検証に使う。
    /// 解決結果は常に `Some` になる (Largest Object 未知の相対フィルタは先頭寄りに解決される)。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub fetch_start: Option<Location>,
    /// 要求で明示された範囲の終端 (draft §9.19.9 (LOCATION FILTER Parameter))
    ///
    /// Location Filter が End を運ぶ形式 (Type 0x03 / 0x04) のときだけ `Some`。
    /// End を省略した FETCH の End は Largest Object であるため (同節 / §3.2 (Fetch))、
    /// `None` は「終端が要求で固定されていない」ことを意味し、応答範囲の終端は
    /// FETCH_OK の送受信時点の Largest Object になる。
    /// publisher 側では送信 Object が要求範囲を超えていないかの検証
    /// (`send_fetch_object`) にも使う。応答範囲の終端 (`end_location`) は
    /// `send_fetch_ok` / 初回応答の受信で確定する。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub request_end: Option<Location>,
    /// 現在の状態
    pub state: FetchState,
    /// 応答範囲の終端 (FETCH_OK の送受信時に確定する)
    ///
    /// draft-ietf-moq-transport-23 §3.2 (Fetch) / §3.2.2 (Gaps in a Fetch Stream):
    /// LARGEST_OBJECT が要求範囲内ならその値、そうでなければ要求で明示した End Location
    /// になる。publisher 側は `send_fetch_ok` が、subscriber 側は初回応答の受信が設定する。
    /// Track の終端はこの値では表さない (FETCH 応答ストリームの End of Group /
    /// End of Track で受け取る。§11.4.2.2 (Prior Object Fields))。
    pub end_location: Option<Location>,
    /// FETCH への初回応答 (REQUEST_OK / FETCH への応答の REQUEST_ERROR) を受信済みか
    ///
    /// subscriber 側の受信経路のみで更新され、publisher 側は常に false のまま
    /// (送信側の応答は `send_fetch_ok` / `send_request_error` 呼び出しが担う)。
    /// draft-ietf-moq-transport-23 §3.2 (Fetch) は REQUEST_OK / REQUEST_ERROR の
    /// 到着タイミングをオブジェクト配信に対して制約しないため、データストリームの
    /// FIN 後に届くこともある。アプリが応答受領を確認するために使う。
    ///
    /// 同じ REQUEST_OK で届く FETCH の REQUEST_UPDATE_OK との dispatch にも使う
    /// (§9.3 (REQUEST_OK): Track Properties を運べるのは初回の FETCH_OK だけであり、
    /// REQUEST_UPDATE_OK は空でなければならない)。
    pub response_received: bool,
    /// 自端点が FETCH データストリームを終端済みか (publisher 側のみで更新)
    ///
    /// `send_fetch_data_stream_closed` が呼ばれた時点で `true` になる (FIN と RESET の
    /// 両方の終端通知を含む)。「1 本以上の outgoing stream の終端」を意味し、全ストリーム
    /// の終端はアプリの責務 (draft-ietf-moq-transport-22 §11 は 1 つの FETCH に
    /// 対する応答を 1 本のデータストリームに限定する)。draft-ietf-moq-transport-22 §3.2.4
    /// (Fetch State Management): "It can remove all FETCH state after closing the data
    /// stream with a FIN." に従い、データストリーム終端済みの publisher 側 fetch は
    /// `Established` のままでも破棄可能にするための記録。subscriber 側は常に false のまま
    /// (受信データストリームの FIN で `Terminated` に遷移するため不要)。
    pub data_stream_finished: bool,
    /// 自側が bidi request stream の送信方向へ cancel (RESET_STREAM) を発行済みか
    ///
    /// `Session::send_fetch_stop_sending` (アプリ起点の cancel) と
    /// `Session::send_request_error` (REQUEST_UPDATE 失敗応答) が `true` にする。
    /// 未知の Mandatory Track Property を含む FETCH_OK の受信 (§3.7 (Mandatory to Understand
    /// Track Properties) の cancel) は、この記録を見て cancel イベントの再発行を抑止する。
    /// `FetchState::Terminated` はデータストリームの終端でも遷移するため、状態ではなく
    /// 本フラグで判定する (データストリーム終端は bidi request stream を閉じない)。
    pub local_cancel_sent: bool,
    /// publisher 側が FETCH データストリームで受信した STOP_SENDING のエラーコード
    ///
    /// [`Session::fetch_stop_sending_received`](crate::session::core::Session::fetch_stop_sending_received)
    /// がデータストリームの STOP_SENDING を受理したときだけ設定する。外側の `Option` は
    /// 「その受理が起きたか」、内側は「解釈できたアプリケーションエラーコード」を表す
    /// (内側の `None` の意味は [`RequestStreamEnd::Reset`] の `error_code` を参照する)。
    /// bidi request stream 側の STOP_SENDING が先に fetch を終端した場合、後着の
    /// データストリーム側は受理されないため本フィールドは `None` のままになる
    /// (その場合も cancel の理由は `TerminationReason::PeerStreamReset` が運ぶ)。
    /// subscriber 側は常に `None` のまま。
    /// この節番号・規則は draft 由来であり将来 draft 改版で変わる可能性がある。
    pub peer_stop_sending_error_code: Option<Option<u64>>,
    /// Subscriber Priority (draft-ietf-moq-transport-22 §9.20.7 (SUBSCRIBER PRIORITY Parameter))
    ///
    /// SUBSCRIBE / FETCH / PUBLISH_OK / REQUEST_UPDATE に含まれ、動的に変更可能。
    /// 省略時は `None` で、application 層が default (128) を適用する。
    pub subscriber_priority: Option<u8>,
    /// Group Order (draft-ietf-moq-transport-22 §9.20.8 (GROUP ORDER Parameter))
    ///
    /// SUBSCRIBE / SUBSCRIBE_TRACKS / FETCH に含まれる。draft-ietf-moq-transport-22 §5.1.1 (Definitions) により
    /// 確定後は変更不可。
    pub group_order: Option<u8>,
    /// INCLUDE_PROPERTIES (draft-ietf-moq-transport-22 §9.20.21 (INCLUDE_PROPERTIES Parameter))
    ///
    /// peer が FETCH で指定した値。`Some(0)` のとき FETCH_OK の Track Properties を
    /// 空にする。`None` (省略時) は default 1 として扱い従来どおり含める。
    pub include_properties: Option<u8>,
}

/// TRACK_STATUS (draft §9.13 (TRACK_STATUS)) の応答
///
/// `TrackStatusEntry.response` が `None` なら応答待ち、`Some(_)` なら応答済み。
/// `Ok` / `Unsupported` / `Error` で結果を区別する。`error_code` 等の詳細や未対応の
/// Property Type は `SessionEvent` 経由で application へ通知済みなので本構造体には含めない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackStatusResponse {
    /// REQUEST_OK 受信 / 送信済み (LARGEST_OBJECT parameter があれば保持する)
    Ok {
        /// 確定した Largest Location
        largest_location: Option<Location>,
    },
    /// REQUEST_OK 受信済みだが、未知の Mandatory Track Property を含むため Track を利用しない
    ///
    /// draft-ietf-moq-transport-23 §3.7 (Mandatory to Understand Track Properties): 未知の
    /// Mandatory Track Property を含む TRACK_STATUS_OK を受信した endpoint は、その Track を
    /// 処理してはならない (MUST)。応答自体は受領済みであるため [`Self::Error`] とは区別し、
    /// 未対応の型は [`SessionEvent::TrackStatusUnsupported`] が運ぶ。
    Unsupported,
    /// REQUEST_ERROR 受信 / 送信済み
    Error,
}

/// TRACK_STATUS の状態エントリ
///
/// 自側が送った要求 (requester) と peer から受けた要求 (responder) の両方を表す。
/// `my_role` でどちらかを判別する。
#[derive(Debug, Clone)]
pub struct TrackStatusEntry {
    /// 対象 request の Request ID
    pub request_id: u64,
    /// 自側がこの request で担う役割
    ///
    /// `Subscriber` は自側が送った要求の応答待ち (`Session::send_track_status`)、
    /// `Publisher` は peer から受けた要求への応答待ち (`Session::send_request_ok` で
    /// TRACK_STATUS_OK を送る側) を表す。
    pub my_role: TrackRole,
    /// 対象 Track の名前空間
    pub track_namespace: TrackNamespace,
    /// 対象 Track 名
    pub track_name: Vec<u8>,
    /// peer が TRACK_STATUS で指定した INCLUDE_PROPERTIES
    /// (draft-ietf-moq-transport-22 §9.20.21 (INCLUDE_PROPERTIES Parameter))
    ///
    /// 自側が responder のときだけ値が入る。`Some(0)` のとき TRACK_STATUS_OK の
    /// Track Properties を空にする。`None` (省略時) は default 1 として扱い従来どおり含める。
    pub include_properties: Option<u8>,
    /// 応答情報。`None` = 応答待ち、`Some(Ok { .. })` = TRACK_STATUS_OK 受信 / 送信済み、
    /// `Some(Unsupported)` = 未知の Mandatory Track Property を含む TRACK_STATUS_OK を受信済み
    /// (`draft-ietf-moq-transport-23` §3.7 (Mandatory to Understand Track Properties))、
    /// `Some(Error)` = REQUEST_ERROR 受信 / 送信。
    pub response: Option<TrackStatusResponse>,
    /// 自側の送信方向が閉じたか
    ///
    /// `true` になるのは次の場合である。
    ///
    /// - 自側が requester のときに peer の FIN / RESET_STREAM を受けた場合
    /// - 自側が responder のときに peer の RESET_STREAM (cancel) を受けた場合。responder は
    ///   peer の FIN を終端として扱わず応答を送るまで遅延するため (`Session::defers_peer_fin`)、
    ///   FIN だけでは `false` のまま
    /// - request stream 上の GOAWAY の deadline 満了で Session が終端した場合
    ///   ([`TerminationReason::GoawayTimeout`])
    ///
    /// 自側が応答を送った場合は `response` が `Some` になり、`terminated` は変化しない。
    /// entry は `response` が `Some` か `terminated` が `true` なら破棄できる
    /// (`Session::forget_track_status` の受理条件)。
    pub terminated: bool,
}

// ─── GOAWAY / Migration ────────────────────────────────────
//
// draft-ietf-moq-transport-22 §6.6 (Termination), §6.6.1 (Graceful Session Migration), §9.2 (GOAWAY)

/// New Session URI の最大長 (draft-ietf-moq-transport-22 §9.2 (GOAWAY))
pub const MAX_NEW_SESSION_URI_LENGTH: usize = 8192;

/// peer から受信した GOAWAY の情報
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerGoawayInfo {
    /// 新しいセッション URI (Server 送信のみ non-empty、最大 8192 バイト)
    pub new_session_uri: Vec<u8>,
    /// sender が session を閉じるまで待つ時間 (ミリ秒)
    pub timeout: u64,
}

/// GOAWAY sender 側の drain blocker snapshot
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoawayDrainSnapshot {
    /// cleanup 未完了の subscription の Request ID 群
    pub blocking_subscription_request_ids: Vec<u64>,
    /// cleanup 未完了の fetch の Request ID 群
    pub blocking_fetch_request_ids: Vec<u64>,
    /// 応答未完了の track status (TRACK_STATUS) の Request ID 群
    ///
    /// 応答が `None` かつ破棄可能でない (`terminated` が `false`) entry のみを含む。
    /// `terminated` の entry は応答を送れず `forget_track_status` で破棄できるため、
    /// 応答未受領でも drain を妨げない。subscription / fetch の blocker と同じく
    /// 「破棄可能な entry は blocker にしない」という本実装の判定である。
    pub blocking_track_status_request_ids: Vec<u64>,
}

impl GoawayDrainSnapshot {
    /// blocker が残っておらず drain 完了しているか
    pub fn ready(&self) -> bool {
        self.blocking_subscription_request_ids.is_empty()
            && self.blocking_fetch_request_ids.is_empty()
            && self.blocking_track_status_request_ids.is_empty()
    }
}
