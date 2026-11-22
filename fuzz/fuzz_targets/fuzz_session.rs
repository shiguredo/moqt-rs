#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use shiguredo_moqt::message::common::{Location, TrackNamespace};
use shiguredo_moqt::message::{ControlMessage, Fetch, RequestOk, Setup};
use shiguredo_moqt::message_parameter::MessageParameters;
use shiguredo_moqt::parameter::{
    SETUP_OPTION_AUTHORITY, SETUP_OPTION_PATH, SetupOption, SetupOptionValue, SetupOptions,
};
use shiguredo_moqt::session::core::Session;
use shiguredo_moqt::session::types::{
    DataStreamId, FetchHeaderAcceptance, PUBLISHER_PRIORITY_DEFAULT, RequestStreamEnd, Transport,
};
use shiguredo_moqt::stream::FETCH_HEADER_TYPE;
use shiguredo_moqt::stream::datagram::ObjectDatagram;
use shiguredo_moqt::stream::decoder::{
    DecodedFetchEntry, DecodedFetchObject, DecodedSubgroupObject,
};
use shiguredo_moqt::stream::fetch::FetchHeader;
use shiguredo_moqt::stream::subgroup::SubgroupHeader;
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
/// 応答系のうち `send_subscribe_ok` / `send_request_ok` / `send_request_error` 等は
/// request_id を相関させない方針のため、応答対象 subscription の request_id を入力から
/// 供給せず成功経路を対象外とする。これら応答系の送信状態機械は pbt 側で検証する。
///
/// FETCH だけは例外として、成功経路を決定的に踏むために request_id と stream_id を
/// 相関させる。`SendFetch` が発行した request_id をローカル変数に保持し、
/// `RecvFetchResponse` が同じ request_id の REQUEST_OK (FETCH_OK) と FETCH_HEADER を
/// 通知する。受理した FETCH_HEADER の stream_id も保持し、`RecvFetchEntry` がその
/// stream_id に entry を通知する。FETCH stream の終端は既存の `RecvDataStreamClosed` で
/// 通知する。これらは入力バイト列の decode に依存せず、入力の object_id / status /
/// properties から entry を組み立てる。
///
/// server role では `ServeFetch` が入力の track 名 / request_id から FETCH request を
/// 組み立てて `recv_request` へ渡し、同じ request_id で FETCH_OK / FETCH_HEADER /
/// Object / stream 終端を順に送る成功応答経路を踏む。
///
/// 受信 subgroup 経路は従来どおり受信 Op の request_id / stream と送信発行 id を
/// 相関させない。REQUEST_UPDATE も同様に相関させず、`require_established` ガードと
/// `locate_request` 失敗経路の検証を対象にする (成功経路は pbt 側で検証する)。
/// 共有 Track Alias の帰属判定、stream 会計、delivery timeout override のライフサイクル
/// (pbt の `prop_session/subgroup.rs` が検証する不変条件) を、任意の操作列で panic と
/// 状態破壊が起きないかの観点で fuzz する。
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
    /// 入力バイト列を SUBGROUP_HEADER として decode し、受信 subgroup stream の header として通知する
    RecvSubgroupHeader {
        stream_id: u64,
        bytes: Vec<u8>,
    },
    /// decode 済みの Subgroup Object を受信 subgroup stream に通知する
    ///
    /// payload 長 1 / status 未指定で正常 Object を作り、status があれば payload 長 0 にする
    /// (wire では status は payload が無い場合のみ存在する)。
    RecvSubgroupObject {
        stream_id: u64,
        object_id: u64,
        status: Option<u64>,
        properties_bytes: Option<Vec<u8>>,
    },
    /// 入力バイト列を FETCH_HEADER として decode し、データ stream の header として通知する
    ///
    /// fill fetch stream の Request ID は subscription と一致することもあるため、
    /// 会計 (Stream Count の open 数) の経路も踏む。
    RecvFetchHeader {
        stream_id: u64,
        bytes: Vec<u8>,
    },
    /// 直前に送信した FETCH の成功応答 (REQUEST_OK / FETCH_HEADER) を受信する
    ///
    /// `SendFetch` が発行した request_id を相関させるため、REQUEST_OK が FETCH_OK として
    /// 受理され、FETCH_HEADER も同じ FETCH に帰属する。入力バイト列の decode を待たずに
    /// 成功経路を踏む。FETCH_HEADER が受理されたら stream_id を保持し、後続の
    /// `RecvFetchEntry` と `RecvDataStreamClosed` が同じ stream を使えるようにする。
    RecvFetchResponse {
        stream_id: u64,
    },
    /// 保持した FETCH 応答 stream に Object entry を受信させる
    ///
    /// `RecvSubgroupObject` と同じく入力から object_id / status / properties を作る。
    /// FETCH Object の wire format は Status フィールドを持たない
    /// (draft-ietf-moq-transport-23 §11.4.1 (Fetch Header) Figure 29) ため、status は
    /// payload を持たない Object (payload 長 0) を作る判定にだけ使う。
    /// Group ID と Subgroup ID は固定値にし、FETCH 応答の暗黙 gap 判定が入力の任意値で
    /// 桁あふれしないようにする。
    RecvFetchEntry {
        object_id: u64,
        status: Option<u64>,
        properties_bytes: Option<Vec<u8>>,
    },
    /// server role で FETCH 要求を受信し、成功応答一式を送信する
    ///
    /// 固定 namespace と入力の track 名 / request_id から FETCH request を組み立てて
    /// `recv_request` へ渡し、同じ request_id で FETCH_OK / FETCH_HEADER / Object /
    /// stream 終端を順に送る。FETCH_OK には要求 (LOCATION_FILTER 省略) の応答範囲の終端に
    /// なる LARGEST_OBJECT を載せ、送る Object は固定の Location にする。
    ServeFetch {
        track_name: Vec<u8>,
        request_id: u64,
        stream_id: u64,
    },
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
            error_code: Some(error_code),
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

    // 直前に送信した FETCH の request_id。`SendFetch` が Ok を返したときだけ更新する。
    // FETCH の成功応答だけは、任意バイト列の decode を待たずに成功経路を踏むため
    // この id を後続の受信 Op へ相関させる。
    let mut last_fetch_request_id: Option<u64> = None;
    // 直前に受理した FETCH_HEADER の stream_id。`recv_fetch_header` が Accepted を
    // 返したときだけ更新し、`RecvFetchEntry` が entry を送る stream として使う。
    let mut last_fetch_stream_id: Option<DataStreamId> = None;

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
            Op::RecvSubgroupHeader { stream_id, bytes } => {
                if let Ok((header, _)) = SubgroupHeader::decode(&bytes) {
                    let _ = session.recv_subgroup_header(DataStreamId(stream_id), &header);
                }
            }
            Op::RecvSubgroupObject {
                stream_id,
                object_id,
                status,
                properties_bytes,
            } => {
                // status は payload が無い場合のみ存在するため、status の有無で payload 長を決める
                let object = DecodedSubgroupObject {
                    object_id,
                    payload_length: if status.is_some() { 0 } else { 1 },
                    status,
                    properties_bytes,
                };
                let _ = session.recv_subgroup_object(DataStreamId(stream_id), &object);
            }
            Op::RecvFetchHeader { stream_id, bytes } => {
                if let Ok((header, _)) = FetchHeader::decode(&bytes) {
                    let _ = session.recv_fetch_header(DataStreamId(stream_id), &header);
                }
            }
            Op::RecvFetchResponse { stream_id } => {
                // 直前に送信した FETCH が無い場合は相関先が無いため何もしない。
                // 入力の request_id をでっち上げると別の request の応答として解釈される。
                if let Some(request_id) = last_fetch_request_id {
                    // `SendFetch` は LOCATION_FILTER を載せない open-ended な FETCH を送るため、
                    // FETCH_OK は応答範囲の終端を LARGEST_OBJECT で確定する必要がある
                    // (draft-ietf-moq-transport-23 §3.2 (Fetch) / §9.19.17 (LARGEST OBJECT
                    // Parameter))。入力由来の Object ID をすべて応答範囲に収めるため、上限なしを
                    // 表す u64::MAX を Largest Object にする (本ターゲットは wire エンコードを
                    // 行わないため varint の値域は問題にならない)。
                    let mut ok_parameters = MessageParameters::new();
                    ok_parameters.set_largest_object(0, u64::MAX);
                    let ok = ControlMessage::RequestOk(RequestOk {
                        parameters: ok_parameters,
                        track_properties: TrackProperties::new(),
                    });
                    let _ = session.recv_stream_message(request_id, ok);

                    // 実 I/O 層は FETCH_HEADER の前に uni stream の type を通知する。同じ順序で
                    // 通知しないと `recv_fetch_header` が未知 stream として拒否し、成功経路に
                    // 到達しない。
                    let stream_id = DataStreamId(stream_id);
                    let _ = session.recv_data_stream_type(stream_id, FETCH_HEADER_TYPE);
                    // FETCH_HEADER の Request ID を直前に送信した FETCH と一致させる。
                    let header = FetchHeader { request_id };
                    if matches!(
                        session.recv_fetch_header(stream_id, &header),
                        Ok(FetchHeaderAcceptance::Accepted)
                    ) {
                        // 受理した stream だけを保持し、後続の entry / 終端通知に使う。
                        last_fetch_stream_id = Some(stream_id);
                    }
                }
            }
            Op::RecvFetchEntry {
                object_id,
                status,
                properties_bytes,
            } => {
                // 受理済みの FETCH_HEADER が無い場合は流す先が無いため何もしない。
                if let Some(stream_id) = last_fetch_stream_id {
                    // status は payload が無い場合のみ存在するため、status の有無で payload 長を
                    // 決める (subgroup の受信 Op と同じ扱い)。FETCH Object の wire format は
                    // Status フィールドを持たないため、status の値自体は Object に持たせない。
                    let object = DecodedFetchObject {
                        // Group ID / Subgroup ID は固定し、入力からは object_id だけを採る。
                        group_id: 0,
                        subgroup_id: 0,
                        object_id,
                        publisher_priority: PUBLISHER_PRIORITY_DEFAULT,
                        is_datagram_origin: false,
                        payload_length: if status.is_some() { 0 } else { 1 },
                        properties_bytes,
                    };
                    let entry = DecodedFetchEntry::Object(object);
                    let _ = session.recv_fetch_entry(stream_id, &entry);
                }
            }
            Op::ServeFetch {
                track_name,
                request_id,
                stream_id,
            } => {
                // server role の Session は peer (client) からの FETCH を publisher として処理する。
                // peer の Request ID は送信者 (client) の parity である偶数でなければ
                // INVALID_REQUEST_ID でセッションが閉じる (draft-ietf-moq-transport-23
                // §6.4.2.1 (Request ID))。成功応答経路を決定的に踏めるよう、入力 request_id の
                // 最下位ビットを落として偶数に正規化する。
                // 空の Track Name は PROTOCOL_VIOLATION でセッションが閉じるため送らない。
                if is_server && !track_name.is_empty() {
                    let request_id = request_id & !1;
                    if let Some(namespace) = fuzz_namespace() {
                        let _ = session.recv_request(ControlMessage::Fetch(Fetch {
                            request_id,
                            track_namespace: namespace,
                            track_name,
                            parameters: MessageParameters::new(),
                        }));
                        // 要求は LOCATION_FILTER を省略しているため End は Largest Object になり、
                        // FETCH_OK には LARGEST_OBJECT が必須 (§3.2 (Fetch) / §9.19.17
                        // (LARGEST OBJECT Parameter))。送る Object を `{0, 0}` に固定するので
                        // Largest Object も `{0, 0}` にする。
                        let mut ok_parameters = MessageParameters::new();
                        ok_parameters.set_largest_object(0, 0);
                        let _ = session.send_fetch_ok(
                            request_id,
                            ok_parameters,
                            TrackProperties::new(),
                        );
                        let stream_id = DataStreamId(stream_id);
                        let _ = session.send_fetch_header(stream_id, request_id);
                        let _ = session.send_fetch_object(
                            stream_id,
                            &Location {
                                group_id: 0,
                                object_id: 0,
                            },
                        );
                        let _ = session.send_fetch_data_stream_closed(stream_id);
                    }
                }
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
                if let Some(namespace) = fuzz_namespace()
                    && let Ok(request_id) =
                        session.send_fetch(namespace, track_name, MessageParameters::new())
                {
                    // 送信できた FETCH の request_id を保持し、後続の `RecvFetchResponse` が
                    // REQUEST_OK と FETCH_HEADER を相関させられるようにする。
                    last_fetch_request_id = Some(request_id);
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
