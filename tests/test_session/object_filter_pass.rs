//! Pass 評価 (Forward AND Location Filters AND Range Filters) のテスト
//!
//! draft-ietf-moq-transport-21 §3.3.3 (Combining Filters):
//! "The publisher MUST forward only objects that pass all filters.
//!  Pass = Forward AND Location Filters AND Range Filters"
//!
//! フィルタは subscriber が SUBSCRIBE / PUBLISH_OK / REQUEST_UPDATE で指定し、publisher 側の
//! subscription 状態に保持される。したがって本テストは client(subscriber) → server(publisher)
//! の SUBSCRIBE で条件を作り、server の送信 API が拒否することを断言する。
//!
//! 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。

use super::*;
use shiguredo_moqt::message_parameter::{
    LocationFilter, PARAM_FORWARD, PARAM_LOCATION_FILTER, PARAM_OBJECT_PROPERTY_FILTER,
    PARAM_OBJECTID_FILTER, PARAM_PRIORITY_FILTER, PARAM_SUBGROUP_FILTER,
};
use shiguredo_moqt::session::types::SendRequestError;

/// peer が MAX_FILTER_RANGES を宣言した状態で SUBSCRIBE を確立し (client, server, rid) を返す
///
/// Range Filter を送るには peer (server) 側の宣言が必要 (§9.1.6 (MAX FILTER RANGES))。
fn establish_with_filters(sub_params: MessageParameters) -> (Session, Session, u64) {
    let (mut client, mut server) = establish_pair_with_options(SetupOptions::new(), {
        let mut opts = SetupOptions::new();
        opts.push(SetupOption {
            option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_FILTER_RANGES, // MAX_FILTER_RANGES
            value: SetupOptionValue::VarInt(8),
        });
        opts
    });
    let rid = client
        .send_subscribe(ns(&[b"live"]), b"cam".to_vec(), sub_params)
        .expect("SUBSCRIBE の送信に成功すること");
    let (_, sub_msg) = take_send_request(&mut client);
    server
        .recv_request(sub_msg)
        .expect("SUBSCRIBE の受信に成功すること");
    server
        .send_subscribe_ok(rid, 1, MessageParameters::new(), TrackProperties::new())
        .expect("SUBSCRIBE_OK の送信に成功すること");
    let (_, ok_msg) = take_send_on_stream(&mut server);
    client
        .recv_stream_message(rid, ok_msg)
        .expect("SUBSCRIBE_OK の受信に成功すること");
    (client, server, rid)
}

/// publisher (server) 側で subgroup stream を開く
fn open_subgroup(
    server: &mut Session,
    rid: u64,
    stream_id: DataStreamId,
    group_id: u64,
    subgroup_id: u64,
    publisher_priority: Option<u8>,
) {
    server
        .send_subgroup_header(
            stream_id,
            rid,
            &SubgroupHeader {
                track_alias: 1,
                group_id,
                subgroup_id: SubgroupIdMode::Explicit(subgroup_id),
                publisher_priority,
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("subgroup header の送信に成功すること");
}

/// Property Type 付きの Range Filter パラメータを作る (0x28 / 0x29 用)
fn property_range_filter(
    param_type: u64,
    set_id: u8,
    property_type: u64,
    start: u64,
    end: u64,
) -> MessageParameter {
    let mut bytes = vec![set_id];
    shiguredo_moqt::varint::encode(property_type, &mut bytes);
    shiguredo_moqt::varint::encode(start, &mut bytes);
    shiguredo_moqt::varint::encode(end - start, &mut bytes);
    MessageParameter {
        param_type,
        value: MessageParameterValue::LengthPrefixed(bytes),
    }
}

/// 拒否が Pass 評価由来であることを断言する
fn assert_filter_mismatch(err: SendRequestError, label: &str) {
    assert!(
        matches!(err, SendRequestError::LocalFilterMismatch),
        "{label}: フィルタ不一致専用のエラーであること"
    );
}

// ─── Forward ────────────────────────────────────────────────────

/// forward_state == 0 では subgroup object を送れない
///
/// draft-ietf-moq-transport-21 §3.1 (Subscriptions):
/// "The publisher does not send Objects if the Forward State is 0"
#[test]
fn forward_zero_rejects_subgroup_object() {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    let (_client, mut server, rid) = establish_with_filters(params);
    let stream_id = DataStreamId(60);
    open_subgroup(&mut server, rid, stream_id, 3, 1, Some(10));

    let err = server
        .send_subgroup_object(stream_id, 0, None)
        .expect_err("forward=0 では送信できない");
    assert_filter_mismatch(err, "forward=0 の subgroup 経路");
}

/// forward_state == 0 では datagram も送れない
#[test]
fn forward_zero_rejects_datagram() {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    let (_client, mut server, rid) = establish_with_filters(params);

    let err = server
        .send_object_datagram(rid, 3, 0, None, None)
        .expect_err("forward=0 では送信できない");
    assert_filter_mismatch(err, "forward=0 の datagram 経路");
}

/// 制御メッセージ (PUBLISH_DONE) は Forward State に影響されない
///
/// draft-ietf-moq-transport-21 §3.1 (Subscriptions): "Control messages, such as PUBLISH_DONE
/// (Section 9.9) are sent regardless of the forward state"
#[test]
fn forward_zero_still_allows_publish_done() {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    let (_client, mut server, rid) = establish_with_filters(params);

    server
        .send_publish_done(
            rid,
            0x2, // PUBLISH_DONE Code: Track ended (draft-ietf-moq-transport-21 §16.11.3) の値
            0,
            shiguredo_moqt::message::ReasonPhrase::new("done").expect("有効な reason"),
        )
        .expect("PUBLISH_DONE は forward state に関係なく送れる");
}

// ─── Location Filter ────────────────────────────────────────────

/// AbsoluteStart の Start Location 未満は拒否される
#[test]
fn location_filter_before_start_is_rejected() {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteStart {
                start: Location {
                    group_id: 5,
                    object_id: 0,
                },
            }
            .encode_to_bytes(),
        ),
    });
    let (_client, mut server, rid) = establish_with_filters(params);
    let stream_id = DataStreamId(61);
    open_subgroup(&mut server, rid, stream_id, 4, 0, Some(10));

    let err = server
        .send_subgroup_object(stream_id, 0, None)
        .expect_err("Start Location 未満は拒否される");
    assert_filter_mismatch(err, "Location Filter の Start 未満");

    // Start 以上なら通る
    let stream_id = DataStreamId(62);
    open_subgroup(&mut server, rid, stream_id, 5, 0, Some(10));
    server
        .send_subgroup_object(stream_id, 0, None)
        .expect("Start Location 以上なら送れる");
}

/// AbsoluteRange の End Group より後は拒否される
///
/// draft-ietf-moq-transport-21 §3.3.1 (Location Filters): End Group は Group 単位で inclusive。
#[test]
fn location_filter_after_end_group_is_rejected() {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        // End Group = start.group_id + end_group_delta = 2 + 1 = 3
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteRange {
                start: Location {
                    group_id: 2,
                    object_id: 0,
                },
                end_group_delta: 1,
            }
            .encode_to_bytes(),
        ),
    });
    let (_client, mut server, rid) = establish_with_filters(params);

    // End Group ちょうど (3) は通る
    let stream_id = DataStreamId(63);
    open_subgroup(&mut server, rid, stream_id, 3, 0, Some(10));
    server
        .send_subgroup_object(stream_id, 0, None)
        .expect("End Group ちょうどは inclusive で通る");

    // End Group を超える (4) は拒否
    let stream_id = DataStreamId(64);
    open_subgroup(&mut server, rid, stream_id, 4, 0, Some(10));
    let err = server
        .send_subgroup_object(stream_id, 0, None)
        .expect_err("End Group を超えたら拒否される");
    assert_filter_mismatch(err, "Location Filter の End Group 超過");
}

/// End Group 超過の拒否後も subscription は Established のままである
///
/// draft-ietf-moq-transport-21 §3.3.1 / §9.9 (PUBLISH_DONE): Largest Object が
/// End Group を超えても subscription は終了しない。PUBLISH_DONE status
/// SUBSCRIPTION_ENDED (0x3) は削除されたため、End Group 超過だけでは終了しない。
#[test]
fn location_filter_end_exceeded_keeps_subscription_established() {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        // End Group = start.group_id + end_group_delta = 2 + 1 = 3
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteRange {
                start: Location {
                    group_id: 2,
                    object_id: 0,
                },
                end_group_delta: 1,
            }
            .encode_to_bytes(),
        ),
    });
    let (_client, mut server, rid) = establish_with_filters(params);

    // End Group を超える (4) Object は拒否される
    let stream_id = DataStreamId(65);
    open_subgroup(&mut server, rid, stream_id, 4, 0, Some(10));
    let err = server
        .send_subgroup_object(stream_id, 0, None)
        .expect_err("End Group を超えたら拒否される");
    assert_filter_mismatch(err, "Location Filter の End Group 超過");

    // 拒否後も subscription は Established のままである
    let sub = server
        .subscription(rid)
        .expect("subscription が残っていること");
    assert_eq!(
        sub.state,
        SubscriptionState::Established,
        "End Group 超過だけでは subscription は終了しない"
    );
}

/// AbsoluteRangeWithEnd の EndObject より後は拒否される
///
/// draft-ietf-moq-transport-21 §3.3.1 (Location Filters): 4 フィールドの End は
/// {StartGroup + EndGroupDelta, EndObject} で inclusive。同一 End Group 内でも
/// EndObject を超えた Object は通らない。
#[test]
fn location_filter_after_end_object_is_rejected() {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        // Start = {2, 0}、End = {2 + 0, 5} = {2, 5}
        value: MessageParameterValue::LengthPrefixed(
            LocationFilter::AbsoluteRangeWithEnd {
                start: Location {
                    group_id: 2,
                    object_id: 0,
                },
                end_group_delta: 0,
                end_object: 5,
            }
            .encode_to_bytes(),
        ),
    });
    let (_client, mut server, rid) = establish_with_filters(params);

    // EndObject ちょうど (group 2, object 5) は通る
    let stream_id = DataStreamId(66);
    open_subgroup(&mut server, rid, stream_id, 2, 0, Some(10));
    server
        .send_subgroup_object(stream_id, 5, None)
        .expect("EndObject ちょうどは inclusive で通る");

    // 同一 stream で EndObject を超える (group 2, object 6) は拒否
    let err = server
        .send_subgroup_object(stream_id, 6, None)
        .expect_err("EndObject を超えたら拒否される");
    assert_filter_mismatch(err, "Location Filter の EndObject 超過");
}

// ─── Range Filter ───────────────────────────────────────────────

/// SUBGROUP_FILTER の範囲外は拒否される
#[test]
fn subgroup_filter_out_of_range_is_rejected() {
    let mut params = MessageParameters::new();
    params.push(range_filter(PARAM_SUBGROUP_FILTER, 0, 10, 20));
    let (_client, mut server, rid) = establish_with_filters(params);

    // 範囲内 (15)
    let stream_id = DataStreamId(65);
    open_subgroup(&mut server, rid, stream_id, 0, 15, Some(10));
    server
        .send_subgroup_object(stream_id, 0, None)
        .expect("範囲内の subgroup は送れる");

    // 範囲外 (21)
    let stream_id = DataStreamId(66);
    open_subgroup(&mut server, rid, stream_id, 0, 21, Some(10));
    let err = server
        .send_subgroup_object(stream_id, 0, None)
        .expect_err("範囲外の subgroup は拒否される");
    assert_filter_mismatch(err, "SUBGROUP_FILTER の範囲外");
}

/// OBJECTID_FILTER の範囲外は拒否される
#[test]
fn objectid_filter_out_of_range_is_rejected() {
    let mut params = MessageParameters::new();
    params.push(range_filter(PARAM_OBJECTID_FILTER, 0, 5, 7));
    let (_client, mut server, rid) = establish_with_filters(params);
    let stream_id = DataStreamId(67);
    open_subgroup(&mut server, rid, stream_id, 0, 0, Some(10));

    server
        .send_subgroup_object(stream_id, 6, None)
        .expect("範囲内の object id は送れる");
    let err = server
        .send_subgroup_object(stream_id, 8, None)
        .expect_err("範囲外の object id は拒否される");
    assert_filter_mismatch(err, "OBJECTID_FILTER の範囲外");
}

/// PRIORITY_FILTER の範囲外は拒否される
#[test]
fn priority_filter_out_of_range_is_rejected() {
    let mut params = MessageParameters::new();
    params.push(range_filter(PARAM_PRIORITY_FILTER, 0, 100, 110));
    let (_client, mut server, rid) = establish_with_filters(params);

    // header が priority=105 を明示 → 範囲内
    let stream_id = DataStreamId(68);
    open_subgroup(&mut server, rid, stream_id, 0, 0, Some(105));
    server
        .send_subgroup_object(stream_id, 0, None)
        .expect("範囲内の priority は送れる");

    // header が priority=200 → 範囲外
    let stream_id = DataStreamId(69);
    open_subgroup(&mut server, rid, stream_id, 0, 1, Some(200));
    let err = server
        .send_subgroup_object(stream_id, 0, None)
        .expect_err("範囲外の priority は拒否される");
    assert_filter_mismatch(err, "PRIORITY_FILTER の範囲外");
}

/// OBJECT_PROPERTY_FILTER は Object Properties の値で判定する
#[test]
fn object_property_filter_uses_properties() {
    let mut params = MessageParameters::new();
    // PROP_PRIOR_OBJECT_ID_GAP (0x3E) の値が 1..=3 のとき通す
    params.push(property_range_filter(
        PARAM_OBJECT_PROPERTY_FILTER,
        0,
        0x3E,
        1,
        3,
    ));
    let (_client, mut server, rid) = establish_with_filters(params);

    let in_range = {
        let mut props = ObjectProperties::new();
        props.push(ObjectProperty {
            prop_type: PROP_PRIOR_OBJECT_ID_GAP,
            value: ObjectPropertyValue::VarInt(2),
        });
        let mut inner = Vec::new();
        props.encode(&mut inner).expect("encode に成功すること");
        inner
    };
    let out_of_range = {
        let mut props = ObjectProperties::new();
        props.push(ObjectProperty {
            prop_type: PROP_PRIOR_OBJECT_ID_GAP,
            value: ObjectPropertyValue::VarInt(9),
        });
        let mut inner = Vec::new();
        props.encode(&mut inner).expect("encode に成功すること");
        inner
    };

    // datagram 経路 (properties を直接渡せる)
    server
        .send_object_datagram(rid, 0, 0, Some(in_range.clone()), None)
        .expect("範囲内の Object Property は送れる");
    let err = server
        .send_object_datagram(rid, 0, 1, Some(out_of_range.clone()), None)
        .expect_err("範囲外の Object Property は拒否される");
    assert_filter_mismatch(err, "OBJECT_PROPERTY_FILTER (datagram) の範囲外");

    // Object Properties が付いていない Object も条件を満たせないので拒否される
    let err = server
        .send_object_datagram(rid, 0, 2, None, None)
        .expect_err("Property が無いと OBJECT_PROPERTY_FILTER を満たせない");
    assert_filter_mismatch(err, "OBJECT_PROPERTY_FILTER (property なし)");

    // subgroup 経路 (properties_bytes 引数で渡す)
    let stream_id = DataStreamId(70);
    open_subgroup(&mut server, rid, stream_id, 0, 0, Some(10));
    server
        .send_subgroup_object(stream_id, 3, Some(&in_range))
        .expect("範囲内の Object Property は送れる");
    let err = server
        .send_subgroup_object(stream_id, 4, Some(&out_of_range))
        .expect_err("範囲外の Object Property は拒否される");
    assert_filter_mismatch(err, "OBJECT_PROPERTY_FILTER (subgroup) の範囲外");
}

/// 同一 SetID の複数フィルタは AND、SetID 間は OR で結合される
///
/// draft-ietf-moq-transport-21 §3.3.2 (Range Filters): "Filter parameters with the same
/// SetID are AND'd; distinct SetIDs are OR'd."
#[test]
fn range_filters_and_within_set_or_across_sets() {
    let mut params = MessageParameters::new();
    // SetID=0: subgroup 0..=0 AND object id 0..=1
    params.push(range_filter(PARAM_SUBGROUP_FILTER, 0, 0, 0));
    params.push(range_filter(PARAM_OBJECTID_FILTER, 0, 0, 1));
    // SetID=1: subgroup 5..=5 AND object id 100..=101
    params.push(range_filter(PARAM_SUBGROUP_FILTER, 1, 5, 5));
    params.push(range_filter(PARAM_OBJECTID_FILTER, 1, 100, 101));
    let (_client, mut server, rid) = establish_with_filters(params);

    // SetID=0 を満たす (subgroup=0, object=1)
    let stream0 = DataStreamId(71);
    open_subgroup(&mut server, rid, stream0, 0, 0, Some(10));
    server
        .send_subgroup_object(stream0, 1, None)
        .expect("SetID=0 の AND を満たすので通る");

    // SetID=1 を満たす (subgroup=5, object=100)
    let stream1 = DataStreamId(72);
    open_subgroup(&mut server, rid, stream1, 0, 5, Some(10));
    server
        .send_subgroup_object(stream1, 100, None)
        .expect("SetID=1 の AND を満たすので通る");

    // どちらの SetID も満たさない組み合わせ (subgroup=0, object=100)
    // SetID=0 は object id が外れ、SetID=1 は subgroup が外れる。
    // (track_alias, group_id, subgroup_id) の同時 open 制約を避けるため group を変える。
    let stream2 = DataStreamId(73);
    open_subgroup(&mut server, rid, stream2, 1, 0, Some(10));
    let err = server
        .send_subgroup_object(stream2, 100, None)
        .expect_err("どの SetID の AND も満たさないので拒否される");
    assert_filter_mismatch(err, "SetID 間 OR の不一致");
}

/// datagram には Subgroup ID フィールドが無いので SUBGROUP_FILTER は評価されない
///
/// draft-ietf-moq-transport-21 §11.2.1 (Object Datagram) のワイヤ構造に Subgroup ID は無く、
/// §3.3.2 が対象とする "other Object header fields (Subgroup ID, ...)" が存在しない。
#[test]
fn subgroup_filter_does_not_apply_to_datagram() {
    let mut params = MessageParameters::new();
    params.push(range_filter(PARAM_SUBGROUP_FILTER, 0, 10, 20));
    let (_client, mut server, rid) = establish_with_filters(params);

    server
        .send_object_datagram(rid, 0, 0, None, None)
        .expect("datagram に SUBGROUP_FILTER は適用されない");
}

// ─── フィルタなし ───────────────────────────────────────────────

/// フィルタなし (unfiltered) では従来どおり送れる
#[test]
fn unfiltered_subscription_sends_everything() {
    let (_client, mut server, rid) = establish_with_filters(MessageParameters::new());

    let stream_id = DataStreamId(74);
    open_subgroup(&mut server, rid, stream_id, 999, 999, Some(200));
    server
        .send_subgroup_object(stream_id, 999, None)
        .expect("フィルタなしなら任意の object を送れる");
    server
        .send_object_datagram(rid, 12345, 678, None, None)
        .expect("フィルタなしなら任意の datagram を送れる");
}

/// 拒否はローカルな判断でありセッションを閉じない
#[test]
fn filter_mismatch_does_not_close_session() {
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_FORWARD,
        value: MessageParameterValue::Uint8(0),
    });
    let (_client, mut server, rid) = establish_with_filters(params);

    let _ = server
        .send_object_datagram(rid, 3, 0, None, None)
        .expect_err("forward=0 では送信できない");
    assert_eq!(
        server.state(),
        SessionState::Established,
        "送信拒否でセッションが閉じてはいけない"
    );
    while let Some(e) = server.poll_event() {
        assert!(
            !matches!(e, SessionEvent::CloseSession(_)),
            "CloseSession を発行してはいけない: {e:?}"
        );
    }
}

/// FirstObjectId モードの stream で SUBGROUP_FILTER に通らない最初のオブジェクトが拒否
/// され、内部状態を汚染しないこと
///
/// draft-ietf-moq-transport-21 §3.3.3 (Combining Filters): "The publisher MUST forward only
/// objects that pass all filters." filter 評価が FirstObjectId 解決 (`my_subgroups.open` /
/// `stream.subgroup_id` 代入) より後に実行されると、拒否されたオブジェクトの ID で
/// 解決され、publisher 側の subgroup id (拒否された ID) と subscriber 側の認識 (最初に
/// 受信したオブジェクトの ID) が食い違う。filter 拒否時は内部状態を一切変更せず、
/// 通過した最初のオブジェクトの ID で解決されることを検証する
/// (draft-ietf-moq-transport-21 §11.3.1 の SUBGROUP_ID_MODE 0b01: "the Subgroup ID is the
/// Object ID of the first Object transmitted in this Subgroup")。
#[test]
fn first_object_id_subgroup_out_of_filter_rejected_without_state_change() {
    let mut sub_params = MessageParameters::new();
    sub_params.push(range_filter(PARAM_SUBGROUP_FILTER, 0, 10, 20));
    let (_client, mut server, rid) = establish_with_filters(sub_params);
    let stream_id = DataStreamId(170);
    server
        .send_subgroup_header(
            stream_id,
            rid,
            &SubgroupHeader {
                track_alias: 1,
                group_id: 0,
                subgroup_id: SubgroupIdMode::FirstObjectId,
                publisher_priority: None,
                has_properties: false,
                end_of_group: false,
                first_object: false,
            },
        )
        .expect("subgroup header の送信に成功すること");
    // SUBGROUP_FILTER (subgroup 10-20) の範囲外オブジェクト (id=5) は拒否される
    let err = server
        .send_subgroup_object(stream_id, 5, None)
        .expect_err("SUBGROUP_FILTER の範囲外オブジェクトは拒否されること");
    assert!(
        matches!(err, SendRequestError::LocalFilterMismatch),
        "FirstObjectId モードでもフィルタ不一致エラーが返ること"
    );
    // 内部状態が汚染されていないこと (filter 評価が FirstObjectId 解決より前に実行される)。
    // 後続の通過送信が ID=15 で解決されることが裏付けになる
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .largest_received_location,
        None,
        "拒否された送信で largest_received_location が更新されないこと"
    );
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .stream_counts
            .published_count,
        1,
        "拒否後も stream は残っていること"
    );
    assert_ne!(
        server.state(),
        SessionState::Closing,
        "ローカル検証の拒否でセッションが閉じないこと"
    );
    // 拒否後に通過するオブジェクト (id=15) を送ると、その ID で FirstObjectId 解決される
    // (汚染の回帰防御: 拒否された ID=5 で解決されていないことの裏付け。
    // 通過送信の成功自体が正しい ID での解決を意味する)
    server
        .send_subgroup_object(stream_id, 15, None)
        .expect("SUBGROUP_FILTER の範囲内オブジェクトは送信できること");
}

/// 同一 Group 内で start > end の空範囲はエラーを返し何も通さない
///
/// draft-ietf-moq-transport-21 §3.3.1 (Location Filters): subscription のフィルタは
/// 常時 valid のためエラーにはならない。空範囲は何も通さない。
/// Group 差分は非負のため、開始 Group を上回る End Group の逆転は wire 上表現できず、
/// 同一 Group 内の Object 逆転のみが到達可能である。
/// header 送信はフィルタ検査なしで受理され (1 段階目)、全 Object が拒否される
/// (2 段階目)。subscription は Established を維持する。
#[test]
fn empty_location_filter_range_passes_nothing() {
    use shiguredo_moqt::message::common::Location;
    // start {3, 5} → end {3, 2} の空範囲
    let filter = LocationFilter::AbsoluteRangeWithEnd {
        start: Location {
            group_id: 3,
            object_id: 5,
        },
        end_group_delta: 0,
        end_object: 2,
    };
    let mut params = MessageParameters::new();
    params.push(MessageParameter {
        param_type: PARAM_LOCATION_FILTER,
        value: MessageParameterValue::LengthPrefixed(filter.encode_to_bytes()),
    });
    let (_client, mut server, rid) = establish_with_filters(params);
    // 解決値が空範囲として保持されること
    let sub = server
        .subscription(rid)
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(
        sub.filter_start,
        Some(Location {
            group_id: 3,
            object_id: 5,
        })
    );
    assert_eq!(
        sub.filter_end,
        Some(Location {
            group_id: 3,
            object_id: 2,
        })
    );
    // 1 段階目: header 送信は受理される
    let stream_id = DataStreamId(300);
    let header = SubgroupHeader {
        track_alias: 1,
        group_id: 3,
        subgroup_id: SubgroupIdMode::Explicit(0),
        publisher_priority: Some(1),
        has_properties: false,
        end_of_group: false,
        first_object: false,
    };
    server
        .send_subgroup_header(stream_id, rid, &header)
        .expect("header 送信は受理されること");
    // 2 段階目: 範囲内外を問わず全 Object が拒否される
    for object_id in [0, 2, 5, 9] {
        let err = server
            .send_subgroup_object(stream_id, object_id, None)
            .expect_err("空範囲では全 Object が拒否されること");
        assert!(
            matches!(err, SendRequestError::LocalFilterMismatch),
            "空範囲では LocalFilterMismatch が返ること"
        );
    }
    // subscription は Established を維持し、セッションは閉じない
    assert_eq!(
        server
            .subscription(rid)
            .expect("テストフィクスチャの前提条件を満たす")
            .state,
        SubscriptionState::Established,
        "空範囲の拒否後も subscription は Established であること"
    );
    assert_eq!(
        server.state(),
        SessionState::Established,
        "空範囲の拒否後もセッションは Established であること"
    );
}
