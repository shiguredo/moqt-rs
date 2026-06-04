use shiguredo_moqt::{
    error::MessageError,
    message::ControlMessage,
    message::Fetch,
    message::FetchOk,
    message::ReasonPhrase,
    message::Redirect,
    message::RequestError,
    message::RequestUpdate,
    message::Setup,
    message::Subscribe,
    message::SubscribeOk,
    message::common::Location,
    message::common::TrackNamespace,
    message_parameter::MessageParameters,
    parameter::{SetupOption, SetupOptionValue, SetupOptions},
    track_properties::TrackProperties,
};

mod setup_messages {
    use super::*;

    #[test]
    fn setup_wire_format() {
        // SETUP: type=0x2F00, length(u16), payload=空 (カウントプレフィックスなし)
        let msg = ControlMessage::Setup(Setup {
            options: SetupOptions::new(),
        });
        let encoded = msg.encode().expect("正当なテスト入力の encode は成功する");
        // type = 0x2F00 (2 byte vi64: 0x80|0x2F=0xAF, 0x00)
        assert_eq!(encoded[0], 0xAF);
        assert_eq!(encoded[1], 0x00);
        // length = 0 (空の SetupOptions はカウントプレフィックスなしなので 0 バイト)
        assert_eq!(encoded[2], 0x00);
        assert_eq!(encoded[3], 0x00);
        // ペイロードなし
        assert_eq!(encoded.len(), 4);
    }

    /// malformed PATH を含む SETUP の wire decode が parameter 層では成功することを確認する
    /// (セッション層の validate_setup_uri_format で正しいエラーコードに変換される)
    #[test]
    fn setup_with_malformed_path_decodes_successfully() {
        let mut opts = SetupOptions::new();
        opts.push(SetupOption {
            option_type: shiguredo_moqt::parameter::SETUP_OPTION_PATH,
            value: SetupOptionValue::Bytes(b"relative/path".to_vec()),
        });
        let msg = ControlMessage::Setup(Setup { options: opts });
        let encoded = msg.encode().expect("正当なテスト入力の encode は成功する");
        let (decoded, consumed) =
            ControlMessage::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, encoded.len());
        if let ControlMessage::Setup(setup) = decoded {
            assert_eq!(setup.options.path(), Some(b"relative/path".as_slice()));
        } else {
            panic!("SETUP メッセージとしてデコードされるべき");
        }
    }
}

mod goaway {
    use super::*;

    /// Timeout 後に余剰バイト (draft-19 §10.4 で削除された Request ID) があると PROTOCOL_VIOLATION
    ///
    /// draft-ietf-moq-transport-21 §9 (Control Messages): Length が Message Body と
    /// 一致しなければ PROTOCOL_VIOLATION。
    #[test]
    fn trailing_bytes_after_timeout_are_protocol_violation() {
        use shiguredo_moqt::error::MessageError;
        use shiguredo_moqt::varint;

        // URI 空 + timeout=0 + 余剰 varint(1) を Length に含めてエンコード相当のバイト列を作る
        let mut payload = Vec::new();
        varint::encode(0, &mut payload); // URI length
        varint::encode(0, &mut payload); // timeout
        varint::encode(1, &mut payload); // 余剰 (旧 Request ID)

        let mut buf = Vec::new();
        varint::encode(0x10, &mut buf); // MSG_GOAWAY
        let len = payload.len() as u16;
        buf.push((len >> 8) as u8);
        buf.push((len & 0xff) as u8);
        buf.extend_from_slice(&payload);

        let err = ControlMessage::decode(&buf).expect_err("余剰バイトは PROTOCOL_VIOLATION");
        assert!(
            matches!(err, MessageError::ProtocolViolation(_)),
            "got {err:?}"
        );
    }

    /// New Session URI が 8192 バイトを超える GOAWAY はデコードで拒否される
    ///
    /// draft-ietf-moq-transport-21 §9.2 (GOAWAY): New Session URI は最大 8192 バイト。
    #[test]
    fn uri_longer_than_8192_is_rejected_on_decode() {
        use shiguredo_moqt::varint;

        // URI 長だけ 8193 を書き、本体データは不要 (長さチェックが先に走る)
        let mut payload = Vec::new();
        varint::encode(8193, &mut payload);

        let mut buf = Vec::new();
        varint::encode(0x10, &mut buf); // MSG_GOAWAY
        let len = u16::try_from(payload.len()).expect("payload は u16 に収まる");
        buf.push((len >> 8) as u8);
        buf.push((len & 0xff) as u8);
        buf.extend_from_slice(&payload);

        let err = ControlMessage::decode(&buf).expect_err("URI > 8192 は PROTOCOL_VIOLATION");
        assert!(
            matches!(err, MessageError::ProtocolViolation(_)),
            "got {err:?}"
        );
    }

    /// New Session URI がちょうど 8192 バイトの GOAWAY はデコードに成功する
    ///
    /// draft-ietf-moq-transport-21 §9.2 (GOAWAY): 上限は 8192 バイト (含む)。
    #[test]
    fn uri_exactly_8192_decodes_successfully() {
        use shiguredo_moqt::message::Goaway;

        let uri = vec![b'x'; 8192];
        let msg = ControlMessage::Goaway(Goaway {
            new_session_uri: uri.clone(),
            timeout: 0,
        });
        let encoded = msg
            .encode()
            .expect("8192 バイト URI の GOAWAY はエンコードできる");
        let (decoded, consumed) =
            ControlMessage::decode(&encoded).expect("8192 バイト URI の GOAWAY はデコードできる");
        assert_eq!(consumed, encoded.len());
        match decoded {
            ControlMessage::Goaway(g) => {
                assert_eq!(g.new_session_uri, uri);
                assert_eq!(g.timeout, 0);
            }
            other => panic!("Goaway が期待されたが {other:?}"),
        }
    }

    /// New Session URI が 8192 バイトを超える GOAWAY はエンコードで拒否される
    ///
    /// draft-ietf-moq-transport-21 §9.2 (GOAWAY): New Session URI は最大 8192 バイト。
    #[test]
    fn uri_longer_than_8192_is_rejected_on_encode() {
        use shiguredo_moqt::message::Goaway;

        let msg = ControlMessage::Goaway(Goaway {
            new_session_uri: vec![b'x'; 8193],
            timeout: 0,
        });
        let err = msg
            .encode()
            .expect_err("URI > 8192 のエンコードは PROTOCOL_VIOLATION");
        assert!(
            matches!(err, MessageError::ProtocolViolation(_)),
            "got {err:?}"
        );
    }
}

mod request_flow {
    use super::*;

    // ─── REQUEST_ERROR の REDIRECT present 整合検証 (draft-ietf-moq-transport-21 §9.4.2 (REQUEST_ERROR Message Format)) ────
    //
    // draft-ietf-moq-transport-21 §9.4.2 (REQUEST_ERROR Message Format) "Redirect: Present only when Error Code is REDIRECT" に基づき、
    // error_code と Redirect の present 整合をデコード・エンコード双方で強制する。
    // 以下は意図的エラーパス (PBT のラウンドトリップでは表現できない) の単体テスト。

    /// REDIRECT なのに Redirect 本体を欠くバイト列をデコードすると PROTOCOL_VIOLATION
    /// (UnexpectedEof ではなく ProtocolViolation であること)
    #[test]
    fn request_error_redirect_missing_redirect_is_protocol_violation() {
        // type=0x05, length=0x0003, payload=[error_code=0x34 (REDIRECT), retry=0x00, reason_len=0x00]
        // reason まで読むと pos == payload.len() となり Redirect が欠落している
        let bytes = [0x05u8, 0x00, 0x03, 0x34, 0x00, 0x00];
        let err = ControlMessage::decode(&bytes).unwrap_err();
        assert!(
            matches!(err, MessageError::ProtocolViolation(_)),
            "REDIRECT で Redirect 欠落は ProtocolViolation であるべきだが {err:?}"
        );
    }

    /// 非 REDIRECT なのに末尾に余剰バイトがあるバイト列をデコードすると、末尾チェックで
    /// PROTOCOL_VIOLATION になる
    #[test]
    fn request_error_non_redirect_trailing_bytes_is_protocol_violation() {
        // type=0x05, length=0x0004, payload=[error_code=0x03, retry=0x00, reason_len=0x00, 余剰=0xAB]
        // error_code != REDIRECT なので redirect=None となり、余剰バイトで pos != payload.len()
        let bytes = [0x05u8, 0x00, 0x04, 0x03, 0x00, 0x00, 0xAB];
        let err = ControlMessage::decode(&bytes).unwrap_err();
        assert!(
            matches!(err, MessageError::ProtocolViolation(_)),
            "非 REDIRECT で末尾余剰は ProtocolViolation であるべきだが {err:?}"
        );
    }

    /// REDIRECT で Redirect が途中で切れたバイト列をデコードすると、デコードエラー
    /// (UnexpectedEof 系) になる。空欠落の ProtocolViolation との境界を固定する。
    #[test]
    fn request_error_redirect_truncated_redirect_is_unexpected_eof() {
        // type=0x05, length=0x0004, payload=[error_code=0x34, retry=0x00, reason_len=0x00,
        // connect_uri_len=0x05 (但し後続バイトなし)]。Redirect::decode_from が checked_len で
        // UnexpectedEof を返す (pos < payload.len() なので ProtocolViolation 分岐には入らない)
        let bytes = [0x05u8, 0x00, 0x04, 0x34, 0x00, 0x00, 0x05];
        let err = ControlMessage::decode(&bytes).unwrap_err();
        assert!(
            matches!(err, MessageError::UnexpectedEof),
            "Redirect 途中切れは UnexpectedEof であるべきだが {err:?}"
        );
    }

    /// REDIRECT なのに redirect=None の RequestError をエンコードすると PROTOCOL_VIOLATION
    #[test]
    fn request_error_encode_redirect_code_without_redirect_is_protocol_violation() {
        let msg = ControlMessage::RequestError(RequestError {
            error_code: 0x34, // REQUEST_REDIRECT
            retry_interval: 0,
            reason: ReasonPhrase::new("redirect").expect("テストフィクスチャの前提条件を満たす"),
            redirect: None,
        });
        let err = msg.encode().unwrap_err();
        assert!(
            matches!(err, MessageError::ProtocolViolation(_)),
            "REDIRECT + None のエンコードは ProtocolViolation であるべきだが {err:?}"
        );
    }

    /// 非 REDIRECT なのに redirect=Some の RequestError をエンコードすると PROTOCOL_VIOLATION
    #[test]
    fn request_error_encode_non_redirect_code_with_redirect_is_protocol_violation() {
        let ns = TrackNamespace::new(vec![b"example.com".to_vec()])
            .expect("テストフィクスチャの前提条件を満たす");
        let msg = ControlMessage::RequestError(RequestError {
            error_code: 3, // REDIRECT 以外
            retry_interval: 0,
            reason: ReasonPhrase::new("not found").expect("テストフィクスチャの前提条件を満たす"),
            redirect: Some(Redirect {
                connect_uri: vec![],
                track_namespace: ns,
                track_name: vec![],
            }),
        });
        let err = msg.encode().unwrap_err();
        assert!(
            matches!(err, MessageError::ProtocolViolation(_)),
            "非 REDIRECT + Some のエンコードは ProtocolViolation であるべきだが {err:?}"
        );
    }

    /// REDIRECT + retry_interval=0 の RequestError は encode/decode で往復できる
    ///
    /// draft-ietf-moq-transport-21 §9.4.2 (REQUEST_ERROR Message Format): retry_interval の
    /// 意味論 (plus one、値 1 は即時可、値 0 は SHOULD NOT retry as sent) は doc で確認し、
    /// ここでは値 0 が欠落なく往復することと Redirect 本体が保持されることを固定する。
    #[test]
    fn request_error_redirect_with_zero_retry_interval_round_trip() {
        let ns = TrackNamespace::new(vec![b"example.com".to_vec()])
            .expect("テストフィクスチャの前提条件を満たす");
        let msg = ControlMessage::RequestError(RequestError {
            error_code: shiguredo_moqt::error::REQUEST_REDIRECT,
            retry_interval: 0,
            reason: ReasonPhrase::new("redirect").expect("テストフィクスチャの前提条件を満たす"),
            redirect: Some(Redirect {
                connect_uri: Vec::new(),
                track_namespace: ns,
                track_name: Vec::new(),
            }),
        });
        let encoded = msg.encode().expect("正当なテスト入力の encode は成功する");
        let (decoded, consumed) =
            ControlMessage::decode(&encoded).expect("encode 直後の decode は成功する");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, msg);
        let ControlMessage::RequestError(decoded_err) = decoded else {
            panic!("REDIRECT が往復すること");
        };
        // 全体一致に含まれるが、失敗時の診断粒度のために個別にも固定する。
        assert_eq!(
            decoded_err.retry_interval, 0,
            "retry_interval=0 が欠落なく往復すること"
        );
        assert!(
            decoded_err.redirect.is_some(),
            "Redirect 本体が保持されること"
        );
    }
}

mod fetch {
    use super::*;

    #[test]
    fn fetch_unified_format_round_trip() {
        use shiguredo_moqt::message_parameter::LocationFilter;
        use shiguredo_moqt::message_parameter::{
            MessageParameter, MessageParameterValue, PARAM_LOCATION_FILTER,
        };
        // draft-ietf-moq-transport-21 §9.11 (FETCH): 単一形式の往復
        let mut parameters = MessageParameters::new();
        parameters.push(MessageParameter {
            param_type: PARAM_LOCATION_FILTER,
            value: MessageParameterValue::LengthPrefixed(
                LocationFilter::AbsoluteRangeWithEnd {
                    start: Location {
                        group_id: 2,
                        object_id: 3,
                    },
                    end_group_delta: 4,
                    end_object: 5,
                }
                .encode_to_bytes(),
            ),
        });
        let msg = ControlMessage::Fetch(Fetch {
            request_id: 7,
            track_namespace: TrackNamespace::new(vec![b"live".to_vec()])
                .expect("テストフィクスチャの前提条件を満たす"),
            track_name: b"cam".to_vec(),
            parameters,
        });
        let encoded = msg.encode().expect("正当なテスト入力の encode は成功する");
        let (decoded, consumed) =
            ControlMessage::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, msg);
    }

    #[test]
    fn legacy_fetch_type_bytes_are_rejected() {
        // draft-19 の Fetch Type 付きバイト列は draft-20 形式として解釈できず失敗する。
        // 旧 Relative Joining: request_id=2, type=0x02, joining_request_id=0, joining_start=0,
        // parameters count=0。Fetch Type バイト (0x02) が namespace フィールド数と読まれ、
        // 後続バイトでは正当な namespace + track name + parameters が構成できない。
        for payload in [
            // 旧 Relative Joining FETCH
            vec![0x02u8, 0x02, 0x00, 0x00, 0x00],
            // 旧 Absolute Joining FETCH
            vec![0x02u8, 0x03, 0x00, 0x00, 0x00],
            // 旧 Standalone FETCH (type=0x01 のみ。後続なし)
            vec![0x00u8, 0x01],
            // 旧 Standalone FETCH 完全形
            // (request_id=0, type=0x01, ns=["live"], name="cam", {0,0}-{1,0}, params=0)。
            // type バイト (0x01) が namespace フィールド数と読まれ、track name 長に
            // 0x6c (108) が来るため後続不足で失敗する。
            vec![
                0x00u8, 0x01, 0x01, 0x04, b'l', b'i', b'v', b'e', 0x03, b'c', b'a', b'm', 0x00,
                0x00, 0x01, 0x00, 0x00,
            ],
        ] {
            let mut buf = vec![
                0x16u8, // MSG_FETCH type
                0x00,
            ];
            buf.push(payload.len() as u8);
            buf.extend_from_slice(&payload);

            assert!(
                ControlMessage::decode(&buf).is_err(),
                "旧 Fetch Type 形式は拒否されること: {payload:?}"
            );
        }
    }

    #[test]
    fn fetch_ok_with_expires_is_rejected_on_encode() {
        use shiguredo_moqt::message_parameter::{
            MessageParameter, MessageParameterValue, PARAM_EXPIRES,
        };
        let mut parameters = MessageParameters::new();
        parameters.push(MessageParameter {
            param_type: PARAM_EXPIRES,
            value: MessageParameterValue::VarInt(100),
        });
        let msg = ControlMessage::FetchOk(FetchOk {
            end_of_track: 0,
            end_location: Location {
                group_id: 1,
                object_id: 0,
            },
            parameters,
            track_properties: TrackProperties::new(),
        });
        assert!(matches!(
            msg.encode(),
            Err(MessageError::ProtocolViolation(_))
        ));
    }

    #[test]
    fn fetch_ok_with_largest_object_is_rejected_on_decode() {
        use shiguredo_moqt::message_parameter::{
            MessageParameter, MessageParameterValue, PARAM_LARGEST_OBJECT,
        };
        let mut payload = vec![0u8, 0u8, 0u8];
        let mut parameters = MessageParameters::new();
        parameters.push(MessageParameter {
            param_type: PARAM_LARGEST_OBJECT,
            value: MessageParameterValue::Location {
                group: 3,
                object: 7,
            },
        });
        parameters
            .encode(&mut payload)
            .expect("正当なテスト入力の encode は成功する");
        TrackProperties::new()
            .encode(&mut payload)
            .expect("正当なテスト入力の encode は成功する");

        let len = payload.len() as u16;
        let mut encoded = vec![0x18, (len >> 8) as u8, len as u8];
        encoded.extend_from_slice(&payload);

        assert!(matches!(
            ControlMessage::decode(&encoded),
            Err(MessageError::ProtocolViolation(_))
        ));
    }
}

mod error_cases {
    use super::*;

    #[test]
    fn unknown_message_type() {
        // 0x3F は 1 バイト varint (< 64) だが有効なメッセージ型 ID ではない
        let buf = vec![0x3F, 0x00, 0x00];
        assert!(matches!(
            ControlMessage::decode(&buf),
            Err(MessageError::InvalidMessageType(0x3F))
        ));
    }

    #[test]
    fn empty_buffer() {
        assert_eq!(
            ControlMessage::decode(&[]),
            Err(MessageError::UnexpectedEof)
        );
    }

    #[test]
    fn truncated_length_field() {
        // type=0x03 だが length フィールドが 1 バイトしかない
        let buf = vec![0x03, 0x00];
        assert_eq!(
            ControlMessage::decode(&buf),
            Err(MessageError::UnexpectedEof)
        );
    }

    #[test]
    fn payload_shorter_than_length() {
        // type=0xAF, 0x00 (=0x2F00 SETUP), length=10, payload=5 bytes
        let buf = vec![0xAF, 0x00, 0x00, 0x0A, 0x01, 0x02, 0x03, 0x04, 0x05];
        assert_eq!(
            ControlMessage::decode(&buf),
            Err(MessageError::UnexpectedEof)
        );
    }

    #[test]
    fn invalid_fetch_namespace() {
        // FETCH ペイロードの namespace フィールド数が後続バイトを超える場合は失敗する。
        // (旧形式の fetch_type バイトは namespace フィールド数として読まれるため、
        // 旧形式との互換性はない)
        // request_id = 0 の後に namespace count = 4 が来るが後続バイトがない
        let inner = vec![
            0x00u8, // request_id = 0
            0x04,   // namespace field count = 4 (満たせない)
        ];

        let mut buf = vec![
            0x16u8, // MSG_FETCH type
            0x00,
        ];
        buf.push(inner.len() as u8);
        buf.extend_from_slice(&inner);

        assert!(
            ControlMessage::decode(&buf).is_err(),
            "namespace フィールド不足の FETCH は拒否されること"
        );
    }

    #[test]
    fn reason_phrase_builds_at_boundary_lengths() {
        // 0 バイト
        assert!(ReasonPhrase::new("").is_ok());
        // 1 バイト
        assert!(ReasonPhrase::new("x").is_ok());
        // 1023 バイト
        assert!(ReasonPhrase::new("x".repeat(1023)).is_ok());
        // 1024 バイト (上限)
        assert!(ReasonPhrase::new("x".repeat(1024)).is_ok());
    }

    #[test]
    fn reason_phrase_rejects_1025_bytes() {
        let result = ReasonPhrase::new("x".repeat(1025));
        assert_eq!(result, Err(MessageError::ReasonPhraseTooLong));
    }

    #[test]
    fn decode_rejects_reason_phrase_longer_than_1024_as_protocol_violation() {
        // draft-ietf-moq-transport-21 §8.5 (Reason Phrase Structure):
        // 1024 バイトを超える Reason Phrase の受信は PROTOCOL_VIOLATION。
        // PUBLISH_DONE (0x0B) のペイロードに長さ 1025 の Reason Phrase を載せる。
        let mut payload = vec![
            0x00, // status_code = 0
            0x00, // stream_count = 0
            0x44, 0x01, // reason phrase length = 1025 の vi64
        ];
        payload.extend_from_slice(&[b'x'; 1025]);

        let mut buf = vec![0x0Bu8, (payload.len() >> 8) as u8, payload.len() as u8];
        buf.extend_from_slice(&payload);

        assert!(
            matches!(
                ControlMessage::decode(&buf),
                Err(MessageError::ProtocolViolation(_))
            ),
            "1024 バイト超の Reason Phrase の decode は PROTOCOL_VIOLATION になること"
        );
    }

    #[test]
    fn reason_phrase_rejects_at_utf8_multibyte_boundary() {
        // 1023 バイト ASCII + 3 バイト UTF-8 文字 ("あ") = 1026 バイト
        let mut s = "x".repeat(1023);
        s.push('\u{3042}'); // "あ" = 3 バイト
        assert_eq!(s.len(), 1026);
        assert_eq!(ReasonPhrase::new(s), Err(MessageError::ReasonPhraseTooLong));
    }

    #[test]
    fn reason_phrase_as_str_returns_inner_string() {
        let rp = ReasonPhrase::new("hello").expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(rp.as_str(), "hello");
    }

    // ─── パラメータスコープ検証 (draft-ietf-moq-transport-21 §9.20.1 (Parameter Scope)) ──────────────────

    #[test]
    fn subscribe_with_rendezvous_timeout_is_accepted() {
        use shiguredo_moqt::message_parameter::{
            MessageParameter, MessageParameterValue, PARAM_RENDEZVOUS_TIMEOUT,
        };
        let ns = TrackNamespace::new(vec![b"example.com".to_vec()])
            .expect("テストフィクスチャの前提条件を満たす");
        let mut parameters = MessageParameters::new();
        parameters.push(MessageParameter {
            param_type: PARAM_RENDEZVOUS_TIMEOUT,
            value: MessageParameterValue::VarInt(500),
        });
        let msg = ControlMessage::Subscribe(Subscribe {
            request_id: 0,
            track_namespace: ns,
            track_name: b"video".to_vec(),
            parameters,
        });
        assert!(msg.encode().is_ok());
    }

    #[test]
    fn subscribe_ok_with_subscriber_priority_is_rejected() {
        use shiguredo_moqt::message_parameter::{
            MessageParameter, MessageParameterValue, PARAM_SUBSCRIBER_PRIORITY,
        };
        let mut parameters = MessageParameters::new();
        parameters.push(MessageParameter {
            param_type: PARAM_SUBSCRIBER_PRIORITY,
            value: MessageParameterValue::Uint8(100),
        });
        let msg = ControlMessage::SubscribeOk(SubscribeOk {
            track_alias: 0,
            parameters,
            track_properties: TrackProperties::new(),
        });
        assert!(matches!(
            msg.encode(),
            Err(MessageError::ProtocolViolation(_))
        ));
    }

    #[test]
    fn request_update_with_group_order_is_rejected() {
        use shiguredo_moqt::message_parameter::{
            MessageParameter, MessageParameterValue, PARAM_GROUP_ORDER,
        };
        let mut parameters = MessageParameters::new();
        parameters.push(MessageParameter {
            param_type: PARAM_GROUP_ORDER,
            value: MessageParameterValue::Uint8(1),
        });
        let msg = ControlMessage::RequestUpdate(RequestUpdate {
            request_id: 1,
            parameters,
        });
        assert!(matches!(
            msg.encode(),
            Err(MessageError::ProtocolViolation(_))
        ));
    }
}

mod publish_state_notify {
    use super::*;
    use shiguredo_moqt::message::PublishStateNotify;
    use shiguredo_moqt::message_parameter::LocationFilter;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_EXPIRES, PARAM_FORWARD,
        PARAM_LARGEST_OBJECT, PARAM_LOCATION_FILTER,
    };

    /// 許可パラメータ群 (FORWARD / LOCATION_FILTER / LARGEST_OBJECT) を作る
    /// (encode 時に型昇順へソートされるため、昇順で積む)
    fn sample_params() -> MessageParameters {
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_LARGEST_OBJECT,
            value: MessageParameterValue::Location {
                group: 4,
                object: 5,
            },
        });
        params.push(MessageParameter {
            param_type: PARAM_FORWARD,
            value: MessageParameterValue::Uint8(0),
        });
        params.push(MessageParameter {
            param_type: PARAM_LOCATION_FILTER,
            value: MessageParameterValue::LengthPrefixed(
                LocationFilter::NextObject.encode_to_bytes(),
            ),
        });
        params
    }

    #[test]
    fn round_trip() {
        // draft-ietf-moq-transport-21 §9.10 (PUBLISH_STATE_NOTIFY) の往復
        let msg = ControlMessage::PublishStateNotify(PublishStateNotify {
            parameters: sample_params(),
        });
        let encoded = msg.encode().expect("正当なテスト入力の encode は成功する");
        let (decoded, consumed) =
            ControlMessage::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, msg);
    }

    #[test]
    fn disallowed_parameter_rejected_on_encode() {
        // EXPIRES は PUBLISH_STATE_NOTIFY に出現できない
        let mut params = MessageParameters::new();
        params.push(MessageParameter {
            param_type: PARAM_EXPIRES,
            value: MessageParameterValue::VarInt(100),
        });
        let msg = ControlMessage::PublishStateNotify(PublishStateNotify { parameters: params });
        assert!(matches!(
            msg.encode(),
            Err(MessageError::ProtocolViolation(_))
        ));
    }

    #[test]
    fn disallowed_parameter_rejected_on_decode() {
        // EXPIRES (0x08) を含むワイヤ形式は PROTOCOL_VIOLATION
        // payload: count=1, delta=0x08, value=100
        let payload = vec![0x01u8, 0x08, 0x64];
        let mut buf = vec![
            0x22u8, // MSG_PUBLISH_STATE_NOTIFY
            0x00,
        ];
        buf.push(payload.len() as u8);
        buf.extend_from_slice(&payload);
        assert!(matches!(
            ControlMessage::decode(&buf),
            Err(MessageError::ProtocolViolation(_))
        ));
    }
}
