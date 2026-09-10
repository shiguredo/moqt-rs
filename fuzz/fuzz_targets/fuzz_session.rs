#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use shiguredo_moqt::message::common::TrackNamespace;
use shiguredo_moqt::message::{ControlMessage, Setup};
use shiguredo_moqt::message_parameter::MessageParameters;
use shiguredo_moqt::parameter::{
    SETUP_OPTION_AUTHORITY, SETUP_OPTION_PATH, SetupOption, SetupOptionValue, SetupOptions,
};
use shiguredo_moqt::session::core::Session;
use shiguredo_moqt::session::types::{DataStreamId, RequestStreamEnd, Transport};
use shiguredo_moqt::stream::datagram::ObjectDatagram;
use shiguredo_moqt::track_properties::TrackProperties;

/// fuzz 入力。role 選択と操作列を持つ
#[derive(Arbitrary, Debug)]
struct FuzzInput {
    /// true なら server role、false なら client role の Session を作る
    is_server: bool,
    /// SETUP に与える AUTHORITY (None なら省略)
    setup_authority: Option<Vec<u8>>,
    /// SETUP に与える PATH (None なら省略)
    setup_path: Option<Vec<u8>>,
    /// 適用する操作列
    ops: Vec<Op>,
}

/// Session へ与える 1 操作
///
/// 送信 API は subscribe / publish / fetch / request_update を対象にする。引数は固定の
/// 有効な namespace と入力由来の track 名で構成する。生バイトを `ControlMessage::decode`
/// する方式は、任意バイトから妥当なメッセージに到達せず送信 API が呼ばれないため採用しない。
/// 応答系 (`send_subscribe_ok` / `send_request_ok` / `send_request_error` 等) は
/// request_id を相関させない方針のため、応答対象 subscription の request_id を入力から
/// 供給せず成功経路を対象外とする。応答系の送信状態機械は pbt 側で検証する。
///
/// 送信で発行された request_id は後続の受信 Op に相関させない。受信 Op の request_id は
/// 入力から与え、送信側はイベント排出までを検証対象にする。REQUEST_UPDATE は
/// `require_established` ガードと `locate_request` 失敗経路の検証を対象にし、成功経路は
/// 対象外とする。
#[derive(Arbitrary, Debug)]
enum Op {
    RecvControl(Vec<u8>),
    RecvRequest(Vec<u8>),
    RecvStreamMessage {
        request_id: u64,
        bytes: Vec<u8>,
    },
    RecvDataStreamType {
        stream_id: u64,
        stream_type: u64,
    },
    RecvDataStreamClosed {
        stream_id: u64,
        reset: bool,
        error_code: u64,
        reliable_size: Option<u64>,
    },
    RecvDatagram(Vec<u8>),
    RecvObjectDatagram(Vec<u8>),
    RecvPaddingDatagram,
    /// 固定 namespace と入力 track 名で SUBSCRIBE を送信する
    SendSubscribe {
        track_name: Vec<u8>,
    },
    /// 固定 namespace と入力 track 名 / alias で PUBLISH を送信する
    SendPublish {
        track_name: Vec<u8>,
        track_alias: u64,
    },
    /// 固定 namespace と入力 track 名で FETCH を送信する
    SendFetch {
        track_name: Vec<u8>,
    },
    /// 入力 request_id に対する REQUEST_UPDATE を送信する (送信発行 id とは相関させない)
    SendRequestUpdate {
        request_id: u64,
    },
    Tick(u64),
    Close,
}

/// 任意バイト列を ControlMessage にデコードする (失敗は無視)
fn decode_control(bytes: &[u8]) -> Option<ControlMessage> {
    ControlMessage::decode(bytes).ok().map(|(msg, _)| msg)
}

/// RequestStreamEnd を生成する
fn stream_end(reset: bool, error_code: u64, reliable_size: Option<u64>) -> RequestStreamEnd {
    if reset {
        RequestStreamEnd::Reset {
            error_code,
            reliable_size,
        }
    } else {
        RequestStreamEnd::Fin
    }
}

/// 送信 API 用の固定 namespace を作る
fn fuzz_namespace() -> Option<TrackNamespace> {
    TrackNamespace::new(vec![b"fuzz".to_vec()]).ok()
}

/// 入力由来の AUTHORITY / PATH を持つ peer SETUP を作る
///
/// AUTHORITY / PATH を入力から与えることで `validate_setup_role_transport` の
/// peer role / transport 検証と `validate_setup_uri_format` の構文検証経路を fuzz する。
fn peer_setup(authority: Option<&[u8]>, path: Option<&[u8]>) -> Setup {
    let mut options = SetupOptions::new();
    if let Some(value) = authority {
        options.push(SetupOption {
            option_type: SETUP_OPTION_AUTHORITY,
            value: SetupOptionValue::Bytes(value.to_vec()),
        });
    }
    if let Some(value) = path {
        options.push(SetupOption {
            option_type: SETUP_OPTION_PATH,
            value: SetupOptionValue::Bytes(value.to_vec()),
        });
    }
    Setup { options }
}

fuzz_target!(|input: FuzzInput| {
    let FuzzInput {
        is_server,
        setup_authority,
        setup_path,
        ops,
    } = input;
    let new_session = || {
        if is_server {
            Session::new_server(Transport::Quic, SetupOptions::new())
        } else {
            Session::new_client(Transport::Quic, SetupOptions::new())
        }
    };

    // 入力由来の SETUP を別 Session に注入し、SETUP の検証経路を fuzz する。検証に
    // 失敗すると Session は閉じるため、操作列用の Session とは分ける。pbt の
    // `establish_pair` とは別 crate のため共有せず、単一 Session 向けに簡略化した。
    if let Ok(mut probe) = new_session() {
        while probe.poll_event().is_some() {}
        let _ = probe.recv_control(ControlMessage::Setup(peer_setup(
            setup_authority.as_deref(),
            setup_path.as_deref(),
        )));
        while probe.poll_event().is_some() {}
    }

    // 操作列用の Session は固定の有効な SETUP で確実に Established にし、送信 API を
    // `require_established` で落とさず実行できるようにする。
    let Ok(mut session) = new_session() else {
        return;
    };
    while session.poll_event().is_some() {}
    let _ = session.recv_control(ControlMessage::Setup(Setup {
        options: SetupOptions::new(),
    }));
    while session.poll_event().is_some() {}

    for op in ops {
        match op {
            Op::RecvControl(bytes) => {
                if let Some(msg) = decode_control(&bytes) {
                    let _ = session.recv_control(msg);
                }
            }
            Op::RecvRequest(bytes) => {
                if let Some(msg) = decode_control(&bytes) {
                    let _ = session.recv_request(msg);
                }
            }
            Op::RecvStreamMessage { request_id, bytes } => {
                if let Some(msg) = decode_control(&bytes) {
                    let _ = session.recv_stream_message(request_id, msg);
                }
            }
            Op::RecvDataStreamType {
                stream_id,
                stream_type,
            } => {
                let _ = session.recv_data_stream_type(DataStreamId(stream_id), stream_type);
            }
            Op::RecvDataStreamClosed {
                stream_id,
                reset,
                error_code,
                reliable_size,
            } => {
                let _ = session.recv_data_stream_closed(
                    DataStreamId(stream_id),
                    stream_end(reset, error_code, reliable_size),
                );
            }
            Op::RecvDatagram(bytes) => {
                let _ = session.recv_datagram(&bytes);
            }
            Op::RecvObjectDatagram(bytes) => {
                if let Ok((datagram, _)) = ObjectDatagram::decode(&bytes) {
                    let _ = session.recv_object_datagram(&datagram);
                }
            }
            Op::RecvPaddingDatagram => {
                let _ = session.recv_padding_datagram();
            }
            Op::SendSubscribe { track_name } => {
                if let Some(namespace) = fuzz_namespace() {
                    let _ = session.send_subscribe(namespace, track_name, MessageParameters::new());
                }
            }
            Op::SendPublish {
                track_name,
                track_alias,
            } => {
                if let Some(namespace) = fuzz_namespace() {
                    let _ = session.send_publish(
                        namespace,
                        track_name,
                        track_alias,
                        MessageParameters::new(),
                        TrackProperties::new(),
                    );
                }
            }
            Op::SendFetch { track_name } => {
                if let Some(namespace) = fuzz_namespace() {
                    let _ = session.send_fetch(namespace, track_name, MessageParameters::new());
                }
            }
            Op::SendRequestUpdate { request_id } => {
                let _ = session.send_request_update(request_id, MessageParameters::new());
            }
            Op::Tick(now) => session.tick(now),
            Op::Close => session.close(0, "fuzz close"),
        }
        while session.poll_event().is_some() {}
    }
});
