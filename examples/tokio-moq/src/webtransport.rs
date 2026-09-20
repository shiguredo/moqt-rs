//! WebTransport のトランスポート共通定義
//!
//! WebTransport over HTTP/3 (`webtransport_h3`) と WebTransport over HTTP/2
//! (`webtransport_h2`) が共有するセッション状態・セッション方針・エラーコード変換を提供する。
//! プロトコル固有の処理は各トランスポートのモジュールに置き、両者で意味が揺れては困る
//! 定義だけをここに集める。

use tokio::sync::watch;

/// WebTransport セッションの状態 (draft-ietf-webtrans-http3-16 §6 / §4.7、draft-ietf-webtrans-http2-15 §6.12 / §6.13)
///
/// `tokio::sync::watch` で各タスクへ配る値であり、セッション状態の唯一の置き場所である。
/// ストリームを所有するタスクはこの値の変化を観測し、終了を検知したら自分のストリームを
/// 中断する。状態は `Active` から終了方向にしか進まない。
///
/// セッションの終了 (§6) も接続エラー (HTTP/3 は RFC 9114 §8、HTTP/2 は RFC 9113 §5.4) も
/// 受信経路 (ストリームの accept / datagram の受信 / ストリームの受信待ち) が同じように
/// 観測するため、1 つの `watch` に載せる。接続エラーを別のスロット (`Option<u64>` など) に
/// 分けると、読み出し側が 2 つの状態を突き合わせる必要が生じ、片方の更新を観測し損ねる
/// 余地が残る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WtSessionState {
    /// セッションが確立し、通常どおり使える
    Active,
    /// peer から drain (WT_DRAIN_SESSION / HTTP/3 GOAWAY) の通知を受けた (§4.7 / §6.13)
    Draining,
    /// peer からの終了通知 (CONNECT stream の close / WT_CLOSE_SESSION) を検知した (§6 / §6.12)
    ClosedByPeer,
    /// 自側が WT_CLOSE_SESSION を送ってセッションを終了した (§6 / §6.12)
    ClosedLocally,
    /// 接続層のエラー、または datagram 受信 API のエラーで、受信を継続できない終了
    ///
    /// HTTP/3 の `Error::ConnectionError(code)` は RFC 9114 §8 の接続エラーである。
    /// HTTP/2 は RFC 9113 §5.4 の接続エラーと、CONNECT stream の RST_STREAM
    /// (draft-ietf-webtrans-http2-15 §3.4 がストリームエラーを求める場合を含む) である。
    /// 依存 crate は接続エラーを記録した接続を回復不能として扱い、以後のデータ処理・
    /// イベント生成・送信 API の呼び出しを拒否する。datagram 受信 API のエラーも以後
    /// datagram を読めないため、購読を静かに止めないよう同じ終了として扱う。どちらの場合も
    /// `WT_CLOSE_SESSION` は送らない (接続自体を閉じるため終了イベントは発火しない)。
    ConnectionFailed,
}

impl WtSessionState {
    /// 状態の進み具合。状態はこの値が増える方向にしか遷移しない
    ///
    /// セッションが終了したかどうかの判断は [`session_policy`] の `abort_streams` に
    /// 一本化しており、この型は状態の表現だけを持つ (述語は持たない)。
    /// `ConnectionFailed` はセッション終了 (`ClosedByPeer` / `ClosedLocally`) と同じ段階に
    /// 置く。どちらも新しい作業を始められない終了状態だからである。
    fn stage(self) -> u8 {
        match self {
            Self::Active => 0,
            Self::Draining => 1,
            Self::ClosedByPeer | Self::ClosedLocally | Self::ConnectionFailed => 2,
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
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SessionPolicy {
    /// 新しいストリームの open を拒否する
    pub(crate) reject_new_streams: bool,
    /// 新しい datagram の送信を拒否する
    pub(crate) reject_datagrams: bool,
    /// 自セッションの既存ストリームを中断する
    pub(crate) abort_streams: bool,
}

/// セッションの状態から動作を決める純関数
///
/// 終了時の MUST / MAY の根拠は draft-ietf-webtrans-http3-16 §6 / §4.7 と
/// draft-ietf-webtrans-http2-15 §6.12 / §6.13 が同じ内容を定める。
///
/// セッション終了を検知したら関連する全 uni / bidi ストリームを `WT_SESSION_GONE` で
/// 中断する MUST、新しい datagram の送信・新しいストリームの open を禁じる MUST NOT を
/// 定める。drain は終了の開始の合図であり、通知後も "an endpoint MAY continue using the
/// session" である。drain を終了と同一視すると既存ストリームを不必要に中断してこの MAY を
/// 潰すため、drain ではストリームを中断せず、新規ストリームの open と datagram の送信だけを
/// 拒否する。
///
/// 接続エラーを検知した場合は drain 中でも既存ストリームを中断する。接続そのものが閉じる
/// ため、既存ストリームを継続することはできない (drain の MAY は接続が生きていることを
/// 前提とする)。
///
/// セッションが終了したかどうかの判断は `abort_streams` に一本化する。`WtSessionState` は
/// 状態の表現だけを持ち、終了を表す述語を別に持たない (二重表現を作らない)。
pub(crate) fn session_policy(state: WtSessionState) -> SessionPolicy {
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
        WtSessionState::ClosedByPeer
        | WtSessionState::ClosedLocally
        | WtSessionState::ConnectionFailed => SessionPolicy {
            reject_new_streams: true,
            reject_datagrams: true,
            abort_streams: true,
        },
    }
}

/// セッションの状態を進める
///
/// 状態は `Active` → `Draining` → 終了 の順にしか進まない。終了後に遅れて届いた drain の
/// 通知で状態を戻すと中断済みストリームの扱いが変わるため無視する。同じ状態の再通知でも
/// watch の値を変えない (`send_if_modified` は変更があるときだけ通知する)。
pub(crate) fn update_session_state(
    session_state: &watch::Sender<WtSessionState>,
    next: WtSessionState,
) {
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
/// 中断するかどうかはこの関数の待機が起点になる。drain では中断しないため返らず、
/// `abort_streams` が真になる状態へ遷移するまで待ち続ける。
///
/// `watch::Receiver::wait_for` は待機に入るときに現在値で述語を評価するため、呼び出し時点で
/// 既に終了していれば待たずに返る。戻り値は待機が終わったことだけを表し、観測した状態は
/// 呼び出し側が `borrow()` で読む (`ClosedByPeer` と `ClosedLocally` を区別できる)。
///
/// 状態を配る sender がすべて drop された場合も `wait_for` が `Err` を返すため、状態を
/// 観測できないものとして待ち続けずに終了として扱う (防御)。この経路は production では
/// 通常到達しない。
pub(crate) async fn wait_until_terminated(session_state: &mut watch::Receiver<WtSessionState>) {
    let _ = session_state
        .wait_for(|state| session_policy(*state).abort_streams)
        .await;
}

/// MOQT の close code をどの経路で接続へ伝えるか
///
/// MOQT §13 (Grease) の greasing 値は `0x7f * N + 0x9D` で 32 ビットを超える
/// (`0x9D, 0x11C, ..., 0x3fffffffffffffde` を取り得る) ため、`WT_CLOSE_SESSION` capsule では
/// 運べない値がある。切り捨てると別のコードに化けるため、運べない値は接続レベルの close に
/// 切り替える。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoqtCloseCode {
    /// `WT_CLOSE_SESSION` capsule の Application Error Code として送る
    ///
    /// capsule の Application Error Code は 32 ビットである
    /// (draft-ietf-webtrans-http3-16 §6 / draft-ietf-webtrans-http2-15 §6.12)。
    Capsule(u32),
    /// QUIC の `CONNECTION_CLOSE` で接続を閉じ、application error code に元のコードを載せる
    ///
    /// `WT_CLOSE_SESSION` の詳細メッセージは送れないが、接続は閉じる。
    /// draft-ietf-webtrans-http3-16 §6 は CONNECT stream の close もセッション終了の条件とする。
    ConnectionClose(u64),
    /// varint の上限 (2^62-1) を超えるため `CONNECTION_CLOSE` にコードを載せられない
    ///
    /// QUIC の application error code は varint であり、上限を超える値は送れない。
    /// 既存の QUIC 経路 (`StreamHandle::close` の QUIC 分岐) と同じく `Error::UNKNOWN` に
    /// 落として接続を閉じる (コードは伝わらない)。
    ConnectionCloseUnknown,
}

/// MOQT の close code から接続を閉じる経路を決める
///
/// `WT_CLOSE_SESSION` capsule の Application Error Code は 32 ビットのため収まる値は capsule で
/// 送り、収まらない値は接続レベルの close に切り替える。`Session` の公開 API は任意の `u64` を
/// 受け付けるため、32 ビットを超える値が到達し得る。
pub fn moqt_close_code(code: u64) -> MoqtCloseCode {
    match u32::try_from(code) {
        Ok(code) => MoqtCloseCode::Capsule(code),
        Err(_) => {
            // RFC 9000 §19.19: application error code は varint (最大 2^62-1)
            const MAX_VARINT: u64 = (1 << 62) - 1;
            if code <= MAX_VARINT {
                MoqtCloseCode::ConnectionClose(code)
            } else {
                MoqtCloseCode::ConnectionCloseUnknown
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 状態は進む方向にだけ遷移し、終了後の drain 通知では戻らない
    #[test]
    fn session_state_only_advances() {
        let (tx, _rx) = watch::channel(WtSessionState::Active);
        update_session_state(&tx, WtSessionState::Draining);
        assert_eq!(*tx.borrow(), WtSessionState::Draining);
        update_session_state(&tx, WtSessionState::Active);
        assert_eq!(*tx.borrow(), WtSessionState::Draining);
        update_session_state(&tx, WtSessionState::ClosedByPeer);
        assert_eq!(*tx.borrow(), WtSessionState::ClosedByPeer);
        update_session_state(&tx, WtSessionState::Draining);
        assert_eq!(*tx.borrow(), WtSessionState::ClosedByPeer);
    }

    /// drain ではストリームを中断せず、終了と接続エラーでは中断する
    #[test]
    fn session_policy_aborts_only_after_termination() {
        assert!(!session_policy(WtSessionState::Active).abort_streams);
        assert!(!session_policy(WtSessionState::Draining).abort_streams);
        assert!(session_policy(WtSessionState::ClosedByPeer).abort_streams);
        assert!(session_policy(WtSessionState::ClosedLocally).abort_streams);
        assert!(session_policy(WtSessionState::ConnectionFailed).abort_streams);
    }

    /// drain と終了のどちらでも新規ストリームと datagram を拒否する
    #[test]
    fn session_policy_rejects_new_work_after_drain() {
        let active = session_policy(WtSessionState::Active);
        assert!(!active.reject_new_streams);
        assert!(!active.reject_datagrams);

        for state in [
            WtSessionState::Draining,
            WtSessionState::ClosedByPeer,
            WtSessionState::ClosedLocally,
            WtSessionState::ConnectionFailed,
        ] {
            let policy = session_policy(state);
            assert!(policy.reject_new_streams, "{state:?}");
            assert!(policy.reject_datagrams, "{state:?}");
        }
    }

    /// 32 ビットに収まる MOQT の close code は capsule で送る
    ///
    /// 期待値は実装 (`u32::try_from`) の写しにせず、リテラルで固定する。
    #[test]
    fn moqt_close_code_accepts_32_bit_values() {
        for (code, expected) in [
            (0_u64, 0_u32),
            (0x1, 0x1),
            (0x12, 0x12),
            (0xffff_ffff, u32::MAX),
        ] {
            assert_eq!(
                moqt_close_code(code),
                MoqtCloseCode::Capsule(expected),
                "{code:#x} が切り捨てられずに capsule で送られること"
            );
        }
    }

    /// 32 ビットを超える greasing 値は接続レベルの close に切り替える
    ///
    /// MOQT §13 (Grease) の greasing 値 `0x7f * N + 0x9D` は 32 ビットを超え得る。
    /// `WT_CLOSE_SESSION` の Application Error Code は 32 ビットのため capsule では運べない。
    #[test]
    fn moqt_close_code_falls_back_to_connection_close_over_32_bits() {
        for code in [
            u64::from(u32::MAX) + 1,
            0x1_0000_009d,
            // greasing 値の上限 (0x7f * N + 0x9D の最大)
            0x3fff_ffff_ffff_ffde,
        ] {
            assert_eq!(
                moqt_close_code(code),
                MoqtCloseCode::ConnectionClose(code),
                "{code:#x} が元のコードのまま接続レベルの close になること"
            );
        }
    }

    /// varint の上限を超える値はコードを載せずに接続を閉じる
    ///
    /// RFC 9000 §19.19 の application error code は varint (最大 2^62-1) のため、
    /// 超える値は `CONNECTION_CLOSE` にも載せられない。
    #[test]
    fn moqt_close_code_uses_unknown_over_varint_limit() {
        for code in [(1_u64 << 62), u64::MAX] {
            assert_eq!(
                moqt_close_code(code),
                MoqtCloseCode::ConnectionCloseUnknown,
                "{code:#x} はコードを載せずに接続を閉じること"
            );
        }
    }
}
