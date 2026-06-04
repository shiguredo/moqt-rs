use shiguredo_moqt::{
    message::ControlMessage,
    message::Fetch,
    message::FetchOk,
    message::Goaway,
    message::Namespace,
    message::NamespaceDone,
    message::Publish,
    message::PublishDone,
    message::PublishNamespace,
    message::PublishSkipped,
    message::PublishStateNotify,
    message::ReasonPhrase,
    message::Redirect,
    message::RequestError,
    message::RequestOk,
    message::RequestUpdate,
    message::Setup,
    message::Subscribe,
    message::SubscribeNamespace,
    message::SubscribeOk,
    message::SubscribeTracks,
    message::TrackStatus,
    message::common::Location,
    message_parameter::AuthorizationToken,
    message_parameter::LocationFilter,
    message_parameter::MessageParameter,
    message_parameter::MessageParameterValue,
    message_parameter::MessageParameters,
    message_parameter::{
        PARAM_AUTHORIZATION_TOKEN, PARAM_EXPIRES, PARAM_FILL_PARAMETERS, PARAM_FILL_TIMEOUT,
        PARAM_FORWARD, PARAM_GROUP_ORDER, PARAM_INCLUDE_PROPERTIES, PARAM_LARGEST_OBJECT,
        PARAM_LOCATION_FILTER, PARAM_NEW_GROUP_REQUEST, PARAM_OBJECT_DELIVERY_TIMEOUT,
        PARAM_OBJECT_PROPERTY_FILTER, PARAM_OBJECTID_FILTER, PARAM_PRIORITY_FILTER,
        PARAM_RENDEZVOUS_TIMEOUT, PARAM_SUBGROUP_DELIVERY_TIMEOUT, PARAM_SUBGROUP_FILTER,
        PARAM_SUBSCRIBER_PRIORITY, PARAM_TRACK_NAMESPACE_PREFIX, PARAM_TRACK_PROPERTY_FILTER,
    },
    track_properties::TrackProperties,
    varint,
};

// ─── 共通ヘルパー ────────────────────────────────────────────────

use pbt::common::{
    SETUP_SAMPLING_SMALL, is_range_filter_type, sample_bytes, sample_namespace_prefix,
    sample_setup_options_with, sample_small_varint, sample_track_properties, test_runner,
};
/// 指定されたパラメータ型に対応する 1 つの MessageParameter を生成する
fn sample_parameter_from(ctx: &mut noprop::TestCaseContext, param_type: u64) -> MessageParameter {
    match param_type {
        PARAM_OBJECT_DELIVERY_TIMEOUT
        | PARAM_SUBGROUP_DELIVERY_TIMEOUT
        | PARAM_EXPIRES
        | PARAM_FILL_TIMEOUT
        | PARAM_RENDEZVOUS_TIMEOUT
        | PARAM_NEW_GROUP_REQUEST => MessageParameter {
            param_type,
            value: MessageParameterValue::VarInt(sample_small_varint(ctx)),
        },
        // FORWARD は 0 または 1 のみ (draft-ietf-moq-transport-21 §9.20.19 (FORWARD Parameter))
        // INCLUDE_PROPERTIES も 0 または 1 のみ (draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter))
        PARAM_FORWARD | PARAM_INCLUDE_PROPERTIES => MessageParameter {
            param_type,
            value: MessageParameterValue::Uint8(noprop::sample_choice(ctx, &[0u8, 1u8])),
        },
        PARAM_SUBSCRIBER_PRIORITY => MessageParameter {
            param_type,
            value: MessageParameterValue::Uint8(noprop::sample_u8(ctx)),
        },
        // GROUP_ORDER は 1 または 2 のみ (draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter))
        PARAM_GROUP_ORDER => MessageParameter {
            param_type,
            value: MessageParameterValue::Uint8(noprop::sample_choice(ctx, &[1u8, 2u8])),
        },
        PARAM_LARGEST_OBJECT => MessageParameter {
            param_type,
            value: MessageParameterValue::Location {
                group: sample_small_varint(ctx),
                object: sample_small_varint(ctx),
            },
        },
        PARAM_AUTHORIZATION_TOKEN => {
            // 有効な AuthorizationToken を AuthorizationToken variant として生成する
            // decode 時に LengthPrefixed から AuthorizationToken に変換されるため、
            // ラウンドトリップを成立させるには AuthorizationToken variant を使う必要がある。
            // REGISTER (0x01) と USE_VALUE (0x03) を生成する
            // (DELETE/USE_ALIAS は SETUP 以外でも使えるが、簡潔にする)
            let token = match noprop::sample_weighted_index(ctx, &[1, 1, 1, 1]) {
                0 => AuthorizationToken::Register {
                    alias: sample_small_varint(ctx),
                    token_type: sample_small_varint(ctx),
                    token_value: sample_bytes(ctx, 20),
                },
                1 => AuthorizationToken::UseValue {
                    token_type: sample_small_varint(ctx),
                    token_value: sample_bytes(ctx, 20),
                },
                2 => AuthorizationToken::Delete {
                    alias: sample_small_varint(ctx),
                },
                _ => AuthorizationToken::UseAlias {
                    alias: sample_small_varint(ctx),
                },
            };
            MessageParameter {
                param_type,
                value: MessageParameterValue::AuthorizationToken(token),
            }
        }
        PARAM_LOCATION_FILTER => MessageParameter {
            param_type,
            value: MessageParameterValue::LengthPrefixed(sample_location_filter_bytes(ctx)),
        },
        // draft-ietf-moq-transport-21 §9.20.16 (FILL PARAMETERS Parameter):
        // 内側は Table 6 の別スコープとして生成する (再帰なし)。
        // decode 時に typed variant に変換されるため、往復には typed variant を使う。
        PARAM_FILL_PARAMETERS => MessageParameter {
            param_type,
            value: MessageParameterValue::FillParameters(sample_scoped_parameters(
                ctx,
                FILL_INNER_PARAMS,
            )),
        },
        PARAM_TRACK_NAMESPACE_PREFIX => MessageParameter {
            param_type,
            value: MessageParameterValue::TrackNamespacePrefix(sample_namespace_prefix(ctx)),
        },
        // draft-ietf-moq-transport-21 §3.3.2 (Range Filters): length-prefixed バイト列
        PARAM_SUBGROUP_FILTER
        | PARAM_OBJECTID_FILTER
        | PARAM_PRIORITY_FILTER
        | PARAM_OBJECT_PROPERTY_FILTER
        | PARAM_TRACK_PROPERTY_FILTER => MessageParameter {
            param_type,
            value: MessageParameterValue::LengthPrefixed(sample_range_filter_bytes(
                ctx, param_type,
            )),
        },
        _ => panic!("不明なパラメータタイプ: {param_type}"),
    }
}

/// 指定された許可パラメータ型リストからサブセットを選んで MessageParameters を生成する
fn sample_scoped_parameters(
    ctx: &mut noprop::TestCaseContext,
    allowed: &[u64],
) -> MessageParameters {
    if allowed.is_empty() {
        return MessageParameters::new();
    }
    let count = noprop::sample_usize_in(ctx, 0..=allowed.len().min(5));
    let mut params = Vec::new();
    for _ in 0..count {
        let param_type = noprop::sample_choice(ctx, allowed);
        params.push(sample_parameter_from(ctx, param_type));
    }
    params.sort_by_key(|p| p.param_type);
    // draft-ietf-moq-transport-21 §3.3.2 (Range Filters): Range Filter (0x25-0x29) は
    // 同一 Parameter Type が同一メッセージ内で複数回出現できるため重複排除の対象外とする。
    // AUTHORIZATION_TOKEN も複数回出現できるが、(Token Type, Token Value) の一意性を
    // encode が検証するため、ここでは従来どおり 1 件に絞る。
    params.dedup_by(|a, b| a.param_type == b.param_type && !is_range_filter_type(a.param_type));
    let mut result = MessageParameters::new();
    for p in params {
        result.push(p);
    }
    result
}

fn sample_request_id(ctx: &mut noprop::TestCaseContext) -> u64 {
    sample_small_varint(ctx)
}

/// 1 バイト (表示可能 ASCII) とマルチバイト (2..=4 バイト) の文字を 9:1 で混ぜて生成する
///
/// `sample_char` の一様分布では 4 バイト文字が大半を占めるため、文字数を固定しても
/// ReasonPhrase の 1024 バイト制限を大きく超えてしまう。9:1 で混ぜれば 256 文字でも
/// バイト長は 1024 を超えず、マルチバイト文字の往復も検証できる。
fn sample_reason_char(ctx: &mut noprop::TestCaseContext) -> char {
    if noprop::sample_ratio(ctx, noprop::Ratio::new(9, 10)) {
        noprop::sample_ascii_printable_char(ctx)
    } else {
        noprop::sample_char(ctx)
    }
}

fn sample_reason_phrase(ctx: &mut noprop::TestCaseContext) -> ReasonPhrase {
    if noprop::sample_bool(ctx) {
        // 表示可能 ASCII のみ (最大 1024 バイト)
        let len = noprop::sample_usize_in(ctx, 0..=1024);
        ReasonPhrase::new(noprop::sample_ascii_printable_string(ctx, len))
            .expect("テストフィクスチャの前提条件を満たす")
    } else {
        // UTF-8 マルチバイト文字を含む任意文字列 (最大 256 文字)
        // 1 文字あたり最大 4 バイトなので、256 文字でも 1024 バイトを超えない。
        let char_count = noprop::sample_usize_in(ctx, 0..=256);
        let s: String = (0..char_count).map(|_| sample_reason_char(ctx)).collect();
        ReasonPhrase::new(s).expect("テストフィクスチャの前提条件を満たす")
    }
}

/// REQUEST_ERROR の Redirect 構造 (draft-ietf-moq-transport-21 §9.4.1 (Redirect Structure)) を生成する
fn sample_redirect(ctx: &mut noprop::TestCaseContext) -> Redirect {
    Redirect {
        connect_uri: sample_bytes(ctx, 40),
        track_namespace: sample_namespace_prefix(ctx),
        track_name: sample_bytes(ctx, 40),
    }
}

fn sample_location(ctx: &mut noprop::TestCaseContext) -> Location {
    Location {
        group_id: sample_small_varint(ctx),
        object_id: sample_small_varint(ctx),
    }
}

fn sample_location_filter_bytes(ctx: &mut noprop::TestCaseContext) -> Vec<u8> {
    // 重み付きで 6 通りを生成する。空バイト列は Length 0 (no filter) の削除指示であり、
    // encode/decode 往復可能として受け付ける。
    match noprop::sample_weighted_index(ctx, &[2, 2, 2, 2, 2, 1]) {
        0 => LocationFilter::RelativeGroup {
            start_group: sample_small_varint(ctx),
        }
        .encode_to_bytes(),
        1 => LocationFilter::NextObject.encode_to_bytes(),
        2 => LocationFilter::AbsoluteStart {
            start: sample_location(ctx),
        }
        .encode_to_bytes(),
        3 => LocationFilter::AbsoluteRange {
            start: sample_location(ctx),
            end_group_delta: sample_small_varint(ctx),
        }
        .encode_to_bytes(),
        4 => LocationFilter::AbsoluteRangeWithEnd {
            start: sample_location(ctx),
            end_group_delta: sample_small_varint(ctx),
            end_object: sample_small_varint(ctx),
        }
        .encode_to_bytes(),
        _ => Vec::new(),
    }
}

/// Range Filter (0x25-0x29) のワイヤバイト列を生成する
///
/// draft-ietf-moq-transport-21 §3.3.2: SetID (1 byte) | [Property Type (vi64)] | Range...
/// Length=0 は REQUEST_UPDATE での削除セマンティクス。
/// PRIORITY_FILTER は 0..=255、Property Type は偶数のみ。
/// ラウンドトリップ用なので、構造は最小の正当形 (0 または 1 Range) に留める。
fn sample_range_filter_bytes(ctx: &mut noprop::TestCaseContext, param_type: u64) -> Vec<u8> {
    let with_property_type = matches!(
        param_type,
        PARAM_OBJECT_PROPERTY_FILTER | PARAM_TRACK_PROPERTY_FILTER
    );
    let is_priority = param_type == PARAM_PRIORITY_FILTER;

    // Length=0 (削除セマンティクス) と 1 Range の 2 分岐
    if noprop::sample_bool(ctx) {
        return Vec::new();
    }
    let set_id = noprop::sample_u8(ctx);
    let property_type = noprop::sample_choice(ctx, &[0u64, 2u64, 4u64, 6u64]);
    let start = if is_priority {
        noprop::sample_u64_in(ctx, 0..=255)
    } else {
        sample_small_varint(ctx)
    };
    let end = if is_priority {
        if noprop::sample_bool(ctx) {
            let v = noprop::sample_u64_in(ctx, 0..=32);
            Some(v.min(255u64.saturating_sub(start)))
        } else {
            None
        }
    } else if noprop::sample_bool(ctx) {
        Some(sample_small_varint(ctx))
    } else {
        None
    };
    let mut bytes = Vec::new();
    bytes.push(set_id);
    if with_property_type {
        varint::encode(property_type, &mut bytes);
    }
    varint::encode(start, &mut bytes);
    if let Some(end_delta) = end {
        varint::encode(end_delta, &mut bytes);
    }
    bytes
}

// ─── パラメータスコープテーブル (Parameter Scope) ────
//
// 各定数は `src/message.rs` の対応する `*_ALLOWED_PARAMS` と一致させること。
// Range Filters (0x25-0x29) は draft-19 §3.4 で追加された。
// PUBLISH 系は draft-21 §9.20 の MAY appear 導出に従う。

const SUBSCRIBE_PARAMS: &[u64] = &[
    PARAM_AUTHORIZATION_TOKEN,
    PARAM_OBJECT_DELIVERY_TIMEOUT,
    PARAM_SUBGROUP_DELIVERY_TIMEOUT,
    PARAM_SUBSCRIBER_PRIORITY,
    PARAM_FORWARD,
    PARAM_LOCATION_FILTER,
    PARAM_GROUP_ORDER,
    PARAM_NEW_GROUP_REQUEST,
    PARAM_RENDEZVOUS_TIMEOUT,
    PARAM_INCLUDE_PROPERTIES,
    PARAM_FILL_PARAMETERS,
    PARAM_SUBGROUP_FILTER,
    PARAM_OBJECTID_FILTER,
    PARAM_PRIORITY_FILTER,
    PARAM_OBJECT_PROPERTY_FILTER,
];
// draft-ietf-moq-transport-21 §9.20.16 (FILL PARAMETERS Parameter) Table 6:
// FILL_PARAMETERS 内側スコープ。`src/message_parameter.rs` の
// `FILL_PARAMETERS_ALLOWED_PARAMS` と一致させること。
const FILL_INNER_PARAMS: &[u64] = &[
    PARAM_FILL_TIMEOUT,
    PARAM_SUBSCRIBER_PRIORITY,
    PARAM_LOCATION_FILTER,
    PARAM_GROUP_ORDER,
    PARAM_SUBGROUP_FILTER,
    PARAM_OBJECTID_FILTER,
    PARAM_PRIORITY_FILTER,
    PARAM_OBJECT_PROPERTY_FILTER,
];
const SUBSCRIBE_OK_PARAMS: &[u64] = &[PARAM_EXPIRES, PARAM_LARGEST_OBJECT];
// draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope) / §9.20.3 以降: REQUEST_OK では AUTHORIZATION_TOKEN は非許可。
// `src/message.rs::REQUEST_OK_ALLOWED_PARAMS` と一致させる。
const REQUEST_OK_PARAMS: &[u64] = &[
    PARAM_OBJECT_DELIVERY_TIMEOUT,
    PARAM_SUBGROUP_DELIVERY_TIMEOUT,
    PARAM_SUBSCRIBER_PRIORITY,
    PARAM_LOCATION_FILTER,
    PARAM_EXPIRES,
    PARAM_LARGEST_OBJECT,
    PARAM_FORWARD,
    PARAM_NEW_GROUP_REQUEST,
    PARAM_SUBGROUP_FILTER,
    PARAM_OBJECTID_FILTER,
    PARAM_PRIORITY_FILTER,
    PARAM_OBJECT_PROPERTY_FILTER,
];
// draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter): REQUEST_UPDATE では GROUP_ORDER は非許可。
// `src/message.rs::REQUEST_UPDATE_ALLOWED_PARAMS` と一致させる。
const REQUEST_UPDATE_PARAMS: &[u64] = &[
    PARAM_AUTHORIZATION_TOKEN,
    PARAM_OBJECT_DELIVERY_TIMEOUT,
    PARAM_SUBGROUP_DELIVERY_TIMEOUT,
    PARAM_SUBSCRIBER_PRIORITY,
    PARAM_FORWARD,
    PARAM_LOCATION_FILTER,
    PARAM_NEW_GROUP_REQUEST,
    PARAM_TRACK_NAMESPACE_PREFIX,
    PARAM_FILL_PARAMETERS,
    PARAM_SUBGROUP_FILTER,
    PARAM_OBJECTID_FILTER,
    PARAM_PRIORITY_FILTER,
    PARAM_OBJECT_PROPERTY_FILTER,
    PARAM_TRACK_PROPERTY_FILTER,
];
const PUBLISH_PARAMS: &[u64] = &[
    PARAM_OBJECT_DELIVERY_TIMEOUT,
    PARAM_AUTHORIZATION_TOKEN,
    PARAM_SUBGROUP_DELIVERY_TIMEOUT,
    PARAM_EXPIRES,
    PARAM_LARGEST_OBJECT,
    PARAM_FORWARD,
    PARAM_SUBSCRIBER_PRIORITY,
    PARAM_LOCATION_FILTER,
    PARAM_GROUP_ORDER,
];
const FETCH_PARAMS: &[u64] = &[
    PARAM_AUTHORIZATION_TOKEN,
    PARAM_FILL_TIMEOUT,
    PARAM_SUBSCRIBER_PRIORITY,
    PARAM_GROUP_ORDER,
    PARAM_INCLUDE_PROPERTIES,
    PARAM_LOCATION_FILTER,
    PARAM_SUBGROUP_FILTER,
    PARAM_OBJECTID_FILTER,
    PARAM_PRIORITY_FILTER,
    PARAM_OBJECT_PROPERTY_FILTER,
];
const FETCH_OK_PARAMS: &[u64] = &[];
// draft-ietf-moq-transport-21 §9.10 (PUBLISH_STATE_NOTIFY) / §9.20.10 / §9.20.18 / §9.20.19:
// `src/message.rs::PUBLISH_STATE_NOTIFY_ALLOWED_PARAMS` と一致させること。
const PUBLISH_STATE_NOTIFY_PARAMS: &[u64] =
    &[PARAM_FORWARD, PARAM_LOCATION_FILTER, PARAM_LARGEST_OBJECT];
const TRACK_STATUS_PARAMS: &[u64] = &[PARAM_AUTHORIZATION_TOKEN, PARAM_INCLUDE_PROPERTIES];
const PUBLISH_NAMESPACE_PARAMS: &[u64] = &[PARAM_AUTHORIZATION_TOKEN];
const SUBSCRIBE_NAMESPACE_PARAMS: &[u64] = &[PARAM_AUTHORIZATION_TOKEN];
// draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter): GROUP_ORDER は SUBSCRIBE_TRACKS で許可。
// `src/message.rs::SUBSCRIBE_TRACKS_ALLOWED_PARAMS` と一致させる。
const SUBSCRIBE_TRACKS_PARAMS: &[u64] = &[
    PARAM_AUTHORIZATION_TOKEN,
    PARAM_FORWARD,
    PARAM_GROUP_ORDER,
    PARAM_INCLUDE_PROPERTIES,
    PARAM_SUBGROUP_FILTER,
    PARAM_OBJECTID_FILTER,
    PARAM_PRIORITY_FILTER,
    PARAM_OBJECT_PROPERTY_FILTER,
    PARAM_TRACK_PROPERTY_FILTER,
];

// ─── メッセージ生成 ──────────────────────────────────────────

/// 任意の ControlMessage を生成する (20 ブランチを等確率で選ぶ)
fn sample_control_message(ctx: &mut noprop::TestCaseContext) -> ControlMessage {
    match noprop::sample_weighted_index(ctx, &[1u32; 20]) {
        // Setup
        0 => ControlMessage::Setup(Setup {
            options: sample_setup_options_with(ctx, &SETUP_SAMPLING_SMALL),
        }),
        // Goaway
        1 => ControlMessage::Goaway(Goaway {
            new_session_uri: sample_bytes(ctx, 100),
            timeout: sample_small_varint(ctx),
        }),
        // RequestOk
        2 => ControlMessage::RequestOk(RequestOk {
            parameters: sample_scoped_parameters(ctx, REQUEST_OK_PARAMS),
            track_properties: TrackProperties::default(),
        }),
        // RequestError (非 REDIRECT: error_code から 0x34 を除外し redirect=None)
        // draft-ietf-moq-transport-21 §9.4.2 (REQUEST_ERROR Message Format) のエンコード整合検証により、非 REDIRECT + Some や
        // REDIRECT + None はエンコードが Err を返すため、整合した組のみを生成する。
        3 => {
            let v = noprop::sample_usize_in(ctx, 0..1000) as u64;
            // 0x34 (REQUEST_REDIRECT) を除外する (0..=999 のうち 52 を飛ばして 53..=1000 に射影)
            let error_code = if v >= 0x34 { v + 1 } else { v };
            ControlMessage::RequestError(RequestError {
                error_code,
                retry_interval: sample_small_varint(ctx),
                reason: sample_reason_phrase(ctx),
                redirect: None,
            })
        }
        // RequestError (REDIRECT: error_code=0x34 で redirect=Some)
        4 => ControlMessage::RequestError(RequestError {
            error_code: 0x34, // REQUEST_REDIRECT
            retry_interval: sample_small_varint(ctx),
            reason: sample_reason_phrase(ctx),
            redirect: Some(sample_redirect(ctx)),
        }),
        // Subscribe
        5 => ControlMessage::Subscribe(Subscribe {
            request_id: sample_request_id(ctx),
            track_namespace: sample_namespace_prefix(ctx),
            track_name: sample_bytes(ctx, 50),
            parameters: sample_scoped_parameters(ctx, SUBSCRIBE_PARAMS),
        }),
        // SubscribeOk
        6 => ControlMessage::SubscribeOk(SubscribeOk {
            track_alias: sample_small_varint(ctx),
            parameters: sample_scoped_parameters(ctx, SUBSCRIBE_OK_PARAMS),
            track_properties: sample_track_properties(ctx),
        }),
        // RequestUpdate
        7 => ControlMessage::RequestUpdate(RequestUpdate {
            request_id: sample_request_id(ctx),
            parameters: sample_scoped_parameters(ctx, REQUEST_UPDATE_PARAMS),
        }),
        // Publish
        8 => ControlMessage::Publish(Publish {
            request_id: sample_request_id(ctx),
            track_namespace: sample_namespace_prefix(ctx),
            track_name: sample_bytes(ctx, 50),
            track_alias: sample_small_varint(ctx),
            parameters: sample_scoped_parameters(ctx, PUBLISH_PARAMS),
            track_properties: sample_track_properties(ctx),
        }),
        // PublishDone
        9 => ControlMessage::PublishDone(PublishDone {
            status_code: sample_small_varint(ctx),
            stream_count: sample_small_varint(ctx),
            reason: sample_reason_phrase(ctx),
        }),
        // PublishSkipped
        10 => ControlMessage::PublishSkipped(PublishSkipped {
            track_namespace_suffix: sample_namespace_prefix(ctx),
            track_name: sample_bytes(ctx, 50),
        }),
        // Fetch (draft-ietf-moq-transport-21 §9.11: 単一形式。range は LOCATION_FILTER で指定)
        11 => ControlMessage::Fetch(Fetch {
            request_id: sample_request_id(ctx),
            track_namespace: sample_namespace_prefix(ctx),
            track_name: sample_bytes(ctx, 50),
            parameters: sample_scoped_parameters(ctx, FETCH_PARAMS),
        }),
        // FetchOk
        12 => ControlMessage::FetchOk(FetchOk {
            // End Of Track は 0 または 1 のみ (draft-ietf-moq-transport-21 §9.12 (FETCH_OK))
            end_of_track: noprop::sample_choice(ctx, &[0u8, 1u8]),
            end_location: sample_location(ctx),
            parameters: sample_scoped_parameters(ctx, FETCH_OK_PARAMS),
            track_properties: sample_track_properties(ctx),
        }),
        // TrackStatus
        13 => ControlMessage::TrackStatus(TrackStatus {
            request_id: sample_request_id(ctx),
            track_namespace: sample_namespace_prefix(ctx),
            track_name: sample_bytes(ctx, 50),
            parameters: sample_scoped_parameters(ctx, TRACK_STATUS_PARAMS),
        }),
        // PublishNamespace
        14 => ControlMessage::PublishNamespace(PublishNamespace {
            request_id: sample_request_id(ctx),
            track_namespace: sample_namespace_prefix(ctx),
            parameters: sample_scoped_parameters(ctx, PUBLISH_NAMESPACE_PARAMS),
        }),
        // Namespace
        15 => ControlMessage::Namespace(Namespace {
            track_namespace_suffix: sample_namespace_prefix(ctx),
        }),
        // NamespaceDone
        16 => ControlMessage::NamespaceDone(NamespaceDone {
            track_namespace_suffix: sample_namespace_prefix(ctx),
        }),
        // SubscribeNamespace
        17 => ControlMessage::SubscribeNamespace(SubscribeNamespace {
            request_id: sample_request_id(ctx),
            track_namespace_prefix: sample_namespace_prefix(ctx),
            parameters: sample_scoped_parameters(ctx, SUBSCRIBE_NAMESPACE_PARAMS),
        }),
        // PublishStateNotify (draft-ietf-moq-transport-21 §9.10)
        18 => ControlMessage::PublishStateNotify(PublishStateNotify {
            parameters: sample_scoped_parameters(ctx, PUBLISH_STATE_NOTIFY_PARAMS),
        }),
        // SubscribeTracks
        _ => ControlMessage::SubscribeTracks(SubscribeTracks {
            request_id: sample_request_id(ctx),
            track_namespace_prefix: sample_namespace_prefix(ctx),
            parameters: sample_scoped_parameters(ctx, SUBSCRIBE_TRACKS_PARAMS),
        }),
    }
}

/// 任意 ControlMessage のエンコード → デコード ラウンドトリップ
#[test]
fn roundtrip() -> noprop::TestResult {
    // Fetch (統一形式) と PublishStateNotify の生成がラウンドトリップを
    // 通って観測されたかを数える
    let fetch_seen = std::cell::Cell::new(false);
    let notify_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let msg = sample_control_message(ctx);
        let encoded = msg.encode().expect("正当なテスト入力の encode は成功する");
        let (decoded, consumed) =
            ControlMessage::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, msg);
        if matches!(&decoded, ControlMessage::Fetch(_)) {
            fetch_seen.set(true);
        }
        if matches!(&decoded, ControlMessage::PublishStateNotify(_)) {
            notify_seen.set(true);
        }
        Ok(())
    })?;
    assert!(
        fetch_seen.get(),
        "Fetch のケースが 1 つも観測されなかった\n{runner}"
    );
    assert!(
        notify_seen.get(),
        "PublishStateNotify のケースが 1 つも観測されなかった\n{runner}"
    );
    Ok(())
}

/// フレーム末尾より後ろの余剰バイトは Length 区切りで消費されず、次フレームとして残る
///
/// `ControlMessage::decode` は Length プレフィックスが示す範囲だけを payload として
/// 切り出し (`src/message.rs:628`)、フレーム末尾より後ろのバイトは消費しない。
/// draft-ietf-moq-transport-21 §9 (Control Messages) の Length 区切り (フレーミング) 契約により、
/// 後続バイトは別フレームとして呼び出し側 (`src/decoder.rs` のストリーミング・デコーダ) に委ねられる。
/// この契約が将来 (例: 末尾余剰を誤ってエラー扱いする、Length を無視して buffer 末尾まで読む等)
/// 破れる回帰を検出する。
#[test]
fn decode_treats_trailing_bytes_as_next_frame() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let msg = sample_control_message(ctx);
        let extra_len = noprop::sample_usize_in(ctx, 1..=16);
        let extra = noprop::sample_bytes_vec(ctx, extra_len);
        let frame = msg
            .encode()
            .expect("sample_control_message は常に encode 可能");
        let frame_len = frame.len();
        let mut buf = frame;
        buf.extend_from_slice(&extra);

        let (decoded, consumed) = ControlMessage::decode(&buf)
            .expect("Length 区切りなので末尾余剰があっても decode 成功する");
        // 復元結果は元メッセージと一致する
        assert_eq!(decoded, msg);
        // 消費バイト数はフレーム長と一致し、末尾余剰は消費されない
        assert_eq!(consumed, frame_len);
        assert!(consumed < buf.len());
        Ok(())
    })?;
    Ok(())
}
