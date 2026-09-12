use shiguredo_moqt::{
    error::MessageError,
    message::common::Location,
    message_parameter::LocationFilter,
    message_parameter::MessageParameter,
    message_parameter::MessageParameterValue,
    message_parameter::MessageParameters,
    message_parameter::PARAM_FILL_TIMEOUT,
    message_parameter::PARAM_OBJECT_PROPERTY_FILTER,
    message_parameter::PARAM_PRIORITY_FILTER,
    message_parameter::PARAM_SUBGROUP_FILTER,
    message_parameter::PARAM_TRACK_PROPERTY_FILTER,
    message_parameter::{
        PARAM_EXPIRES, PARAM_INCLUDE_PROPERTIES, PARAM_LOCATION_FILTER,
        PARAM_OBJECT_DELIVERY_TIMEOUT, PARAM_OBJECTID_FILTER,
    },
};

mod delta_encoding {
    use super::*;

    #[test]
    fn delta_type_correctness() {
        // type=0x02 (delta=2), type=0x08 (delta=6) の順でエンコードされるはず
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_OBJECT_DELIVERY_TIMEOUT,
            value: MessageParameterValue::VarInt(100),
        });
        params.push(MessageParameter {
            param_type: PARAM_EXPIRES,
            value: MessageParameterValue::VarInt(200),
        });
        let mut buf = Vec::new();
        params
            .encode(&mut buf)
            .expect("正当なテスト入力の encode は成功する");

        // count = 2
        assert_eq!(buf[0], 0x02);
        // delta = 2 (type=0x02 - prev=0)
        assert_eq!(buf[1], 0x02);

        let (decoded, consumed) =
            MessageParameters::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, buf.len());
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded.object_delivery_timeout(), Some(100));
        assert_eq!(decoded.expires(), Some(200));
    }

    #[test]
    fn encode_sorts_by_type() {
        // 逆順に push してもソートされてエンコードされる
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_EXPIRES, // 0x08
            value: MessageParameterValue::VarInt(200),
        });
        params.push(MessageParameter {
            param_type: PARAM_OBJECT_DELIVERY_TIMEOUT, // 0x02
            value: MessageParameterValue::VarInt(100),
        });
        let mut buf = Vec::new();
        params
            .encode(&mut buf)
            .expect("正当なテスト入力の encode は成功する");

        let (decoded, _) =
            MessageParameters::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(decoded.object_delivery_timeout(), Some(100));
        assert_eq!(decoded.expires(), Some(200));
    }
}

mod error_cases {
    use super::*;

    #[test]
    fn unknown_parameter_type() {
        // 未知のパラメータ型 0x7F を含むバイト列を手動構築する
        // count=1, delta=0x7F (未知の型), 値=0x00
        let buf = vec![0x01, 0x7F, 0x00];

        assert!(matches!(
            MessageParameters::decode(&buf),
            Err(MessageError::ProtocolViolation(_))
        ));
    }

    #[test]
    fn unexpected_eof_in_value() {
        // count=1, type=PARAM_LARGEST_OBJECT (Location: 2 varint 必要) だが値がない
        // count=1, delta=0x09, group=0x05, object なし
        let buf = vec![0x01, 0x09, 0x05];

        assert_eq!(
            MessageParameters::decode(&buf),
            Err(MessageError::UnexpectedEof)
        );
    }

    #[test]
    fn unexpected_eof_in_length_prefixed() {
        // count=1, type=PARAM_AUTHORIZATION_TOKEN (length-prefixed) だがデータが足りない
        let mut buf = Vec::new();
        // count = 1
        buf.push(0x01);
        // delta = 0x03 (PARAM_AUTHORIZATION_TOKEN)
        buf.push(0x03);
        // length = 10
        buf.push(0x0A);
        // データが 3 バイトしかない
        buf.extend_from_slice(&[0x01, 0x02, 0x03]);

        assert_eq!(
            MessageParameters::decode(&buf),
            Err(MessageError::UnexpectedEof)
        );
    }

    #[test]
    fn non_auth_token_duplicate_rejected() {
        // AUTHORIZATION_TOKEN 以外の重複は引き続き PROTOCOL_VIOLATION
        // 手動構築: count=2, delta=0x02 (OBJECT_DELIVERY_TIMEOUT), value=100,
        //           delta=0x00 (重複), value=200
        let buf = vec![
            0x02, // count = 2
            0x02, // delta = 0x02 (DELIVERY_TIMEOUT)
            0x64, // value = 100
            0x00, // delta = 0 (同一型の繰り返し)
            0x40, // value = 200 の上位
            0xC8, // value = 200 の下位
        ];

        assert!(matches!(
            MessageParameters::decode(&buf),
            Err(MessageError::ProtocolViolation(_))
        ));
    }

    #[test]
    fn location_filter_end_group_delta_overflow_is_protocol_violation() {
        // StartGroup + EndGroupDelta が 2^64 - 1 を超えたら PROTOCOL_VIOLATION
        // (draft-ietf-moq-transport-21 §3.3.1 (Location Filters))
        // 3 フィールド [group=1, object=0, delta=u64::MAX]: 1 + (2^64 - 1) が溢出する。
        // vi64 の u64::MAX は 9 バイト (0xFF x 9) で表す。
        let buf = vec![
            0x01, // count = 1
            0x21, // delta = PARAM_LOCATION_FILTER
            0x0B, // length = 11
            0x01, // StartGroup = 1
            0x00, // StartObject = 0
            0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // EndGroupDelta = u64::MAX
        ];

        assert!(matches!(
            MessageParameters::decode(&buf),
            Err(MessageError::ProtocolViolation(_))
        ));
    }

    #[test]
    fn malformed_location_filter_is_key_value_formatting_error() {
        // 5 フィールドは定義された serialization と一致しないため KEY_VALUE_FORMATTING_ERROR
        let buf = vec![
            0x01, // count = 1
            0x21, // delta = PARAM_LOCATION_FILTER
            0x05, // length = 5
            0x01, 0x02, 0x03, 0x04, 0x05,
        ];

        assert!(matches!(
            MessageParameters::decode(&buf),
            Err(MessageError::KeyValueFormattingError(_))
        ));
    }

    #[test]
    fn encode_rejects_malformed_location_filter() {
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_LOCATION_FILTER,
            value: MessageParameterValue::LengthPrefixed(vec![0x01, 0x02, 0x03, 0x04, 0x05]),
        });
        let mut buf = Vec::new();

        assert!(matches!(
            params.encode(&mut buf),
            Err(MessageError::KeyValueFormattingError(_))
        ));
    }
}

/// 型ごとに許可する variant の厳密性を検証する。
///
/// LengthPrefixed を共有する型 (0x03 / 0x21 / 0x23) を取り違えて encode すると、
/// decode 側が別 variant へ解釈したり拒否したりしてラウンドトリップが壊れるため、
/// encode 側で variant を厳密に拒否することを固定する。
mod param_encoding_strictness {
    use super::*;
    use shiguredo_moqt::message_parameter::{
        AuthorizationToken, PARAM_AUTHORIZATION_TOKEN, PARAM_FILL_PARAMETERS,
    };

    fn use_value_token() -> AuthorizationToken {
        AuthorizationToken::UseValue {
            token_type: 0,
            token_value: b"token".to_vec(),
        }
    }

    #[test]
    fn auth_token_type_rejects_length_prefixed() {
        // 0x03 に生の LengthPrefixed を encode することはできない
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_AUTHORIZATION_TOKEN,
            value: MessageParameterValue::LengthPrefixed(vec![0x03, 0x00]),
        });
        let mut buf = Vec::new();

        assert_eq!(
            params.encode(&mut buf),
            Err(MessageError::InvalidParameter),
            "AUTHORIZATION_TOKEN に LengthPrefixed は許可されないこと"
        );
    }

    #[test]
    fn auth_token_type_accepts_authorization_token() {
        // 0x03 は AuthorizationToken variant のみを受け付け、ラウンドトリップする
        let token = use_value_token();
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_AUTHORIZATION_TOKEN,
            value: MessageParameterValue::AuthorizationToken(token.clone()),
        });
        let mut buf = Vec::new();
        params
            .encode(&mut buf)
            .expect("AuthorizationToken variant の encode は成功する");

        let (decoded, consumed) = MessageParameters::decode(&buf)
            .expect("encode した AUTHORIZATION_TOKEN は decode できる");
        assert_eq!(consumed, buf.len());
        assert_eq!(decoded.authorization_tokens(), vec![&token]);
    }

    #[test]
    fn location_filter_type_rejects_authorization_token_variant() {
        // 0x21 に AuthorizationToken variant を encode することはできない
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_LOCATION_FILTER,
            value: MessageParameterValue::AuthorizationToken(use_value_token()),
        });
        let mut buf = Vec::new();

        assert_eq!(
            params.encode(&mut buf),
            Err(MessageError::InvalidParameter),
            "LOCATION_FILTER に AuthorizationToken は許可されないこと"
        );
    }

    #[test]
    fn location_filter_type_accepts_length_prefixed() {
        // 0x21 は LengthPrefixed のみを受け付ける
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_LOCATION_FILTER,
            value: MessageParameterValue::LengthPrefixed(
                LocationFilter::NextObject.encode_to_bytes(),
            ),
        });
        let mut buf = Vec::new();
        params
            .encode(&mut buf)
            .expect("LOCATION_FILTER の LengthPrefixed encode は成功する");
        let (decoded, _) =
            MessageParameters::decode(&buf).expect("encode した LOCATION_FILTER は decode できる");
        assert_eq!(
            decoded.location_filter_typed(),
            Ok(Some(LocationFilter::NextObject))
        );
    }

    #[test]
    fn fill_parameters_type_rejects_length_prefixed() {
        // 0x23 に生の LengthPrefixed を encode することはできない
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_FILL_PARAMETERS,
            value: MessageParameterValue::LengthPrefixed(Vec::new()),
        });
        let mut buf = Vec::new();

        assert_eq!(
            params.encode(&mut buf),
            Err(MessageError::InvalidParameter),
            "FILL_PARAMETERS に LengthPrefixed は許可されないこと"
        );
    }

    #[test]
    fn fill_parameters_type_accepts_fill_parameters() {
        // 0x23 は FillParameters variant のみを受け付ける
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_FILL_PARAMETERS,
            value: MessageParameterValue::FillParameters(MessageParameters::new()),
        });
        let mut buf = Vec::new();
        params
            .encode(&mut buf)
            .expect("FILL_PARAMETERS variant の encode は成功する");
        let (decoded, _) =
            MessageParameters::decode(&buf).expect("encode した FILL_PARAMETERS は decode できる");
        assert_eq!(decoded.fill_parameters(), Some(&MessageParameters::new()));
    }
}

/// AUTHORIZATION_TOKEN の (Token Type, Token Value) 一意性検証を検証する。
///
/// draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter):
/// alias 解決後の (Token Type, Token Value) の組み合わせは一意でなければならない。
/// encode / decode の両経路で同じ検証が働くことを固定する。
mod auth_token_uniqueness {
    use super::*;
    use shiguredo_moqt::message_parameter::{AuthorizationToken, PARAM_AUTHORIZATION_TOKEN};

    fn use_value_token(token_value: &[u8]) -> AuthorizationToken {
        AuthorizationToken::UseValue {
            token_type: 0,
            token_value: token_value.to_vec(),
        }
    }

    #[test]
    fn encode_rejects_duplicate_token_type_value() {
        let token = use_value_token(b"x");
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_AUTHORIZATION_TOKEN,
            value: MessageParameterValue::AuthorizationToken(token.clone()),
        });
        params.push(MessageParameter {
            param_type: PARAM_AUTHORIZATION_TOKEN,
            value: MessageParameterValue::AuthorizationToken(token),
        });
        let mut buf = Vec::new();

        assert!(
            matches!(
                params.encode(&mut buf),
                Err(MessageError::MalformedAuthToken(_))
            ),
            "同一 (Token Type, Token Value) の encode は MALFORMED_AUTH_TOKEN になること"
        );
    }

    #[test]
    fn encode_accepts_distinct_token_values() {
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_AUTHORIZATION_TOKEN,
            value: MessageParameterValue::AuthorizationToken(use_value_token(b"x")),
        });
        params.push(MessageParameter {
            param_type: PARAM_AUTHORIZATION_TOKEN,
            value: MessageParameterValue::AuthorizationToken(use_value_token(b"y")),
        });
        let mut buf = Vec::new();

        params
            .encode(&mut buf)
            .expect("(Token Type, Token Value) が異なれば encode できる");
        let (decoded, _) = MessageParameters::decode(&buf)
            .expect("(Token Type, Token Value) が異なれば decode できる");
        assert_eq!(decoded.authorization_tokens().len(), 2);
    }

    #[test]
    fn decode_rejects_duplicate_token_type_value() {
        // count=2, delta1=0x03, len=3, Token=[0x03, 0x00, 'x'],
        // delta2=0x00 (同一型の繰り返しは AUTHORIZATION_TOKEN では許可), len=3, 同一 Token
        let buf = vec![
            0x02, 0x03, 0x03, 0x03, 0x00, 0x78, 0x00, 0x03, 0x03, 0x00, 0x78,
        ];

        assert!(
            matches!(
                MessageParameters::decode(&buf),
                Err(MessageError::MalformedAuthToken(_))
            ),
            "同一 (Token Type, Token Value) の decode は MALFORMED_AUTH_TOKEN になること"
        );
    }
}

mod accessors {
    use super::*;

    #[test]
    fn missing_returns_none() {
        let params = MessageParameters::new();
        assert_eq!(params.object_delivery_timeout(), None);
        assert_eq!(params.authorization_tokens().len(), 0);
        assert_eq!(params.rendezvous_timeout(), None);
        assert!(!params.has_expires());
        assert_eq!(params.expires(), None);
        assert_eq!(params.largest_object(), None);
        assert_eq!(params.forward(), None);
        assert_eq!(params.subscriber_priority(), None);
        assert_eq!(params.location_filter(), None);
        assert_eq!(params.group_order(), None);
        assert_eq!(params.new_group_request(), None);
        assert_eq!(params.fill_timeout(), None);
    }

    /// FILL_TIMEOUT (type 0x0A) のアクセサ
    ///
    /// draft-ietf-moq-transport-21 §9.20.6 (FILL TIMEOUT Parameter)
    #[test]
    fn fill_timeout_accessor() {
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_FILL_TIMEOUT,
            value: MessageParameterValue::VarInt(1500),
        });
        assert_eq!(params.fill_timeout(), Some(1500));
    }

    /// FILL_TIMEOUT は SUBSCRIBE では拒否される (FETCH のみ許可)
    ///
    /// draft-ietf-moq-transport-21 §9.20.6 / §9.20.1 (Parameter Scope)
    /// FETCH 側の正常系は PBT の FETCH_PARAMS ラウンドトリップで担保する。
    #[test]
    fn fill_timeout_rejected_on_subscribe() {
        use shiguredo_moqt::{
            message::ControlMessage, message::Subscribe, message::common::TrackNamespace,
        };

        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_FILL_TIMEOUT,
            value: MessageParameterValue::VarInt(100),
        });
        let subscribe = ControlMessage::Subscribe(Subscribe {
            request_id: 1,
            track_namespace: TrackNamespace::new(vec![b"ns".to_vec()])
                .expect("正当な namespace である"),
            track_name: b"track".to_vec(),
            parameters: params,
        });
        assert!(
            matches!(subscribe.encode(), Err(MessageError::ProtocolViolation(_))),
            "FILL_TIMEOUT は SUBSCRIBE で拒否される"
        );
    }

    #[test]
    fn expires_zero_means_no_expiration() {
        // EXPIRES=0 は「期限なし」と扱い、expires() は None を返す。
        // draft-ietf-moq-transport-21 §9.20.17
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_EXPIRES,
            value: MessageParameterValue::VarInt(0),
        });
        assert!(params.has_expires());
        assert_eq!(params.expires(), None);
    }

    #[test]
    fn expires_non_zero_returns_some() {
        // EXPIRES=n（n > 0）は有効な期限値として Some(n) を返す。
        // draft-ietf-moq-transport-21 §9.20.17
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_EXPIRES,
            value: MessageParameterValue::VarInt(300),
        });
        assert!(params.has_expires());
        assert_eq!(params.expires(), Some(300));
    }

    #[test]
    fn typed_location_filter_accessor() {
        let filter = LocationFilter::AbsoluteStart {
            start: Location {
                group_id: 9,
                object_id: 4,
            },
        };
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_LOCATION_FILTER,
            value: MessageParameterValue::LengthPrefixed(filter.encode_to_bytes()),
        });

        assert_eq!(params.location_filter_typed(), Ok(Some(filter)));
    }

    /// 空の LOCATION_FILTER (Length 0 = no filter) は typed では None になる
    #[test]
    fn typed_location_filter_empty_is_none() {
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_LOCATION_FILTER,
            value: MessageParameterValue::LengthPrefixed(Vec::new()),
        });

        assert_eq!(params.location_filter_typed(), Ok(None));
    }

    /// INCLUDE_PROPERTIES (type 0x35) のアクセサと round-trip
    ///
    /// draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
    /// 0 (送らない) / 1 (送る)。不在時は default 1 として扱う。
    #[test]
    fn include_properties_accessor_and_round_trip() {
        for value in [0u8, 1u8] {
            let mut params = MessageParameters::new();
            params.push(MessageParameter {
                param_type: PARAM_INCLUDE_PROPERTIES,
                value: MessageParameterValue::Uint8(value),
            });
            assert_eq!(params.include_properties(), Some(value));
            let mut buf = Vec::new();
            params
                .encode(&mut buf)
                .expect("正当なテスト入力の encode は成功する");
            let (decoded, _) =
                MessageParameters::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
            assert_eq!(decoded.include_properties(), Some(value));
        }
        assert_eq!(MessageParameters::new().include_properties(), None);
    }

    /// INCLUDE_PROPERTIES の範囲外値 (2+) は encode/decode 両経路で拒否される
    ///
    /// draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter):
    /// 範囲外受信時は MUST close the session with PROTOCOL_VIOLATION。
    #[test]
    fn include_properties_out_of_range_rejected() {
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_INCLUDE_PROPERTIES,
            value: MessageParameterValue::Uint8(2),
        });
        let mut buf = Vec::new();
        assert!(
            params.encode(&mut buf).is_err(),
            "範囲外値の encode は失敗する"
        );
        // decode 経路: count=1, delta=0x35, 値=0x02 を手動構築する
        let raw = vec![0x01, 0x35, 0x02];
        assert!(
            matches!(
                MessageParameters::decode(&raw),
                Err(MessageError::ProtocolViolation(_))
            ),
            "範囲外値の decode は PROTOCOL_VIOLATION になる"
        );
    }

    #[test]
    fn empty_authorization_token_is_error() {
        // 空の AUTHORIZATION_TOKEN は Token 構造としてデコードできないため
        // KEY_VALUE_FORMATTING_ERROR になる
        // 手動でバイト列を構築する: count=1, delta=0x03, length=0
        let buf = vec![0x01, 0x03, 0x00];
        assert!(matches!(
            MessageParameters::decode(&buf),
            Err(MessageError::KeyValueFormattingError(_))
        ));
    }
}

/// draft-ietf-moq-transport-21 §3.3.1 (Location Filters): `location_filter_update` の 3 状態。
mod location_filter_update {
    use super::*;
    use shiguredo_moqt::message_parameter::LocationFilterUpdate;

    #[test]
    fn absent_is_unchanged() {
        // パラメータ省略時は値 unchanged になる
        assert_eq!(
            MessageParameters::new().location_filter_update(),
            Ok(LocationFilterUpdate::Unchanged)
        );
    }

    #[test]
    fn empty_is_removed() {
        // Length 0 は no filter であり、REQUEST_UPDATE ではフィルタ削除になる
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_LOCATION_FILTER,
            value: MessageParameterValue::LengthPrefixed(Vec::new()),
        });

        assert_eq!(
            params.location_filter_update(),
            Ok(LocationFilterUpdate::Removed)
        );
    }

    #[test]
    fn present_is_set() {
        // 非空の正当値は typed filter として返る
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_LOCATION_FILTER,
            value: MessageParameterValue::LengthPrefixed(
                LocationFilter::NextObject.encode_to_bytes(),
            ),
        });

        assert_eq!(
            params.location_filter_update(),
            Ok(LocationFilterUpdate::Set(LocationFilter::NextObject))
        );
    }

    #[test]
    fn malformed_is_err() {
        // 5 フィールドは定義された serialization と一致しない
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_LOCATION_FILTER,
            value: MessageParameterValue::LengthPrefixed(vec![0x01, 0x02, 0x03, 0x04, 0x05]),
        });

        assert!(matches!(
            params.location_filter_update(),
            Err(MessageError::KeyValueFormattingError(_))
        ));
    }

    #[test]
    fn empty_survives_encode_decode_round_trip() {
        // Length 0 の削除指示が wire 往復で保たれること
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_LOCATION_FILTER,
            value: MessageParameterValue::LengthPrefixed(Vec::new()),
        });
        let mut buf = Vec::new();
        params
            .encode(&mut buf)
            .expect("正当なテスト入力の encode は成功する");
        let (decoded, _) =
            MessageParameters::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(
            decoded.location_filter_update(),
            Ok(LocationFilterUpdate::Removed)
        );
    }
}

/// draft-ietf-moq-transport-21 §3.3.1 (Location Filters): LocationFilter の実効 Start / End Location 導出。
mod location_filter_derivation {
    use super::*;
    use shiguredo_moqt::message_parameter::LocationFilterContext;

    fn loc(group_id: u64, object_id: u64) -> Location {
        Location {
            group_id,
            object_id,
        }
    }

    // RelativeGroup / NextObject の通常導出 (group/object の関係) は
    // pbt/tests/prop_message_parameter.rs の property で網羅する。ここでは PBT が生成しない
    // 境界・特異値 (未配信 None、クランプ、overflow、open-ended / Fetch 終端、{0, 0} 正規化) を検証する。

    #[test]
    fn undelivered_falls_back_to_zero() {
        // 未配信 (largest = None) の相対フィルタはどちらも {0, 0}
        assert_eq!(
            LocationFilter::RelativeGroup { start_group: 3 }.effective_start_location(None),
            Some(loc(0, 0))
        );
        assert_eq!(
            LocationFilter::NextObject.effective_start_location(None),
            Some(loc(0, 0))
        );
    }

    #[test]
    fn relative_group_examples() {
        // StartGroup = 0 で Next Group、1 で現 Group、2 で 1 つ前の Group から始める
        let largest = loc(10, 7);
        assert_eq!(
            LocationFilter::RelativeGroup { start_group: 0 }
                .effective_start_location(Some(&largest)),
            Some(loc(11, 0))
        );
        assert_eq!(
            LocationFilter::RelativeGroup { start_group: 1 }
                .effective_start_location(Some(&largest)),
            Some(loc(10, 0))
        );
        assert_eq!(
            LocationFilter::RelativeGroup { start_group: 2 }
                .effective_start_location(Some(&largest)),
            Some(loc(9, 0))
        );
    }

    #[test]
    fn relative_group_clamps_negative_to_zero() {
        // 相対計算が 0 未満になる場合は 0 にクランプする
        let largest = loc(1, 7);
        assert_eq!(
            LocationFilter::RelativeGroup { start_group: 5 }
                .effective_start_location(Some(&largest)),
            Some(loc(0, 0))
        );
    }

    #[test]
    fn relative_group_clamps_overflow_to_max() {
        // Largest Group が u64::MAX でも panic せず u64::MAX にクランプする
        let largest = loc(u64::MAX, 7);
        assert_eq!(
            LocationFilter::RelativeGroup { start_group: 0 }
                .effective_start_location(Some(&largest)),
            Some(loc(u64::MAX, 0))
        );
    }

    #[test]
    fn next_object_overflow_saturates_to_largest() {
        // object = u64::MAX は正当な vi64 値だが object + 1 がオーバーフローするため
        // Largest 自体に飽和する (下限なしの全通しにはしない)
        let filter = LocationFilter::NextObject;
        let largest = loc(5, u64::MAX);
        assert_eq!(
            filter.effective_start_location(Some(&largest)),
            Some(loc(5, u64::MAX))
        );
    }

    #[test]
    fn absolute_start_zero_normalizes_to_next_object() {
        // 2 フィールドとも 0 は wire 上区別できないため NextObject に正規化される
        let bytes = LocationFilter::AbsoluteStart { start: loc(0, 0) }.encode_to_bytes();
        assert_eq!(
            LocationFilter::decode(&bytes),
            Ok(LocationFilter::NextObject)
        );
    }

    #[test]
    fn open_ended_filters_have_no_end_in_subscription() {
        // End フィールド省略時は subscription open-ended のため End なし
        let largest = loc(5, 9);
        for filter in [
            LocationFilter::RelativeGroup { start_group: 1 },
            LocationFilter::NextObject,
            LocationFilter::AbsoluteStart { start: loc(2, 3) },
        ] {
            assert_eq!(
                filter.effective_end_location(Some(&largest), LocationFilterContext::Subscription),
                None
            );
        }
    }

    #[test]
    fn open_ended_filters_end_at_largest_in_fetch() {
        // End フィールド省略時は Fetch の End = Largest Object になる
        let largest = loc(5, 9);
        for filter in [
            LocationFilter::RelativeGroup { start_group: 1 },
            LocationFilter::NextObject,
            LocationFilter::AbsoluteStart { start: loc(2, 3) },
        ] {
            assert_eq!(
                filter.effective_end_location(Some(&largest), LocationFilterContext::Fetch),
                Some(largest)
            );
        }
        // 未配信の Fetch は終端未定のため None
        assert_eq!(
            LocationFilter::NextObject.effective_end_location(None, LocationFilterContext::Fetch),
            None
        );
    }

    #[test]
    fn absolute_range_end_covers_whole_end_group() {
        // EndObject 省略時は End Group の全 Object を含む
        let filter = LocationFilter::AbsoluteRange {
            start: loc(2, 8),
            end_group_delta: 1,
        };
        assert_eq!(
            filter.effective_end_location(None, LocationFilterContext::Subscription),
            Some(loc(3, u64::MAX))
        );
    }

    #[test]
    fn absolute_range_with_end_location() {
        // 4 フィールドは {StartGroup + EndGroupDelta, EndObject} が End になる
        let filter = LocationFilter::AbsoluteRangeWithEnd {
            start: loc(2, 8),
            end_group_delta: 1,
            end_object: 4,
        };
        assert_eq!(
            filter.effective_end_location(None, LocationFilterContext::Subscription),
            Some(loc(3, 4))
        );
    }

    #[test]
    fn end_location_saturates_on_overflow() {
        // decode を経ない in-memory 構築で start.group + delta がオーバーフローしても panic せず
        // u64::MAX に飽和する
        let filter = LocationFilter::AbsoluteRange {
            start: loc(u64::MAX, 0),
            end_group_delta: 1,
        };
        assert_eq!(
            filter.effective_end_location(None, LocationFilterContext::Subscription),
            Some(loc(u64::MAX, u64::MAX))
        );
    }
}

mod kvp_value_length_limit {
    use super::*;

    /// 65536 バイトの LengthPrefixed 値はエンコード時に拒否される
    #[test]
    fn length_prefixed_65536_bytes_encode_error() {
        use shiguredo_moqt::message_parameter::PARAM_SUBGROUP_FILTER;
        // LOCATION_FILTER は encode 前の値形式検証 (validate_param_encoding) で先に落ちるため、
        // 値長検証だけを固定できる型を使う
        let param = MessageParameter {
            param_type: PARAM_SUBGROUP_FILTER,
            value: MessageParameterValue::LengthPrefixed(vec![0x01; 65536]),
        };
        let mut params = MessageParameters::new();
        params.push(param);
        let mut buf = Vec::new();
        assert!(
            matches!(
                params.encode(&mut buf),
                Err(MessageError::ProtocolViolation(_))
            ),
            "65536 バイトの LengthPrefixed 値は ProtocolViolation で拒否される"
        );
    }

    /// デコード側で 65536 バイト長の LengthPrefixed 値は ProtocolViolation
    #[test]
    fn decode_length_prefixed_65536_bytes_error() {
        use shiguredo_moqt::message_parameter::PARAM_LOCATION_FILTER;
        use shiguredo_moqt::varint;
        // カウント=1, delta_key=PARAM_LOCATION_FILTER, length=65536
        let mut buf = Vec::new();
        varint::encode(1, &mut buf); // count=1
        varint::encode(PARAM_LOCATION_FILTER, &mut buf); // delta_key
        varint::encode(65536, &mut buf); // length=65536 (over limit)
        buf.push(0); // dummy data byte
        let err = MessageParameters::decode(&buf).unwrap_err();
        assert!(
            matches!(err, MessageError::ProtocolViolation(_)),
            "Expected ProtocolViolation, got {err:?}"
        );
    }

    /// 65535 バイトの AuthorizationToken 値はエンコードでき、65536 バイトは拒否される
    #[test]
    fn authorization_token_length_boundary() {
        use shiguredo_moqt::message_parameter::{AuthorizationToken, PARAM_AUTHORIZATION_TOKEN};
        // REGISTER の値は alias type (1 バイト) + alias (値 0 の 1 バイト varint)
        // + token_type (値 0 の 1 バイト varint) + Token Value。
        // KVP 値長 = 3 + token_value_len となり、65532 + 3 = 65535 (上限) /
        // 65533 + 3 = 65536 (超過) が境界になる。
        for (token_value_len, expected_ok) in [(65532usize, true), (65533usize, false)] {
            let token = AuthorizationToken::Register {
                alias: 0,
                token_type: 0,
                token_value: vec![0x01; token_value_len],
            };
            let param = MessageParameter {
                param_type: PARAM_AUTHORIZATION_TOKEN,
                value: MessageParameterValue::AuthorizationToken(token.clone()),
            };
            let mut params = MessageParameters::new();
            params.push(param);
            let mut buf = Vec::new();
            let result = params.encode(&mut buf);
            if expected_ok {
                result.expect("65535 バイトの AuthorizationToken 値はエンコードできる");
                let (decoded, consumed) =
                    MessageParameters::decode(&buf).expect("エンコード結果を decode できる");
                assert_eq!(consumed, buf.len());
                assert_eq!(decoded.authorization_tokens(), vec![&token]);
            } else {
                assert!(
                    matches!(result, Err(MessageError::ProtocolViolation(_))),
                    "65536 バイトの AuthorizationToken 値は ProtocolViolation で拒否される"
                );
            }
        }
    }
}

/// draft-ietf-moq-transport-21 §3.3.2 (Range Filters): REQUEST_UPDATE での Range Filter マージ
///
/// "In REQUEST_UPDATE, Length can be 0 to remove a filter parameter or non-zero to replace
/// that entire filter parameter including all sets and Property Types."
/// "If a filter parameter is omitted from REQUEST_UPDATE, the value is unchanged."
///
/// 「filter parameter 全体の置換」なので、型単位で全インスタンスを削除してから追加する。
mod merge_from_range_filter {
    use super::*;

    /// SUBGROUP_FILTER パラメータを構築する (空バイト列を渡せば Length=0 の削除指示になる)
    fn subgroup_filter(bytes: Vec<u8>) -> MessageParameter {
        MessageParameter {
            param_type: PARAM_SUBGROUP_FILTER,
            value: MessageParameterValue::LengthPrefixed(bytes),
        }
    }

    /// 指定した型のパラメータが 1 つも残っていないことを確認する
    ///
    /// `range_filters()` は `LengthPrefixed` 以外の値を読み飛ばすため、
    /// 削除の確認には param_type の残存を直接見る。
    fn assert_no_param_of_type(params: &MessageParameters, param_type: u64) {
        assert!(
            params.as_slice().iter().all(|p| p.param_type != param_type),
            "型 {param_type:#x} のパラメータが残存していてはいけない"
        );
    }

    /// Length=0 が既存の全インスタンスを削除すること
    #[test]
    fn length_zero_removes_all_instances() {
        let mut base = MessageParameters::new();
        base.push(subgroup_filter(vec![0x00, 0x01]));
        base.push(subgroup_filter(vec![0x01, 0x02]));

        let mut update = MessageParameters::new();
        update.push(subgroup_filter(Vec::new()));
        base.merge_from(&update);

        assert_no_param_of_type(&base, PARAM_SUBGROUP_FILTER);
        assert_eq!(
            base.count_range_filters(),
            0,
            "削除後は Range が 1 つも残らないこと"
        );
    }

    /// Range Filter が省略された場合は既存値が維持されること
    ///
    /// `other` に Range Filter 型が 1 つも無いため削除パスが no-op になる唯一の経路。
    #[test]
    fn omitted_filter_preserves_existing_value() {
        let mut base = MessageParameters::new();
        base.push(subgroup_filter(vec![0x00, 0x05, 0x0A]));

        let mut update = MessageParameters::new();
        update.push(MessageParameter {
            param_type: PARAM_EXPIRES,
            value: MessageParameterValue::VarInt(300),
        });

        base.merge_from(&update);

        let expected: Vec<&[u8]> = vec![&[0x00, 0x05, 0x0A]];
        assert_eq!(
            base.range_filters(PARAM_SUBGROUP_FILTER),
            expected,
            "省略時に SUBGROUP_FILTER が維持されなければならない"
        );
    }

    /// 削除後に再度 non-zero で設定し直せること (REQUEST_UPDATE を 2 回に分けた場合)
    #[test]
    fn re_set_after_deletion() {
        let mut base = MessageParameters::new();
        base.push(subgroup_filter(vec![0x00, 0x01]));

        let mut delete_update = MessageParameters::new();
        delete_update.push(subgroup_filter(Vec::new()));
        base.merge_from(&delete_update);
        assert_no_param_of_type(&base, PARAM_SUBGROUP_FILTER);

        let mut set_update = MessageParameters::new();
        set_update.push(subgroup_filter(vec![0x01, 0x03, 0x07]));
        base.merge_from(&set_update);

        let expected: Vec<&[u8]> = vec![&[0x01, 0x03, 0x07]];
        assert_eq!(
            base.range_filters(PARAM_SUBGROUP_FILTER),
            expected,
            "削除後に再設定した値が反映されなければならない"
        );
    }

    /// 非ゼロ置換が既存の全インスタンスを削除してから update の全インスタンスを追加すること
    ///
    /// 削除パスと追加パスの分離が壊れた場合の回帰を検出する
    /// (分離の根拠は `MessageParameters::merge_from` のコメント参照)。
    #[test]
    fn non_zero_replaces_all_instances() {
        let mut base = MessageParameters::new();
        base.push(subgroup_filter(vec![0x00, 0x01]));
        base.push(subgroup_filter(vec![0x01, 0x02]));

        let mut update = MessageParameters::new();
        update.push(subgroup_filter(vec![0x05, 0x03]));
        update.push(subgroup_filter(vec![0x06, 0x04]));
        base.merge_from(&update);

        let expected: Vec<&[u8]> = vec![&[0x05, 0x03], &[0x06, 0x04]];
        assert_eq!(
            base.range_filters(PARAM_SUBGROUP_FILTER),
            expected,
            "既存 2 件が削除され update の 2 件がそのまま残らなければならない"
        );
    }

    /// 同一 REQUEST_UPDATE 内に Length=0 と非ゼロが混在したら順序によらず非ゼロが優先されること
    ///
    /// 削除を先にまとめて済ませるため `other` 内の並び順に依存しない。単一パス実装では
    /// 非ゼロが先に来た場合に後続の Length=0 がそれを消してしまう。
    #[test]
    fn non_zero_takes_precedence_over_length_zero() {
        for zero_first in [true, false] {
            let mut base = MessageParameters::new();
            base.push(subgroup_filter(vec![0x00, 0x01]));

            let mut update = MessageParameters::new();
            if zero_first {
                update.push(subgroup_filter(Vec::new()));
                update.push(subgroup_filter(vec![0x09, 0x02]));
            } else {
                update.push(subgroup_filter(vec![0x09, 0x02]));
                update.push(subgroup_filter(Vec::new()));
            }
            base.merge_from(&update);

            let expected: Vec<&[u8]> = vec![&[0x09, 0x02]];
            assert_eq!(
                base.range_filters(PARAM_SUBGROUP_FILTER),
                expected,
                "Length=0 が先か後か (zero_first={zero_first}) によらず非ゼロが残らなければならない"
            );
        }
    }

    /// Range Filter 型の値が `LengthPrefixed` 以外でも型単位の置換対象になること
    ///
    /// この状態は公開 API による in-memory 構築でのみ作れる (decode は 0x25-0x29 を必ず
    /// `LengthPrefixed` にする)。`merge_from` の doc が規定した挙動を固定する。
    #[test]
    fn non_length_prefixed_range_filter_still_replaces_type() {
        let mut base = MessageParameters::new();
        base.push(subgroup_filter(vec![0x00, 0x01]));

        let mut update = MessageParameters::new();
        update.push(MessageParameter {
            param_type: PARAM_SUBGROUP_FILTER,
            value: MessageParameterValue::VarInt(5),
        });
        base.merge_from(&update);

        // 既存インスタンスは型単位で消え、不正な値だけが残る
        assert!(
            base.range_filters(PARAM_SUBGROUP_FILTER).is_empty(),
            "LengthPrefixed 以外は range_filters() から読み飛ばされる"
        );
        assert_eq!(
            base.as_slice()
                .iter()
                .filter(|p| p.param_type == PARAM_SUBGROUP_FILTER)
                .count(),
            1,
            "型単位の置換で update の値だけが残らなければならない"
        );

        // 残った不正な値は encode と validate の双方で検出される
        let mut buf = Vec::new();
        assert_eq!(
            base.encode(&mut buf),
            Err(MessageError::InvalidParameter),
            "型と値形式の不一致は encode で検出されなければならない"
        );
        assert!(
            matches!(
                base.validate_range_filters(),
                Err(MessageError::ProtocolViolation(_))
            ),
            "型と値形式の不一致は validate_range_filters でも検出されなければならない"
        );
    }

    /// update に含まれない Range Filter 型は変更されないこと
    #[test]
    fn other_range_filter_types_are_untouched() {
        let mut base = MessageParameters::new();
        base.push(subgroup_filter(vec![0x00, 0x01]));
        base.push(MessageParameter {
            param_type: PARAM_OBJECTID_FILTER,
            value: MessageParameterValue::LengthPrefixed(vec![0x00, 0x05]),
        });

        let mut update = MessageParameters::new();
        update.push(subgroup_filter(Vec::new()));
        base.merge_from(&update);

        assert_no_param_of_type(&base, PARAM_SUBGROUP_FILTER);
        let expected: Vec<&[u8]> = vec![&[0x00, 0x05]];
        assert_eq!(
            base.range_filters(PARAM_OBJECTID_FILTER),
            expected,
            "update に含まれない型は変更されてはいけない"
        );
    }
}

/// draft-ietf-moq-transport-21 §3.3.2 (Range Filters): `validate_range_filters()` の単体テスト
///
/// 正常系の受理は `pbt/tests/prop_message_parameter.rs` の
/// `validate_range_filters_errs_exactly_on_duplicate_keys` に任せ、
/// ここでは PBT が生成しにくいエラーパス・境界値のみを検証する。
///
/// 同一 Parameter Type の複数出現そのものは §3.3.2 で許可されており encode/decode 層を通る。
/// (Parameter Type, SetID, Property Type) 単位の重複拒否は
/// `range_filter_multiple_appearance` モジュールで検証する。
mod validate_range_filters {
    use super::*;
    use shiguredo_moqt::varint;

    /// Start デルタが u64 を溢出する場合は拒否される
    #[test]
    fn start_delta_overflow_is_rejected() {
        // Range1: Start=0, End=u64::MAX → prev_end = MAX
        // Range2: Start_delta=1 → MAX + 1 で overflow
        let mut bytes = Vec::new();
        bytes.push(0x00); // SetID
        varint::encode(0, &mut bytes); // Start1
        varint::encode(u64::MAX, &mut bytes); // End1
        varint::encode(1, &mut bytes); // Start2_delta → overflow

        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_SUBGROUP_FILTER,
            value: MessageParameterValue::LengthPrefixed(bytes),
        });
        assert!(matches!(
            params.validate_range_filters(),
            Err(MessageError::ProtocolViolation(_))
        ));
    }

    /// End デルタが u64 を溢出する場合は拒否される
    #[test]
    fn end_delta_overflow_is_rejected() {
        // SetID=0, Start=1, End_delta = u64::MAX → 1 + MAX overflow
        let mut bytes = Vec::new();
        bytes.push(0x00);
        varint::encode(1, &mut bytes); // Start
        varint::encode(u64::MAX, &mut bytes); // End_delta → overflow

        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_SUBGROUP_FILTER,
            value: MessageParameterValue::LengthPrefixed(bytes),
        });
        assert!(matches!(
            params.validate_range_filters(),
            Err(MessageError::ProtocolViolation(_))
        ));
    }

    /// PRIORITY_FILTER の Start 値が 255 を超える場合は拒否される
    ///
    /// draft-ietf-moq-transport-21 §9.20.13
    #[test]
    fn priority_filter_value_over_255_is_rejected() {
        // SetID=0, Start=256 (> 255)
        let mut bytes = Vec::new();
        bytes.push(0x00);
        varint::encode(256, &mut bytes);

        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_PRIORITY_FILTER,
            value: MessageParameterValue::LengthPrefixed(bytes),
        });
        assert!(matches!(
            params.validate_range_filters(),
            Err(MessageError::ProtocolViolation(_))
        ));
    }

    /// PRIORITY_FILTER の End 値が 255 を超える場合は拒否される
    ///
    /// draft-ietf-moq-transport-21 §9.20.13
    #[test]
    fn priority_filter_end_over_255_is_rejected() {
        // SetID=0, Start=200, End_delta=100 → absolute End=300 (> 255)
        let mut bytes = Vec::new();
        bytes.push(0x00);
        varint::encode(200, &mut bytes);
        varint::encode(100, &mut bytes);

        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_PRIORITY_FILTER,
            value: MessageParameterValue::LengthPrefixed(bytes),
        });
        assert!(matches!(
            params.validate_range_filters(),
            Err(MessageError::ProtocolViolation(_))
        ));
    }

    /// PRIORITY_FILTER の値 255 は受理される (境界)
    #[test]
    fn priority_filter_value_255_passes() {
        let mut bytes = Vec::new();
        bytes.push(0x00);
        varint::encode(255, &mut bytes);

        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_PRIORITY_FILTER,
            value: MessageParameterValue::LengthPrefixed(bytes),
        });
        assert!(params.validate_range_filters().is_ok());
    }

    /// OBJECT_PROPERTY_FILTER の奇数 Property Type は拒否される
    ///
    /// draft-ietf-moq-transport-21 §9.20.14: Property Type MUST be even
    #[test]
    fn object_property_filter_odd_property_type_is_rejected() {
        // SetID=0, Property Type=0x03 (奇数), Start=1
        let mut bytes = Vec::new();
        bytes.push(0x00);
        varint::encode(0x03, &mut bytes);
        varint::encode(1, &mut bytes);

        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_OBJECT_PROPERTY_FILTER,
            value: MessageParameterValue::LengthPrefixed(bytes),
        });
        assert!(matches!(
            params.validate_range_filters(),
            Err(MessageError::ProtocolViolation(_))
        ));
    }

    /// TRACK_PROPERTY_FILTER の奇数 Property Type は拒否される
    #[test]
    fn track_property_filter_odd_property_type_is_rejected() {
        let mut bytes = Vec::new();
        bytes.push(0x00);
        varint::encode(0x01, &mut bytes); // 奇数
        varint::encode(1, &mut bytes);

        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_TRACK_PROPERTY_FILTER,
            value: MessageParameterValue::LengthPrefixed(bytes),
        });
        assert!(matches!(
            params.validate_range_filters(),
            Err(MessageError::ProtocolViolation(_))
        ));
    }
}

/// draft-ietf-moq-transport-21 §3.3.2 (Range Filters): 同一 Parameter Type の複数出現
///
/// §3.3.2 は TRACK_PROPERTY_FILTER (0x29) を SUBSCRIBE_TRACKS / REQUEST_UPDATE で、
/// その他 4 種 (0x25-0x28) を FETCH / SUBSCRIBE / SUBSCRIBE_TRACKS / PUBLISH_OK /
/// REQUEST_UPDATE (subscription 上、subscriber からのみ) で "MAY appear multiple times" と
/// 規定する。1 インスタンスは 1 SetID しか持てず SetID ごとの結果は OR 結合されるため、
/// 複数 SetID を表現するには複数出現が必須になる。
///
/// 重複拒否の粒度は (Parameter Type, SetID, Property Type) であり、これは codec 層 (§9.20) では
/// なく `validate_range_filters()` (§3.3.2) の管轄になる。この 2 層の棲み分けを検証する。
///
/// 複数出現の round-trip そのものは `pbt/tests/prop_message_parameter.rs` の
/// `range_filter_instances_survive_roundtrip` が任意個数・任意型で担保する。ここでは
/// PBT で表現できないワイヤバイト列の断定と、重複判定キーの境界だけを扱う。
mod range_filter_multiple_appearance {
    use super::*;
    use shiguredo_moqt::varint;

    /// Property Type を持たない Range Filter (0x25-0x27) の 1 Range 分のバイト列を作る
    ///
    /// ワイヤフォーマット: SetID (1 byte) | Start (vi64)。End を省略した open-ended な 1 Range。
    fn filter_bytes(set_id: u8, start: u64) -> Vec<u8> {
        let mut bytes = vec![set_id];
        varint::encode(start, &mut bytes);
        bytes
    }

    /// Property Type を持つ Range Filter (0x28 / 0x29) の 1 Range 分のバイト列を作る
    ///
    /// ワイヤフォーマット: SetID (1 byte) | Property Type (vi64) | Start (vi64)。
    /// Property Type は §9.20.14 / §9.20.15 により偶数でなければならない。
    fn property_filter_bytes(set_id: u8, property_type: u64, start: u64) -> Vec<u8> {
        let mut bytes = vec![set_id];
        varint::encode(property_type, &mut bytes);
        varint::encode(start, &mut bytes);
        bytes
    }

    fn filter_param(param_type: u64, bytes: Vec<u8>) -> MessageParameter {
        MessageParameter {
            param_type,
            value: MessageParameterValue::LengthPrefixed(bytes),
        }
    }

    /// 同一 Parameter Type ・異なる SetID の 2 件が round-trip すること
    ///
    /// 2 件目は Type Delta = 0 でエンコードされる。修正前はこの delta=0 を
    /// decode 側が PROTOCOL_VIOLATION として拒否していた。
    #[test]
    fn same_type_different_set_id_roundtrips() {
        let mut params = MessageParameters::new();
        params.push(filter_param(PARAM_SUBGROUP_FILTER, filter_bytes(0, 3)));
        params.push(filter_param(PARAM_SUBGROUP_FILTER, filter_bytes(1, 7)));

        let mut buf = Vec::new();
        params
            .encode(&mut buf)
            .expect("複数出現が許可された Range Filter の encode は成功しなければならない");

        assert_eq!(
            buf,
            vec![
                0x02, // パラメータ数 = 2
                0x25, // 1 件目: Type Delta = 0x25 (SUBGROUP_FILTER)
                0x02, // Length = 2
                0x00, // SetID = 0
                0x03, // Start = 3
                0x00, // 2 件目: Type Delta = 0 (同一型の繰り返し)
                0x02, // Length = 2
                0x01, // SetID = 1
                0x07, // Start = 7
            ],
            "2 件目は Type Delta = 0 でエンコードされなければならない"
        );

        let (decoded, consumed) =
            MessageParameters::decode(&buf).expect("同一型 2 件の decode は成功しなければならない");
        assert_eq!(consumed, buf.len());
        assert_eq!(
            decoded, params,
            "round-trip で出現順と値の両方が保たれなければならない"
        );
        assert!(
            decoded.validate_range_filters().is_ok(),
            "SetID が異なるので (Type, SetID) 重複検証を通らなければならない"
        );
    }

    /// 0x28 / 0x29 は (Type, SetID, Property Type) が完全一致したときだけ重複として拒否されること
    #[test]
    fn duplicate_type_set_id_property_type_is_rejected() {
        for param_type in [PARAM_OBJECT_PROPERTY_FILTER, PARAM_TRACK_PROPERTY_FILTER] {
            let mut params = MessageParameters::new();
            params.push(filter_param(param_type, property_filter_bytes(1, 0x02, 3)));
            params.push(filter_param(param_type, property_filter_bytes(1, 0x02, 9)));

            // codec 層 (§9.20) は複数出現を通す
            let mut buf = Vec::new();
            assert!(
                params.encode(&mut buf).is_ok(),
                "型 {param_type:#x} の重複は codec 層では拒否してはいけない"
            );
            let Ok((decoded, _)) = MessageParameters::decode(&buf) else {
                panic!("型 {param_type:#x} の重複は codec 層では拒否してはいけない");
            };

            // §3.3.2 層で重複として拒否される
            assert!(
                matches!(
                    decoded.validate_range_filters(),
                    Err(MessageError::ProtocolViolation(_))
                ),
                "型 {param_type:#x} は (Type, SetID, Property Type) 同一なら拒否されなければならない"
            );
        }
    }

    /// 0x28 / 0x29 は SetID が同じでも Property Type が異なれば受理されること (AND 結合の正常系)
    #[test]
    fn same_set_id_different_property_type_passes() {
        let mut params = MessageParameters::new();
        params.push(filter_param(
            PARAM_OBJECT_PROPERTY_FILTER,
            property_filter_bytes(1, 0x02, 3),
        ));
        params.push(filter_param(
            PARAM_OBJECT_PROPERTY_FILTER,
            property_filter_bytes(1, 0x04, 3),
        ));

        assert!(
            params.validate_range_filters().is_ok(),
            "同一 SetID でも Property Type が異なれば AND 結合の別条件として受理されなければならない"
        );
    }

    /// 0x25-0x27 は Property Type を持たないので (Type, SetID) 同一で拒否されること
    ///
    /// あわせて SetID が異なれば codec 層を往復できることも確認する。0x26 / 0x27 は
    /// ワイヤバイト列を断定するテスト (0x25) にも Property Type のテスト (0x28 / 0x29) にも
    /// 含まれないため、ここが唯一の決定的な codec 往復になる。
    #[test]
    fn duplicate_type_set_id_is_rejected_for_filters_without_property_type() {
        for param_type in [
            PARAM_SUBGROUP_FILTER,
            PARAM_OBJECTID_FILTER,
            PARAM_PRIORITY_FILTER,
        ] {
            let mut duplicated = MessageParameters::new();
            duplicated.push(filter_param(param_type, filter_bytes(2, 3)));
            duplicated.push(filter_param(param_type, filter_bytes(2, 9)));

            assert!(
                matches!(
                    duplicated.validate_range_filters(),
                    Err(MessageError::ProtocolViolation(_))
                ),
                "型 {param_type:#x} は (Type, SetID) が同一なら拒否されなければならない"
            );

            // SetID が異なれば別キーなので受理され、codec 層も往復できる
            let mut distinct = MessageParameters::new();
            distinct.push(filter_param(param_type, filter_bytes(2, 3)));
            distinct.push(filter_param(param_type, filter_bytes(3, 9)));

            let mut buf = Vec::new();
            assert!(
                distinct.encode(&mut buf).is_ok(),
                "型 {param_type:#x} の複数出現は encode できなければならない"
            );
            let Ok((decoded, consumed)) = MessageParameters::decode(&buf) else {
                panic!("型 {param_type:#x} の複数出現は decode できなければならない");
            };
            assert_eq!(consumed, buf.len());
            assert_eq!(
                decoded, distinct,
                "型 {param_type:#x} の round-trip で出現順と値が保たれなければならない"
            );
            assert!(
                decoded.validate_range_filters().is_ok(),
                "型 {param_type:#x} は SetID が異なるので重複検証を通らなければならない"
            );
        }
    }

    /// Length=0 のインスタンスは重複検証の対象外であること
    ///
    /// Length=0 は REQUEST_UPDATE でのフィルタ削除を表し、ワイヤ上に SetID フィールドを持たない。
    /// 合成キー SetID=0 として扱うと SetID=0 の非ゼロインスタンスと誤って衝突するため除外する。
    ///
    /// なお §3.3.2 は SetID を持たない Length=0 同士の重複について何も規定していないため、
    /// 「Length=0 同士を受理する」のは仕様の要求ではなく本実装の選択である。
    #[test]
    fn length_zero_is_excluded_from_duplicate_check() {
        // Length=0 と SetID=0 の非ゼロが同一メッセージに混在しても誤拒否されない
        for param_type in [PARAM_SUBGROUP_FILTER, PARAM_OBJECT_PROPERTY_FILTER] {
            // Length=0 のペイロードはどの型でも空バイト列で同じだが、衝突相手となる非ゼロ側の
            // 判定キーは 0x25 が (型, SetID, None)、0x28 が (型, SetID, Some(Property Type)) と
            // 形が違う。どちらの形でも Length=0 が衝突しないことを見る。
            let non_empty = if param_type == PARAM_OBJECT_PROPERTY_FILTER {
                property_filter_bytes(0, 0x02, 3)
            } else {
                filter_bytes(0, 3)
            };
            let mut mixed = MessageParameters::new();
            mixed.push(filter_param(param_type, Vec::new()));
            mixed.push(filter_param(param_type, non_empty));
            assert!(
                mixed.validate_range_filters().is_ok(),
                "型 {param_type:#x} の Length=0 は SetID を持たないので SetID=0 の非ゼロと衝突してはいけない"
            );
        }

        // Length=0 同士が並んでも重複にはならない
        let mut both_empty = MessageParameters::new();
        both_empty.push(filter_param(PARAM_SUBGROUP_FILTER, Vec::new()));
        both_empty.push(filter_param(PARAM_SUBGROUP_FILTER, Vec::new()));
        assert!(
            both_empty.validate_range_filters().is_ok(),
            "Length=0 同士は重複検証の対象外として受理する (本実装の選択)"
        );
    }

    /// `range_filters()` が同一型の全インスタンスを出現順に返すこと
    ///
    /// 非 Range Filter 型に対するガードは、LengthPrefixed でありながら Range Filter ではない
    /// LOCATION_FILTER (0x21) を実際に積んで検証する。値の型で弾かれる型 (EXPIRES 等) では
    /// ガードを外しても素通りしてしまい、検証にならない。
    #[test]
    fn range_filters_returns_all_instances_in_order() {
        let mut params = MessageParameters::new();
        params.push(filter_param(PARAM_SUBGROUP_FILTER, filter_bytes(0, 3)));
        params.push(filter_param(PARAM_OBJECTID_FILTER, filter_bytes(0, 1)));
        params.push(filter_param(PARAM_SUBGROUP_FILTER, filter_bytes(2, 7)));
        params.push(MessageParameter {
            param_type: PARAM_LOCATION_FILTER,
            value: MessageParameterValue::LengthPrefixed(
                LocationFilter::NextObject.encode_to_bytes(),
            ),
        });

        let expected: Vec<&[u8]> = vec![&[0x00, 0x03], &[0x02, 0x07]];
        assert_eq!(
            params.range_filters(PARAM_SUBGROUP_FILTER),
            expected,
            "SUBGROUP_FILTER の全インスタンスが push 順で返されなければならない"
        );

        assert!(
            params.range_filters(PARAM_LOCATION_FILTER).is_empty(),
            "LengthPrefixed でも Range Filter 型でなければ空の Vec を返さなければならない"
        );
        assert!(
            params.range_filters(PARAM_PRIORITY_FILTER).is_empty(),
            "Range Filter 型でもインスタンスが無ければ空の Vec を返さなければならない"
        );
    }

    /// `has_range_filters()` が Range Filter 型以外を数えないこと
    ///
    /// LOCATION_FILTER (0x21) は LengthPrefixed だが Range Filter ではない。
    /// 判定が「パラメータが空でないか」に退化すると true を返してしまい、
    /// Range Filter を含まないメッセージにまで §5.1.4 の検証が走る。
    #[test]
    fn has_range_filters_ignores_non_range_filter_types() {
        let mut params = MessageParameters::new();
        assert!(
            !params.has_range_filters(),
            "空の MessageParameters は Range Filter を持たない"
        );

        params.push(MessageParameter {
            param_type: PARAM_LOCATION_FILTER,
            value: MessageParameterValue::LengthPrefixed(
                LocationFilter::NextObject.encode_to_bytes(),
            ),
        });
        params.push(MessageParameter {
            param_type: PARAM_EXPIRES,
            value: MessageParameterValue::VarInt(300),
        });
        assert!(
            !params.has_range_filters(),
            "Range Filter 型以外だけなら false を返さなければならない"
        );

        params.push(filter_param(PARAM_SUBGROUP_FILTER, filter_bytes(0, 3)));
        assert!(
            params.has_range_filters(),
            "Range Filter が 1 件でもあれば true を返さなければならない"
        );
    }

    /// Range Filter 以外の同一型重複は encode で引き続き拒否されること
    #[test]
    fn non_range_filter_duplicate_still_rejected_on_encode() {
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_EXPIRES,
            value: MessageParameterValue::VarInt(100),
        });
        params.push(MessageParameter {
            param_type: PARAM_EXPIRES,
            value: MessageParameterValue::VarInt(200),
        });

        let mut buf = Vec::new();
        assert_eq!(
            params.encode(&mut buf),
            Err(MessageError::InvalidParameter),
            "Range Filter 以外の重複は従来どおり拒否されなければならない"
        );
    }
}

mod fill_parameters {
    use super::*;
    use shiguredo_moqt::message_parameter::{
        PARAM_FILL_PARAMETERS, PARAM_FILL_TIMEOUT, PARAM_GROUP_ORDER, PARAM_SUBSCRIBER_PRIORITY,
    };

    /// FILL 内側パラメータ群を作る (Table 6 の正当な組み合わせ)
    fn sample_inner() -> MessageParameters {
        let mut inner = MessageParameters::new();
        inner.push(MessageParameter {
            param_type: PARAM_FILL_TIMEOUT,
            value: MessageParameterValue::VarInt(100),
        });
        inner.push(MessageParameter {
            param_type: PARAM_SUBSCRIBER_PRIORITY,
            value: MessageParameterValue::Uint8(5),
        });
        inner.push(MessageParameter {
            param_type: PARAM_LOCATION_FILTER,
            value: MessageParameterValue::LengthPrefixed(
                LocationFilter::AbsoluteStart {
                    start: Location {
                        group_id: 1,
                        object_id: 2,
                    },
                }
                .encode_to_bytes(),
            ),
        });
        inner.push(MessageParameter {
            param_type: PARAM_GROUP_ORDER,
            value: MessageParameterValue::Uint8(1),
        });
        inner.push(MessageParameter {
            param_type: PARAM_SUBGROUP_FILTER,
            value: MessageParameterValue::LengthPrefixed(vec![0x00, 0x00, 0x01]),
        });
        inner
    }

    fn fill_param(inner: MessageParameters) -> MessageParameter {
        MessageParameter {
            param_type: PARAM_FILL_PARAMETERS,
            value: MessageParameterValue::FillParameters(inner),
        }
    }

    #[test]
    fn round_trip_nested_parameters() {
        // 内側パラメータ群の encode/decode 往復と、外側との同型共存を確認する
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_LOCATION_FILTER,
            value: MessageParameterValue::LengthPrefixed(
                LocationFilter::NextObject.encode_to_bytes(),
            ),
        });
        params.push(fill_param(sample_inner()));

        let mut buf = Vec::new();
        params
            .encode(&mut buf)
            .expect("正当なテスト入力の encode は成功する");
        let (decoded, consumed) =
            MessageParameters::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, buf.len());
        // 外側 LOCATION_FILTER と内側は別スコープで両方保持される
        assert!(decoded.location_filter().is_some());
        let inner = decoded
            .fill_parameters()
            .expect("FILL_PARAMETERS が保持されること");
        assert_eq!(inner.fill_timeout(), Some(100));
        assert_eq!(inner.subscriber_priority(), Some(5));
        assert_eq!(inner.group_order(), Some(1));
        assert_eq!(inner.range_filters(PARAM_SUBGROUP_FILTER).len(), 1);
        // 再エンコードは一致する
        let mut buf2 = Vec::new();
        decoded
            .encode(&mut buf2)
            .expect("デコード結果の再 encode は成功する");
        assert_eq!(buf, buf2);
    }

    #[test]
    fn empty_fill_accepted() {
        // 空の FILL_PARAMETERS は内側パラメータなしとして受け付ける
        let mut params = MessageParameters::new();
        params.push(fill_param(MessageParameters::new()));

        let mut buf = Vec::new();
        params
            .encode(&mut buf)
            .expect("正当なテスト入力の encode は成功する");
        let (decoded, consumed) =
            MessageParameters::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, buf.len());
        let inner = decoded
            .fill_parameters()
            .expect("FILL_PARAMETERS が保持されること");
        assert!(inner.is_empty());
    }

    #[test]
    fn table6_violation_on_decode_is_protocol_violation() {
        // 内側に Table 6 外 (AUTHORIZATION_TOKEN) を含むと PROTOCOL_VIOLATION
        // 内側: count=1, delta=0x03, len=3, USE_VALUE(token_type=1, value=[9])
        let inner = vec![0x01, 0x03, 0x03, 0x03, 0x01, 0x09];
        // 外側: count=1, delta=0x23, len=inner.len(), inner...
        let mut buf = vec![0x01, 0x23, inner.len() as u8];
        buf.extend_from_slice(&inner);

        assert_eq!(
            MessageParameters::decode(&buf),
            Err(MessageError::ProtocolViolation(
                "message parameter not allowed in this message type"
            )),
            "Table 6 外の内側パラメータは PROTOCOL_VIOLATION であること"
        );
    }

    #[test]
    fn nested_fill_on_decode_is_protocol_violation() {
        // FILL の内側に FILL (0x23) を含むと PROTOCOL_VIOLATION (別スコープの証拠)
        // 内側: count=1, delta=0x23, len=0
        let inner = vec![0x01, 0x23, 0x00];
        let mut buf = vec![0x01, 0x23, inner.len() as u8];
        buf.extend_from_slice(&inner);

        assert!(
            matches!(
                MessageParameters::decode(&buf),
                Err(MessageError::ProtocolViolation(_))
            ),
            "FILL の入れ子は PROTOCOL_VIOLATION であること"
        );
    }

    #[test]
    fn nested_fill_rejected_before_recursion() {
        // FILL の内側に FILL (0x23) が現れた時点で、再帰する前に拒否されること。
        // 再帰後に validate_scope で弾く実装では深い入れ子でスタックオーバーフローする。
        // 内側: count=1, delta=0x23, len=0
        let inner = vec![0x01, 0x23, 0x00];
        let mut buf = vec![0x01, 0x23, inner.len() as u8];
        buf.extend_from_slice(&inner);

        assert_eq!(
            MessageParameters::decode(&buf),
            Err(MessageError::ProtocolViolation(
                "FILL_PARAMETERS must not be nested"
            )),
            "FILL の入れ子は再帰前に拒否されること"
        );
    }

    #[test]
    fn trailing_bytes_inside_fill_is_key_value_formatting_error() {
        // 内側の末尾に余剰バイトがあると KEY_VALUE_FORMATTING_ERROR
        // 内側: count=0 の後に 0xFF が残る
        let inner = vec![0x00, 0xFF];
        let mut buf = vec![0x01, 0x23, inner.len() as u8];
        buf.extend_from_slice(&inner);

        assert!(
            matches!(
                MessageParameters::decode(&buf),
                Err(MessageError::KeyValueFormattingError(_))
            ),
            "内側の余剰バイトは KEY_VALUE_FORMATTING_ERROR であること"
        );
    }

    #[test]
    fn duplicate_fill_rejected() {
        // FILL_PARAMETERS の重複は encode/decode の両方で拒否する (単一出現のみ)
        let mut params = MessageParameters::new();
        params.push(fill_param(MessageParameters::new()));
        params.push(fill_param(MessageParameters::new()));
        let mut buf = Vec::new();
        assert_eq!(
            params.encode(&mut buf),
            Err(MessageError::InvalidParameter),
            "FILL の重複 encode は拒否されること"
        );

        // 外側: count=2, FILL(len=0), delta=0 の FILL(len=0)
        let wire = vec![0x02, 0x23, 0x00, 0x00, 0x00];
        assert!(
            matches!(
                MessageParameters::decode(&wire),
                Err(MessageError::ProtocolViolation(_))
            ),
            "FILL の重複 decode は拒否されること"
        );
    }

    #[test]
    fn fill_variant_under_wrong_type_rejected_on_encode() {
        // typed variant は型 0x23 専用であり、他の length-prefixed 型では拒否する
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_LOCATION_FILTER,
            value: MessageParameterValue::FillParameters(MessageParameters::new()),
        });
        let mut buf = Vec::new();
        assert_eq!(
            params.encode(&mut buf),
            Err(MessageError::InvalidParameter),
            "他型での FillParameters variant は拒否されること"
        );
    }

    #[test]
    fn raw_bytes_under_fill_type_rejected_on_encode() {
        // 型 0x23 は typed variant のみ受け付け、生バイト列での構築は認めない
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_FILL_PARAMETERS,
            value: MessageParameterValue::LengthPrefixed(Vec::new()),
        });
        let mut buf = Vec::new();
        assert_eq!(
            params.encode(&mut buf),
            Err(MessageError::InvalidParameter),
            "型 0x23 の生バイト列は拒否されること"
        );
    }

    #[test]
    fn fill_inner_group_order_out_of_range_rejected() {
        // 内側の値域検証も encode で行う (GROUP_ORDER=3 は Ascending/Descending 以外で不正)
        let mut inner = MessageParameters::new();
        inner.push(MessageParameter {
            param_type: PARAM_GROUP_ORDER,
            value: MessageParameterValue::Uint8(3),
        });
        let mut params = MessageParameters::new();
        params.push(fill_param(inner));
        let mut buf = Vec::new();
        assert!(
            params.encode(&mut buf).is_err(),
            "内側の不正値は encode で拒否されること"
        );
    }

    #[test]
    fn fill_inner_track_property_filter_rejected_on_decode() {
        // TRACK_PROPERTY_FILTER (0x29) は Table 6 外のため decode で PROTOCOL_VIOLATION
        // 内側: count=1, delta=0x29, len=0
        let inner = vec![0x01, 0x29, 0x00];
        let mut buf = vec![0x01, 0x23, inner.len() as u8];
        buf.extend_from_slice(&inner);

        assert!(
            matches!(
                MessageParameters::decode(&buf),
                Err(MessageError::ProtocolViolation(_))
            ),
            "Table 6 外の 0x29 は PROTOCOL_VIOLATION であること"
        );
    }
}

mod resulting_publish_parameters {
    use super::*;
    use shiguredo_moqt::message_parameter::{
        PARAM_AUTHORIZATION_TOKEN, PARAM_FILL_PARAMETERS, PARAM_FORWARD, PARAM_GROUP_ORDER,
        PARAM_LARGEST_OBJECT, PARAM_SUBGROUP_DELIVERY_TIMEOUT, PARAM_SUBSCRIBER_PRIORITY,
    };

    /// 各エンコーディング種別の代表値を積んだパラメータ群を作る
    ///
    /// SUBSCRIBE_TRACKS の wire には載らない型 (SUBSCRIBER_PRIORITY 等) も含む
    /// 合成入力である。純粋濾過関数の振る舞いを固定するためのものであり、
    /// wire 到達可能性を主張しない。
    fn sample_params_for_filter() -> MessageParameters {
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_AUTHORIZATION_TOKEN,
            value: MessageParameterValue::AuthorizationToken(
                shiguredo_moqt::message_parameter::AuthorizationToken::UseValue {
                    token_type: 1,
                    token_value: vec![9],
                },
            ),
        });
        params.push(MessageParameter {
            param_type: PARAM_FORWARD,
            value: MessageParameterValue::Uint8(0),
        });
        params.push(MessageParameter {
            param_type: PARAM_GROUP_ORDER,
            value: MessageParameterValue::Uint8(1),
        });
        params.push(MessageParameter {
            param_type: PARAM_SUBSCRIBER_PRIORITY,
            value: MessageParameterValue::Uint8(64),
        });
        params.push(MessageParameter {
            param_type: PARAM_LOCATION_FILTER,
            value: MessageParameterValue::LengthPrefixed(
                LocationFilter::NextObject.encode_to_bytes(),
            ),
        });
        params.push(MessageParameter {
            param_type: PARAM_OBJECT_DELIVERY_TIMEOUT,
            value: MessageParameterValue::VarInt(100),
        });
        params.push(MessageParameter {
            param_type: PARAM_SUBGROUP_DELIVERY_TIMEOUT,
            value: MessageParameterValue::VarInt(200),
        });
        params.push(MessageParameter {
            param_type: PARAM_EXPIRES,
            value: MessageParameterValue::VarInt(300),
        });
        params.push(MessageParameter {
            param_type: PARAM_LARGEST_OBJECT,
            value: MessageParameterValue::Location {
                group: 1,
                object: 2,
            },
        });
        // PUBLISH に出現できない型 (FILL_PARAMETERS / Range Filter / TRACK_PROPERTY_FILTER)
        params.push(MessageParameter {
            param_type: PARAM_FILL_PARAMETERS,
            value: MessageParameterValue::FillParameters(MessageParameters::new()),
        });
        params.push(MessageParameter {
            param_type: PARAM_SUBGROUP_FILTER,
            value: MessageParameterValue::LengthPrefixed(vec![0x00, 0x00, 0x01]),
        });
        params.push(MessageParameter {
            param_type: PARAM_TRACK_PROPERTY_FILTER,
            value: MessageParameterValue::LengthPrefixed(vec![0x00, 0x02, 0x00, 0x01]),
        });
        params
    }

    #[test]
    fn keeps_publish_subset_and_drops_auth_token() {
        // SUBSCRIBE_TRACKS 由来の PUBLISH には subset のみ載り、auth token は載らない
        // (draft-ietf-moq-transport-21 §9.18.1 / §9.20.3)。
        // 集合一致で検証する (順序は実装の入力順保持であり仕様要求ではない)。
        let derived = sample_params_for_filter().resulting_publish_parameters();
        let mut types: Vec<u64> = derived.as_slice().iter().map(|p| p.param_type).collect();
        types.sort_unstable();
        assert_eq!(
            types,
            vec![
                PARAM_OBJECT_DELIVERY_TIMEOUT,
                PARAM_SUBGROUP_DELIVERY_TIMEOUT,
                PARAM_EXPIRES,
                PARAM_LARGEST_OBJECT,
                PARAM_FORWARD,
                PARAM_SUBSCRIBER_PRIORITY,
                PARAM_LOCATION_FILTER,
                PARAM_GROUP_ORDER,
            ],
            "PUBLISH 出現可能な 8 種だけが残ること"
        );
        // 値は複写される
        assert_eq!(derived.forward(), Some(0));
        assert_eq!(derived.group_order(), Some(1));
        assert_eq!(derived.subscriber_priority(), Some(64));
        assert_eq!(derived.object_delivery_timeout(), Some(100));
        assert_eq!(derived.subgroup_delivery_timeout(), Some(200));
        assert_eq!(derived.expires(), Some(300));
        assert_eq!(derived.largest_object(), Some((1, 2)));
        // 導出結果はエンコード可能であること
        let mut buf = Vec::new();
        derived
            .encode(&mut buf)
            .expect("導出パラメータの encode は成功する");
    }

    #[test]
    fn empty_stays_empty() {
        // 空入力は空出力になる
        assert!(
            MessageParameters::new()
                .resulting_publish_parameters()
                .is_empty()
        );
    }
}
