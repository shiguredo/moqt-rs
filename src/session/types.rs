//! MoQT セッション状態機械の型定義
//!
//! 1 本の `MOQT Transport Session` に閉じた [`crate::session::core::Session`] 構造体以外の
//! 全 public 型をまとめる。draft-ietf-moq-transport-21 由来の実装のため将来変更される
//! 可能性がある。

use crate::message::{
    ControlMessage, ReasonPhrase, Redirect, common::Location, common::TrackNamespace,
};
use crate::message_parameter::{LocationFilter, MessageParameters, RangeFilterSet};
use alloc::vec::Vec;
use hashbrown::HashMap;

/// `Transport Session` における endpoint の役割 (draft-ietf-moq-transport-21 §6.2 (Session establishment))
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// セッションを開始する側
    Client,
    /// セッションを受け付ける側
    Server,
}

/// 下位トランスポート種別 (draft-ietf-moq-transport-21 §6.2 (Session establishment))
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// QUIC connection 直接 (moqt:// URI)
    Quic,
    /// WebTransport over HTTP/3
    WebTransport,
}

/// この endpoint から見た 1 本の `Transport Session` の状態
/// (draft-ietf-moq-transport-21 §6.3 (Session initialization))
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

/// セッション終了エラー (draft-ietf-moq-transport-21 §16.11.1 (Session Termination Error Codes))
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionError {
    /// Session Termination Error Code (draft-ietf-moq-transport-21 §16.11.1 (Session Termination Error Codes))
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
/// `SessionError` (draft-ietf-moq-transport-21 §16.11.1 由来) とは分離し、wire への流出を防ぐ。
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
    /// (draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection):
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
    RequestUpdateReceived {
        /// 対象 request の Request ID
        request_id: u64,
        /// 受信したパラメータ
        parameters: MessageParameters,
    },
    /// peer からの PUBLISH_STATE_NOTIFY を受信した
    /// (draft-ietf-moq-transport-21 §9.10 (PUBLISH_STATE_NOTIFY))
    ///
    /// 応答は不要であり、MAX_REQUEST_UPDATES のクレジットも消費しない。
    /// FORWARD / LOCATION_FILTER / LARGEST_OBJECT は session 層で
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
    /// `request_kind` で context を判別する (Publish = 旧 PUBLISH_OK 相当)。
    /// FORWARD は session 層で `Subscription::forward_state` に反映済み。
    /// application は `parameters` から `NEW_GROUP_REQUEST` / `AUTHORIZATION_TOKEN`
    /// 等を読み取る。
    RequestOkReceived {
        /// 対象 request の Request ID
        request_id: u64,
        /// 応答対象の request 種別
        request_kind: RequestKind,
        /// 受信したパラメータ
        parameters: MessageParameters,
    },
    /// peer からの FETCH_OK を受信した (終端情報を含む)
    ///
    /// draft-ietf-moq-transport-21 §9.12 (FETCH_OK): FETCH への応答は FETCH_OK /
    /// REQUEST_ERROR のみ。FIN 前後どちらの到着でも受理時に発火する。
    /// アプリは `Session::fetch` のポーリングなしに再生終了の確定などを
    /// イベント駆動で書ける。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    FetchOkReceived {
        /// 対象 request の Request ID
        request_id: u64,
        /// fetch の終端 Location
        end_location: Location,
        /// Track 終端に達したか
        end_of_track: bool,
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
    /// draft-ietf-moq-transport-21 §9.4 (REQUEST_ERROR): REQUEST_ERROR は任意の request への失敗応答。
    /// - `Pending*` 状態からの受信: 初回 SUBSCRIBE / PUBLISH / FETCH /
    ///   SUBSCRIBE_NAMESPACE / PUBLISH_NAMESPACE / TRACK_STATUS への失敗応答で、
    ///   対象 request は Terminated に遷移済み。
    /// - `Established` 状態からの受信: REQUEST_UPDATE への失敗応答。
    ///   subscription / fetch は Terminated に遷移する (draft §3.1.1: REQUEST_ERROR の
    ///   受信で subscription state を終える。fetch は draft §3.2.1 の破棄許可を行使する)。
    ///   namespace 系 (SUBSCRIBE_NAMESPACE / PUBLISH_NAMESPACE / SUBSCRIBE_TRACKS) は
    ///   state を維持する。
    ///   subscription の publisher role は `send_request_error` 内で Terminated に遷移し、
    ///   PUBLISH_DONE(UPDATE_FAILED) を自動送信する (ワイヤ順序は
    ///   REQUEST_ERROR → PUBLISH_DONE、open 中の outgoing subgroup stream がある場合は
    ///   §9.9 の MUST NOT に従い全 stream 終端後に送信、
    ///   draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions))。
    ///   残存 outgoing subgroup stream の FIN / RESET はアプリケーション層の責務である。
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
        /// (draft-ietf-moq-transport-21 §9.4.2 (REQUEST_ERROR Message Format)
        /// および §9.4.2 内の REDIRECT 定義)。
        /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
        retry_interval: u64,
        /// 理由
        reason: ReasonPhrase,
        /// Error Code = REDIRECT の場合のみ付加される (draft-ietf-moq-transport-21 §9.4.2 (REQUEST_ERROR Message Format))
        redirect: Option<Redirect>,
    },
    /// peer からの NAMESPACE を受信した (draft §9.16 (NAMESPACE))
    NamespaceReceived {
        /// 対象 request の Request ID
        request_id: u64,
        /// 対象の名前空間サフィックス
        suffix: TrackNamespace,
    },
    /// peer からの NAMESPACE_DONE を受信した (draft §9.17 (NAMESPACE_DONE))
    NamespaceDoneReceived {
        /// 対象 request の Request ID
        request_id: u64,
        /// 対象の名前空間サフィックス
        suffix: TrackNamespace,
    },
    /// peer からの PUBLISH_SKIPPED を受信した (draft §9.19 (PUBLISH_SKIPPED))
    PublishSkippedReceived {
        /// 対象 request の Request ID
        request_id: u64,
        /// 対象の名前空間サフィックス
        suffix: TrackNamespace,
        /// 対象 Track 名
        track_name: Vec<u8>,
    },
    /// peer からの SUBSCRIBE_TRACKS を受信した (draft §9.18 (SUBSCRIBE_TRACKS))
    ///
    /// PREFIX_OVERLAP 自動拒否の**後**（受け入れが確定した場合）に発火する。
    ///
    /// パラメータ伝播 (draft-ietf-moq-transport-21 §9.18.1 (Parameters on
    /// SUBSCRIBE_TRACKS)): SUBSCRIBE_TRACKS の Parameters は resulting PUBLISH の
    /// initial subscription parameters として明示的に載る。伝播の構築はアプリの
    /// 責務であり、ライブラリは自動注入しない。アプリは本イベントの `parameters` に
    /// `MessageParameters::resulting_publish_parameters` を適用し、得られた値を
    /// `send_publish` の `parameters` に渡すことで伝播を実現できる
    /// (AUTHORIZATION TOKEN は除外される。FORWARD=0 のときのみ明示し、1 は省略でよい)。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    SubscribeTracksReceived {
        /// 対象 request の Request ID
        request_id: u64,
        /// 対象の名前空間プレフィックス
        prefix: TrackNamespace,
        /// 受信したパラメータ
        parameters: MessageParameters,
    },
    /// bidi request stream の終端 (または REQUEST_ERROR 等による) で request が
    /// Terminated 状態に遷移した (draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection))
    ///
    /// `reason` に応じて:
    /// - `PeerStreamFin` / `PeerStreamReset`: 相手側が bidi request stream を閉じた
    /// - `LocalCancel`: 自側から cancel した
    /// - `SupersededByPublish`: PUBLISH 受信で既存 Pending(Subscriber) が置き換えられた (draft §3.1 (Subscriptions))
    /// - `NamespaceImplicitDone`: SUBSCRIBE_NAMESPACE 終端時に残った active suffix を暗黙 NAMESPACE_DONE として通知 (draft §9.15 (SUBSCRIBE_NAMESPACE))
    RequestTerminated {
        /// 対象 request の Request ID
        request_id: u64,
        /// request の種別
        kind: RequestKind,
        /// 理由
        reason: TerminationReason,
    },
    /// peer からの GOAWAY を受信した (draft-ietf-moq-transport-21 §9.2 (GOAWAY))
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
    /// (draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams))。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    ResetDataStream {
        /// 対象 data stream の ID
        stream_id: DataStreamId,
        /// エラーコード
        error_code: u64,
        /// RESET_STREAM_AT の reliable size (`None` は RESET_STREAM)
        reliable_size: Option<u64>,
    },
    /// fill fetch stream を開く (draft-ietf-moq-transport-21 §3.4 (Fill Semantics))
    ///
    /// publisher が FILL_PARAMETERS 付き SUBSCRIBE / REQUEST_UPDATE を Forward State 1 で
    /// 処理し、fill range が空でなく Largest Object より後に始まらない場合に発火する。
    /// I/O 層は uni stream を開き、先頭に `request_id` を載せた FETCH_HEADER を書いて
    /// fill range の Object 送出を開始し、送り終わったら FIN で閉じる
    /// (失敗時は FETCH_HEADER の直後で即 reset する)。
    /// アプリは開いた stream を [`Session::send_fill_fetch_header`](crate::session::core::Session::send_fill_fetch_header)
    /// で Session に登録すること。1 つの subscription に複数本の fill stream が
    /// 同時に開くことがあり、いずれも同じ `request_id` (起因メッセージのもの) を載せる。
    /// fill range が empty または Largest Object より後に始まる場合は発火しない。
    /// subscription のキャンセル時は開いた fill stream を Session が
    /// `ResetDataStream` で自動 reset する。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    OpenFillFetchStream {
        /// fill 起因の request の Request ID
        request_id: u64,
    },
    /// bidi request stream の送信方向を RESET_STREAM で打ち切る
    ///
    /// draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection):
    /// "Implementations cancel a request by abruptly terminating any directions of the
    /// stream that are still open, using RESET_STREAM for a direction they are sending
    /// and STOP_SENDING for a direction they are receiving."
    /// I/O 層は `request_id` に対応する bidi stream の送信方向を `error_code` で reset する。
    /// `error_code` は §12.5 (Stream Reset Error Codes) のコードを使う。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    ResetRequestStream {
        /// 対象 request の Request ID
        request_id: u64,
        /// Stream Reset Error Code (§12.5)
        error_code: u64,
    },
    /// bidi request stream の受信方向を STOP_SENDING で打ち切る
    ///
    /// draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection) の
    /// 受信方向の打ち切り。I/O 層は `request_id` に対応する bidi stream の受信方向に
    /// `error_code` で STOP_SENDING を送る。`error_code` は §12.5 のコードを使う。
    /// Session の自動発火経路は現在なく、受信方向の cancel は I/O 層主導で行う。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    StopSendingRequestStream {
        /// 対象 request の Request ID
        request_id: u64,
        /// Stream Reset Error Code (§12.5)
        error_code: u64,
    },
}

/// data stream を reset する理由 (draft-ietf-moq-transport-21 §12.5 (Stream Reset Error Codes))
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
    /// may have a more detailed status code."
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
    /// is being terminated."
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
    /// draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) の検出経路 (Data Stream 側のヘッダ検証・
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
/// QUIC / WebTransport の stream ID をそのまま使ってもよいし、caller 独自の
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
    /// draft-ietf-moq-transport-21 §3.1.2 (Track Alias): "Objects can arrive after a
    /// subscription has been cancelled. Subscribers SHOULD retain sufficient state to quickly
    /// discard these unwanted Objects, rather than treating them as belonging to an unknown
    /// Track Alias."
    ///
    /// `UnknownTrackAlias` と区別することで、caller は「知らない alias なので buffer するか
    /// 判断が必要」と「元は知っていた alias なので確実に捨てて良い」を切り分けられる。
    Discarded,
    /// 候補 subscription は存在するが、フィルタ再適用の結果どの subscription にも属さなかった
    ///
    /// draft-ietf-moq-transport-21 §3.1 (Subscriptions): "the subscriber re-applies each
    /// subscription's filter to determine which subscription a received Object belongs to."
    /// どのフィルタも通らない Object はセッションを閉じずに破棄する (§3.1 / §11.2 は
    /// 受信側の破棄を要求していないが、紐づけ先がないため受け入れられない)。
    FilteredOut,
}

/// `Session::recv_datagram` の判定結果 (draft-ietf-moq-transport-21 §11 (Data Streams and Datagrams))
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

// ─── bidi request stream のライフサイクル型 ─────────────────
//
// draft-ietf-moq-transport-21 §6.3 (Session initialization) / §6.4.2.3 (Request Cancellation and Rejection)
// bidi request stream のライフサイクルを追跡するための基礎型。
// 将来 draft が変更される可能性がある。

/// bidi request stream の開始メッセージ種別 (draft-ietf-moq-transport-21 §6.3 (Session initialization))
///
/// bidi request stream は以下の 7 種類のいずれかの制御メッセージで開始する。
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
    /// PUBLISH_NAMESPACE で開始
    PublishNamespace,
    /// SUBSCRIBE_NAMESPACE で開始
    SubscribeNamespace,
    /// SUBSCRIBE_TRACKS で開始
    SubscribeTracks,
}

/// bidi request stream の終端原因 (draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection))
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
        error_code: u64,
        /// RESET_STREAM_AT の reliable size (`None` は RESET_STREAM)
        reliable_size: Option<u64>,
    },
}

/// request が終端した原因 (draft-ietf-moq-transport-21 §3.1 (Subscriptions) / §9.15 (SUBSCRIBE_NAMESPACE) / §6.4.2.3 (Request Cancellation and Rejection))
///
/// `SessionEvent::RequestTerminated` の payload として使う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminationReason {
    /// peer が FIN で bidi request stream を閉じた
    PeerStreamFin,
    /// peer が RESET_STREAM で打ち切った
    PeerStreamReset {
        /// peer が載せた Stream Reset Error Code
        error_code: u64,
    },
    /// 自側が cancel / abort / close で打ち切った
    LocalCancel,
    /// draft §3.1 (Subscriptions): PUBLISH 受信により既存 Pending(Subscriber) が
    /// 自動的に Terminated に遷移した
    SupersededByPublish {
        /// 置き換え後の PUBLISH の Request ID
        new_request_id: u64,
    },
    /// SUBSCRIBE_NAMESPACE の終端時、残っていた active suffix に対する
    /// implicit NAMESPACE_DONE 相当 (draft §9.15 (SUBSCRIBE_NAMESPACE))
    NamespaceImplicitDone {
        /// 暗黙 NAMESPACE_DONE とみなした suffix 群
        suffixes: Vec<TrackNamespace>,
    },
    /// Malformed Track を検出して該当 request をキャンセルした (draft §12.1 (Malformed Tracks))
    ///
    /// §12.1: "When a subscriber detects a Malformed Track, it MUST cancel any corresponding
    /// subscription or fetches for that Track from that publisher (see Section 3.3.3), and
    /// SHOULD deliver an error to the application."
    ///
    /// 本イベントが SHOULD の「アプリケーションへのエラー通知」に相当する。アプリケーションは
    /// §6.4.2.3 に従い当該 bidi request stream を RESET_STREAM / STOP_SENDING で閉じる。
    /// その際のコードは [`REQUEST_MALFORMED_TRACK`](crate::error::REQUEST_MALFORMED_TRACK) /
    /// [`STREAM_MALFORMED_TRACK`](crate::error::STREAM_MALFORMED_TRACK) を使う。
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
// draft-ietf-moq-transport-21 §3.1 (Subscriptions), §3.1.1 (Subscription State Management)

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
    /// 終了 (REQUEST_ERROR / PUBLISH_DONE / STOP_SENDING / supersede で遷移)
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
    /// `duration_ms == 0` は即期限切れとして扱う。例えば draft-ietf-moq-transport-21
    /// §9.20.17（将来の draft で変更される可能性がある）の EXPIRES=0 を「期限なし」
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
        DeadlineTimer, DeliveryTimeoutState, HashMap, StreamCountState, Subscription,
        SubscriptionInitiator, SubscriptionPublishDone, SubscriptionRangeFilters,
        SubscriptionState, TrackNamespace, TrackRole,
    };
    use crate::message::ReasonPhrase;
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
                subgroup_overrides: HashMap::new(),
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
            include_properties: None,
            filter_start: None,
            filter_end: None,
            range_filters: SubscriptionRangeFilters::default(),
            ended_groups: HashMap::new(),
            end_of_track: None,
            pending_update_params: None,
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
/// draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE):
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

/// draft-ietf-moq-transport-21 §5.2 (Delivery Timeouts and Data Reliability) の
/// OBJECT_DELIVERY_TIMEOUT / SUBGROUP_DELIVERY_TIMEOUT の subscriber 申告 /
/// publisher 申告 / effective 値の 3 つ組と、§10.1-12.2 の per-subgroup オーバーライドを保持する。
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
    /// subgroup 固有の delivery timeout オーバーライド (draft-ietf-moq-transport-21 §10.1-12.2)
    ///
    /// キー: (group_id, subgroup_id)、値: (subgroup_timeout, object_timeout)。
    /// subgroup の先頭 object に付与された Object Property から抽出した raw 値を保持する。
    /// `None` フィールドは Object Property 不在を意味し、Track Property にフォールバックする。
    pub subgroup_overrides: HashMap<(u64, u64), (Option<u64>, Option<u64>)>,
}

/// draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE) の Stream Count 監視と
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
    /// Forward State (draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter), 0 = 送らない / 1 = 送る)
    pub forward_state: u8,
    /// 広告された Largest Object (draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter))
    ///
    /// peer が SUBSCRIBE_OK / PUBLISH / REQUEST_UPDATE_OK / PUBLISH_STATE_NOTIFY の
    /// LARGEST_OBJECT parameter で広告した値を subscriber 側で保存する。
    /// publisher 側は保存しない (自側の観測値は `largest_received_location` で追跡する)。
    /// `effective_largest_object` の max 導出に使う。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub largest_location: Option<Location>,
    /// 購読に紐づく object の最大 Location (draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter))
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
    /// draft-ietf-moq-transport-21 §9.20.18 は LARGEST_OBJECT parameter が SUBSCRIBE_OK / PUBLISH /
    /// REQUEST_UPDATE_OK / TRACK_STATUS_OK / PUBLISH_STATE_NOTIFY に乗ると規定する。
    /// 自端点が送信する LARGEST_OBJECT 計算に `largest_location` との max を取るために使う。
    pub largest_received_location: Option<Location>,
    /// delivery timeout 関連の状態 (subscriber / publisher 申告値、effective 値、per-subgroup オーバーライド)
    pub delivery_timeouts: DeliveryTimeoutState,
    /// subscriber 側が指定した RENDEZVOUS_TIMEOUT parameter (ms)
    ///
    /// relay / application layer が publisher discovery policy を判断できるよう、
    /// `SUBSCRIBE` に含まれていた値を lossless に保持する。`Some(0)` は「待たない」
    /// を表し、`None` とは区別する。`Session` 自体はこの値で timer を動かさない。
    pub subscriber_rendezvous_timeout_ms: Option<u64>,
    /// 直近に観測した EXPIRES parameter
    ///
    /// sender がこの subscription を終了しうる時刻の advisory deadline を表す。
    pub expires: Option<DeadlineTimer>,
    /// Track の DYNAMIC_GROUPS Property (draft §10.6 (DYNAMIC GROUPS)) が `1` で発行されているか
    ///
    /// 自側が Publisher として送信した PUBLISH / SUBSCRIBE_OK、または peer から受信した
    /// PUBLISH / SUBSCRIBE_OK の `TrackProperties` の `DYNAMIC_GROUPS` が `1` であれば `true`。
    /// Publisher 側は peer が `INCLUDE_PROPERTIES=0` を指定して wire 上を空化しても発行時の値で
    /// 保持するが、Subscriber 側は空化された Track Properties を受けるため `false` のままとなり、
    /// その subscription からは `NEW_GROUP_REQUEST` を送れない。
    /// draft-ietf-moq-transport-21 §9.20.20 (NEW GROUP REQUEST Parameter): `NEW_GROUP_REQUEST` parameter は `DYNAMIC_GROUPS == 1` の
    /// Track 以外で REQUEST_UPDATE に送受信してはならない。
    pub dynamic_groups: bool,
    /// publisher が設定した Publisher Priority (draft §5.1.2 Scheduling Algorithm, §10.4 DEFAULT PUBLISHER PRIORITY)
    ///
    /// subgroup header 受信時に「直近の header」の解決値で上書きされる。header が
    /// DEFAULT_PRIORITY bit を立てている (`SubgroupHeader::publisher_priority` が `None`)
    /// 場合は、§10.4 に従って `default_publisher_priority` → 128 の順で解決した値が入る。
    /// Subgroup 単位の検証 (§12.1 条件 1 の priority 一致、重複 Object の優先度一貫性) は
    /// 並行 Subgroup で上書きされうるこの値ではなく、`IncomingDataStream::Subgroup` が
    /// header 時点で保持する解決値を使う。
    pub publisher_priority: Option<u8>,
    /// Track Property の DEFAULT_PUBLISHER_PRIORITY (draft §10.4 (DEFAULT PUBLISHER PRIORITY))
    ///
    /// PUBLISH / SUBSCRIBE_OK / REQUEST_OK の `TrackProperties` から取り込む。
    /// 宣言されていなければ `None` で、その場合の実効値は §10.4 のデフォルト 128 になる。
    /// 実効値の解決は [`effective_publisher_priority`](Self::effective_publisher_priority)。
    pub default_publisher_priority: Option<u8>,
    /// Track Property の DEFAULT_PUBLISHER_GROUP_ORDER (draft §10.5 (DEFAULT PUBLISHER GROUP ORDER))
    ///
    /// publisher の選好であり、§9.20.9 の GROUP ORDER Parameter が運ぶ subscriber からの
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
    /// draft-ietf-moq-transport-21 §9.9 (PUBLISH_DONE): "A sender MUST NOT send
    /// PUBLISH_DONE until it has closed all streams it will ever open..." の MUST NOT と
    /// §9.5.1 の MUST (REQUEST_UPDATE 失敗時、publisher は PUBLISH_DONE (UPDATE_FAILED) を
    /// 送る) を両立するため、open 中の outgoing subgroup stream が残っている間は push を
    /// 保留し、全 stream 終端後 (`send_data_stream_closed` / `reset_outgoing_data_stream`
    /// で `has_open_outgoing_data_streams_for_request` が false になった時点) に自動送信する。
    /// 既存の `publish_done` フィールド (受信側 drain timer 用。送信側 PUBLISH_DONE とは
    /// 無関係) とは別フィールドであり、`cleanup_ready()` の判定に影響しない。
    /// push と同時に `None` に戻り (二重 push を防ぐ)、`forget_subscription` で除去される。
    pub pending_publish_done: Option<u64>,
    /// Subscription Filter (draft-ietf-moq-transport-21 §3.3.1 (Location Filters), §9.20.10 (LOCATION FILTER Parameter))
    ///
    /// SUBSCRIBE / REQUEST_UPDATE に含まれる。
    /// `None` は unfiltered subscription に相当する (draft-ietf-moq-transport-21 §3.3.1 (Location Filters))。
    pub filter: Option<LocationFilter>,
    /// Subscriber Priority (draft-ietf-moq-transport-21 §9.20.8 (SUBSCRIBER PRIORITY Parameter))
    ///
    /// SUBSCRIBE / FETCH / PUBLISH_OK / REQUEST_UPDATE に含まれ、動的に変更可能。
    /// 省略時は `None` で、application 層が default (128) を適用する。
    pub subscriber_priority: Option<u8>,
    /// Group Order (draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter))
    ///
    /// SUBSCRIBE / SUBSCRIBE_TRACKS / FETCH に含まれる。draft-ietf-moq-transport-21 §5.1.1 (Definitions) により
    /// 確定後は変更不可。
    pub group_order: Option<u8>,
    /// INCLUDE_PROPERTIES (draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter))
    ///
    /// peer が SUBSCRIBE で指定した値。`Some(0)` のとき OK 応答の Track Properties を
    /// 空にする。`None` (省略時) は default 1 として扱い従来どおり含める。
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
    /// Group ごとの終端位置 (draft §11.1.2 (Object Status) / §11.3.1 (Subgroup Header))
    ///
    /// 値は「その Group に存在しない最小の Object ID」。この値以上の Object ID を持つ
    /// Object を受信したら §12.1 (Malformed Tracks) の条件 4 に該当する。
    ///
    /// 登録契機は 3 つあり、いずれも同じ表現に正規化する。
    /// - Object Status 0x3 (End of Group) を位置 (G, N) で受信 → `N`
    ///   (§11.1.2 は "Object ID that is greater than or equal to the one specified" と
    ///   その位置自身を含めて存在しないと述べる)
    /// - datagram の END_OF_GROUP bit を位置 (G, N) で受信 → `N + 1`
    ///   (§11.2.1 は "an Object ID greater than the Object ID in this datagram" と
    ///   その位置より大きいものが存在しないと述べる)
    /// - ヘッダの END_OF_GROUP bit が立った subgroup stream が FIN で終わった → 最終 Object ID + 1
    ///   (§12.1 の "the last Object before a FIN in a Subgroup which has the END_OF_GROUP bit set")
    pub ended_groups: HashMap<u64, u64>,
    /// Track の終端位置 (draft §11.1.2 (Object Status))
    ///
    /// 値は「Track に存在しない最小の Location」。Object Status 0x4 (End of Track) を
    /// 位置 L で受信したら `L` を格納する (§11.1.2 は "location that is equal to or greater
    /// than the one specified" とその位置自身を含めて存在しないと述べる)。
    pub end_of_track: Option<Location>,
    /// 合体された REQUEST_UPDATE の累積パラメータ (draft §9.5.1 (Updating Subscriptions))
    ///
    /// REQUEST_UPDATE 受信時に即座に適用せず、マージして蓄積する。
    /// `send_request_ok` 応答時に適用されクリアされる。
    pub pending_update_params: Option<MessageParameters>,
}

/// subscription に適用されている Range Filter 群 (draft-ietf-moq-transport-21 §3.3.2 (Range Filters))
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

/// DEFAULT_PUBLISHER_PRIORITY の既定値 (draft-ietf-moq-transport-21 §10.4 (DEFAULT PUBLISHER PRIORITY))
///
/// §10.4: "If omitted, the Default Publisher Priority is 128."
pub const PUBLISHER_PRIORITY_DEFAULT: u8 = 128;

/// DEFAULT_PUBLISHER_GROUP_ORDER の既定値 Ascending (draft-ietf-moq-transport-21 §10.5 (DEFAULT PUBLISHER GROUP ORDER))
///
/// §10.5: "If omitted, the publisher's preference is Ascending (0x1)."
pub const DEFAULT_PUBLISHER_GROUP_ORDER_ASCENDING: u8 = 0x1;

impl Subscription {
    /// publisher の実効優先度を返す (draft §10.4 (DEFAULT PUBLISHER PRIORITY))
    ///
    /// 解決順は subgroup header の明示値 → Track Property の DEFAULT_PUBLISHER_PRIORITY →
    /// 既定値 128。`publisher_priority` は header 受信時にこの解決を済ませた値が入るため、
    /// header を 1 つも受信していない段階では Track Property か既定値を返す。
    pub fn effective_publisher_priority(&self) -> u8 {
        self.publisher_priority
            .or(self.default_publisher_priority)
            .unwrap_or(PUBLISHER_PRIORITY_DEFAULT)
    }

    /// header 明示値 → Track Property → 既定値 128 の順で解決する (draft §10.4 (DEFAULT PUBLISHER PRIORITY))
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
    /// `group_order` (§9.20.9) とは別の値である。
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
}

// ─── Fetch 状態管理 ───────────────────────────────────────────
//
// draft-ietf-moq-transport-21 §3.2.1 (Fetch State Management) / §9.11 (FETCH) / §9.12 (FETCH_OK)

/// Fetch の状態機械 (draft §3.2.1 (Fetch State Management))
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
    /// REQUEST_ERROR / STOP_SENDING / FIN / RESET_STREAM で終了
    ///
    /// draft-ietf-moq-transport-21 §9.11 (FETCH) により、FIN 後に
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
    /// subscriber が要求した range の Start Location
    ///
    /// draft-ietf-moq-transport-21 §9.11 (FETCH): range は LOCATION_FILTER
    /// パラメータで指定する。送信時に解決した値を保持し、FETCH_OK の
    /// End Location 検証に使う。送信経路の解決は常に `Some` になる
    /// (Largest 未知でも先頭寄りに解決される) ため、`None` は publisher 側
    /// (保持しない) のみであり、その場合は FETCH_OK 検証をスキップする。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub fetch_start: Option<Location>,
    /// 現在の状態
    pub state: FetchState,
    /// FETCH_OK 受信 / 送信で確定した end_location
    pub end_location: Option<Location>,
    /// FETCH_OK 受信 / 送信で確定した end_of_track フラグ
    pub end_of_track: bool,
    /// FETCH への初回応答 (FETCH_OK / FETCH への応答の REQUEST_ERROR) を受信済みか
    ///
    /// subscriber 側の受信経路のみで更新され、publisher 側は常に false のまま
    /// (送信側の応答は `send_fetch_ok` / `send_request_error` 呼び出しが担う)。
    /// draft-ietf-moq-transport-21 §9.11 (FETCH) は FETCH_OK / REQUEST_ERROR の
    /// 到着タイミングをオブジェクト配信に対して制約しないため、データストリームの
    /// FIN 後に届くこともある。二重応答の検出と、アプリが応答受領を確認するために使う。
    pub response_received: bool,
    /// 自端点が FETCH データストリームを終端済みか (publisher 側のみで更新)
    ///
    /// `send_fetch_data_stream_closed` が呼ばれた時点で `true` になる (FIN と RESET の
    /// 両方の終端通知を含む)。「1 本以上の outgoing stream の終端」を意味し、全ストリーム
    /// の終端はアプリの責務 (draft-ietf-moq-transport-21 §9.11 は 1 つの FETCH に
    /// 対する応答を 1 本のデータストリームに限定する)。draft-ietf-moq-transport-21 §3.2.1
    /// (Fetch State Management): "It can remove all FETCH state after closing the data
    /// stream with a FIN." に従い、データストリーム終端済みの publisher 側 fetch は
    /// `Established` のままでも破棄可能にするための記録。subscriber 側は常に false のまま
    /// (受信データストリームの FIN で `Terminated` に遷移するため不要)。
    pub data_stream_finished: bool,
    /// Subscriber Priority (draft-ietf-moq-transport-21 §9.20.8 (SUBSCRIBER PRIORITY Parameter))
    ///
    /// SUBSCRIBE / FETCH / PUBLISH_OK / REQUEST_UPDATE に含まれ、動的に変更可能。
    /// 省略時は `None` で、application 層が default (128) を適用する。
    pub subscriber_priority: Option<u8>,
    /// Group Order (draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter))
    ///
    /// SUBSCRIBE / SUBSCRIBE_TRACKS / FETCH に含まれる。draft-ietf-moq-transport-21 §5.1.1 (Definitions) により
    /// 確定後は変更不可。
    pub group_order: Option<u8>,
    /// INCLUDE_PROPERTIES (draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter))
    ///
    /// peer が FETCH で指定した値。`Some(0)` のとき FETCH_OK の Track Properties を
    /// 空にする。`None` (省略時) は default 1 として扱い従来どおり含める。
    pub include_properties: Option<u8>,
}

// ─── Namespace 系 / TRACK_STATUS 状態管理 ──────────────────
//
// draft-ietf-moq-transport-21 §4 (Namespace Discovery) / §9.13 (TRACK_STATUS) — §9.19 (PUBLISH_SKIPPED)

/// PUBLISH_NAMESPACE (draft §9.14 (PUBLISH_NAMESPACE)) の状態
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamespacePublicationState {
    /// 応答待ち
    Pending,
    /// REQUEST_OK 受信 / 送信済み
    Established,
    /// REQUEST_ERROR or cancel で終了
    Terminated,
}

/// PUBLISH_NAMESPACE の状態エントリ
#[derive(Debug, Clone)]
pub struct NamespacePublication {
    /// 対象 request の Request ID
    pub request_id: u64,
    /// 自端点の役割
    pub my_role: TrackRole,
    /// 対象 Track の名前空間
    pub track_namespace: TrackNamespace,
    /// 現在の状態
    pub state: NamespacePublicationState,
}

/// SUBSCRIBE_NAMESPACE (draft §9.15 (SUBSCRIBE_NAMESPACE)) の状態
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamespaceSubscriptionState {
    /// 応答待ち
    Pending,
    /// 確立済み
    Established,
    /// 終了
    Terminated,
}

/// SUBSCRIBE_NAMESPACE の状態エントリ (draft §9.15 (SUBSCRIBE_NAMESPACE))
#[derive(Debug, Clone)]
pub struct NamespaceSubscription {
    /// 対象 request の Request ID
    pub request_id: u64,
    /// 自端点の役割
    pub my_role: TrackRole,
    /// 対象の名前空間プレフィックス
    ///
    /// 自側 subscriber では REQUEST_OK で確定した適用済みの値 (送信した
    /// REQUEST_UPDATE の TRACK_NAMESPACE_PREFIX は REQUEST_OK を受信するまで
    /// 反映されず、確定待ちは Session 内部で保持する)。自側 publisher では
    /// 受理した REQUEST_UPDATE を反映した値 (確定待ちは保持しない)。
    pub prefix: TrackNamespace,
    /// 現在の状態
    pub state: NamespaceSubscriptionState,
    /// 現在 active な namespace suffix 集合 (NAMESPACE 受信済み、NAMESPACE_DONE 未受信)
    ///
    /// draft §9.15 (SUBSCRIBE_NAMESPACE): NAMESPACE_DONE が対応する NAMESPACE より先に届いた場合は
    /// PROTOCOL_VIOLATION。ストリームリセット時は全件を implicit NAMESPACE_DONE として扱う。
    pub active_suffixes: hashbrown::HashSet<TrackNamespace>,
}

/// SUBSCRIBE_TRACKS の状態 (draft §9.18 (SUBSCRIBE_TRACKS))
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackSubscriptionState {
    /// 応答待ち
    Pending,
    /// 確立済み
    Established,
    /// 終了
    Terminated,
}

/// SUBSCRIBE_TRACKS の状態エントリ (draft §9.18 (SUBSCRIBE_TRACKS))
#[derive(Debug, Clone)]
pub struct TrackSubscription {
    /// 対象 request の Request ID
    pub request_id: u64,
    /// 自端点の役割
    pub my_role: TrackRole,
    /// 対象の名前空間プレフィックス
    ///
    /// 自側 subscriber では REQUEST_OK で確定した適用済みの値 (送信した
    /// REQUEST_UPDATE の TRACK_NAMESPACE_PREFIX は REQUEST_OK を受信するまで
    /// 反映されず、確定待ちは Session 内部で保持する)。自側 publisher では
    /// 受理した REQUEST_UPDATE を反映した値 (確定待ちは保持しない)。
    pub prefix: TrackNamespace,
    /// 現在の状態
    pub state: TrackSubscriptionState,
    /// FORWARD パラメータの値 (draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter))
    pub forward_state: u8,
    /// INCLUDE_PROPERTIES (draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter))
    ///
    /// peer が SUBSCRIBE_TRACKS で指定した値。`Some(0)` のとき resulting PUBLISH の
    /// Track Properties を空にする (application 層が `track_subscription()` で参照する)。
    /// `None` (省略時) は default 1 として扱い従来どおり含める。
    pub include_properties: Option<u8>,
    /// Stream 閉鎖時に未完了の subscription を implicit PUBLISH_DONE 扱いするための追跡
    pub active_track_aliases: hashbrown::HashSet<u64>,
    /// PUBLISH_SKIPPED 送信済みの Track (suffix, track_name) 集合
    ///
    /// draft-ietf-moq-transport-21 §9.19 (PUBLISH_SKIPPED): PUBLISH_SKIPPED 送信後は
    /// 同一 SUBSCRIBE_TRACKS に対して同一 Track の PUBLISH を送信してはならない (MUST NOT)。
    pub skipped_tracks: hashbrown::HashSet<(TrackNamespace, Vec<u8>)>,
    /// TRACK_PROPERTY_FILTER (0x29) の型付き状態 (draft §3.3.2 (Range Filters))
    ///
    /// §3.3.2: "The Track Property Filter can be used in SUBSCRIBE_TRACKS to filter PUBLISH
    /// messages with required Track Property types and values. PUBLISH messages which pass the
    /// filter will be forwarded while those which do not pass it will not be forwarded nor will
    /// any Objects."
    ///
    /// SetID ごとに AND、SetID 間で OR で結合する。空なら選別しない。
    pub track_property_filters: Vec<RangeFilterSet>,
}

/// TRACK_STATUS (draft §9.13 (TRACK_STATUS)) の応答
///
/// `TrackStatusEntry.response` が `None` なら応答待ち、`Some(_)` なら応答済み。
/// `Ok` / `Error` で成功失敗を区別する。`error_code` 等の詳細は `SessionEvent`
/// 経由で application へ通知済みなので本構造体には含めない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackStatusResponse {
    /// REQUEST_OK 受信 / 送信済み (LARGEST_OBJECT parameter があれば保持する)
    Ok {
        /// 確定した Largest Location
        largest_location: Option<Location>,
    },
    /// REQUEST_ERROR 受信 / 送信済み
    Error,
}

/// TRACK_STATUS の状態エントリ
#[derive(Debug, Clone)]
pub struct TrackStatusEntry {
    /// 対象 request の Request ID
    pub request_id: u64,
    /// 自端点の役割
    pub my_role: TrackRole,
    /// 対象 Track の名前空間
    pub track_namespace: TrackNamespace,
    /// 対象 Track 名
    pub track_name: Vec<u8>,
    /// 応答情報。`None` = 応答待ち (旧 `TrackStatusState::Pending`)、
    /// `Some(Ok { .. })` = 旧 `Completed`、`Some(Error)` = 旧 `Failed`。
    pub response: Option<TrackStatusResponse>,
    /// INCLUDE_PROPERTIES (draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter))
    ///
    /// peer が TRACK_STATUS で指定した値。`Some(0)` のとき TRACK_STATUS_OK の
    /// Track Properties を空にする。`None` (省略時) は default 1 として扱い従来どおり含める。
    pub include_properties: Option<u8>,
}

// ─── GOAWAY / Migration ────────────────────────────────────
//
// draft-ietf-moq-transport-21 §6.6 (Termination), §6.6.1 (Graceful Session Migration), §9.2 (GOAWAY)

/// New Session URI の最大長 (draft-ietf-moq-transport-21 §9.2 (GOAWAY))
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
    /// cleanup 未完了の track subscription (SUBSCRIBE_TRACKS) の Request ID 群
    pub blocking_track_subscription_request_ids: Vec<u64>,
    /// cleanup 未完了の namespace subscription (SUBSCRIBE_NAMESPACE) の Request ID 群
    pub blocking_namespace_subscription_request_ids: Vec<u64>,
    /// cleanup 未完了の namespace publication (PUBLISH_NAMESPACE) の Request ID 群
    pub blocking_namespace_publication_request_ids: Vec<u64>,
    /// 応答未完了の track status (TRACK_STATUS) の Request ID 群
    pub blocking_track_status_request_ids: Vec<u64>,
}

impl GoawayDrainSnapshot {
    /// blocker が残っておらず drain 完了しているか
    pub fn ready(&self) -> bool {
        self.blocking_subscription_request_ids.is_empty()
            && self.blocking_fetch_request_ids.is_empty()
            && self.blocking_track_subscription_request_ids.is_empty()
            && self.blocking_namespace_subscription_request_ids.is_empty()
            && self.blocking_namespace_publication_request_ids.is_empty()
            && self.blocking_track_status_request_ids.is_empty()
    }
}
