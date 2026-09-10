use super::auth_token_cache::AuthTokenCache;
use super::core::Session;
use super::namespace::prefix_overlaps;
use super::request_id::{MAX_OUT_OF_ORDER_REQUEST_IDS, RequestIdGenerator, RequestIdTracker};
use super::subscription::validation::extract_forward_state;
use super::types::*;
use alloc::vec;

use crate::error::{SESSION_DUPLICATE_AUTH_TOKEN_ALIAS, SESSION_INVALID_REQUEST_ID};
use crate::message::common::TrackNamespace;
use crate::message_parameter::{
    MessageParameter, MessageParameterValue, MessageParameters, PARAM_LOCATION_FILTER,
};
use crate::{message::common::Location, message_parameter::LocationFilter};

/// FETCH の range 指定 (AbsoluteRangeWithEnd) を持つ MessageParameters を作る
///
/// draft-ietf-moq-transport-21 §9.11 (FETCH): range は LOCATION_FILTER
/// パラメータで指定する。`end.group_id < start.group_id` の場合は delta が
/// 求まらないため panic する (テストフィクスチャの前提)。
fn fetch_range_params(start: Location, end: Location) -> MessageParameters {
    let end_group_delta = end.group_id - start.group_id;
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteRangeWithEnd {
                start,
                end_group_delta,
                end_object: end.object_id,
            }
            .encode_to_bytes(),
        ),
    });
    params
}

#[test]
fn auth_token_cache_register_and_resolve() {
    let mut cache = AuthTokenCache::new(1024);
    assert!(
        cache
            .try_register(1, 10, vec![1, 2, 3])
            .expect("テストフィクスチャの前提条件を満たす")
    );
    assert_eq!(cache.resolve(1), Some((10, &[1, 2, 3][..])));
    assert_eq!(cache.total_size(), 16 + 3);
}

#[test]
fn auth_token_cache_duplicate_alias() {
    let mut cache = AuthTokenCache::new(1024);
    cache
        .try_register(1, 10, vec![1])
        .expect("テストフィクスチャの前提条件を満たす");
    let err = cache
        .try_register(1, 20, vec![2])
        .expect_err("重複した alias の登録は失敗するはず");
    assert_eq!(err.code, SESSION_DUPLICATE_AUTH_TOKEN_ALIAS);
}

#[test]
fn auth_token_cache_overflow_returns_false() {
    let mut cache = AuthTokenCache::new(16);
    assert!(
        cache
            .try_register(1, 10, vec![])
            .expect("テストフィクスチャの前提条件を満たす")
    );
    assert!(
        !cache
            .try_register(2, 20, vec![0])
            .expect("テストフィクスチャの前提条件を満たす")
    );
}

#[test]
fn auth_token_cache_delete() {
    let mut cache = AuthTokenCache::new(1024);
    cache
        .try_register(1, 10, vec![1, 2, 3])
        .expect("テストフィクスチャの前提条件を満たす");
    cache.delete(1);
    assert!(cache.resolve(1).is_none());
    assert_eq!(cache.total_size(), 0);
}

#[test]
fn request_id_generator_client_yields_even() {
    let mut generator = RequestIdGenerator::new(Role::Client);
    assert_eq!(generator.peek(), 0);
    assert_eq!(generator.next_id(), 0);
    assert_eq!(generator.next_id(), 2);
    assert_eq!(generator.next_id(), 4);
    assert_eq!(generator.peek(), 6);
}

#[test]
fn request_id_generator_server_yields_odd() {
    let mut generator = RequestIdGenerator::new(Role::Server);
    assert_eq!(generator.next_id(), 1);
    assert_eq!(generator.next_id(), 3);
}

#[test]
fn peer_request_tracker_rejects_wrong_parity() {
    let mut tracker = RequestIdTracker::new(Role::Server);
    let err = tracker
        .accept(0)
        .expect_err("parity が一致しない request_id は拒否されるはず");
    assert_eq!(err.code, SESSION_INVALID_REQUEST_ID);
    assert!(tracker.accept(1).is_ok());
}

#[test]
fn peer_request_tracker_rejects_duplicate() {
    let mut tracker = RequestIdTracker::new(Role::Client);
    tracker
        .accept(4)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = tracker
        .accept(4)
        .expect_err("重複した request_id は拒否されるはず");
    assert_eq!(err.code, SESSION_INVALID_REQUEST_ID);
}

#[test]
fn forward_parameter_default_is_one() {
    let params = MessageParameters::new();
    assert_eq!(extract_forward_state(&params), 1);
}

#[test]
fn prefix_overlaps_detection() {
    let a = TrackNamespace::new(vec![b"a".to_vec()]).expect("テストフィクスチャの前提条件を満たす");
    let ab = TrackNamespace::new(vec![b"a".to_vec(), b"b".to_vec()])
        .expect("テストフィクスチャの前提条件を満たす");
    let c = TrackNamespace::new(vec![b"c".to_vec()]).expect("テストフィクスチャの前提条件を満たす");
    let empty = TrackNamespace::new(vec![]).expect("テストフィクスチャの前提条件を満たす");
    assert!(prefix_overlaps(&a, &ab));
    assert!(prefix_overlaps(&ab, &a));
    assert!(!prefix_overlaps(&a, &c));
    assert!(prefix_overlaps(&empty, &a));
}

/// forget_subscription が datagram の object header 提供完了追跡エントリを掃除する
///
/// draft-ietf-moq-transport-21 §5.2 (Delivery Timeouts and Data Reliability) は object header 提供完了
/// 時刻の保持 (MUST) と timeout 超過での drop (MUST) を規定する。drop 時にエントリを
/// 削除すると同じオブジェクトの再送が新しい object header 提供完了時刻として記録され判定がリセット
/// されるため、本実装では subscription の forget まで保持し、forget 時に掃除する。
#[test]
fn forget_subscription_cleans_datagram_header_complete_entries() {
    use crate::error::REQUEST_INTERNAL_ERROR;
    use crate::message::{ControlMessage, RequestOk, Setup};
    use crate::message_parameter::MessageParameters;
    use crate::parameter::SetupOptions;
    use crate::session::types::SessionState;
    use crate::track_properties::{
        PROP_OBJECT_DELIVERY_TIMEOUT, TrackProperties, TrackProperty, TrackPropertyValue,
    };

    // Session を Established にする (QUIC / client)
    let mut client = Session::new_client(Transport::Quic, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(ControlMessage::Setup(Setup {
            options: SetupOptions::new(),
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(client.state(), SessionState::Established);

    // OBJECT_DELIVERY_TIMEOUT=500 を持つ PUBLISH を送信して Pending(Publisher) を作る
    let mut track_properties = TrackProperties::new();
    track_properties.push(TrackProperty {
        prop_type: PROP_OBJECT_DELIVERY_TIMEOUT,
        value: TrackPropertyValue::VarInt(500),
    });
    let rid = client
        .send_publish(
            TrackNamespace::new(vec![b"live".to_vec()])
                .expect("テストフィクスチャの前提条件を満たす"),
            b"cam".to_vec(),
            1,
            MessageParameters::new(),
            track_properties,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    // REQUEST_OK を注入して Established にする
    client
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(RequestOk {
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }),
        )
        .expect("テストフィクスチャの前提条件を満たす");

    // datagram を送信して object header 提供完了エントリを作る (timeout が設定されているため)
    client
        .send_object_datagram(rid, 0, 0, None, None)
        .expect("テストフィクスチャの前提条件を満たす");
    // 他 request のエントリ (掃除対象外) も混在させる。tick 未確定 (None) のエントリも含める
    client
        .timing
        .datagram_header_complete_ms
        .insert((9999, 0, 0), Some(100));
    client
        .timing
        .datagram_header_complete_ms
        .insert((9999, 0, 1), None);
    assert!(
        client
            .timing
            .datagram_header_complete_ms
            .contains_key(&(rid, 0, 0)),
        "datagram 送信で object header 提供完了エントリが作られること"
    );

    // subscription を Terminated にして forget する
    client
        .send_request_error(
            rid,
            REQUEST_INTERNAL_ERROR,
            0,
            crate::message::ReasonPhrase::new("rejected")
                .expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let sub = client
        .forget_subscription(rid)
        .expect("cleanup_ready な subscription は forget できること");
    assert_eq!(sub.request_id, rid);
    // 該当 request のエントリのみ掃除され、他 request のエントリは残る
    assert!(
        !client
            .timing
            .datagram_header_complete_ms
            .contains_key(&(rid, 0, 0)),
        "forget_subscription で該当 request の datagram object header 提供完了エントリが掃除されること"
    );
    assert_eq!(
        client.timing.datagram_header_complete_ms.len(),
        2,
        "他 request のエントリは残ること"
    );
}

/// forget 系 API が REQUEST_UPDATE のクレジットカウントエントリを掃除する
///
/// draft-ietf-moq-transport-21 §9.1.7 (MAX_REQUEST_UPDATES) のクレジット管理に使う
/// `outgoing_request_updates` / `incoming_request_updates` は request の終端時に
/// 掃除されずセッション寿命まで残る。送信側エントリは peer の SETUP の
/// MAX_REQUEST_UPDATES > 0 宣言時のみ、受信側エントリは local の SETUP の
/// MAX_REQUEST_UPDATES > 0 宣言時のみ生成される。
/// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
#[test]
fn forget_subscription_cleans_request_update_credit_entries() {
    use crate::error::REQUEST_INTERNAL_ERROR;
    use crate::message::{ControlMessage, ReasonPhrase, RequestOk, RequestUpdate, Setup};
    use crate::message_parameter::{MessageParameter, MessageParameterValue, PARAM_FORWARD};
    use crate::parameter::{SetupOption, SetupOptionValue, SetupOptions};
    use crate::session::types::SessionState;
    use crate::track_properties::TrackProperties;

    // Session を Established にする (MAX_REQUEST_UPDATES = 1 を local / peer 両方に設定)
    let mut local_opts = SetupOptions::new();
    local_opts.push(SetupOption {
        option_type: crate::parameter::SETUP_OPTION_MAX_REQUEST_UPDATES,
        value: SetupOptionValue::VarInt(1),
    });
    let mut peer_opts = SetupOptions::new();
    peer_opts.push(SetupOption {
        option_type: crate::parameter::SETUP_OPTION_MAX_REQUEST_UPDATES,
        value: SetupOptionValue::VarInt(1),
    });
    let mut client = Session::new_client(Transport::Quic, local_opts)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(ControlMessage::Setup(Setup { options: peer_opts }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(client.state(), SessionState::Established);

    // PUBLISH を送信して Pending(Publisher) を作り、REQUEST_OK で Established にする
    let rid = client
        .send_publish(
            TrackNamespace::new(vec![b"live".to_vec()])
                .expect("テストフィクスチャの前提条件を満たす"),
            b"cam".to_vec(),
            1,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(RequestOk {
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }),
        )
        .expect("テストフィクスチャの前提条件を満たす");

    // 送信側エントリ: REQUEST_UPDATE を送信する (subscription で許可される FORWARD を載せる)
    let mut update_params = MessageParameters::new();
    update_params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(1),
    });
    client
        .send_request_update(rid, update_params.clone())
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client.outgoing_request_updates.contains_key(&rid),
        "REQUEST_UPDATE 送信で outgoing エントリが生成されること"
    );

    // 受信側エントリ: REQUEST_UPDATE を受信する
    client
        .recv_stream_message(
            rid,
            ControlMessage::RequestUpdate(RequestUpdate {
                request_id: rid,
                parameters: update_params,
            }),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client.incoming_request_updates.contains_key(&rid),
        "REQUEST_UPDATE 受信で incoming エントリが生成されること"
    );

    // 他 request のエントリ (掃除対象外) も混在させる
    client.outgoing_request_updates.insert(9999, 1);
    client.incoming_request_updates.insert(9999, 1);

    // subscription を Terminated にして forget する
    client
        .send_request_error(
            rid,
            REQUEST_INTERNAL_ERROR,
            0,
            ReasonPhrase::new("rejected").expect("テストフィクスチャの前提条件を満たす"),
            None,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let sub = client
        .forget_subscription(rid)
        .expect("cleanup_ready な subscription は forget できること");
    assert_eq!(sub.request_id, rid);
    // 該当 request のエントリのみ掃除され、他 request のエントリは残る
    assert!(
        !client.outgoing_request_updates.contains_key(&rid),
        "forget_subscription で該当 request の outgoing エントリが掃除されること"
    );
    assert!(
        !client.incoming_request_updates.contains_key(&rid),
        "forget_subscription で該当 request の incoming エントリが掃除されること"
    );
    assert_eq!(
        client.outgoing_request_updates.len(),
        1,
        "他 request の outgoing エントリは残ること"
    );
    assert_eq!(
        client.incoming_request_updates.len(),
        1,
        "他 request の incoming エントリは残ること"
    );
}

/// fetch の forget が REQUEST_UPDATE のクレジットカウントエントリを掃除する
///
/// `forget_subscription_cleans_request_update_credit_entries` の fetch 版。
/// FETCH の REQUEST_UPDATE は subscriber 役のみが送信でき (draft §9.11)、受信は
/// publisher 役限定のため incoming は生成しない。受信側エントリの掃除は publisher 役の
/// subscription テストで検証済み。
/// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
#[test]
fn forget_fetch_cleans_request_update_credit_entries() {
    use crate::message::{ControlMessage, FetchOk, Setup, common::Location};
    use crate::message_parameter::MessageParameters;
    use crate::parameter::{SetupOption, SetupOptionValue, SetupOptions};
    use crate::session::types::SessionState;
    use crate::track_properties::TrackProperties;

    // Session を Established にする (MAX_REQUEST_UPDATES = 1 を local / peer 両方に設定)
    let mut local_opts = SetupOptions::new();
    local_opts.push(SetupOption {
        option_type: crate::parameter::SETUP_OPTION_MAX_REQUEST_UPDATES,
        value: SetupOptionValue::VarInt(1),
    });
    let mut peer_opts = SetupOptions::new();
    peer_opts.push(SetupOption {
        option_type: crate::parameter::SETUP_OPTION_MAX_REQUEST_UPDATES,
        value: SetupOptionValue::VarInt(1),
    });
    let mut client = Session::new_client(Transport::Quic, local_opts)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(ControlMessage::Setup(Setup { options: peer_opts }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(client.state(), SessionState::Established);

    // FETCH を送信して FETCH_OK で Established にする
    let rid = client
        .send_fetch(
            TrackNamespace::new(vec![b"live".to_vec()])
                .expect("テストフィクスチャの前提条件を満たす"),
            b"cam".to_vec(),
            fetch_range_params(
                Location {
                    group_id: 0,
                    object_id: 0,
                },
                Location {
                    group_id: 10,
                    object_id: 0,
                },
            ),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_stream_message(
            rid,
            ControlMessage::FetchOk(FetchOk {
                end_of_track: 0,
                end_location: Location {
                    group_id: 10,
                    object_id: 0,
                },
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }),
        )
        .expect("テストフィクスチャの前提条件を満たす");

    // 送信側エントリ: REQUEST_UPDATE を送信する (FETCH の REQUEST_UPDATE は
    // パラメータなしでも成立する)
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client.outgoing_request_updates.contains_key(&rid),
        "REQUEST_UPDATE 送信で outgoing エントリが生成されること"
    );

    // 他 request のエントリ (掃除対象外) も混在させる。受信側エントリ (incoming) は
    // subscriber 役の fetch では REQUEST_UPDATE を受信できないため生成しない
    // (incoming の掃除は publisher 役の subscription テストで検証済み)
    client.outgoing_request_updates.insert(9999, 1);
    client.incoming_request_updates.insert(9999, 1);

    // bidi request stream を閉じて Terminated にして forget する
    client
        .recv_request_stream_closed(rid, crate::session::types::RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    let fetch = client
        .forget_fetch(rid)
        .expect("Terminated な fetch は forget できること");
    assert_eq!(fetch.request_id, rid);
    // 該当 request のエントリのみ掃除され、他 request のエントリは残る
    assert!(
        !client.outgoing_request_updates.contains_key(&rid),
        "forget_fetch で該当 request の outgoing エントリが掃除されること"
    );
    assert_eq!(
        client.outgoing_request_updates.len(),
        1,
        "他 request の outgoing エントリは残ること"
    );
    assert_eq!(
        client.incoming_request_updates.len(),
        1,
        "他 request の incoming エントリは残ること"
    );
}

/// cleanup 不可状態の subscription では forget してもクレジットエントリが残ること
///
/// `forget_subscription` は cleanup_ready (Terminated かつ drain 終了等) でない限り
/// `None` を返し、掃除も行わない。Established の subscription は REQUEST_UPDATE の
/// 送受信が可能なため、エントリ生成後に forget が失敗しても両エントリが残ることを
/// 検証する。namespace 系 3 種は Pending 状態でのみ forget が `None` を返すが、
/// Pending では REQUEST_UPDATE が禁止されエントリを生成できないため、同型の検証は
/// 構築できない。
#[test]
fn forget_subscription_does_not_clean_credit_entries_when_not_cleanup_ready() {
    use crate::message::{ControlMessage, RequestOk, RequestUpdate, Setup};
    use crate::message_parameter::{MessageParameter, MessageParameterValue, PARAM_FORWARD};
    use crate::parameter::{SetupOption, SetupOptionValue, SetupOptions};
    use crate::session::types::SessionState;
    use crate::track_properties::TrackProperties;

    // Session を Established にする (MAX_REQUEST_UPDATES = 1 を local / peer 両方に設定)
    let mut local_opts = SetupOptions::new();
    local_opts.push(SetupOption {
        option_type: crate::parameter::SETUP_OPTION_MAX_REQUEST_UPDATES,
        value: SetupOptionValue::VarInt(1),
    });
    let mut peer_opts = SetupOptions::new();
    peer_opts.push(SetupOption {
        option_type: crate::parameter::SETUP_OPTION_MAX_REQUEST_UPDATES,
        value: SetupOptionValue::VarInt(1),
    });
    let mut client = Session::new_client(Transport::Quic, local_opts)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(ControlMessage::Setup(Setup { options: peer_opts }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(client.state(), SessionState::Established);

    // PUBLISH を送信して REQUEST_OK で Established にする (cleanup 不可の状態)
    let rid = client
        .send_publish(
            TrackNamespace::new(vec![b"live".to_vec()])
                .expect("テストフィクスチャの前提条件を満たす"),
            b"cam".to_vec(),
            1,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(RequestOk {
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }),
        )
        .expect("テストフィクスチャの前提条件を満たす");

    // エントリを生成する (送信側: REQUEST_UPDATE 送信、受信側: REQUEST_UPDATE 受信)
    let mut update_params = MessageParameters::new();
    update_params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(1),
    });
    client
        .send_request_update(rid, update_params.clone())
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_stream_message(
            rid,
            ControlMessage::RequestUpdate(RequestUpdate {
                request_id: rid,
                parameters: update_params,
            }),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client.outgoing_request_updates.contains_key(&rid)
            && client.incoming_request_updates.contains_key(&rid),
        "REQUEST_UPDATE 送受信で両エントリが生成されること"
    );

    // Established のまま forget すると None が返り、エントリは掃除されない
    assert!(
        client.forget_subscription(rid).is_none(),
        "cleanup 不可の subscription は forget できないこと"
    );
    assert!(
        client.outgoing_request_updates.contains_key(&rid)
            && client.incoming_request_updates.contains_key(&rid),
        "forget 失敗時は両エントリが残ること"
    );
}

/// cleanup 不可状態の fetch では forget してもクレジットエントリが残ること
///
/// `forget_fetch` は subscriber 側の fetch が Terminated でない限り `None` を返し、
/// 掃除も行わない。Established の fetch は REQUEST_UPDATE の送信が可能なため、
/// エントリ生成後に forget が失敗しても outgoing エントリが残ることを検証する。
/// incoming エントリは subscriber 役の fetch では生成できないため対象外。
#[test]
fn forget_fetch_does_not_clean_credit_entries_when_not_finished() {
    use crate::message::{ControlMessage, FetchOk, Setup, common::Location};
    use crate::message_parameter::MessageParameters;
    use crate::parameter::{SetupOption, SetupOptionValue, SetupOptions};
    use crate::session::types::SessionState;
    use crate::track_properties::TrackProperties;

    // Session を Established にする (MAX_REQUEST_UPDATES = 1 を local / peer 両方に設定)
    let mut local_opts = SetupOptions::new();
    local_opts.push(SetupOption {
        option_type: crate::parameter::SETUP_OPTION_MAX_REQUEST_UPDATES,
        value: SetupOptionValue::VarInt(1),
    });
    let mut peer_opts = SetupOptions::new();
    peer_opts.push(SetupOption {
        option_type: crate::parameter::SETUP_OPTION_MAX_REQUEST_UPDATES,
        value: SetupOptionValue::VarInt(1),
    });
    let mut client = Session::new_client(Transport::Quic, local_opts)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(ControlMessage::Setup(Setup { options: peer_opts }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(client.state(), SessionState::Established);

    // FETCH を送信して FETCH_OK で Established にする (cleanup 不可の状態)
    let rid = client
        .send_fetch(
            TrackNamespace::new(vec![b"live".to_vec()])
                .expect("テストフィクスチャの前提条件を満たす"),
            b"cam".to_vec(),
            fetch_range_params(
                Location {
                    group_id: 0,
                    object_id: 0,
                },
                Location {
                    group_id: 10,
                    object_id: 0,
                },
            ),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_stream_message(
            rid,
            ControlMessage::FetchOk(FetchOk {
                end_of_track: 0,
                end_location: Location {
                    group_id: 10,
                    object_id: 0,
                },
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }),
        )
        .expect("テストフィクスチャの前提条件を満たす");

    // エントリを生成する (送信側: REQUEST_UPDATE 送信)
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client.outgoing_request_updates.contains_key(&rid),
        "REQUEST_UPDATE 送信で outgoing エントリが生成されること"
    );

    // Established のまま forget すると None が返り、エントリは掃除されない
    assert!(
        client.forget_fetch(rid).is_none(),
        "cleanup 不可の fetch は forget できないこと"
    );
    assert!(
        client.outgoing_request_updates.contains_key(&rid),
        "forget 失敗時は outgoing エントリが残ること"
    );
}

/// namespace publication の forget が REQUEST_UPDATE のクレジットカウントエントリを掃除する
///
/// `forget_subscription_cleans_request_update_credit_entries` の namespace publication 版。
/// PUBLISH_NAMESPACE の REQUEST_UPDATE は publisher 役のみが送信でき (draft
/// §9.14)、受信は subscriber 役限定のため incoming は生成しない。受信側エントリの
/// 掃除は publisher 役の subscription テストで検証済み。
/// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
#[test]
fn forget_namespace_publication_cleans_request_update_credit_entries() {
    use crate::message::{ControlMessage, RequestOk, Setup};
    use crate::message_parameter::MessageParameters;
    use crate::parameter::{SetupOption, SetupOptionValue, SetupOptions};
    use crate::session::types::SessionState;
    use crate::track_properties::TrackProperties;

    // Session を Established にする (MAX_REQUEST_UPDATES = 1 を local / peer 両方に設定)
    let mut local_opts = SetupOptions::new();
    local_opts.push(SetupOption {
        option_type: crate::parameter::SETUP_OPTION_MAX_REQUEST_UPDATES,
        value: SetupOptionValue::VarInt(1),
    });
    let mut peer_opts = SetupOptions::new();
    peer_opts.push(SetupOption {
        option_type: crate::parameter::SETUP_OPTION_MAX_REQUEST_UPDATES,
        value: SetupOptionValue::VarInt(1),
    });
    let mut client = Session::new_client(Transport::Quic, local_opts)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(ControlMessage::Setup(Setup { options: peer_opts }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(client.state(), SessionState::Established);

    // PUBLISH_NAMESPACE を送信して REQUEST_OK で Established にする
    let rid = client
        .send_publish_namespace(
            TrackNamespace::new(vec![b"live".to_vec()])
                .expect("テストフィクスチャの前提条件を満たす"),
            MessageParameters::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(RequestOk {
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }),
        )
        .expect("テストフィクスチャの前提条件を満たす");

    // 送信側エントリ: REQUEST_UPDATE を送信する (PUBLISH_NAMESPACE の REQUEST_UPDATE は
    // パラメータなしでも成立する)
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client.outgoing_request_updates.contains_key(&rid),
        "REQUEST_UPDATE 送信で outgoing エントリが生成されること"
    );

    // 他 request のエントリ (掃除対象外) も混在させる。受信側エントリ (incoming) は
    // publisher 役の namespace publication では REQUEST_UPDATE を受信できないため生成
    // しない (incoming の掃除は publisher 役の subscription テストで検証済み)
    client.outgoing_request_updates.insert(9999, 1);
    client.incoming_request_updates.insert(9999, 1);

    // bidi request stream を閉じて Terminated にして forget する
    client
        .recv_request_stream_closed(rid, crate::session::types::RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    let publication = client
        .forget_namespace_publication(rid)
        .expect("Terminated な namespace publication は forget できること");
    assert_eq!(publication.request_id, rid);
    // 該当 request のエントリのみ掃除され、他 request のエントリは残る
    assert!(
        !client.outgoing_request_updates.contains_key(&rid),
        "forget_namespace_publication で該当 request の outgoing エントリが掃除されること"
    );
    assert_eq!(
        client.outgoing_request_updates.len(),
        1,
        "他 request の outgoing エントリは残ること"
    );
    assert_eq!(
        client.incoming_request_updates.len(),
        1,
        "他 request の incoming エントリは残ること"
    );
}

/// namespace subscription の forget が REQUEST_UPDATE のクレジットカウントエントリを掃除する
///
/// `forget_subscription_cleans_request_update_credit_entries` の namespace subscription 版。
/// SUBSCRIBE_NAMESPACE の REQUEST_UPDATE は subscriber 役のみが送信でき (draft
/// §9.15)、受信は publisher 役限定のため incoming は生成しない。受信側エントリの
/// 掃除は publisher 役の subscription テストで検証済み。
/// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
#[test]
fn forget_namespace_subscription_cleans_request_update_credit_entries() {
    use crate::message::{ControlMessage, RequestOk, Setup};
    use crate::message_parameter::MessageParameters;
    use crate::parameter::{SetupOption, SetupOptionValue, SetupOptions};
    use crate::session::types::SessionState;
    use crate::track_properties::TrackProperties;

    // Session を Established にする (MAX_REQUEST_UPDATES = 1 を local / peer 両方に設定)
    let mut local_opts = SetupOptions::new();
    local_opts.push(SetupOption {
        option_type: crate::parameter::SETUP_OPTION_MAX_REQUEST_UPDATES,
        value: SetupOptionValue::VarInt(1),
    });
    let mut peer_opts = SetupOptions::new();
    peer_opts.push(SetupOption {
        option_type: crate::parameter::SETUP_OPTION_MAX_REQUEST_UPDATES,
        value: SetupOptionValue::VarInt(1),
    });
    let mut client = Session::new_client(Transport::Quic, local_opts)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(ControlMessage::Setup(Setup { options: peer_opts }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(client.state(), SessionState::Established);

    // SUBSCRIBE_NAMESPACE を送信して REQUEST_OK で Established にする
    let rid = client
        .send_subscribe_namespace(
            TrackNamespace::new(vec![b"live".to_vec()])
                .expect("テストフィクスチャの前提条件を満たす"),
            MessageParameters::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(RequestOk {
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }),
        )
        .expect("テストフィクスチャの前提条件を満たす");

    // 送信側エントリ: REQUEST_UPDATE を送信する (SUBSCRIBE_NAMESPACE の REQUEST_UPDATE
    // はパラメータなしでも成立する)
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client.outgoing_request_updates.contains_key(&rid),
        "REQUEST_UPDATE 送信で outgoing エントリが生成されること"
    );

    // 他 request のエントリ (掃除対象外) も混在させる。受信側エントリ (incoming) は
    // subscriber 役の namespace subscription では REQUEST_UPDATE を受信できないため
    // 生成しない (incoming の掃除は publisher 役の subscription テストで検証済み)
    client.outgoing_request_updates.insert(9999, 1);
    client.incoming_request_updates.insert(9999, 1);

    // bidi request stream を閉じて Terminated にして forget する
    client
        .recv_request_stream_closed(rid, crate::session::types::RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    let subscription = client
        .forget_namespace_subscription(rid)
        .expect("Terminated な namespace subscription は forget できること");
    assert_eq!(subscription.request_id, rid);
    // 該当 request のエントリのみ掃除され、他 request のエントリは残る
    assert!(
        !client.outgoing_request_updates.contains_key(&rid),
        "forget_namespace_subscription で該当 request の outgoing エントリが掃除されること"
    );
    assert_eq!(
        client.outgoing_request_updates.len(),
        1,
        "他 request の outgoing エントリは残ること"
    );
    assert_eq!(
        client.incoming_request_updates.len(),
        1,
        "他 request の incoming エントリは残ること"
    );
}

/// track subscription の forget が REQUEST_UPDATE のクレジットカウントエントリを掃除する
///
/// `forget_subscription_cleans_request_update_credit_entries` の track subscription 版。
/// SUBSCRIBE_TRACKS の REQUEST_UPDATE は subscriber 役のみが送信でき (draft
/// §9.18)、受信は publisher 役限定のため incoming は生成しない。受信側エントリの
/// 掃除は publisher 役の subscription テストで検証済み。
/// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
#[test]
fn forget_track_subscription_cleans_request_update_credit_entries() {
    use crate::message::{ControlMessage, RequestOk, Setup};
    use crate::message_parameter::MessageParameters;
    use crate::parameter::{SetupOption, SetupOptionValue, SetupOptions};
    use crate::session::types::SessionState;
    use crate::track_properties::TrackProperties;

    // Session を Established にする (MAX_REQUEST_UPDATES = 1 を local / peer 両方に設定)
    let mut local_opts = SetupOptions::new();
    local_opts.push(SetupOption {
        option_type: crate::parameter::SETUP_OPTION_MAX_REQUEST_UPDATES,
        value: SetupOptionValue::VarInt(1),
    });
    let mut peer_opts = SetupOptions::new();
    peer_opts.push(SetupOption {
        option_type: crate::parameter::SETUP_OPTION_MAX_REQUEST_UPDATES,
        value: SetupOptionValue::VarInt(1),
    });
    let mut client = Session::new_client(Transport::Quic, local_opts)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(ControlMessage::Setup(Setup { options: peer_opts }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(client.state(), SessionState::Established);

    // SUBSCRIBE_TRACKS を送信して REQUEST_OK で Established にする
    let rid = client
        .send_subscribe_tracks(
            TrackNamespace::new(vec![b"live".to_vec()])
                .expect("テストフィクスチャの前提条件を満たす"),
            MessageParameters::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(RequestOk {
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }),
        )
        .expect("テストフィクスチャの前提条件を満たす");

    // 送信側エントリ: REQUEST_UPDATE を送信する (SUBSCRIBE_TRACKS の REQUEST_UPDATE は
    // パラメータなしでも成立する)
    client
        .send_request_update(rid, MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client.outgoing_request_updates.contains_key(&rid),
        "REQUEST_UPDATE 送信で outgoing エントリが生成されること"
    );

    // 他 request のエントリ (掃除対象外) も混在させる。受信側エントリ (incoming) は
    // subscriber 役の track subscription では REQUEST_UPDATE を受信できないため生成
    // しない (incoming の掃除は publisher 役の subscription テストで検証済み)
    client.outgoing_request_updates.insert(9999, 1);
    client.incoming_request_updates.insert(9999, 1);

    // bidi request stream を閉じて Terminated にして forget する
    client
        .recv_request_stream_closed(rid, crate::session::types::RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    let subscription = client
        .forget_track_subscription(rid)
        .expect("Terminated な track subscription は forget できること");
    assert_eq!(subscription.request_id, rid);
    // 該当 request のエントリのみ掃除され、他 request のエントリは残る
    assert!(
        !client.outgoing_request_updates.contains_key(&rid),
        "forget_track_subscription で該当 request の outgoing エントリが掃除されること"
    );
    assert_eq!(
        client.outgoing_request_updates.len(),
        1,
        "他 request の outgoing エントリは残ること"
    );
    assert_eq!(
        client.incoming_request_updates.len(),
        1,
        "他 request の incoming エントリは残ること"
    );
}

/// tick 時の未確定エントリ確定が Pending 集合のみで行われる
///
/// `datagram_header_complete_ms` は forget まで保持されるため件数が増えても
/// tick 走査は Pending (未確定) のみに限定される。tick 前送信は None + Pending 登録、
/// tick で確定して Pending から除去、tick 後送信は直接 Some で Pending 登録なし、
/// 同一オブジェクトの再送は初回時刻を上書きしないことを検証する。
#[test]
fn datagram_pending_entries_determined_on_tick() {
    use crate::message::{ControlMessage, RequestOk, Setup};
    use crate::message_parameter::MessageParameters;
    use crate::parameter::SetupOptions;
    use crate::session::types::SessionState;
    use crate::track_properties::{
        PROP_OBJECT_DELIVERY_TIMEOUT, TrackProperties, TrackProperty, TrackPropertyValue,
    };

    let mut client = Session::new_client(Transport::Quic, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(ControlMessage::Setup(Setup {
            options: SetupOptions::new(),
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(client.state(), SessionState::Established);

    let mut track_properties = TrackProperties::new();
    track_properties.push(TrackProperty {
        prop_type: PROP_OBJECT_DELIVERY_TIMEOUT,
        value: TrackPropertyValue::VarInt(500),
    });
    let rid = client
        .send_publish(
            TrackNamespace::new(vec![b"live".to_vec()])
                .expect("テストフィクスチャの前提条件を満たす"),
            b"cam".to_vec(),
            1,
            MessageParameters::new(),
            track_properties,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(RequestOk {
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }),
        )
        .expect("テストフィクスチャの前提条件を満たす");

    // tick 前の送信は未確定 (None) で Pending 登録される
    client
        .send_object_datagram(rid, 0, 0, None, None)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client.timing.datagram_header_complete_ms.get(&(rid, 0, 0)),
        Some(&None),
        "tick 前のエントリは未確定であること"
    );
    assert!(
        client.timing.datagram_pending_ms.contains(&(rid, 0, 0)),
        "tick 前のエントリは Pending 登録されること"
    );

    // tick で確定し Pending から除去される
    client.tick(1_000);
    assert_eq!(
        client.timing.datagram_header_complete_ms.get(&(rid, 0, 0)),
        Some(&Some(1_000)),
        "tick で未確定エントリが確定すること"
    );
    assert!(
        client.timing.datagram_pending_ms.is_empty(),
        "確定後は Pending が空になること"
    );

    // tick 後の送信は直接確定時刻で Pending 登録なし
    client
        .send_object_datagram(rid, 0, 1, None, None)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client.timing.datagram_header_complete_ms.get(&(rid, 0, 1)),
        Some(&Some(1_000)),
        "tick 後のエントリは確定時刻で作られること"
    );
    assert!(
        client.timing.datagram_pending_ms.is_empty(),
        "確定済み送信では Pending 登録されないこと"
    );

    // 同一オブジェクトの再送は初回時刻を上書きしない (timeout=500 のため 200ms 後に再送)
    client.tick(1_200);
    client
        .send_object_datagram(rid, 0, 0, None, None)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client.timing.datagram_header_complete_ms.get(&(rid, 0, 0)),
        Some(&Some(1_000)),
        "再送で初回時刻が上書きされないこと"
    );
}

/// subscription 単位の上限超過で最古 Group から丸ごと破棄される
///
/// 長期ライブ配信で追跡エントリが無制限に増加しないこと、破棄後に件数が上限に収まり
/// 新規エントリが記録されることを検証する。破棄範囲の再送は新規扱いで再計時される。
#[test]
fn datagram_tracking_evicts_oldest_group_over_subscription_cap() {
    use super::core::MAX_DATAGRAM_TRACKING_ENTRIES_PER_SUBSCRIPTION;
    use crate::message::{ControlMessage, RequestOk, Setup};
    use crate::message_parameter::MessageParameters;
    use crate::parameter::SetupOptions;
    use crate::session::types::SessionState;
    use crate::track_properties::{
        PROP_OBJECT_DELIVERY_TIMEOUT, TrackProperties, TrackProperty, TrackPropertyValue,
    };

    let mut client = Session::new_client(Transport::Quic, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(ControlMessage::Setup(Setup {
            options: SetupOptions::new(),
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(client.state(), SessionState::Established);

    let mut track_properties = TrackProperties::new();
    track_properties.push(TrackProperty {
        prop_type: PROP_OBJECT_DELIVERY_TIMEOUT,
        value: TrackPropertyValue::VarInt(500),
    });
    let rid = client
        .send_publish(
            TrackNamespace::new(vec![b"live".to_vec()])
                .expect("テストフィクスチャの前提条件を満たす"),
            b"cam".to_vec(),
            1,
            MessageParameters::new(),
            track_properties,
        )
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_stream_message(
            rid,
            ControlMessage::RequestOk(RequestOk {
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    client.tick(1_000);

    // 上限いっぱいまで Group 0..MAX を直接登録する (送信経路では時間がかかるため)
    for group_id in 0..MAX_DATAGRAM_TRACKING_ENTRIES_PER_SUBSCRIPTION as u64 {
        client
            .timing
            .datagram_header_complete_ms
            .insert((rid, group_id, 0), Some(1_000));
    }
    assert_eq!(
        client.timing.datagram_header_complete_ms.len(),
        MAX_DATAGRAM_TRACKING_ENTRIES_PER_SUBSCRIPTION,
        "上限いっぱいの前提条件を満たすこと"
    );

    // 新規 Group の送信で最古 Group が丸ごと破棄される
    let new_group = MAX_DATAGRAM_TRACKING_ENTRIES_PER_SUBSCRIPTION as u64;
    client
        .send_object_datagram(rid, new_group, 0, None, None)
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        !client
            .timing
            .datagram_header_complete_ms
            .contains_key(&(rid, 0, 0)),
        "上限超過で最古 Group が破棄されること"
    );
    assert!(
        client
            .timing
            .datagram_header_complete_ms
            .contains_key(&(rid, new_group, 0)),
        "新規エントリが記録されること"
    );
    assert_eq!(
        client.timing.datagram_header_complete_ms.len(),
        MAX_DATAGRAM_TRACKING_ENTRIES_PER_SUBSCRIPTION,
        "破棄後は件数が上限に収まること"
    );
}

/// RequestIdTracker は未到達 Request ID の保持数に上限を設ける
///
/// peer が連続受信前縁より先の Request ID を飛び飛びに送り続けても `above` が
/// 無制限に増えないことを検証する。上限ちょうどまでは受理し、超過は
/// INVALID_REQUEST_ID で拒否する。
#[test]
fn peer_request_tracker_rejects_too_many_out_of_order_ids() {
    let mut tracker = RequestIdTracker::new(Role::Client);
    // front を確定させず (id=0 を送らず) 飛び飛びの id を上限まで受理する
    for i in 1..=MAX_OUT_OF_ORDER_REQUEST_IDS as u64 {
        tracker
            .accept(i * 2)
            .expect("上限までの未到達 request_id は受理されること");
    }
    assert_eq!(tracker.seen_count(), MAX_OUT_OF_ORDER_REQUEST_IDS);
    // 上限超過は INVALID_REQUEST_ID で拒否される
    let err = tracker
        .accept((MAX_OUT_OF_ORDER_REQUEST_IDS as u64 + 1) * 2)
        .expect_err("上限超過の未到達 request_id は拒否されること");
    assert_eq!(err.code, SESSION_INVALID_REQUEST_ID);
    // 拒否した request_id は累計受理数に数えない
    assert_eq!(tracker.seen_count(), MAX_OUT_OF_ORDER_REQUEST_IDS);
    // id=0 の受理で front が確定し above が全件吸収される
    tracker.accept(0).expect("front 確定の受理に成功すること");
    assert_eq!(
        tracker.seen_count(),
        MAX_OUT_OF_ORDER_REQUEST_IDS + 1,
        "吸収後も累計受理数は維持されること"
    );
    // 吸収後は above に空きができ、新しい未到達 id を受理できる
    tracker
        .accept((MAX_OUT_OF_ORDER_REQUEST_IDS as u64 + 1) * 2)
        .expect("吸収後は上限がリセットされて受理できること");
}

/// SUBSCRIBE_TRACKS の bidi stream 終端で peer alias の SubgroupTracker エントリが
/// 解放されること
///
/// draft-ietf-moq-transport-21 §6.4.2.3 (Request Cancellation and Rejection): SUBSCRIBE_TRACKS
/// の終端で active_track_aliases に残存する subscription を暗黙終端するのは本実装の扱いである。
/// このとき alias holder が空になったら SubgroupTracker の alias 単位
/// エントリも除去しないと、alias 再利用時に古い subgroup 履歴が干渉する。
#[test]
fn close_track_subscription_on_stream_end_releases_peer_alias_subgroup_entries() {
    use crate::message::{ControlMessage, Publish, RequestOk, Setup};
    use crate::message_parameter::MessageParameters;
    use crate::parameter::SetupOptions;
    use crate::session::types::{RequestStreamEnd, SessionState, TrackSubscriptionState};
    use crate::track_properties::TrackProperties;

    // client (subscriber) を Established にする
    let mut client = Session::new_client(Transport::Quic, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(ControlMessage::Setup(Setup {
            options: SetupOptions::new(),
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(client.state(), SessionState::Established);

    // SUBSCRIBE_TRACKS を確立する
    let track_rid = client
        .send_subscribe_tracks(
            TrackNamespace::new(vec![b"example".to_vec()])
                .expect("テストフィクスチャの前提条件を満たす"),
            MessageParameters::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_stream_message(
            track_rid,
            ControlMessage::RequestOk(RequestOk {
                parameters: MessageParameters::new(),
                track_properties: TrackProperties::new(),
            }),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        client
            .track_subscription(track_rid)
            .expect("track subscription が存在すること")
            .state,
        TrackSubscriptionState::Established
    );

    // peer publisher からの PUBLISH で alias を active_track_aliases と
    // peer_publisher_aliases に登録する (server 起点なので request_id は奇数)
    let alias = 42;
    let publish_rid = 1;
    client
        .recv_request(ControlMessage::Publish(Publish {
            request_id: publish_rid,
            track_namespace: TrackNamespace::new(vec![b"example".to_vec(), b"live".to_vec()])
                .expect("テストフィクスチャの前提条件を満たす"),
            track_name: b"cam".to_vec(),
            track_alias: alias,
            parameters: MessageParameters::new(),
            track_properties: TrackProperties::new(),
        }))
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        client
            .track_subscription(track_rid)
            .expect("track subscription が存在すること")
            .active_track_aliases
            .contains(&alias),
        "PUBLISH 受信で active_track_aliases に alias が登録されること"
    );

    // peer publisher 由来 subgroup のライフサイクルを記録する
    client
        .peer_subgroups
        .open(alias, 0, 0)
        .expect("subgroup tracker への登録に成功すること");
    assert!(
        client.peer_subgroups.get(alias, 0, 0).is_some(),
        "前提条件として SubgroupTracker にエントリが存在すること"
    );

    // SUBSCRIBE_TRACKS の bidi stream を終端する
    client
        .recv_request_stream_closed(track_rid, RequestStreamEnd::Fin)
        .expect("stream 終端の処理に成功すること");

    // alias holder が空になったため SubgroupTracker のエントリが除去されること
    assert!(
        client.peer_subgroups.get(alias, 0, 0).is_none(),
        "SUBSCRIBE_TRACKS 終端で当該 alias の SubgroupTracker エントリが除去されること"
    );
}

/// 購読索引の登録・削除ヘルパの契約を検証する
///
/// `register_subscription` は `subscriptions` と `subscriptions_by_track` を同時に更新し、
/// `remove_subscription_track_index` は 1 件削除では key を残し、最後の 1 件で key ごと消し、
/// 未知 key では no-op になる。`subscriptions_by_track` は pub(super) のため
/// integration test からは観測できず、ここで固定する。
#[test]
fn register_and_remove_subscription_track_index_contracts() {
    use crate::message::{ControlMessage, Setup};
    use crate::parameter::SetupOptions;

    let mut client = Session::new_client(Transport::Quic, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(ControlMessage::Setup(Setup {
            options: SetupOptions::new(),
        }))
        .expect("テストフィクスチャの前提条件を満たす");

    let key = (
        TrackNamespace::new(vec![b"live".to_vec()]).expect("テストフィクスチャの前提条件を満たす"),
        b"cam".to_vec(),
        TrackRole::Subscriber,
    );
    let rid1 = client
        .send_subscribe(key.0.clone(), key.1.clone(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let rid2 = client
        .send_subscribe(key.0.clone(), key.1.clone(), MessageParameters::new())
        .expect("テストフィクスチャの前提条件を満たす");
    // register_subscription が subscriptions と subscriptions_by_track を同時に更新する
    assert!(client.subscription(rid1).is_some());
    assert!(client.subscription(rid2).is_some());
    assert_eq!(
        client.aliases.subscriptions_by_track.get(&key),
        Some(&vec![rid1, rid2])
    );

    // 1 件削除では同じ key の他 subscription が残る
    client.remove_subscription_track_index(rid1, &key);
    assert_eq!(
        client.aliases.subscriptions_by_track.get(&key),
        Some(&vec![rid2])
    );

    // 最後の 1 件削除で key ごと消える
    client.remove_subscription_track_index(rid2, &key);
    assert!(client.aliases.subscriptions_by_track.get(&key).is_none());

    // 未知 key への削除は no-op
    client.remove_subscription_track_index(rid1, &key);
    assert!(client.aliases.subscriptions_by_track.get(&key).is_none());
}

// ─── ローカル専用エラーコードの wire 流出防止 ──────────────────────

/// SETUP を交換して Established にした client / server ペアを返す
fn establish_pair_for_local_code_tests() -> (Session, Session) {
    use crate::parameter::SetupOptions;

    let mut client = Session::new_client(Transport::Quic, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let mut server = Session::new_server(Transport::Quic, SetupOptions::new())
        .expect("テストフィクスチャの前提条件を満たす");
    let c_setup = take_send_control_for_local_code_tests(&mut client);
    let s_setup = take_send_control_for_local_code_tests(&mut server);
    server
        .recv_control(c_setup)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(s_setup)
        .expect("テストフィクスチャの前提条件を満たす");
    (client, server)
}

fn take_send_control_for_local_code_tests(s: &mut Session) -> crate::message::ControlMessage {
    while let Some(e) = s.poll_event() {
        if let SessionEvent::SendControl(msg) = e {
            return msg;
        }
    }
    panic!("SendControl イベントが期待されたが発行されなかった");
}

fn take_send_request_for_local_code_tests(s: &mut Session) -> crate::message::ControlMessage {
    while let Some(e) = s.poll_event() {
        if let SessionEvent::SendRequest { message, .. } = e {
            return message;
        }
    }
    panic!("SendRequest イベントが期待されたが発行されなかった");
}

fn take_send_on_stream_for_local_code_tests(s: &mut Session) -> crate::message::ControlMessage {
    while let Some(e) = s.poll_event() {
        if let SessionEvent::SendOnStream { message, .. } = e {
            return message;
        }
    }
    panic!("SendOnStream イベントが期待されたが発行されなかった");
}

/// publisher 役 subscription を 1 本確立した server を返す
fn establish_publisher_subscription_for_local_code_tests() -> (Session, Session, u64) {
    use crate::track_properties::TrackProperties;

    let (mut client, mut server) = establish_pair_for_local_code_tests();
    let rid = client
        .send_subscribe(
            TrackNamespace::new(vec![b"live".to_vec()]).expect("有効な namespace"),
            b"cam".to_vec(),
            MessageParameters::new(),
        )
        .expect("SUBSCRIBE に成功すること");
    let sub_msg = take_send_request_for_local_code_tests(&mut client);
    server
        .recv_request(sub_msg)
        .expect("SUBSCRIBE の受信に成功すること");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_OK に成功すること");
    let _ = take_send_on_stream_for_local_code_tests(&mut server);
    (client, server, rid)
}

/// close はローカル専用コードを SESSION_INTERNAL_ERROR に置換する
#[test]
fn close_replaces_local_error_code_with_internal_error() {
    use crate::error::{SESSION_INTERNAL_ERROR, SESSION_LOCAL_FILTER_MISMATCH};
    let (mut client, _server) = establish_pair_for_local_code_tests();
    client.close(SESSION_LOCAL_FILTER_MISMATCH, "local filter mismatch");
    let mut code = None;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::CloseSession(err) = ev {
            code = Some(err.code);
        }
    }
    assert_eq!(
        code,
        Some(SESSION_INTERNAL_ERROR),
        "close はローカルコードを SESSION_INTERNAL_ERROR に置換すること"
    );
}

/// fail はローカル専用コードを SESSION_INTERNAL_ERROR に置換する
#[test]
fn fail_replaces_local_error_code_with_internal_error() {
    use crate::error::{SESSION_INTERNAL_ERROR, SESSION_LOCAL_DATAGRAM_TIMEOUT};
    let (mut client, _server) = establish_pair_for_local_code_tests();
    client.fail(SessionError::new(
        SESSION_LOCAL_DATAGRAM_TIMEOUT,
        "local datagram timeout",
    ));
    let mut code = None;
    while let Some(ev) = client.poll_event() {
        if let SessionEvent::CloseSession(err) = ev {
            code = Some(err.code);
        }
    }
    assert_eq!(code, Some(SESSION_INTERNAL_ERROR));
}

/// send_request_error はローカル専用コードを REQUEST_INTERNAL_ERROR に置換する
#[test]
fn send_request_error_replaces_local_error_code_with_request_internal_error() {
    use crate::error::{REQUEST_INTERNAL_ERROR, SESSION_LOCAL_FILTER_MISMATCH};
    use crate::message::ReasonPhrase;
    let (_client, mut server, rid) = establish_publisher_subscription_for_local_code_tests();
    server
        .send_request_error(
            rid,
            SESSION_LOCAL_FILTER_MISMATCH,
            0,
            ReasonPhrase::new("local filter mismatch").expect("有効な reason"),
            None,
        )
        .expect("REQUEST_ERROR の送信に成功すること");
    match take_send_on_stream_for_local_code_tests(&mut server) {
        crate::message::ControlMessage::RequestError(e) => {
            assert_eq!(e.error_code, REQUEST_INTERNAL_ERROR);
        }
        other => panic!("RequestError が期待される: {other:?}"),
    }
}

/// send_publish_done はローカル専用コードを PUBLISH_DONE_INTERNAL_ERROR に置換する
#[test]
fn send_publish_done_replaces_local_error_code_with_publish_done_internal_error() {
    use crate::error::{PUBLISH_DONE_INTERNAL_ERROR, SESSION_LOCAL_FILTER_MISMATCH};
    use crate::message::ReasonPhrase;
    let (_client, mut server, rid) = establish_publisher_subscription_for_local_code_tests();
    server
        .send_publish_done(
            rid,
            SESSION_LOCAL_FILTER_MISMATCH,
            0,
            ReasonPhrase::new("local filter mismatch").expect("有効な reason"),
        )
        .expect("PUBLISH_DONE の送信に成功すること");
    match take_send_on_stream_for_local_code_tests(&mut server) {
        crate::message::ControlMessage::PublishDone(pd) => {
            assert_eq!(pd.status_code, PUBLISH_DONE_INTERNAL_ERROR);
        }
        other => panic!("PublishDone が期待される: {other:?}"),
    }
}

/// reset_outgoing_data_stream_with_code はローカル専用コードを STREAM_INTERNAL_ERROR に置換する
#[test]
fn reset_with_code_replaces_local_error_code_with_stream_internal_error() {
    use crate::error::{SESSION_LOCAL_FILTER_MISMATCH, STREAM_INTERNAL_ERROR};
    use crate::stream::subgroup::{SubgroupHeader, SubgroupIdMode};
    let (_client, mut server, rid) = establish_publisher_subscription_for_local_code_tests();
    let stream_id = DataStreamId(50);
    server
        .send_subgroup_header(
            stream_id,
            rid,
            &SubgroupHeader {
                track_alias: 1,
                group_id: 0,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(1),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("SUBGROUP_HEADER の送信に成功すること");
    server
        .reset_outgoing_data_stream_with_code(stream_id, SESSION_LOCAL_FILTER_MISMATCH)
        .expect("RESET_STREAM の送信に成功すること");
    let mut error_code = None;
    while let Some(ev) = server.poll_event() {
        if let SessionEvent::ResetDataStream {
            error_code: code, ..
        } = ev
        {
            error_code = Some(code);
        }
    }
    assert_eq!(error_code, Some(STREAM_INTERNAL_ERROR));

    // reset_outgoing_data_stream_at_with_code 経由でも同じ置換が行われる
    let at_stream_id = DataStreamId(51);
    server
        .send_subgroup_header(
            at_stream_id,
            rid,
            &SubgroupHeader {
                track_alias: 1,
                group_id: 1,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(1),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("SUBGROUP_HEADER の送信に成功すること");
    server
        .reset_outgoing_data_stream_at_with_code(
            at_stream_id,
            SESSION_LOCAL_FILTER_MISMATCH,
            Some(0),
        )
        .expect("RESET_STREAM_AT の送信に成功すること");
    let mut error_code = None;
    while let Some(ev) = server.poll_event() {
        if let SessionEvent::ResetDataStream {
            error_code: code, ..
        } = ev
        {
            error_code = Some(code);
        }
    }
    assert_eq!(error_code, Some(STREAM_INTERNAL_ERROR));
}

/// ローカル variant は `as_session_error()` が `None` を返す
#[test]
fn local_send_request_error_has_no_session_error() {
    use super::types::SendRequestError;
    assert!(
        SendRequestError::LocalFilterMismatch
            .as_session_error()
            .is_none()
    );
    assert!(
        SendRequestError::LocalDatagramTimeout
            .as_session_error()
            .is_none()
    );
    assert!(
        SendRequestError::PeerGoawayReceived
            .as_session_error()
            .is_none()
    );
}

/// 受信 subgroup Object のフィルタテスト用に client / server を指定の SetupOptions で
/// Established にする
fn establish_pair_for_data_tests(
    client_opts: crate::parameter::SetupOptions,
    server_opts: crate::parameter::SetupOptions,
) -> (Session, Session) {
    let mut client = Session::new_client(Transport::WebTransport, client_opts)
        .expect("テストフィクスチャの前提条件を満たす");
    let mut server = Session::new_server(Transport::WebTransport, server_opts)
        .expect("テストフィクスチャの前提条件を満たす");
    let c_setup = take_send_control_for_local_code_tests(&mut client);
    let s_setup = take_send_control_for_local_code_tests(&mut server);
    server
        .recv_control(c_setup)
        .expect("テストフィクスチャの前提条件を満たす");
    client
        .recv_control(s_setup)
        .expect("テストフィクスチャの前提条件を満たす");
    (client, server)
}

/// MAX_FILTER_RANGES を宣言する server 用 SetupOptions を作る
fn max_filter_options_for_data_tests() -> crate::parameter::SetupOptions {
    use crate::parameter::{SETUP_OPTION_MAX_FILTER_RANGES, SetupOption, SetupOptionValue};

    let mut opts = crate::parameter::SetupOptions::new();
    opts.push(SetupOption {
        option_type: SETUP_OPTION_MAX_FILTER_RANGES,
        value: SetupOptionValue::VarInt(8),
    });
    opts
}

/// 1 つの Range を持つ Range Filter パラメータを作る (param_type, set_id, start, end)
fn object_range_filter_for_data_tests(
    param_type: u64,
    set_id: u8,
    start: u64,
    end: u64,
) -> MessageParameter {
    let mut bytes = vec![set_id];
    crate::varint::encode(start, &mut bytes);
    crate::varint::encode(end - start, &mut bytes);
    MessageParameter {
        param_type,
        value: MessageParameterValue::LengthPrefixed(bytes),
    }
}

/// client (subscriber) の subscription を 1 本 Established にする
fn establish_subscriber_subscription_for_data_tests(
    client: &mut Session,
    server: &mut Session,
    alias: u64,
    parameters: MessageParameters,
) -> u64 {
    use crate::track_properties::TrackProperties;

    let rid = client
        .send_subscribe(
            TrackNamespace::new(vec![b"live".to_vec()]).expect("有効な namespace"),
            b"cam".to_vec(),
            parameters,
        )
        .expect("SUBSCRIBE に成功すること");
    let sub_msg = take_send_request_for_local_code_tests(client);
    server
        .recv_request(sub_msg)
        .expect("SUBSCRIBE の受信に成功すること");
    server
        .send_subscribe_ok(rid, alias, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_OK に成功すること");
    let ok_msg = take_send_on_stream_for_local_code_tests(server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("SUBSCRIBE_OK の受信に成功すること");
    rid
}

/// Malformed Track 検出時は Object の帰属先 subscription を終端すること
///
/// draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 2 は FIN 済み Subgroup への
/// 最終 Object ID 超過を Malformed とする。同一 Subgroup の再オープンは禁止されるため
/// この条件は公開 API の通常手順では到達しない防御的検出であり、tracker に FIN 状態を
/// 直接記録して帰属先の決定ロジックを検証する。
#[test]
fn malformed_object_after_fin_terminates_attributed_subscription() {
    use crate::error::SESSION_PROTOCOL_VIOLATION;
    use crate::message_parameter::PARAM_OBJECTID_FILTER;
    use crate::stream::decoder::DecodedSubgroupObject;
    use crate::stream::subgroup::{SubgroupHeader, SubgroupIdMode};

    let (mut client, mut server) = establish_pair_for_data_tests(
        crate::parameter::SetupOptions::new(),
        max_filter_options_for_data_tests(),
    );
    // rid1 は Object ID [0, 9] のみ通し、rid2 は unfiltered で Object ID=50 が帰属する
    let mut params1 = MessageParameters::new();
    params1.push(object_range_filter_for_data_tests(
        PARAM_OBJECTID_FILTER,
        0,
        0,
        9,
    ));
    let rid1 =
        establish_subscriber_subscription_for_data_tests(&mut client, &mut server, 5000, params1);
    let rid2 = establish_subscriber_subscription_for_data_tests(
        &mut client,
        &mut server,
        5000,
        MessageParameters::new(),
    );

    // header は最初の候補 rid1 に受理される
    let stream_id = DataStreamId(900);
    let header = SubgroupHeader {
        track_alias: 5000,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(7),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_id, header.encode()[0] as u64)
        .expect("subgroup stream type の通知に成功すること");
    assert_eq!(
        client
            .recv_subgroup_header(stream_id, &header)
            .expect("subgroup header の受理に成功すること"),
        TrackDataAcceptance::Accepted
    );

    // FIN 済み Subgroup の最終 Object ID を tracker に記録する (再オープン禁止により
    // 公開 API の通常手順では到達しない状態を直接作る)
    client
        .peer_subgroups
        .mark_fin(5000, 0, 7, Some(5))
        .expect("FIN 状態の記録に成功すること");

    // Object ID=50 は rid1 が不合格、rid2 が合格する。Malformed 検出はフィルタ判定より
    // 優先され、帰属先の rid2 が終端される (stream 所有者 rid1 は Established のまま)
    let err = client
        .recv_subgroup_object(
            stream_id,
            &DecodedSubgroupObject {
                object_id: 50,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect_err("FIN 済み Subgroup への Object は Malformed Track になること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .state,
        SubscriptionState::Terminated,
        "Object の帰属先 rid2 が Terminated になること"
    );
    assert_eq!(
        client
            .subscription(rid1)
            .expect("subscription が存在すること")
            .state,
        SubscriptionState::Established,
        "stream 所有者 rid1 は Established のままであること"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// フィルタ不通過 Object でも Malformed Track (条件 2) が FilteredOut より優先されること
///
/// Malformed の検出はフィルタ判定に依存させない。公開 API では再オープン禁止により
/// 到達しないため、tracker に FIN 状態を直接記録して検証する。
#[test]
fn malformed_after_fin_takes_precedence_over_filtered_out() {
    use crate::error::SESSION_PROTOCOL_VIOLATION;
    use crate::message_parameter::PARAM_OBJECTID_FILTER;
    use crate::stream::decoder::DecodedSubgroupObject;
    use crate::stream::subgroup::{SubgroupHeader, SubgroupIdMode};

    let (mut client, mut server) = establish_pair_for_data_tests(
        crate::parameter::SetupOptions::new(),
        max_filter_options_for_data_tests(),
    );
    // Object ID [0, 9] のみ通すため、Object ID=50 はフィルタ不通過になる
    let mut params = MessageParameters::new();
    params.push(object_range_filter_for_data_tests(
        PARAM_OBJECTID_FILTER,
        0,
        0,
        9,
    ));
    let rid =
        establish_subscriber_subscription_for_data_tests(&mut client, &mut server, 5001, params);

    let stream_id = DataStreamId(901);
    let header = SubgroupHeader {
        track_alias: 5001,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_id, header.encode()[0] as u64)
        .expect("subgroup stream type の通知に成功すること");
    assert_eq!(
        client
            .recv_subgroup_header(stream_id, &header)
            .expect("subgroup header の受理に成功すること"),
        TrackDataAcceptance::Accepted
    );

    client
        .peer_subgroups
        .mark_fin(5001, 0, 0, Some(5))
        .expect("FIN 状態の記録に成功すること");

    // フィルタ不通過で本来 FilteredOut になる Object でも Malformed を優先して Err にする
    let err = client
        .recv_subgroup_object(
            stream_id,
            &DecodedSubgroupObject {
                object_id: 50,
                payload_length: 1,
                status: None,
                properties_bytes: None,
            },
        )
        .expect_err("フィルタ不通過でも Malformed Track は Err になること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        client
            .subscription(rid)
            .expect("subscription が存在すること")
            .state,
        SubscriptionState::Terminated,
        "Malformed として subscription が Terminated になること"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// FIN 時の `mark_fin` 失敗 (条件 3) の Malformed 終端対象が Object の帰属先に
/// なること
///
/// draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 3 は同一 Subgroup が複数
/// stream で FIN され最終 Object が異なる場合を Malformed とする。公開 API では
/// 再オープン禁止により通常手順で到達しないため、tracker に 1 回目の FIN を直接記録して
/// 検証する。終端対象は stream 所有者ではなく Object を受理した帰属先にする。
#[test]
fn malformed_mark_fin_failure_terminates_attributed_subscription() {
    use crate::error::SESSION_PROTOCOL_VIOLATION;
    use crate::message_parameter::PARAM_OBJECTID_FILTER;
    use crate::stream::decoder::DecodedSubgroupObject;
    use crate::stream::subgroup::{SubgroupHeader, SubgroupIdMode};

    let (mut client, mut server) = establish_pair_for_data_tests(
        crate::parameter::SetupOptions::new(),
        max_filter_options_for_data_tests(),
    );
    // rid1 は Object ID [0, 9] のみ通し、Object ID=50 は rid2 (unfiltered) に帰属する
    let mut params1 = MessageParameters::new();
    params1.push(object_range_filter_for_data_tests(
        PARAM_OBJECTID_FILTER,
        0,
        0,
        9,
    ));
    let rid1 =
        establish_subscriber_subscription_for_data_tests(&mut client, &mut server, 5002, params1);
    let rid2 = establish_subscriber_subscription_for_data_tests(
        &mut client,
        &mut server,
        5002,
        MessageParameters::new(),
    );

    let stream_id = DataStreamId(902);
    let header = SubgroupHeader {
        track_alias: 5002,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_id, header.encode()[0] as u64)
        .expect("subgroup stream type の通知に成功すること");
    assert_eq!(
        client
            .recv_subgroup_header(stream_id, &header)
            .expect("subgroup header の受理に成功すること"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(
        client
            .recv_subgroup_object(
                stream_id,
                &DecodedSubgroupObject {
                    object_id: 50,
                    payload_length: 1,
                    status: None,
                    properties_bytes: None,
                },
            )
            .expect("Object 受信に失敗しないこと"),
        TrackDataAcceptance::Accepted,
        "Object ID=50 は rid2 に帰属すること"
    );

    // 同一 Subgroup の 1 回目の FIN (最終 Object ID=5) を tracker に直接記録する
    // (再オープン禁止により公開 API の通常手順では到達しない状態を直接作る)
    client
        .peer_subgroups
        .mark_fin(5002, 0, 0, Some(5))
        .expect("FIN 状態の記録に成功すること");

    // 2 回目の FIN が異なる最終 Object ID=50 を主張するため条件 3 で Malformed になる。
    // 終端対象は帰属先 rid2 (stream 所有者 rid1 は Established のまま)
    let err = client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect_err("同一 Subgroup の最終 Object 不一致は Malformed Track になること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .state,
        SubscriptionState::Terminated,
        "Malformed の終端対象が Object の帰属先 rid2 になること"
    );
    assert_eq!(
        client
            .subscription(rid1)
            .expect("subscription が存在すること")
            .state,
        SubscriptionState::Established,
        "stream 所有者 rid1 は Established のままであること"
    );
    // stream は incoming から除去済みなので、mark_fin 失敗の終端経路でも所有者の
    // open 中の受信 stream 数が戻っている必要がある (残ると cleanup_ready が張り付く)
    assert_eq!(
        client
            .subscription(rid1)
            .expect("subscription が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "mark_fin 失敗の終端経路でも所有者 rid1 の open 中の受信 stream 数が戻ること"
    );
    assert_eq!(client.state(), SessionState::Established);
}

/// キャンセル済み所有者の FIN 破棄分岐でも tracker を終端状態にし、条件 3 不一致で
/// `mark_fin` が失敗した場合は reset として同一 Subgroup の再オープンを妨げないこと
///
/// 公開 API の通常手順では同一 Subgroup の FIN 済み再オープンは禁止されるため、
/// tracker に 1 回目の FIN を直接記録して条件 3 不一致を成立させる。
#[test]
fn discarded_fin_marks_tracker_terminal_and_allows_reopen() {
    use crate::stream::subgroup::{SubgroupHeader, SubgroupIdMode};
    use crate::subgroup_tracker::SubgroupStreamState;

    let (mut client, mut server) = establish_pair_for_data_tests(
        crate::parameter::SetupOptions::new(),
        max_filter_options_for_data_tests(),
    );
    // rid1 (最初の候補) が stream 所有者、rid2 が再オープン時の受理先になる
    let rid1 = establish_subscriber_subscription_for_data_tests(
        &mut client,
        &mut server,
        5003,
        MessageParameters::new(),
    );
    let rid2 = establish_subscriber_subscription_for_data_tests(
        &mut client,
        &mut server,
        5003,
        MessageParameters::new(),
    );

    let stream_id = DataStreamId(903);
    let header = SubgroupHeader {
        track_alias: 5003,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_id, header.encode()[0] as u64)
        .expect("subgroup stream type の通知に成功すること");
    assert_eq!(
        client
            .recv_subgroup_header(stream_id, &header)
            .expect("subgroup header の受理に成功すること"),
        TrackDataAcceptance::Accepted
    );

    // 同一 Subgroup の 1 回目の FIN (最終 Object ID=5) を tracker に直接記録する
    // (再オープン禁止により公開 API の通常手順では到達しない状態を直接作る)
    client
        .peer_subgroups
        .mark_fin(5003, 0, 0, Some(5))
        .expect("FIN 状態の記録に成功すること");

    // stream 所有者 rid1 をキャンセルする。stream は Object の帰属実績が無いため
    // FIN は破棄分岐に入る
    client
        .stop_sending(rid1)
        .expect("Established の subscription は stop_sending できること");

    // stream は Object を 1 つも受信していないため last_object_id は None になり、
    // 記録済みの Some(5) と不一致で mark_fin は Err になる。破棄分岐は状態が
    // Open のまま残らないよう reset として終端状態にする
    client
        .recv_data_stream_closed(stream_id, RequestStreamEnd::Fin)
        .expect("破棄対象 stream の FIN は Err にならないこと");
    assert!(
        matches!(
            client.peer_subgroups.get(5003, 0, 0),
            Some(SubgroupStreamState::Reset { .. })
        ),
        "mark_fin 失敗時は reset として終端状態になること"
    );

    // 同一 Subgroup の再オープン: rid1 はキャンセル済みなので rid2 が受理する。
    // 破棄分岐が tracker を終端状態にしていなければ Open 衝突で session close になる
    let stream_id2 = DataStreamId(904);
    client
        .recv_data_stream_type(stream_id2, header.encode()[0] as u64)
        .expect("subgroup stream type の通知に成功すること");
    assert_eq!(
        client
            .recv_subgroup_header(stream_id2, &header)
            .expect("同一 Subgroup の再オープンが session close せず受理されること"),
        TrackDataAcceptance::Accepted
    );
    assert_eq!(client.state(), SessionState::Established);
    assert_eq!(
        client
            .subscription(rid2)
            .expect("subscription が存在すること")
            .stream_counts
            .incoming_subgroup_count,
        1,
        "再オープンした stream が帰属先 rid2 に紐づくこと"
    );
}

/// 帰属先を forget した後の FIN が生きた所有者へフォールバックし、Group 終端の記録と
/// Malformed 終端の対象が死んだ帰属先にならないこと
///
/// 共有 Track Alias では Object の帰属先が stream 所有者と異なりうる。帰属先を forget すると
/// `last_attributed_request_id` が死んだ request_id を指したまま残るため、生存判定で
/// 所有者へフォールバックしないと END_OF_GROUP の記録が失われ、`mark_fin` 失敗時の
/// Malformed 終端も死んだ request_id への no-op になる。条件 3 の `mark_fin` 失敗は
/// 公開 API の通常手順では到達しないため、tracker に FIN 状態を直接記録して検証する。
#[test]
fn stale_attribution_falls_back_to_live_owner_on_fin() {
    use crate::error::SESSION_PROTOCOL_VIOLATION;
    use crate::message_parameter::PARAM_OBJECTID_FILTER;
    use crate::stream::decoder::DecodedSubgroupObject;
    use crate::stream::subgroup::{SubgroupHeader, SubgroupIdMode};

    let (mut client, mut server) = establish_pair_for_data_tests(
        crate::parameter::SetupOptions::new(),
        max_filter_options_for_data_tests(),
    );
    // rid1 は Object ID [0, 9] のみ通すため header は rid1 に紐づき、Object ID=50 は
    // 2 番目の候補 rid2 (unfiltered) に帰属する
    let mut params1 = MessageParameters::new();
    params1.push(object_range_filter_for_data_tests(
        PARAM_OBJECTID_FILTER,
        0,
        0,
        9,
    ));
    let rid1 =
        establish_subscriber_subscription_for_data_tests(&mut client, &mut server, 5004, params1);
    let rid2 = establish_subscriber_subscription_for_data_tests(
        &mut client,
        &mut server,
        5004,
        MessageParameters::new(),
    );

    // stream A: END_OF_GROUP + FIN を通常処理させ、Group 終端の記録先を検証する
    let stream_a = DataStreamId(905);
    let header_a = SubgroupHeader {
        track_alias: 5004,
        group_id: 0,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: true,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_a, header_a.encode()[0] as u64)
        .expect("subgroup stream type の通知に成功すること");
    assert_eq!(
        client
            .recv_subgroup_header(stream_a, &header_a)
            .expect("subgroup header の受理に成功すること"),
        TrackDataAcceptance::Accepted
    );

    // stream B: mark_fin 失敗 (条件 3) の終端対象を検証する
    let stream_b = DataStreamId(906);
    let header_b = SubgroupHeader {
        track_alias: 5004,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(128),
        has_properties: false,
        end_of_group: true,
        first_object: false,
    };
    client
        .recv_data_stream_type(stream_b, header_b.encode()[0] as u64)
        .expect("subgroup stream type の通知に成功すること");
    assert_eq!(
        client
            .recv_subgroup_header(stream_b, &header_b)
            .expect("subgroup header の受理に成功すること"),
        TrackDataAcceptance::Accepted
    );

    // 両 stream の Object ID=50 は unfiltered の rid2 に帰属する
    for stream_id in [stream_a, stream_b] {
        assert_eq!(
            client
                .recv_subgroup_object(
                    stream_id,
                    &DecodedSubgroupObject {
                        object_id: 50,
                        payload_length: 1,
                        status: None,
                        properties_bytes: None,
                    },
                )
                .expect("Object 受信に失敗しないこと"),
            TrackDataAcceptance::Accepted,
            "Object ID=50 は rid2 に帰属すること"
        );
    }

    // 帰属先 rid2 だけをキャンセルして forget する (rid1 は Established のまま)。
    // 以後 last_attributed_request_id は死んだ rid2 を指したままになる
    client
        .stop_sending(rid2)
        .expect("Established の subscription は stop_sending できること");
    client
        .forget_subscription(rid2)
        .expect("キャンセル由来 Terminated は cleanup_ready で forget できること");

    // stream A: 死んだ rid2 ではなく生きた所有者 rid1 に Group 終端 (50 の次 = 51) が
    // 記録される
    client
        .recv_data_stream_closed(stream_a, RequestStreamEnd::Fin)
        .expect("帰属先回収後の END_OF_GROUP + FIN も正常終端すること");
    assert_eq!(
        client
            .subscription(rid1)
            .expect("所有者 rid1 が存在すること")
            .ended_groups
            .get(&0),
        Some(&51),
        "Group 終端が死んだ帰属先ではなく生きた所有者へ記録されること"
    );

    // stream B: 同一 Subgroup の 1 回目の FIN (最終 Object ID=5) を tracker に直接記録し、
    // 2 回目の FIN の最終 Object ID=50 との不一致で条件 3 の mark_fin 失敗を成立させる。
    // 終端対象は死んだ rid2 ではなく生きた所有者 rid1 になる
    client
        .peer_subgroups
        .mark_fin(5004, 1, 0, Some(5))
        .expect("FIN 状態の記録に成功すること");
    let err = client
        .recv_data_stream_closed(stream_b, RequestStreamEnd::Fin)
        .expect_err("同一 Subgroup の最終 Object 不一致は Malformed Track になること");
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
    assert_eq!(
        client
            .subscription(rid1)
            .expect("所有者 rid1 が存在すること")
            .state,
        SubscriptionState::Terminated,
        "Malformed の終端対象が生きた所有者 rid1 になること"
    );
    assert_eq!(
        client
            .subscription(rid1)
            .expect("所有者 rid1 が存在すること")
            .stream_counts
            .open_incoming_subgroup_count,
        0,
        "mark_fin 失敗の終端経路でも所有者 rid1 の open 中の受信 stream 数が戻ること"
    );
    assert_eq!(client.state(), SessionState::Established);
}
