#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use shiguredo_moqt::message::ControlMessage;
use shiguredo_moqt::parameter::SetupOptions;
use shiguredo_moqt::session::core::Session;
use shiguredo_moqt::session::types::{DataStreamId, RequestStreamEnd, Transport};
use shiguredo_moqt::stream::datagram::ObjectDatagram;

/// Session へ与える 1 操作
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

fuzz_target!(|ops: Vec<Op>| {
    let Ok(mut session) = Session::new_client(Transport::Quic, SetupOptions::new()) else {
        return;
    };
    // 初期 SETUP イベントを排出する
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
            Op::Tick(now) => session.tick(now),
            Op::Close => session.close(0, "fuzz close"),
        }
        while session.poll_event().is_some() {}
    }
});
