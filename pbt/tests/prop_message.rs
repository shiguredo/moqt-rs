use shiguredo_moqt::error::MessageError;
use shiguredo_moqt::parameter::DEFAULT_MAX_CONTROL_MESSAGE_SIZE;
use shiguredo_moqt::{
    message::ControlMessage,
    message::ErrorDetail,
    message::ErrorDetails,
    message::Fetch,
    message::Goaway,
    message::Publish,
    message::PublishDone,
    message::PublishStateNotify,
    message::ReasonPhrase,
    message::Redirect,
    message::RequestError,
    message::RequestOk,
    message::RequestUpdate,
    message::Setup,
    message::Subscribe,
    message::SubscribeOk,
    message::TrackStatus,
    message::UnsupportedProperty,
    message::common::Location,
    message_parameter::AuthorizationToken,
    message_parameter::LocationFilter,
    message_parameter::MessageParameter,
    message_parameter::MessageParameterValue,
    message_parameter::MessageParameters,
    message_parameter::{
        PARAM_AUTHORIZATION_TOKEN, PARAM_EXPIRES, PARAM_FILL_PARAMETERS, PARAM_FILL_TIMEOUT,
        PARAM_GROUP_ORDER, PARAM_INCLUDE_PROPERTIES, PARAM_LARGEST_OBJECT, PARAM_LOCATION_FILTER,
        PARAM_NEW_GROUP_REQUEST, PARAM_OBJECT_DELIVERY_TIMEOUT, PARAM_OBJECT_PROPERTY_FILTER,
        PARAM_OBJECTID_FILTER, PARAM_PAUSED, PARAM_PRIORITY_FILTER,
        PARAM_SUBGROUP_DELIVERY_TIMEOUT, PARAM_SUBGROUP_FILTER, PARAM_SUBSCRIBER_PRIORITY,
        PARAM_TRACK_NAMESPACE_PREFIX, PARAM_TRACK_PROPERTY_FILTER,
    },
    track_properties::TrackProperties,
    varint,
};

// ─── 共通ヘルパー ────────────────────────────────────────────────

use pbt::common::{
    SETUP_SAMPLING_SMALL, is_range_filter_type, sample_bytes, sample_namespace_prefix,
    sample_nonempty_bytes, sample_setup_options_with, sample_small_varint, sample_track_properties,
    test_runner,
};
/// 指定されたパラメータ型に対応する 1 つの MessageParameter を生成する
fn sample_parameter_from(ctx: &mut noprop::TestCaseContext, param_type: u64) -> MessageParameter {
    match param_type {
        PARAM_OBJECT_DELIVERY_TIMEOUT
        | PARAM_SUBGROUP_DELIVERY_TIMEOUT
        | PARAM_EXPIRES
        | PARAM_FILL_TIMEOUT
        | PARAM_NEW_GROUP_REQUEST => MessageParameter {
            param_type,
            value: MessageParameterValue::VarInt(sample_small_varint(ctx)),
        },
        // PAUSED は 0 (unpaused) または 1 (paused) のみ
        // (draft-ietf-moq-transport-23 §9.19.18 (PAUSED Parameter))
        // INCLUDE_PROPERTIES も 0 または 1 のみ (draft-ietf-moq-transport-23 §9.19.22 (INCLUDE_PROPERTIES Parameter))
        PARAM_PAUSED | PARAM_INCLUDE_PROPERTIES => MessageParameter {
            param_type,
            value: MessageParameterValue::Uint8(noprop::sample_choice(ctx, &[0u8, 1u8])),
        },
        PARAM_SUBSCRIBER_PRIORITY => MessageParameter {
            param_type,
            value: MessageParameterValue::Uint8(noprop::sample_u8(ctx)),
        },
        // GROUP_ORDER は 1 または 2 のみ (draft-ietf-moq-transport-23 §9.19.8 (GROUP ORDER Parameter))
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
            value: MessageParameterValue::LocationFilter(sample_location_filter(ctx)),
        },
        // draft-ietf-moq-transport-23 §9.19.15 (FILL PARAMETERS Parameter):
        // 内側は Table 7 の別スコープとして生成する (再帰なし)。
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
        // draft-ietf-moq-transport-23 §3.3.2 (Range Filters): length-prefixed バイト列
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
    // draft-ietf-moq-transport-23 §3.3.2 (Range Filters): Range Filter (0x25-0x29) は
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

/// REQUEST_ERROR の Redirect 構造 (draft-ietf-moq-transport-23 §9.4.2 (Error Details)) を生成する
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

fn sample_location_filter(ctx: &mut noprop::TestCaseContext) -> LocationFilter {
    // 重み付きで 6 通りを生成する。Location Filter Type 0x00 (None) も
    // encode/decode 往復可能な値として受け付ける。
    match noprop::sample_weighted_index(ctx, &[2, 2, 2, 2, 2, 1]) {
        0 => LocationFilter::RelativeGroup {
            start_group: sample_small_varint(ctx),
        },
        1 => LocationFilter::NextObject,
        2 => LocationFilter::AbsoluteStart {
            start: sample_location(ctx),
        },
        3 => LocationFilter::AbsoluteRange {
            start: sample_location(ctx),
            end_group_delta: sample_small_varint(ctx),
        },
        4 => LocationFilter::AbsoluteRangeWithEnd {
            start: sample_location(ctx),
            end_group_delta: sample_small_varint(ctx),
            end_object: sample_small_varint(ctx),
        },
        _ => LocationFilter::NoFilter,
    }
}

/// Range Filter (0x25-0x29) のワイヤバイト列を生成する
///
/// draft-ietf-moq-transport-23 §3.3.2: SetID (1 byte) | [Property Type (vi64)] | Range...
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
// PUBLISH 系は draft-ietf-moq-transport-23 §9.19 の MAY appear 導出に従う。

const SUBSCRIBE_PARAMS: &[u64] = &[
    PARAM_AUTHORIZATION_TOKEN,
    PARAM_OBJECT_DELIVERY_TIMEOUT,
    PARAM_SUBGROUP_DELIVERY_TIMEOUT,
    PARAM_SUBSCRIBER_PRIORITY,
    PARAM_PAUSED,
    PARAM_LOCATION_FILTER,
    PARAM_GROUP_ORDER,
    PARAM_NEW_GROUP_REQUEST,
    PARAM_INCLUDE_PROPERTIES,
    PARAM_FILL_PARAMETERS,
    PARAM_SUBGROUP_FILTER,
    PARAM_OBJECTID_FILTER,
    PARAM_PRIORITY_FILTER,
    PARAM_OBJECT_PROPERTY_FILTER,
];
// draft-ietf-moq-transport-23 §9.19.15 (FILL PARAMETERS Parameter) Table 7:
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
// draft-ietf-moq-transport-23 §9.3 (REQUEST_OK): 応答 context ごとの許可パラメータの和集合は
// EXPIRES と LARGEST_OBJECT の 2 型。`src/message.rs::REQUEST_OK_ALLOWED_PARAMS` と一致させる。
const REQUEST_OK_PARAMS: &[u64] = &[PARAM_EXPIRES, PARAM_LARGEST_OBJECT];
// draft-ietf-moq-transport-23 §9.19.8 (GROUP ORDER Parameter): REQUEST_UPDATE では GROUP_ORDER は非許可。
// `src/message.rs::REQUEST_UPDATE_ALLOWED_PARAMS` と一致させる。
const REQUEST_UPDATE_PARAMS: &[u64] = &[
    PARAM_AUTHORIZATION_TOKEN,
    PARAM_OBJECT_DELIVERY_TIMEOUT,
    PARAM_SUBGROUP_DELIVERY_TIMEOUT,
    PARAM_SUBSCRIBER_PRIORITY,
    PARAM_PAUSED,
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
    PARAM_PAUSED,
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
// draft-ietf-moq-transport-23 §9.3 (REQUEST_OK): FETCH_OK は EXPIRES と
// LARGEST_OBJECT を運べる。`src/message.rs::FETCH_OK_ALLOWED_PARAMS` と一致させること。
const FETCH_OK_PARAMS: &[u64] = &[PARAM_EXPIRES, PARAM_LARGEST_OBJECT];
// draft-ietf-moq-transport-23 §9.10 (PUBLISH_STATE_NOTIFY) / §9.19.9 / §9.19.17 /
// draft-ietf-moq-transport-23 §9.19.18 (PAUSED Parameter):
// `src/message.rs::PUBLISH_STATE_NOTIFY_ALLOWED_PARAMS` と一致させること。
const PUBLISH_STATE_NOTIFY_PARAMS: &[u64] =
    &[PARAM_PAUSED, PARAM_LOCATION_FILTER, PARAM_LARGEST_OBJECT];
const TRACK_STATUS_PARAMS: &[u64] = &[PARAM_AUTHORIZATION_TOKEN, PARAM_INCLUDE_PROPERTIES];

// ─── メッセージ生成 ──────────────────────────────────────────

/// 任意の ControlMessage を生成する (15 ブランチを等確率で選ぶ)
fn sample_control_message(ctx: &mut noprop::TestCaseContext) -> ControlMessage {
    match noprop::sample_weighted_index(ctx, &[1u32; 15]) {
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
        // RequestError (非 REDIRECT: error_code から 0x34 を除外し Error Details は空)
        // draft-ietf-moq-transport-23 §9.4.2 (Error Details): REDIRECT 以外の Error Code には
        // 適用される Error Detail が無いため、空の Error Details を生成する。
        3 => {
            let v = noprop::sample_usize_in(ctx, 0..1000) as u64;
            // 0x34 (REQUEST_REDIRECT) を除外する (0..=999 のうち 52 を飛ばして 53..=1000 に射影)
            let error_code = if v >= 0x34 { v + 1 } else { v };
            ControlMessage::RequestError(RequestError {
                error_code,
                retry_interval: sample_small_varint(ctx),
                reason: sample_reason_phrase(ctx),
                error_details: ErrorDetails::new(),
            })
        }
        // RequestError (REDIRECT: error_code=0x34 で REDIRECT Error Detail を 1 つ持つ)
        4 => {
            let mut details = ErrorDetails::new();
            details.push(ErrorDetail::Redirect(sample_redirect(ctx)));
            ControlMessage::RequestError(RequestError {
                error_code: 0x34, // REQUEST_REDIRECT
                retry_interval: sample_small_varint(ctx),
                reason: sample_reason_phrase(ctx),
                error_details: details,
            })
        }
        // Subscribe
        // (draft-ietf-moq-transport-23 §2.4.1 (Track Naming): Track Name は 1 バイト以上)
        5 => ControlMessage::Subscribe(Subscribe {
            request_id: sample_request_id(ctx),
            track_namespace: sample_namespace_prefix(ctx),
            track_name: sample_nonempty_bytes(ctx, 50),
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
            track_name: sample_nonempty_bytes(ctx, 50),
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
        // Fetch (draft-ietf-moq-transport-23 §9.11: 単一形式。range は LOCATION_FILTER で指定)
        10 => ControlMessage::Fetch(Fetch {
            request_id: sample_request_id(ctx),
            track_namespace: sample_namespace_prefix(ctx),
            track_name: sample_nonempty_bytes(ctx, 50),
            parameters: sample_scoped_parameters(ctx, FETCH_PARAMS),
        }),
        // FETCH_OK (REQUEST_OK の別名。draft-ietf-moq-transport-23 §9.3 (REQUEST_OK))
        //
        // FETCH_OK は REQUEST_OK (Type 0x07) であり、FETCH の REQUEST_UPDATE_OK とは
        // メッセージ型では区別できない。ここでは FETCH_OK context の許可パラメータと
        // Track Properties を持つ形を生成する。
        11 => ControlMessage::RequestOk(RequestOk {
            parameters: sample_scoped_parameters(ctx, FETCH_OK_PARAMS),
            track_properties: sample_track_properties(ctx),
        }),
        // TrackStatus
        12 => ControlMessage::TrackStatus(TrackStatus {
            request_id: sample_request_id(ctx),
            track_namespace: sample_namespace_prefix(ctx),
            track_name: sample_nonempty_bytes(ctx, 50),
            parameters: sample_scoped_parameters(ctx, TRACK_STATUS_PARAMS),
        }),
        // PublishStateNotify (draft-ietf-moq-transport-23 §9.10)
        13 => ControlMessage::PublishStateNotify(PublishStateNotify {
            parameters: sample_scoped_parameters(ctx, PUBLISH_STATE_NOTIFY_PARAMS),
        }),
        // RequestError (UNSUPPORTED_EXTENSION: UNSUPPORTED_PROPERTY Detail を 0〜4 個持つ)
        // draft-ietf-moq-transport-23 §9.4.2.2 (UNSUPPORTED_PROPERTY) は複数インスタンスを
        // 明示的に許可するため、同じ Property Type の重複も生成しうる。
        _ => {
            let count = noprop::sample_usize_in(ctx, 0..=4);
            let mut details = ErrorDetails::new();
            for _ in 0..count {
                details.push(ErrorDetail::UnsupportedProperty(UnsupportedProperty {
                    property_type: noprop::sample_u64_in(ctx, 0x4000..=0x7FFF),
                }));
            }
            ControlMessage::RequestError(RequestError {
                error_code: shiguredo_moqt::error::REQUEST_UNSUPPORTED_EXTENSION,
                retry_interval: sample_small_varint(ctx),
                reason: sample_reason_phrase(ctx),
                error_details: details,
            })
        }
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
        let encoded = msg
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
            .expect("正当なテスト入力の encode は成功する");
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

/// LOCATION_FILTER の sampler が 6 形式すべてを生成すること (roundtrip のカバレッジゲート)
///
/// `roundtrip` は sampler が生成した値だけを往復させるため、sampler が特定の Location
/// Filter Type を生成しないとその形式の往復が検証されない。重み付き抽選の結果に依存せず
/// 6 形式すべてが現れることをここで固定する (draft-ietf-moq-transport-23 §9.19.9)。
#[test]
fn sample_location_filter_covers_all_types() -> noprop::TestResult {
    let seen = std::cell::Cell::new(0u8);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let filter = sample_location_filter(ctx);
        let bit = match filter {
            LocationFilter::NoFilter => 0b000001,
            LocationFilter::RelativeGroup { .. } => 0b000010,
            LocationFilter::AbsoluteStart { .. } => 0b000100,
            LocationFilter::AbsoluteRange { .. } => 0b001000,
            LocationFilter::AbsoluteRangeWithEnd { .. } => 0b010000,
            LocationFilter::NextObject => 0b100000,
        };
        seen.set(seen.get() | bit);
        Ok(())
    })?;
    assert_eq!(
        seen.get(),
        0b111111,
        "LOCATION_FILTER の 6 形式すべてが生成されること\n{runner}"
    );
    Ok(())
}

/// REQUEST_OK の sampler が和集合の 2 型 (EXPIRES / LARGEST_OBJECT) を生成すること
///
/// `roundtrip` は sampler が生成した値だけを往復させるため、sampler が特定の型を生成しないと
/// その型の往復が検証されない。draft-ietf-moq-transport-23 §9.3 (REQUEST_OK) の和集合は
/// 2 型なので、両方が現れることをここで固定する。
#[test]
fn request_ok_params_cover_all_types() -> noprop::TestResult {
    let expires_seen = std::cell::Cell::new(false);
    let largest_object_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        for param in sample_scoped_parameters(ctx, REQUEST_OK_PARAMS).as_slice() {
            if param.param_type == PARAM_EXPIRES {
                expires_seen.set(true);
            }
            if param.param_type == PARAM_LARGEST_OBJECT {
                largest_object_seen.set(true);
            }
        }
        Ok(())
    })?;
    assert!(expires_seen.get(), "EXPIRES が生成されなかった\n{runner}");
    assert!(
        largest_object_seen.get(),
        "LARGEST_OBJECT が生成されなかった\n{runner}"
    );
    Ok(())
}

/// フレーム末尾より後ろの余剰バイトは Length 区切りで消費されず、次フレームとして残る
///
/// `ControlMessage::decode` は Length プレフィックスが示す範囲だけを payload として
/// 切り出し (`src/message.rs:628`)、フレーム末尾より後ろのバイトは消費しない。
/// draft-ietf-moq-transport-23 §9 (Control Messages) の Length 区切り (フレーミング) 契約により、
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
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
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

/// 本文長が peer の MAX_CONTROL_MESSAGE_SIZE 以下なら encode でき、超えるなら拒否される
///
/// draft-ietf-moq-transport-23 §9 (Control Messages): "An endpoint MUST NOT send a control
/// message with a length larger than the peer's MAX_CONTROL_MESSAGE_SIZE." (MUST NOT)
/// encode は本文を組み立てた後に上限を判定するため、境界 (上限ちょうど / 上限 - 1) で
/// 結果が変わることを固定する。
#[test]
fn encode_respects_max_control_message_size() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let msg = sample_control_message(ctx);
        // 上限を課さずにエンコードして、フレーム長と本文長を得る
        let full = msg.encode(u64::MAX).expect("上限なしの encode は成功する");
        let (_, type_len) = varint::decode(&full).expect("Type を読める");
        let (body_len, length_len) = varint::decode(&full[type_len..]).expect("Length を読める");
        assert_eq!(
            type_len + length_len + body_len as usize,
            full.len(),
            "Length は本文長と一致すること"
        );

        // 上限ちょうどの本文は encode できる
        let at_limit = msg
            .encode(body_len)
            .expect("上限ちょうどの本文は encode できる");
        assert_eq!(at_limit, full, "上限ちょうどでもバイト列は変わらないこと");

        // 本文があるメッセージは上限 - 1 で拒否される
        if body_len > 0 {
            let err = msg
                .encode(body_len - 1)
                .expect_err("上限を 1 バイト超える本文は拒否されること");
            assert!(
                matches!(err, MessageError::FieldLengthExceeded(_)),
                "FieldLengthExceeded を期待したが {err:?} になった"
            );
        }
        Ok(())
    })?;
    Ok(())
}

/// 未知の Error Detail 型だけを持つ REQUEST_ERROR を読み飛ばして受理すること
///
/// draft-ietf-moq-transport-23 §9.4.2 (Error Details): "Receivers MUST ignore unknown Error
/// Detail types." と "Receivers MUST allow duplicates of unknown Error Detail types."
/// 同 §8.3 (Key-Value-Pair Structure): 偶数 Type の Value は vi64、奇数 Type の Value は
/// Length (vi64) + バイト列である。未知の型でもこの 2 形式だけで値を読み飛ばせるため、
/// 任意の未知 Detail 列 (既知の 0x01 / 0x02 を除く) を後ろに付けても REQUEST_ERROR の
/// decode は成功し、Detail は保持されない。
#[test]
fn unknown_error_details_are_skipped() -> noprop::TestResult {
    // REDIRECT 以外の Error Code を使う (REDIRECT Detail が必須にならないようにする)
    const ERROR_CODE: u64 = shiguredo_moqt::error::REQUEST_INTERNAL_ERROR;
    // REQUEST_ERROR のメッセージ型 ID (draft-ietf-moq-transport-23 §9 Table 5)
    const MSG_REQUEST_ERROR: u64 = 0x05;
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        // Delta Type は昇順でなければならないため、未知 Type を並べてからソートする。
        // 重複は未知 Type の重複として許可される (§9.4.2)。
        let count = noprop::sample_usize_in(ctx, 0..=8);
        let mut types = Vec::new();
        for _ in 0..count {
            // 0x01 (REDIRECT) と 0x02 (UNSUPPORTED_PROPERTY) は既知のため避ける
            types.push(noprop::sample_u64_in(ctx, 0x03..=0x1FF));
        }
        types.sort_unstable();

        let mut details = Vec::new();
        let mut prev_type = 0;
        for detail_type in types {
            varint::encode(detail_type - prev_type, &mut details);
            if detail_type % 2 == 0 {
                // 偶数 Type の Value は vi64
                varint::encode(sample_small_varint(ctx), &mut details);
            } else {
                // 奇数 Type の Value は Length (vi64) + バイト列
                let len = noprop::sample_usize_in(ctx, 0..=32);
                let value = noprop::sample_bytes_vec(ctx, len);
                varint::encode(len as u64, &mut details);
                details.extend_from_slice(&value);
            }
            prev_type = detail_type;
        }

        let mut payload = Vec::new();
        varint::encode(ERROR_CODE, &mut payload);
        varint::encode(0, &mut payload); // Retry Interval
        varint::encode(0, &mut payload); // Error Reason 長
        payload.extend_from_slice(&details);
        let mut frame = Vec::new();
        varint::encode(MSG_REQUEST_ERROR, &mut frame);
        varint::encode(payload.len() as u64, &mut frame);
        frame.extend_from_slice(&payload);

        let (decoded, consumed) =
            ControlMessage::decode(&frame).expect("未知の Error Detail だけなら decode できる");
        assert_eq!(consumed, frame.len(), "フレーム全体を消費すること");
        let ControlMessage::RequestError(err) = decoded else {
            panic!("RequestError が期待された");
        };
        assert!(
            err.error_details.is_empty(),
            "未知の Error Detail は保持しないこと"
        );
        Ok(())
    })?;
    Ok(())
}

/// UNSUPPORTED_EXTENSION の REQUEST_ERROR が複数の UNSUPPORTED_PROPERTY Detail を往復すること
///
/// draft-ietf-moq-transport-23 §9.4.2.2 (UNSUPPORTED_PROPERTY) は複数インスタンスを許す型で、
/// 値は Property Type (vi64) 1 つである (§8.3 (Key-Value-Pair Structure) の偶数 Type)。
/// 0〜8 個の Detail を生成し、encode → decode で同じ Property Type 列が出現順に取り出せる
/// ことを固定する。同じ Property Type の重複も型の定義が許すため、そのまま往復させる。
#[test]
fn unsupported_property_details_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let count = noprop::sample_usize_in(ctx, 0..=8);
        let mut details = ErrorDetails::new();
        let mut expected = Vec::new();
        for _ in 0..count {
            let property_type = noprop::sample_u64_in(ctx, 0x4000..=0x7FFF);
            expected.push(property_type);
            details.push(ErrorDetail::UnsupportedProperty(UnsupportedProperty {
                property_type,
            }));
        }
        let msg = ControlMessage::RequestError(RequestError {
            error_code: shiguredo_moqt::error::REQUEST_UNSUPPORTED_EXTENSION,
            retry_interval: sample_small_varint(ctx),
            reason: sample_reason_phrase(ctx),
            error_details: details,
        });
        let encoded = msg
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
            .expect("UNSUPPORTED_PROPERTY Detail は常に encode 可能");
        let (decoded, consumed) =
            ControlMessage::decode(&encoded).expect("encode 直後の decode は成功する");
        assert_eq!(consumed, encoded.len(), "メッセージ全体を消費すること");
        assert_eq!(decoded, msg, "往復で同じ構造に戻ること");
        let ControlMessage::RequestError(err) = decoded else {
            panic!("RequestError が期待された");
        };
        assert_eq!(
            err.error_details.unsupported_properties(),
            expected,
            "Property Type が出現順に保持されること"
        );
        Ok(())
    })?;
    Ok(())
}

/// UNSUPPORTED_EXTENSION 以外の Error Code に届いた UNSUPPORTED_PROPERTY Detail を無視すること
///
/// draft-ietf-moq-transport-23 §9.4.2 (Error Details): "Receivers MUST ... ignore a known
/// Error Detail type that accompanies an Error Code it is not defined for."
/// UNSUPPORTED_PROPERTY は UNSUPPORTED_EXTENSION (§9.4.2.2) でのみ意味を持つため、
/// 他の Error Code では保持しない。detail の KVP 形式 (偶数 Type の vi64) は読み飛ばす。
#[test]
fn inapplicable_unsupported_property_details_are_ignored() -> noprop::TestResult {
    // REQUEST_ERROR のメッセージ型 ID (draft-ietf-moq-transport-23 §9 Table 5)
    const MSG_REQUEST_ERROR: u64 = 0x05;
    // UNSUPPORTED_PROPERTY Error Detail の Type 値
    // (draft-ietf-moq-transport-23 §9.4.2.2 (UNSUPPORTED_PROPERTY))
    const UNSUPPORTED_PROPERTY_DETAIL_TYPE: u64 = 0x02;
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut error_code = noprop::sample_u64_in(ctx, 0..=0x200);
        // UNSUPPORTED_EXTENSION は適用対象、REDIRECT は REDIRECT Detail が必須になるため避ける
        if error_code == shiguredo_moqt::error::REQUEST_UNSUPPORTED_EXTENSION
            || error_code == shiguredo_moqt::error::REQUEST_REDIRECT
        {
            error_code += 2;
        }
        let count = noprop::sample_usize_in(ctx, 0..=4);
        let mut detail_bytes = Vec::new();
        let mut prev_type = 0;
        for _ in 0..count {
            // 2 個目以降は同じ Error Detail Type の繰り返しなので Delta Type は 0 になる
            varint::encode(
                UNSUPPORTED_PROPERTY_DETAIL_TYPE - prev_type,
                &mut detail_bytes,
            );
            prev_type = UNSUPPORTED_PROPERTY_DETAIL_TYPE;
            varint::encode(noprop::sample_u64_in(ctx, 0..=0x7FFF), &mut detail_bytes);
        }
        let mut payload = Vec::new();
        varint::encode(error_code, &mut payload);
        varint::encode(0, &mut payload); // Retry Interval
        varint::encode(0, &mut payload); // Error Reason 長
        payload.extend_from_slice(&detail_bytes);
        let mut frame = Vec::new();
        varint::encode(MSG_REQUEST_ERROR, &mut frame);
        varint::encode(payload.len() as u64, &mut frame);
        frame.extend_from_slice(&payload);

        let (decoded, consumed) =
            ControlMessage::decode(&frame).expect("適用外の Detail は無視され decode できる");
        assert_eq!(consumed, frame.len(), "フレーム全体を消費すること");
        let ControlMessage::RequestError(err) = decoded else {
            panic!("RequestError が期待された");
        };
        assert!(
            err.error_details.is_empty(),
            "適用外の UNSUPPORTED_PROPERTY Detail は保持しないこと"
        );
        assert!(
            err.error_details.unsupported_properties().is_empty(),
            "適用外の UNSUPPORTED_PROPERTY Detail は型としても取り出せないこと"
        );
        Ok(())
    })?;
    Ok(())
}
