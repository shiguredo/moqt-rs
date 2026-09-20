//! キャンセル後の Track Alias 保持 (discard 用 tombstone) テスト
//!
//! draft-ietf-moq-transport-21 §3.1.2 (Track Alias):
//! "Objects can arrive after a subscription has been cancelled. Subscribers SHOULD retain
//! sufficient state to quickly discard these unwanted Objects, rather than treating them as
//! belonging to an unknown Track Alias."
//!
//! 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。

use super::*;

/// 受信テスト用の Object Datagram を作る
fn datagram(track_alias: u64) -> ObjectDatagram {
    ObjectDatagram {
        track_alias,
        group_id: 3,
        object_id: 1,
        publisher_priority: Some(7),
        properties_data: None,
        end_of_group: false,
        status: None,
    }
}

/// subscription を確立してから cancel (STOP_SENDING + forget) する
fn establish_then_cancel(alias: u64) -> (Session, u64) {
    let (mut client, _server, rid) = establish_subscribe_track(alias);
    client
        .stop_sending(rid)
        .expect("subscriber からの STOP_SENDING に成功すること");
    client
        .forget_subscription(rid)
        .expect("Terminated な subscription は forget できる");
    (client, rid)
}

/// キャンセル直後の同一 alias Object は未知 alias ではなく破棄経路に入る
#[test]
fn object_after_cancel_is_discarded_not_unknown() {
    const ALIAS: u64 = 1100;
    let (mut client, _rid) = establish_then_cancel(ALIAS);

    let outcome = client
        .recv_object_datagram(&datagram(ALIAS))
        .expect("破棄はセッションを閉じない");
    assert_eq!(
        outcome,
        TrackDataAcceptance::Discarded,
        "キャンセル直後は Discarded になること"
    );
    assert_eq!(
        client.state(),
        SessionState::Established,
        "破棄でセッションが閉じてはいけない"
    );
}

/// 保持期間を過ぎたら従来どおり未知 alias 扱いに戻る
#[test]
fn object_after_retention_expiry_is_unknown() {
    const ALIAS: u64 = 1101;
    let (mut client, _rid) = establish_then_cancel(ALIAS);
    let retention = client.peer_alias_retention_ms();
    assert_eq!(
        retention,
        shiguredo_moqt::session::core::DEFAULT_PEER_ALIAS_RETENTION_MS,
        "デフォルト保持期間が使われること"
    );

    // 最初の tick で deadline が確定し、保持期間を超えた tick で期限切れになる
    client.tick(0);
    assert_eq!(
        client
            .recv_object_datagram(&datagram(ALIAS))
            .expect("破棄はセッションを閉じない"),
        TrackDataAcceptance::Discarded,
        "保持期間内は Discarded のまま"
    );
    client.tick(retention);
    assert_eq!(
        client
            .recv_object_datagram(&datagram(ALIAS))
            .expect("未知 alias はセッションを閉じない"),
        TrackDataAcceptance::UnknownTrackAlias,
        "保持期間を過ぎたら未知 alias に戻ること"
    );
}

/// 保持期間は設定で変更できる
#[test]
fn retention_period_is_configurable() {
    const ALIAS: u64 = 1102;
    let (mut client, _server, rid) = establish_subscribe_track(ALIAS);
    client.set_peer_alias_retention_ms(100);
    assert_eq!(client.peer_alias_retention_ms(), 100);
    client
        .stop_sending(rid)
        .expect("STOP_SENDING に成功すること");
    client
        .forget_subscription(rid)
        .expect("forget に成功すること");

    client.tick(0);
    assert_eq!(
        client
            .recv_object_datagram(&datagram(ALIAS))
            .expect("破棄はセッションを閉じない"),
        TrackDataAcceptance::Discarded,
        "100ms 以内は Discarded"
    );
    client.tick(100);
    assert_eq!(
        client
            .recv_object_datagram(&datagram(ALIAS))
            .expect("未知 alias はセッションを閉じない"),
        TrackDataAcceptance::UnknownTrackAlias,
        "設定した 100ms で期限切れになること"
    );
}

/// 保持期間 0 は tombstone を実質無効化する
#[test]
fn zero_retention_disables_tombstone() {
    const ALIAS: u64 = 1103;
    let (mut client, _server, rid) = establish_subscribe_track(ALIAS);
    client.set_peer_alias_retention_ms(0);
    client
        .stop_sending(rid)
        .expect("STOP_SENDING に成功すること");
    client
        .forget_subscription(rid)
        .expect("forget に成功すること");

    // duration_ms == 0 は DeadlineTimer が即期限切れとして扱う
    assert_eq!(
        client
            .recv_object_datagram(&datagram(ALIAS))
            .expect("未知 alias はセッションを閉じない"),
        TrackDataAcceptance::UnknownTrackAlias,
        "保持期間 0 なら tick を待たずに未知 alias"
    );
}

/// 一度も subscribe していない alias は tombstone を持たないので未知 alias
#[test]
fn never_subscribed_alias_is_unknown() {
    let (mut client, _server) = establish_pair();
    assert_eq!(
        client
            .recv_object_datagram(&datagram(9999))
            .expect("未知 alias はセッションを閉じない"),
        TrackDataAcceptance::UnknownTrackAlias
    );
}

/// tombstone 状態の alias に新しい SUBSCRIBE_OK が来たら上書きして受理する
///
/// draft-ietf-moq-transport-21 §3.1.2 の DUPLICATE_TRACK_ALIAS は "with an Established
/// subscription" が条件なので、tombstone は衝突扱いにならない。
#[test]
fn tombstoned_alias_can_be_reused_by_new_subscription() {
    const ALIAS: u64 = 1104;
    let (mut client, mut server, rid1) = establish_subscribe_track(ALIAS);
    client
        .stop_sending(rid1)
        .expect("STOP_SENDING に成功すること");
    client
        .forget_subscription(rid1)
        .expect("forget に成功すること");
    assert_eq!(
        client
            .recv_object_datagram(&datagram(ALIAS))
            .expect("破棄はセッションを閉じない"),
        TrackDataAcceptance::Discarded,
        "forget 直後は tombstone が効いていること"
    );

    // 同じ alias を別 Track の新しい subscription で使う。
    // publisher (server) 側の索引には旧 subscription が残っているため送信 API は
    // §3.1.2 の 1 文目で拒否する。ここで検証したいのは受信側の条件 ("with an Established
    // subscription") なので、SUBSCRIBE_OK をメッセージ注入する。
    let rid2 = client
        .send_subscribe(ns(&[b"live"]), b"cam2".to_vec(), MessageParameters::new())
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("SUBSCRIBE の受信に成功すること");
    let ok_msg = ControlMessage::SubscribeOk(shiguredo_moqt::message::SubscribeOk {
        track_alias: ALIAS,
        parameters: MessageParameters::new(),
        track_properties: TrackProperties::new(),
    });
    client
        .recv_stream_message(rid2, ok_msg)
        .expect("tombstone 状態の alias は DUPLICATE_TRACK_ALIAS にならない");
    assert_eq!(
        client.state(),
        SessionState::Established,
        "tombstone との衝突でセッションを閉じてはいけない"
    );

    // tombstone は破棄され、新しい subscription へ解決されること
    assert_eq!(
        client
            .recv_object_datagram(&datagram(ALIAS))
            .expect("解決に成功すること"),
        TrackDataAcceptance::Accepted,
        "新しい subscription に紐づくこと"
    );
    assert!(
        client
            .subscription(rid2)
            .expect("subscription が存在する")
            .largest_received_location
            .is_some(),
        "新しい subscription の状態が更新されること"
    );
}

/// 共有 alias の 1 subscription を forget しただけでは tombstone を作らない
///
/// 残った subscription が現役なので、Object は Accepted のまま扱われなければならない。
#[test]
fn shared_alias_partial_forget_does_not_create_tombstone() {
    const ALIAS: u64 = 1105;
    let (mut client, mut server) = establish_pair();
    let mut rids = Vec::new();
    for _ in 0..2 {
        let rid = client
            .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), MessageParameters::new())
            .expect("SUBSCRIBE の送信に成功すること");
        let (_, sub_msg) = take_send_request(&mut client);
        server
            .recv_request(sub_msg)
            .expect("SUBSCRIBE の受信に成功すること");
        server
            .send_subscribe_ok(rid, ALIAS, MessageParameters::new(), TrackProperties::new())
            .expect("SUBSCRIBE_OK の送信に成功すること");
        let (_, ok_msg) = take_send_on_stream(&mut server);
        client
            .recv_stream_message(rid, ok_msg)
            .expect("SUBSCRIBE_OK の受信に成功すること");
        rids.push(rid);
    }

    client
        .stop_sending(rids[0])
        .expect("STOP_SENDING に成功すること");
    client
        .forget_subscription(rids[0])
        .expect("forget に成功すること");

    assert_eq!(
        client
            .recv_object_datagram(&datagram(ALIAS))
            .expect("解決に成功すること"),
        TrackDataAcceptance::Accepted,
        "共有相手が残っている間は tombstone を作らず Accepted のままであること"
    );
}
