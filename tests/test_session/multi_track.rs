//! 1 つの Transport Session 上で複数のトラック (SUBSCRIBE / PUBLISH / FETCH) を
//! 多重保持できることを検証する integration test。
//!
//! draft-ietf-moq-transport-21 §6.4.2.1 (Request ID) は「1 つの Transport Session 上で複数の
//! リクエストを Request ID で識別する (各 endpoint が parity 付きで採番し、複数の
//! リクエストを 1 Session 上で同時に扱える)」性質を定める。relay 採用のシナリオでは
//! 1 Session で多数のトラックを同時に扱うため、HashMap ベースの多重保持で ID 衝突・
//! キーの取り違え・上書きが起きないことを統合テストで担保する。
//!
//! `Session` は sans-I/O であり、ここでいう「多重」はマルチスレッドの並行 (race) では
//! なく「同一 Session に対する順次 API 呼び出しで複数のリクエスト/ストリームが共存する」
//! ことを指す。各シナリオは Client / Server 両方の役割で検証する。
//!
//! 検証は「多重保持で状態が壊れないこと」に絞り、公開 API へマッピングする:
//! - subscriptions 件数: `subscriptions()` の `.count()`
//! - 個別 subscription: `subscription(rid)` の pub フィールド (`track_alias` 等)
//! - publisher alias の多重: `subscriptions()` から `track_alias` を収集し distinct を確認
//! - fetches 件数: `fetches()` の `.count()`
//! - outgoing data stream の共存: `subscription(rid)` の `stream_counts.published_count`
//! - DataStreamId 一意性: 重複 ID が `PROTOCOL_VIOLATION` で拒否されること
//! - GOAWAY の Request ID: `send_goaway` が積む `ControlMessage::Goaway` の `request_id`
//!
//! Request ID の parity 一致・採番の重複なしは純ロジックであり、PBT
//! (`pbt/tests/prop_session` の `request_id_generator_parity_and_increment` /
//! `request_id_tracker_parity_and_duplicate`) が網羅する。規約「PBT でカバー
//! できるものを単体テストで書かない」に従い、本 integration
//! test では純ロジックを再検証しない (多重保持の共存検証に集中する)。
//!
//! なお FETCH レスポンスのデータプレーン (`FetchStreamEncoder`) は Session 状態機械と
//! 独立した stateless な wire エンコーダであり、Session の多重保持には関与しないため
//! 本ファイルの検証対象外とする (`src/stream/encoder.rs` 側で別途検証される)。

use super::*;

// ─── Multi-SUBSCRIBE ──────────────────────────────────────────────────────

/// 異なる Track Namespace への 3 つの SUBSCRIBE を 1 Session で多重保持する。
///
/// `subscriber` が 3 つの SUBSCRIBE を順次発行し、`publisher` が SUBSCRIBE_OK を順不同で
/// 返す。3 つの subscription が共存し、HashMap キーの取り違えなく各 track / alias に
/// 対応すること、1 つを forget しても他に影響しないことを検証する。
fn run_multi_subscribe(subscriber: &mut Session, publisher: &mut Session) {
    // 異なる Track Namespace への 3 つの SUBSCRIBE。track_alias は publisher が採番する。
    let tracks: [(TrackNamespace, Vec<u8>, u64); 3] = [
        (ns(&[b"multi-sub-a"]), b"cam-a".to_vec(), 101),
        (ns(&[b"multi-sub-b"]), b"cam-b".to_vec(), 102),
        (ns(&[b"multi-sub-c"]), b"cam-c".to_vec(), 103),
    ];

    // 3 つの SUBSCRIBE を順次発行し、publisher 側で受理する。
    let mut rids = Vec::new();
    for (namespace, name, _alias) in &tracks {
        let rid = subscriber
            .send_subscribe(namespace.clone(), name.clone(), MessageParameters::new())
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, sub_msg) = take_send_request(subscriber);
        publisher
            .recv_request(sub_msg)
            .expect("テストフィクスチャの前提条件を満たす");
        rids.push(rid);
    }

    // SUBSCRIBE_OK を順不同 (逆順) で返し、各 subscription を Established にする。
    // SUBSCRIBE への応答は SUBSCRIBE_OK であり REQUEST_OK ではない (draft-ietf-moq-transport-21 §9.7 (SUBSCRIBE_OK))。
    for i in (0..tracks.len()).rev() {
        let alias = tracks[i].2;
        publisher
            .send_subscribe_ok(
                rids[i],
                alias,
                MessageParameters::new(),
                TrackProperties::new(),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, ok_msg) = take_send_on_stream(publisher);
        subscriber
            .recv_stream_message(rids[i], ok_msg)
            .expect("テストフィクスチャの前提条件を満たす");
    }

    // 3 つの subscription が同一 Session に共存する (件数が一致すれば ID 衝突による
    // 上書きは起きていない)。
    assert_eq!(
        subscriber.subscriptions().count(),
        3,
        "3 つの subscription が共存する"
    );

    // 各 subscription が正しい track / alias に対応する (HashMap キーの取り違えがない)。
    for (i, (namespace, name, alias)) in tracks.iter().enumerate() {
        let sub = subscriber
            .subscription(rids[i])
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(
            sub.state,
            SubscriptionState::Established,
            "subscription が Established になる"
        );
        assert_eq!(
            &sub.track_namespace, namespace,
            "subscription の Track Namespace が一致する"
        );
        assert_eq!(
            sub.track_name, *name,
            "subscription の Track Name が一致する"
        );
        assert_eq!(
            sub.track_alias,
            Some(*alias),
            "subscription の Track Alias が一致する"
        );
    }

    // publisher が採番した track_alias は 3 つとも distinct。
    let mut aliases: Vec<u64> = subscriber
        .subscriptions()
        .filter_map(|s| s.track_alias)
        .collect();
    aliases.sort_unstable();
    aliases.dedup();
    assert_eq!(
        aliases.len(),
        3,
        "3 つの subscription の Track Alias は distinct"
    );

    // 1 つの subscription を終端 (bidi request stream の FIN) してから forget する。
    // forget は Terminated かつ cleanup 可能な subscription のみ除去する (draft-ietf-moq-transport-21 §3.1.1 (Subscription State Management))。
    subscriber
        .recv_request_stream_closed(rids[0], RequestStreamEnd::Fin)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        subscriber
            .subscription(rids[0])
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Terminated,
        "終端した subscription は Terminated になる"
    );
    assert!(
        subscriber.forget_subscription(rids[0]).is_some(),
        "終端済み subscription を forget できる"
    );

    // 1 つ削除しても残り 2 つは影響を受けない。
    assert_eq!(
        subscriber.subscriptions().count(),
        2,
        "forget 後は 2 つの subscription が残る"
    );
    assert!(
        subscriber.subscription(rids[0]).is_none(),
        "forget した subscription は参照できない"
    );
    for i in 1..tracks.len() {
        let sub = subscriber
            .subscription(rids[i])
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(
            sub.state,
            SubscriptionState::Established,
            "残った subscription は Established のまま"
        );
        assert_eq!(
            sub.track_alias,
            Some(tracks[i].2),
            "残った subscription の Track Alias は不変"
        );
    }
}

/// Client が subscriber となる Multi-SUBSCRIBE シナリオ
#[test]
fn multi_subscribe_holds_distinct_tracks_client_role() {
    let (mut client, mut server) = establish_pair();
    run_multi_subscribe(&mut client, &mut server);
}

/// Server が subscriber となる Multi-SUBSCRIBE シナリオ
#[test]
fn multi_subscribe_holds_distinct_tracks_server_role() {
    let (mut client, mut server) = establish_pair();
    run_multi_subscribe(&mut server, &mut client);
}

// ─── Multi-PUBLISH ────────────────────────────────────────────────────────

/// 異なる Track Alias の 3 つの PUBLISH を 1 Session で多重保持する。
///
/// `publisher` が 3 つの PUBLISH を順次発行し、`subscriber` が REQUEST_OK で応答する
/// (draft-ietf-moq-transport-21 では PUBLISH_OK は REQUEST_OK に統合)。track_alias が distinct であること、
/// 各 publish の outgoing subgroup stream が独立してカウントされること、重複 DataStreamId
/// が拒否されることを検証する。
fn run_multi_publish(publisher: &mut Session, subscriber: &mut Session) {
    // 異なる Track Namespace / Track Alias の 3 つの PUBLISH。track_alias は publisher が採番する。
    let tracks: [(TrackNamespace, Vec<u8>, u64); 3] = [
        (ns(&[b"multi-pub-a"]), b"trk-a".to_vec(), 201),
        (ns(&[b"multi-pub-b"]), b"trk-b".to_vec(), 202),
        (ns(&[b"multi-pub-c"]), b"trk-c".to_vec(), 203),
    ];

    // 3 つの PUBLISH を順次発行し、subscriber 側で受理する。
    let mut rids = Vec::new();
    for (namespace, name, alias) in &tracks {
        let rid = publisher
            .send_publish(
                namespace.clone(),
                name.clone(),
                *alias,
                MessageParameters::new(),
                TrackProperties::new(),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, pub_msg) = take_send_request(publisher);
        subscriber
            .recv_request(pub_msg)
            .expect("テストフィクスチャの前提条件を満たす");
        rids.push(rid);
    }

    // draft-ietf-moq-transport-21 では PUBLISH の成功応答は REQUEST_OK に統合されている (旧 PUBLISH_OK, draft-ietf-moq-transport-21 §9.3 (REQUEST_OK))。
    for &rid in &rids {
        subscriber
            .send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, ok_msg) = take_send_on_stream(subscriber);
        publisher
            .recv_stream_message(rid, ok_msg)
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(
            publisher
                .subscription(rid)
                .expect("テストフィクスチャの前提条件を満たす")
                .state,
            SubscriptionState::Established,
            "PUBLISH が REQUEST_OK で Established になる"
        );
    }

    // publisher 側 subscription の track_alias は 3 つとも distinct。
    let mut aliases: Vec<u64> = publisher
        .subscriptions()
        .filter_map(|s| s.track_alias)
        .collect();
    aliases.sort_unstable();
    aliases.dedup();
    assert_eq!(
        aliases.len(),
        3,
        "3 つの PUBLISH の Track Alias は distinct"
    );

    // 各 publish に subgroup header / object を発行する。
    // DataStreamId は caller (テスト) が一意に採番する。
    for (i, &rid) in rids.iter().enumerate() {
        let stream_id = DataStreamId(300 + i as u64);
        let header = SubgroupHeader {
            track_alias: tracks[i].2,
            group_id: 1,
            subgroup_id: SubgroupIdMode::Explicit(0),
            publisher_priority: Some(1),
            has_properties: false,
            end_of_group: false,
            first_object: false,
        };
        publisher
            .send_subgroup_header(stream_id, rid, &header)
            .expect("テストフィクスチャの前提条件を満たす");
        publisher
            .send_subgroup_object(stream_id, 0, None)
            .expect("テストフィクスチャの前提条件を満たす");
    }
    // 各 request_id の outgoing subgroup stream は独立してカウントされ、互いに干渉しない。
    for &rid in &rids {
        assert_eq!(
            publisher
                .subscription(rid)
                .expect("テストフィクスチャの前提条件を満たす")
                .stream_counts
                .published_count,
            1,
            "各 PUBLISH の open subgroup stream は 1 本"
        );
    }

    // 既に登録済みの DataStreamId を別 request_id で再利用すると PROTOCOL_VIOLATION。
    // header の track_alias は rids[1] と整合しているが、DataStreamId 重複検出が role /
    // alias 検証より先に行われるため、alias の正否に関係なく弾かれる (DataStreamId 一意性)。
    let dup_header = SubgroupHeader {
        track_alias: tracks[1].2,
        group_id: 2,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    let err = publisher
        .send_subgroup_header(DataStreamId(300), rids[1], &dup_header)
        .unwrap_err();
    assert_eq!(
        err.code, SESSION_PROTOCOL_VIOLATION,
        "重複 DataStreamId は PROTOCOL_VIOLATION で拒否される"
    );
}

/// Client が publisher となる Multi-PUBLISH シナリオ
#[test]
fn multi_publish_holds_distinct_tracks_client_role() {
    let (mut client, mut server) = establish_pair();
    run_multi_publish(&mut client, &mut server);
}

/// Server が publisher となる Multi-PUBLISH シナリオ
#[test]
fn multi_publish_holds_distinct_tracks_server_role() {
    let (mut client, mut server) = establish_pair();
    run_multi_publish(&mut server, &mut client);
}

// ─── Multi-FETCH ──────────────────────────────────────────────────────────

/// 3 つの FETCH を 1 Session で多重保持する。
/// (draft-ietf-moq-transport-21 で FETCH は単一形式になった)
///
/// `fetcher` が SUBSCRIBE を先に Established にしてから
/// 3 つの FETCH を発行し、`responder` が FETCH_OK で応答する。subscribe と fetch が同一
/// Session に共存し、3 つの fetch が独立して保持されることを検証する。
fn run_multi_fetch(fetcher: &mut Session, responder: &mut Session) {
    use shiguredo_moqt::message::common::Location;

    // まず共存確認用の SUBSCRIBE を Established にする。
    let sub_rid = fetcher
        .send_subscribe(
            ns(&[b"multi-fetch-sub"]),
            b"cam".to_vec(),
            MessageParameters::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, sub_msg) = take_send_request(fetcher);
    responder
        .recv_request(sub_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    responder
        .send_subscribe_ok(
            sub_rid,
            301,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");

    let sub_stream = DataStreamId(1);
    responder
        .send_subgroup_header(
            sub_stream,
            sub_rid,
            &SubgroupHeader {
                track_alias: 301,
                group_id: 0,
                subgroup_id: SubgroupIdMode::Explicit(0),
                publisher_priority: Some(128),
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("テストフィクスチャの前提条件を満たす");
    responder
        .send_subgroup_object(sub_stream, 0, None)
        .expect("テストフィクスチャの前提条件を満たす");
    let (_, ok_msg) = take_send_on_stream(responder);
    fetcher
        .recv_stream_message(sub_rid, ok_msg)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        fetcher
            .subscription(sub_rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Established,
        "参照元 SUBSCRIBE が Established になる"
    );

    // 3 つの FETCH を発行する。
    let mut fetch_rids = Vec::new();
    for namespace in [
        ns(&[b"multi-fetch-a"]),
        ns(&[b"multi-fetch-b"]),
        ns(&[b"multi-fetch-c"]),
    ] {
        let rid = fetcher
            .send_fetch(
                namespace,
                b"cam".to_vec(),
                fetch_range_params(
                    Location {
                        group_id: 0,
                        object_id: 0,
                    },
                    Location {
                        group_id: 5,
                        object_id: 0,
                    },
                ),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        fetch_rids.push(rid);
    }
    // 3 つの FETCH に対して FETCH_OK を返し、各 fetch を Established にする。
    for &rid in &fetch_rids {
        let (_, fetch_msg) = take_send_request(fetcher);
        responder
            .recv_request(fetch_msg)
            .expect("テストフィクスチャの前提条件を満たす");
        responder
            .send_fetch_ok(
                rid,
                0,
                Location {
                    group_id: 5,
                    object_id: 0,
                },
                MessageParameters::new(),
                TrackProperties::new(),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, ok_msg) = take_send_on_stream(responder);
        fetcher
            .recv_stream_message(rid, ok_msg)
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(
            fetcher
                .fetch(rid)
                .expect("テストフィクスチャの前提条件を満たす")
                .state,
            FetchState::Established,
            "FETCH が FETCH_OK で Established になる"
        );
    }

    // 3 つの fetch が同一 Session に共存する。
    assert_eq!(fetcher.fetches().count(), 3, "3 つの fetch が共存する");
    // subscribe (subscription) と fetch (fetch) は別のマップで管理され、相互に干渉しない。
    assert_eq!(
        fetcher.subscriptions().count(),
        1,
        "参照元 subscribe は subscription として 1 件保持される"
    );
}

/// Client が fetcher となる Multi-FETCH シナリオ
#[test]
fn multi_fetch_holds_distinct_requests_client_role() {
    let (mut client, mut server) = establish_pair();
    run_multi_fetch(&mut client, &mut server);
}

/// Server が fetcher となる Multi-FETCH シナリオ
#[test]
fn multi_fetch_holds_distinct_requests_server_role() {
    let (mut client, mut server) = establish_pair();
    run_multi_fetch(&mut server, &mut client);
}

// ─── Mixed (複数 PUB + 複数 SUB + FETCH 同時) ─────────────────────────────

/// 1 Session 内で 2 SUBSCRIBE + 2 PUBLISH + 1 FETCH を共存させる。
///
/// `local` が 5 つのリクエストを自ら発行して共存させ、各リクエストのデータストリームが
/// 互いに干渉しないことを検証する。あわせて peer から SUBSCRIBE を受けた後に
/// draft-ietf-moq-transport-21 §9.2 (GOAWAY) どおり Request ID 無しの GOAWAY を送信できることを確認する。
fn run_mixed(local: &mut Session, peer: &mut Session, _local_is_client: bool) {
    use shiguredo_moqt::message::common::Location;

    // --- 2 SUBSCRIBE (local が subscriber) ---
    let sub_tracks = [
        (ns(&[b"mixed-sub-a"]), b"s-a".to_vec(), 401u64),
        (ns(&[b"mixed-sub-b"]), b"s-b".to_vec(), 402u64),
    ];
    let mut sub_rids = Vec::new();
    for (namespace, name, alias) in &sub_tracks {
        let rid = local
            .send_subscribe(namespace.clone(), name.clone(), MessageParameters::new())
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, msg) = take_send_request(local);
        peer.recv_request(msg)
            .expect("テストフィクスチャの前提条件を満たす");
        peer.send_subscribe_ok(
            rid,
            *alias,
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
        let (_, ok) = take_send_on_stream(peer);
        local
            .recv_stream_message(rid, ok)
            .expect("テストフィクスチャの前提条件を満たす");
        sub_rids.push(rid);
    }

    // --- 2 PUBLISH (local が publisher) ---
    let pub_tracks = [
        (ns(&[b"mixed-pub-a"]), b"p-a".to_vec(), 411u64),
        (ns(&[b"mixed-pub-b"]), b"p-b".to_vec(), 412u64),
    ];
    let mut pub_rids = Vec::new();
    for (namespace, name, alias) in &pub_tracks {
        let rid = local
            .send_publish(
                namespace.clone(),
                name.clone(),
                *alias,
                MessageParameters::new(),
                TrackProperties::new(),
            )
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, msg) = take_send_request(local);
        peer.recv_request(msg)
            .expect("テストフィクスチャの前提条件を満たす");
        peer.send_request_ok(rid, MessageParameters::new(), TrackProperties::default())
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, ok) = take_send_on_stream(peer);
        local
            .recv_stream_message(rid, ok)
            .expect("テストフィクスチャの前提条件を満たす");
        pub_rids.push(rid);
    }

    // --- 1 FETCH (local が subscriber) ---
    let fetch_rid = local
        .send_fetch(
            ns(&[b"mixed-fetch"]),
            b"f".to_vec(),
            fetch_range_params(
                Location {
                    group_id: 0,
                    object_id: 0,
                },
                Location {
                    group_id: 5,
                    object_id: 0,
                },
            ),
        )
        .expect("テストフィクスチャの前提条件を満たす");
    {
        let (_, msg) = take_send_request(local);
        peer.recv_request(msg)
            .expect("テストフィクスチャの前提条件を満たす");
        peer.send_fetch_ok(
            fetch_rid,
            0,
            Location {
                group_id: 5,
                object_id: 0,
            },
            MessageParameters::new(),
            TrackProperties::new(),
        )
        .expect("テストフィクスチャの前提条件を満たす");
        let (_, ok) = take_send_on_stream(peer);
        local
            .recv_stream_message(fetch_rid, ok)
            .expect("テストフィクスチャの前提条件を満たす");
    }

    // 共存確認: subscriptions() は subscribe (subscriber 役) と publish (publisher 役) の
    // 両方を含むため 4 件、fetches() は 1 件。件数が一致すれば自側採番の衝突による上書きは
    // 起きていない。
    assert_eq!(
        local.subscriptions().count(),
        4,
        "2 SUBSCRIBE + 2 PUBLISH が共存する"
    );
    assert_eq!(local.fetches().count(), 1, "1 FETCH が共存する");

    // データストリーム独立性: 一方の PUBLISH に subgroup stream を開いても他へ干渉しない。
    let header = SubgroupHeader {
        track_alias: pub_tracks[0].2,
        group_id: 1,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    local
        .send_subgroup_header(DataStreamId(500), pub_rids[0], &header)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        local
            .subscription(pub_rids[0])
            .expect("テストフィクスチャの前提条件を満たす")
            .stream_counts
            .published_count,
        1,
        "stream を開いた PUBLISH のカウントは 1"
    );
    assert_eq!(
        local
            .subscription(pub_rids[1])
            .expect("テストフィクスチャの前提条件を満たす")
            .stream_counts
            .published_count,
        0,
        "別の PUBLISH には干渉しない"
    );
    assert_eq!(
        local
            .subscription(sub_rids[0])
            .expect("テストフィクスチャの前提条件を満たす")
            .stream_counts
            .published_count,
        0,
        "subscriber 役の subscription は outgoing subgroup を持たない"
    );

    // --- GOAWAY: Request ID 無しで送信できる (draft-ietf-moq-transport-21 §9.2 (GOAWAY) / Appendix A.2 #1623) ---
    // peer から 2 つの SUBSCRIBE を local が受信し、その後 GOAWAY を control stream で発行する。
    // Client は new_session_uri を空にする必要がある。
    // timeout=0 は specific timeout なし (draft-ietf-moq-transport-21 §9.2 (GOAWAY)) で deadline を設定しない。
    for namespace in [ns(&[b"mixed-peer-x"]), ns(&[b"mixed-peer-y"])] {
        let _peer_rid = peer
            .send_subscribe(namespace, b"peer-trk".to_vec(), MessageParameters::new())
            .expect("テストフィクスチャの前提条件を満たす");
        let (_, msg) = take_send_request(peer);
        local
            .recv_request(msg)
            .expect("テストフィクスチャの前提条件を満たす");
    }

    local
        .send_goaway(Vec::new(), 0)
        .expect("テストフィクスチャの前提条件を満たす");
    let goaway = take_goaway(local);
    assert!(
        goaway.new_session_uri.is_empty(),
        "本テストは空 URI の GOAWAY を送る"
    );
    assert_eq!(goaway.timeout, 0);
}

/// local が Client となる Mixed シナリオ
#[test]
fn mixed_requests_coexist_client_role() {
    let (mut client, mut server) = establish_pair();
    run_mixed(&mut client, &mut server, true);
}

/// local が Server となる Mixed シナリオ
#[test]
fn mixed_requests_coexist_server_role() {
    let (mut client, mut server) = establish_pair();
    run_mixed(&mut server, &mut client, false);
}
