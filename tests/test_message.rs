use shiguredo_moqt::{
    error::MessageError,
    message::ControlMessage,
    message::Fetch,
    message::ReasonPhrase,
    message::Redirect,
    message::RequestError,
    message::RequestOk,
    message::RequestUpdate,
    message::Setup,
    message::SubscribeOk,
    message::common::Location,
    message::common::TrackNamespace,
    message_parameter::MessageParameters,
    parameter::{DEFAULT_MAX_CONTROL_MESSAGE_SIZE, SetupOption, SetupOptionValue, SetupOptions},
    track_properties::TrackProperties,
    varint,
};

/// 制御メッセージのフレーム (Type + Length + 本文) を組み立てる
///
/// Length は vi64 であり、最小バイト数で書き出す
/// (draft-ietf-moq-transport-23 §8.1 (Variable-Length Integers))。
fn frame(type_id: u64, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    varint::encode(type_id, &mut buf);
    varint::encode(payload.len() as u64, &mut buf);
    buf.extend_from_slice(payload);
    buf
}

mod setup_messages {
    use super::*;

    #[test]
    fn setup_wire_format() {
        // SETUP: type=0x2F00, length(vi64), payload=空 (カウントプレフィックスなし)
        let msg = ControlMessage::Setup(Setup {
            options: SetupOptions::new(),
        });
        let encoded = msg
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
            .expect("正当なテスト入力の encode は成功する");
        // type = 0x2F00 (2 byte vi64: 0x80|0x2F=0xAF, 0x00)
        assert_eq!(encoded[0], 0xAF);
        assert_eq!(encoded[1], 0x00);
        // length = 0 (空の SetupOptions はカウントプレフィックスなしなので 0 バイト)
        // vi64 の 0 は 1 バイト
        assert_eq!(encoded[2], 0x00);
        // ペイロードなし
        assert_eq!(encoded.len(), 3);
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
        let encoded = msg
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
            .expect("正当なテスト入力の encode は成功する");
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

/// Length が vi64 であることと送信上限の検証
///
/// draft-ietf-moq-transport-23 §9 (Control Messages): Length は vi64 であり、
/// peer の MAX_CONTROL_MESSAGE_SIZE を超える制御メッセージは送出してはならない (MUST NOT)。
mod framing {
    use super::*;

    /// 128 バイト以上の本文では Length が 2 バイトの vi64 になる
    #[test]
    fn length_is_varint_for_large_body() {
        use shiguredo_moqt::message::Goaway;

        // 本文長 = URI 長 (vi64) + URI + timeout (vi64) で 128 バイトを超える
        let msg = ControlMessage::Goaway(Goaway {
            new_session_uri: vec![b'x'; 200],
            timeout: 0,
        });
        let encoded = msg
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
            .expect("正当なテスト入力の encode は成功する");
        // Type は 1 バイト (0x10)
        assert_eq!(encoded[0], 0x10);
        // Length は 2 バイト vi64 (leading bits 10)
        assert_eq!(
            encoded[1] & 0xC0,
            0x80,
            "Length が 2 バイト vi64 であること"
        );
        let (body_len, length_len) =
            varint::decode(&encoded[1..]).expect("Length を vi64 として読める");
        assert_eq!(length_len, 2, "128 バイト超の Length は 2 バイトになること");
        assert_eq!(
            encoded.len(),
            1 + length_len + body_len as usize,
            "Type + Length + 本文の長さが一致すること"
        );
        let (decoded, consumed) =
            ControlMessage::decode(&encoded).expect("encode したメッセージを decode できること");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, msg);
    }

    /// 非最小符号化された Length も decode できる
    ///
    /// draft-ietf-moq-transport-23 §8.1 (Variable-Length Integers): decode 側は非最小
    /// エンコーディングをエラーにしない。
    #[test]
    fn non_minimal_length_is_accepted_on_decode() {
        // SETUP (0x2F00) の本文長 0 を 4 バイトの vi64 で表す
        let wire = [0xAFu8, 0x00, 0xE0, 0x00, 0x00, 0x00];
        let (decoded, consumed) =
            ControlMessage::decode(&wire).expect("非最小符号化の Length は受理されること");
        assert_eq!(consumed, wire.len());
        assert!(matches!(decoded, ControlMessage::Setup(_)));
    }

    /// 本文長が送信上限を超えると encode が FieldLengthExceeded を返す
    ///
    /// draft-ietf-moq-transport-23 §9 (Control Messages) の MUST NOT に従い、ワイヤへ
    /// 書き出す前に拒否する。境界 (上限ちょうど) は成功する。
    #[test]
    fn encode_rejects_body_over_send_limit() {
        use shiguredo_moqt::message::Goaway;

        let msg = ControlMessage::Goaway(Goaway {
            new_session_uri: vec![b'x'; 100],
            timeout: 0,
        });
        // 上限なしでエンコードして本文長を得る
        let full = msg.encode(u64::MAX).expect("上限なしの encode は成功する");
        let (_, type_len) = varint::decode(&full).expect("Type を読める");
        let (body_len, _) = varint::decode(&full[type_len..]).expect("Length を読める");

        // 上限ちょうどは成功する
        msg.encode(body_len)
            .expect("上限ちょうどの本文は encode できること");
        // 上限 - 1 は拒否される
        let err = msg
            .encode(body_len - 1)
            .expect_err("上限を超える本文は encode できないこと");
        assert!(
            matches!(err, MessageError::FieldLengthExceeded(_)),
            "FieldLengthExceeded を期待したが {err:?} になった"
        );
    }

    /// 本文長が Length と一致しないフレームは MALFORMED_MESSAGE になる
    ///
    /// draft-ietf-moq-transport-23 §9 (Control Messages): Length と本文が一致しなければ
    /// MALFORMED_MESSAGE で閉じる (MUST)。宣言長を実際の本文より 1 バイト長くした
    /// SETUP を decode すると、本文の解析が Length の末尾で終われず MalformedMessage になる。
    #[test]
    fn body_shorter_than_declared_length_is_malformed_message() {
        // SETUP の本文を 1 バイトと宣言するが、その 1 バイトは Setup Options として
        // 解釈できない (0xFF は 9 バイト vi64 の先頭バイト)
        let wire = [0xAFu8, 0x00, 0x01, 0xFF];
        let err = ControlMessage::decode(&wire).expect_err("本文長の不一致は拒否されること");
        assert!(
            matches!(err, MessageError::MalformedMessage(_)),
            "MalformedMessage を期待したが {err:?} になった"
        );
    }

    /// 宣言長が残りバッファを超える場合は UnexpectedEof のまま (追加データ待ち)
    ///
    /// Length は読めたが本文がまだ揃っていない状態は、デコーダーが追加データを待てるよう
    /// UnexpectedEof で表す。MALFORMED_MESSAGE と混同しないことを固定する。
    #[test]
    fn missing_body_is_unexpected_eof() {
        let wire = [0xAFu8, 0x00, 0x05, 0x00];
        assert_eq!(
            ControlMessage::decode(&wire),
            Err(MessageError::UnexpectedEof)
        );
    }
}

mod goaway {
    use super::*;

    /// Timeout 後に余剰バイト (draft-19 §10.4 で削除された Request ID) があると MALFORMED_MESSAGE
    ///
    /// draft-ietf-moq-transport-23 §9 (Control Messages): Length が Message Body と
    /// 一致しなければ MALFORMED_MESSAGE で閉じる (MUST)。
    #[test]
    fn trailing_bytes_after_timeout_are_malformed_message() {
        use shiguredo_moqt::error::MessageError;
        use shiguredo_moqt::varint;

        // URI 空 + timeout=0 + 余剰 varint(1) を Length に含めてエンコード相当のバイト列を作る
        let mut payload = Vec::new();
        varint::encode(0, &mut payload); // URI length
        varint::encode(0, &mut payload); // timeout
        varint::encode(1, &mut payload); // 余剰 (旧 Request ID)

        let buf = frame(0x10, &payload); // MSG_GOAWAY

        let err = ControlMessage::decode(&buf).expect_err("余剰バイトは MALFORMED_MESSAGE");
        assert!(
            matches!(err, MessageError::MalformedMessage(_)),
            "got {err:?}"
        );
    }

    /// New Session URI が 8192 バイトを超える GOAWAY はデコードで拒否される
    ///
    /// draft-ietf-moq-transport-23 §9.2 (GOAWAY): New Session URI は最大 8192 バイト。
    #[test]
    fn uri_longer_than_8192_is_rejected_on_decode() {
        use shiguredo_moqt::varint;

        // URI 長だけ 8193 を書き、本体データは不要 (長さチェックが先に走る)
        let mut payload = Vec::new();
        varint::encode(8193, &mut payload);

        let buf = frame(0x10, &payload); // MSG_GOAWAY

        let err = ControlMessage::decode(&buf).expect_err("URI > 8192 は FIELD_LENGTH_EXCEEDED");
        assert!(
            matches!(err, MessageError::FieldLengthExceeded(_)),
            "got {err:?}"
        );
    }

    /// New Session URI がちょうど 8192 バイトの GOAWAY はデコードに成功する
    ///
    /// draft-ietf-moq-transport-23 §9.2 (GOAWAY): 上限は 8192 バイト (含む)。
    #[test]
    fn uri_exactly_8192_decodes_successfully() {
        use shiguredo_moqt::message::Goaway;

        let uri = vec![b'x'; 8192];
        let msg = ControlMessage::Goaway(Goaway {
            new_session_uri: uri.clone(),
            timeout: 0,
        });
        let encoded = msg
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
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
    /// draft-ietf-moq-transport-23 §9.2 (GOAWAY): New Session URI は最大 8192 バイト。
    #[test]
    fn uri_longer_than_8192_is_rejected_on_encode() {
        use shiguredo_moqt::message::Goaway;

        let msg = ControlMessage::Goaway(Goaway {
            new_session_uri: vec![b'x'; 8193],
            timeout: 0,
        });
        let err = msg
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
            .expect_err("URI > 8192 のエンコードは FIELD_LENGTH_EXCEEDED");
        assert!(
            matches!(err, MessageError::FieldLengthExceeded(_)),
            "got {err:?}"
        );
    }
}

mod request_error_details {
    use super::*;
    use shiguredo_moqt::message::{ErrorDetail, ErrorDetails, UnsupportedProperty};

    /// REQUEST_ERROR のメッセージ型 ID (draft-ietf-moq-transport-23 §9 Table 5)
    const MSG_REQUEST_ERROR: u64 = 0x05;

    /// REDIRECT Error Detail の Type 値 (draft-ietf-moq-transport-23 §9.4.2.1 (REDIRECT))
    const ERROR_DETAIL_REDIRECT: u64 = 0x01;

    /// UNSUPPORTED_PROPERTY Error Detail の Type 値
    /// (draft-ietf-moq-transport-23 §9.4.2.2 (UNSUPPORTED_PROPERTY))
    const ERROR_DETAIL_UNSUPPORTED_PROPERTY: u64 = 0x02;

    /// REDIRECT Detail を 1 つだけ持つ Error Details を組み立てる
    fn redirect_details(redirect: Redirect) -> ErrorDetails {
        let mut details = ErrorDetails::new();
        details.push(ErrorDetail::Redirect(redirect));
        details
    }

    /// すべてのフィールドが空の REDIRECT を組み立てる
    ///
    /// Track Name が空でも正当である。namespace-scoped request 向けの REDIRECT は空の
    /// Track Name を要求する (draft-ietf-moq-transport-23 §9.4.2.1 (REDIRECT))。
    fn empty_redirect() -> Redirect {
        Redirect {
            connect_uri: Vec::new(),
            track_namespace: TrackNamespace::new(Vec::new())
                .expect("0 フィールドの Track Namespace は正当である"),
            track_name: Vec::new(),
        }
    }

    /// REDIRECT Detail を含む REQUEST_ERROR のゴールデンバイト列
    ///
    /// draft-ietf-moq-transport-23 §9.4.1 (REQUEST_ERROR Message Format) の
    /// "Error Details (..) ..." は Error Reason の後ろからメッセージ末尾までを占める
    /// Key-Value-Pair 列であり、REDIRECT Detail は Type 0x01 の length-prefixed 値である
    /// (§9.4.2.1)。空の REDIRECT の値は Connect URI 長 (0) + Track Namespace フィールド数 (0)
    /// + Track Name 長 (0) の 3 バイトになる。
    #[test]
    fn redirect_golden_bytes() {
        let msg = ControlMessage::RequestError(RequestError {
            error_code: shiguredo_moqt::error::REQUEST_REDIRECT,
            retry_interval: 0,
            reason: ReasonPhrase::new("").expect("空の Reason Phrase は正当である"),
            error_details: redirect_details(empty_redirect()),
        });
        let encoded = msg
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
            .expect("正当な REDIRECT は encode できる");
        assert_eq!(
            encoded,
            vec![
                MSG_REQUEST_ERROR as u8,     // Type = 0x05
                0x08,                        // Length = 8 バイト
                0x34,                        // Error Code = REDIRECT
                0x00,                        // Retry Interval
                0x00,                        // Error Reason 長 = 0
                ERROR_DETAIL_REDIRECT as u8, // Error Detail の Delta Type
                0x03,                        // REDIRECT 値の Length = 3 バイト
                0x00,                        // Connect URI 長 = 0
                0x00,                        // Track Namespace フィールド数 = 0
                0x00,                        // Track Name 長 = 0
            ],
            "REDIRECT Error Detail を含む REQUEST_ERROR のゴールデンバイト列"
        );
        let (decoded, consumed) =
            ControlMessage::decode(&encoded).expect("encode 直後の decode は成功する");
        assert_eq!(
            consumed,
            encoded.len(),
            "エンコードした全バイトを消費すること"
        );
        assert_eq!(decoded, msg, "ゴールデンバイト列が同じ構造へ戻ること");
    }

    /// 非空のフィールドを持つ REDIRECT でも Length が値を覆い、全フィールドが往復すること
    #[test]
    fn redirect_golden_bytes_with_content() {
        let connect_uri = b"moqt://relay.example/".to_vec();
        let track_namespace =
            TrackNamespace::new(vec![b"ns".to_vec()]).expect("正当な Track Namespace である");
        let track_name = b"cam".to_vec();
        let msg = ControlMessage::RequestError(RequestError {
            error_code: shiguredo_moqt::error::REQUEST_REDIRECT,
            retry_interval: 0,
            reason: ReasonPhrase::new("redirect").expect("正当な Reason Phrase である"),
            error_details: redirect_details(Redirect {
                connect_uri: connect_uri.clone(),
                track_namespace: track_namespace.clone(),
                track_name: track_name.clone(),
            }),
        });
        let encoded = msg
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
            .expect("正当な REDIRECT は encode できる");
        // REDIRECT 値 = Connect URI 長 (1) + Connect URI (21) + Track Namespace
        // (フィールド数 1 + フィールド長 1 + "ns" 2) + Track Name 長 (1) + Track Name (3)
        // = 30 バイト。全体 Length = Error Code (1) + Retry Interval (1) + Error Reason (9)
        // + Detail の Type (1) + Detail の Length (1) + REDIRECT 値 (30) = 43 バイト。
        let mut expected = vec![
            MSG_REQUEST_ERROR as u8, // Type = 0x05
            0x2B,                    // Length = 43 バイト
            0x34,                    // Error Code = REDIRECT
            0x00,                    // Retry Interval
            0x08,                    // Error Reason 長 = 8
        ];
        expected.extend_from_slice(b"redirect");
        expected.extend_from_slice(&[
            ERROR_DETAIL_REDIRECT as u8, // Delta Type = 0x01
            0x1E,                        // REDIRECT 値の Length = 30
            0x15,                        // Connect URI 長 = 21
        ]);
        expected.extend_from_slice(&connect_uri);
        expected.extend_from_slice(&[
            0x01, // Track Namespace フィールド数 = 1
            0x02, // Track Namespace フィールド長 = 2
        ]);
        expected.extend_from_slice(b"ns");
        expected.extend_from_slice(&[
            0x03, // Track Name 長 = 3
        ]);
        expected.extend_from_slice(&track_name);
        assert_eq!(encoded, expected, "REDIRECT Detail のフィールド配置");

        let (decoded, consumed) =
            ControlMessage::decode(&encoded).expect("encode 直後の decode は成功する");
        assert_eq!(consumed, encoded.len());
        let ControlMessage::RequestError(decoded) = decoded else {
            panic!("RequestError が期待された");
        };
        assert_eq!(
            decoded.error_details.redirect(),
            Some(&Redirect {
                connect_uri,
                track_namespace,
                track_name,
            }),
            "REDIRECT の全フィールドが往復すること"
        );
    }

    /// Error Code = REDIRECT で REDIRECT Detail が無いと MALFORMED_MESSAGE
    ///
    /// draft-ietf-moq-transport-23 §9.4.2.1 (REDIRECT): "An endpoint that receives a
    /// REQUEST_ERROR with Error Code REDIRECT and no REDIRECT Error Detail MUST close the
    /// session with MALFORMED_MESSAGE."
    #[test]
    fn redirect_missing_detail_is_malformed_message() {
        // Type=0x05, Length=3, payload=[Error Code=0x34, Retry Interval=0x00, Error Reason 長=0]
        let bytes = [MSG_REQUEST_ERROR as u8, 0x03, 0x34, 0x00, 0x00];
        let err = ControlMessage::decode(&bytes).unwrap_err();
        assert!(
            matches!(err, MessageError::MalformedMessage(_)),
            "REDIRECT で REDIRECT Detail 欠落は MALFORMED_MESSAGE であるべきだが {err:?}"
        );
    }

    /// Error Code = REDIRECT で REDIRECT Detail を持たない RequestError は encode できない
    #[test]
    fn encode_redirect_without_detail_is_protocol_violation() {
        let msg = ControlMessage::RequestError(RequestError {
            error_code: shiguredo_moqt::error::REQUEST_REDIRECT,
            retry_interval: 0,
            reason: ReasonPhrase::new("redirect").expect("正当な Reason Phrase である"),
            error_details: ErrorDetails::new(),
        });
        let err = msg.encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE).unwrap_err();
        assert!(
            matches!(err, MessageError::ProtocolViolation(_)),
            "REDIRECT + Detail 無しの encode は ProtocolViolation であるべきだが {err:?}"
        );
    }

    /// 適用外の Error Code に REDIRECT Detail を付けた RequestError は encode できない
    ///
    /// 受信側は適用外の既知 Detail を無視する (draft-ietf-moq-transport-23 §9.4.2) ため、
    /// 送信側で生成を許すと decode で Detail が落ちて往復が崩れる。
    #[test]
    fn encode_non_redirect_with_redirect_detail_is_protocol_violation() {
        let msg = ControlMessage::RequestError(RequestError {
            error_code: shiguredo_moqt::error::REQUEST_NOT_SUPPORTED,
            retry_interval: 0,
            reason: ReasonPhrase::new("not supported").expect("正当な Reason Phrase である"),
            error_details: redirect_details(empty_redirect()),
        });
        let err = msg.encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE).unwrap_err();
        assert!(
            matches!(err, MessageError::ProtocolViolation(_)),
            "適用外 Detail の encode は ProtocolViolation であるべきだが {err:?}"
        );
    }

    /// REDIRECT Detail を 2 つ持つ RequestError は encode できない
    ///
    /// REDIRECT の定義は単一インスタンスのみを許す (draft-ietf-moq-transport-23 §9.4.2.1)。
    #[test]
    fn encode_duplicate_redirect_detail_is_protocol_violation() {
        let mut details = redirect_details(empty_redirect());
        details.push(ErrorDetail::Redirect(empty_redirect()));
        let msg = ControlMessage::RequestError(RequestError {
            error_code: shiguredo_moqt::error::REQUEST_REDIRECT,
            retry_interval: 0,
            reason: ReasonPhrase::new("redirect").expect("正当な Reason Phrase である"),
            error_details: details,
        });
        let err = msg.encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE).unwrap_err();
        assert!(
            matches!(err, MessageError::ProtocolViolation(_)),
            "REDIRECT Detail の重複は ProtocolViolation であるべきだが {err:?}"
        );
    }

    /// REDIRECT Detail が 2 つあるバイト列の decode は ProtocolViolation
    ///
    /// draft-ietf-moq-transport-23 §9.4.2: 型の定義が複数インスタンスを明示的に許可しない限り
    /// 送信側は同じ Error Detail 型を繰り返してはならない (MUST NOT)。
    #[test]
    fn duplicate_redirect_detail_is_protocol_violation() {
        let bytes = [
            MSG_REQUEST_ERROR as u8,
            0x0D, // Length = 13 バイト
            0x34, // Error Code = REDIRECT
            0x00, // Retry Interval
            0x00, // Error Reason 長 = 0
            ERROR_DETAIL_REDIRECT as u8,
            0x03, // REDIRECT 値の Length = 3
            0x00,
            0x00,
            0x00,
            0x00, // 2 つ目の Delta Type = 0 (同じ型)
            0x03, // REDIRECT 値の Length = 3
            0x00,
            0x00,
            0x00,
        ];
        let err = ControlMessage::decode(&bytes).unwrap_err();
        assert!(
            matches!(err, MessageError::ProtocolViolation(_)),
            "REDIRECT Detail の重複は ProtocolViolation であるべきだが {err:?}"
        );
    }

    /// REDIRECT Detail の Length が残りバイトを超えると MALFORMED_KEY_VALUE
    ///
    /// draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure): 受信側が型を理解する場合、
    /// Length/Value が定義と一致しなければ MALFORMED_KEY_VALUE でセッションを閉じる (MUST)。
    #[test]
    fn redirect_detail_length_exceeds_remaining_bytes_is_key_value_formatting_error() {
        let bytes = [
            MSG_REQUEST_ERROR as u8,
            0x06, // Length = 6 バイト
            0x34, // Error Code = REDIRECT
            0x00, // Retry Interval
            0x00, // Error Reason 長 = 0
            ERROR_DETAIL_REDIRECT as u8,
            0x05, // REDIRECT 値の Length = 5 だが残りは 1 バイト
            0x00, // Connect URI 長 = 0
        ];
        let err = ControlMessage::decode(&bytes).unwrap_err();
        assert!(
            matches!(err, MessageError::MalformedKeyValue(_)),
            "Detail の Length 超過は MALFORMED_KEY_VALUE であるべきだが {err:?}"
        );
    }

    /// REDIRECT Detail の Length が REDIRECT のフィールドを覆い切れないと MALFORMED_KEY_VALUE
    #[test]
    fn redirect_detail_length_smaller_than_fields_is_key_value_formatting_error() {
        let bytes = [
            MSG_REQUEST_ERROR as u8,
            0x06, // Length = 6 バイト
            0x34, // Error Code = REDIRECT
            0x00, // Retry Interval
            0x00, // Error Reason 長 = 0
            ERROR_DETAIL_REDIRECT as u8,
            0x01, // REDIRECT 値の Length = 1 (Connect URI 長だけ)
            0x00, // Connect URI 長 = 0
        ];
        let err = ControlMessage::decode(&bytes).unwrap_err();
        assert!(
            matches!(err, MessageError::MalformedKeyValue(_)),
            "Detail の Length 不足は MALFORMED_KEY_VALUE であるべきだが {err:?}"
        );
    }

    /// REDIRECT Detail の Length が REDIRECT 構造より長いと MALFORMED_KEY_VALUE
    ///
    /// Length が覆う範囲のうち REDIRECT 構造が消費しないバイトは、次の KVP の境界を
    /// 壊すため MALFORMED_KEY_VALUE として拒否する。
    #[test]
    fn redirect_detail_trailing_bytes_inside_value_is_key_value_formatting_error() {
        let bytes = [
            MSG_REQUEST_ERROR as u8,
            0x09, // Length = 9 バイト
            0x34, // Error Code = REDIRECT
            0x00, // Retry Interval
            0x00, // Error Reason 長 = 0
            ERROR_DETAIL_REDIRECT as u8,
            0x04, // REDIRECT 値の Length = 4 (構造は 3 バイト)
            0x00, // Connect URI 長 = 0
            0x00, // Track Namespace フィールド数 = 0
            0x00, // Track Name 長 = 0
            0xAB, // REDIRECT 構造の外にある余剰バイト
        ];
        let err = ControlMessage::decode(&bytes).unwrap_err();
        assert!(
            matches!(err, MessageError::MalformedKeyValue(_)),
            "Detail 値内の余剰バイトは MALFORMED_KEY_VALUE であるべきだが {err:?}"
        );
    }

    /// UNSUPPORTED_PROPERTY Detail を含む REQUEST_ERROR のゴールデンバイト列
    ///
    /// draft-ietf-moq-transport-23 §9.4.2.2 (UNSUPPORTED_PROPERTY) の値は理解できなかった
    /// Mandatory Track Property の Property Type (vi64) 1 つである。Type 0x02 は偶数型の
    /// ため Length フィールドを持たず、KVP の値は vi64 そのものになる
    /// (draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure))。
    /// 0x4000 は 2 バイト varint の上限 (0x3FFF) を超えるため 3 バイトで符号化される。
    #[test]
    fn unsupported_property_golden_bytes() {
        let mut details = ErrorDetails::new();
        details.push(ErrorDetail::UnsupportedProperty(UnsupportedProperty {
            property_type: 0x4000,
        }));
        let msg = ControlMessage::RequestError(RequestError {
            error_code: shiguredo_moqt::error::REQUEST_UNSUPPORTED_EXTENSION,
            retry_interval: 0,
            reason: ReasonPhrase::new("").expect("空の Reason Phrase は正当である"),
            error_details: details,
        });
        let encoded = msg
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
            .expect("正当な UNSUPPORTED_PROPERTY は encode できる");
        assert_eq!(
            encoded,
            vec![
                MSG_REQUEST_ERROR as u8,                 // Type = 0x05
                0x07,                                    // Length = 7 バイト
                0x33,                                    // Error Code = UNSUPPORTED_EXTENSION
                0x00,                                    // Retry Interval
                0x00,                                    // Error Reason 長 = 0
                ERROR_DETAIL_UNSUPPORTED_PROPERTY as u8, // Delta Type = 0x02
                0xC0,                                    // Property Type 0x4000 の vi64 (3 バイト)
                0x40,
                0x00,
            ],
            "UNSUPPORTED_PROPERTY Error Detail は Length を持たない vi64 であること"
        );
        let (decoded, consumed) =
            ControlMessage::decode(&encoded).expect("encode 直後の decode は成功する");
        assert_eq!(
            consumed,
            encoded.len(),
            "エンコードした全バイトを消費すること"
        );
        assert_eq!(decoded, msg, "ゴールデンバイト列が同じ構造へ戻ること");
    }

    /// 複数の UNSUPPORTED_PROPERTY Detail を 1 つの REQUEST_ERROR に載せられる
    ///
    /// draft-ietf-moq-transport-23 §9.4.2.2 (UNSUPPORTED_PROPERTY) は複数インスタンスを
    /// 許す。各 Detail の KVP Type は同じ 0x02 であるため、2 個目以降の Delta Type は 0 に
    /// なる。値はそれぞれの Property Type (vi64) である。
    #[test]
    fn multiple_unsupported_property_details_golden_bytes() {
        let mut details = ErrorDetails::new();
        details.push(ErrorDetail::UnsupportedProperty(UnsupportedProperty {
            property_type: 0x4000,
        }));
        details.push(ErrorDetail::UnsupportedProperty(UnsupportedProperty {
            property_type: 0x7FFF,
        }));
        let msg = ControlMessage::RequestError(RequestError {
            error_code: shiguredo_moqt::error::REQUEST_UNSUPPORTED_EXTENSION,
            retry_interval: 0,
            reason: ReasonPhrase::new("").expect("空の Reason Phrase は正当である"),
            error_details: details,
        });
        let encoded = msg
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
            .expect("複数の UNSUPPORTED_PROPERTY は encode できる");
        assert_eq!(
            encoded,
            vec![
                MSG_REQUEST_ERROR as u8,                 // Type = 0x05
                0x0B,                                    // Length = 11 バイト
                0x33,                                    // Error Code = UNSUPPORTED_EXTENSION
                0x00,                                    // Retry Interval
                0x00,                                    // Error Reason 長 = 0
                ERROR_DETAIL_UNSUPPORTED_PROPERTY as u8, // 1 個目: Delta Type = 0x02
                0xC0,                                    // Property Type 0x4000 の vi64
                0x40,
                0x00,
                0x00, // 2 個目: Delta Type = 0 (同じ Error Detail Type の繰り返し)
                0xC0, // Property Type 0x7FFF の vi64
                0x7F,
                0xFF,
            ],
            "複数 Detail の Delta Type は 0 で、値はそれぞれの Property Type であること"
        );
        let (decoded, consumed) =
            ControlMessage::decode(&encoded).expect("encode 直後の decode は成功する");
        assert_eq!(consumed, encoded.len());
        let ControlMessage::RequestError(decoded) = decoded else {
            panic!("RequestError が期待された");
        };
        assert_eq!(
            decoded.error_details.len(),
            2,
            "Detail が 2 個保持されること"
        );
        assert_eq!(
            decoded.error_details.unsupported_properties(),
            vec![0x4000, 0x7FFF],
            "Property Type が出現順に取り出せること"
        );
    }

    /// 同じ Property Type を持つ UNSUPPORTED_PROPERTY Detail の重複も許可される
    ///
    /// draft-ietf-moq-transport-23 §9.4.2: 送信側は型の定義が複数インスタンスを明示的に
    /// 許可しない限り同じ Error Detail 型を繰り返してはならない (MUST NOT)。
    /// §9.4.2.2 は複数インスタンスを許可し、同じ Property Type の重複も型としては許される。
    #[test]
    fn duplicate_unsupported_property_detail_allowed() {
        let mut details = ErrorDetails::new();
        for _ in 0..2 {
            details.push(ErrorDetail::UnsupportedProperty(UnsupportedProperty {
                property_type: 0x4000,
            }));
        }
        let msg = ControlMessage::RequestError(RequestError {
            error_code: shiguredo_moqt::error::REQUEST_UNSUPPORTED_EXTENSION,
            retry_interval: 0,
            reason: ReasonPhrase::new("").expect("空の Reason Phrase は正当である"),
            error_details: details,
        });
        let encoded = msg
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
            .expect("重複した UNSUPPORTED_PROPERTY は encode できる");
        let (decoded, consumed) = ControlMessage::decode(&encoded)
            .expect("重複した UNSUPPORTED_PROPERTY は decode できる");
        assert_eq!(consumed, encoded.len());
        let ControlMessage::RequestError(decoded) = decoded else {
            panic!("RequestError が期待された");
        };
        assert_eq!(
            decoded.error_details.unsupported_properties(),
            vec![0x4000, 0x4000],
            "重複した Detail は拒否せず、そのまま保持すること"
        );
    }

    /// UNSUPPORTED_EXTENSION 以外の Error Code に付いた UNSUPPORTED_PROPERTY は無視される
    ///
    /// draft-ietf-moq-transport-23 §9.4.2 (Error Details): "Receivers MUST ... ignore a known
    /// Error Detail type that accompanies an Error Code it is not defined for."
    /// UNSUPPORTED_PROPERTY は UNSUPPORTED_EXTENSION でのみ意味を持つ (§9.4.2.2)。
    #[test]
    fn unsupported_property_detail_for_other_error_code_is_ignored() {
        let mut payload = vec![
            0x03, // Error Code = NOT_SUPPORTED
            0x00, // Retry Interval
            0x00, // Error Reason 長 = 0
        ];
        // Delta Type = 0x02, Property Type = 0x4000 (vi64 4 バイト、Length なし)
        payload.extend_from_slice(&[
            ERROR_DETAIL_UNSUPPORTED_PROPERTY as u8,
            0xE0,
            0x00,
            0x40,
            0x00,
        ]);
        let bytes = frame(MSG_REQUEST_ERROR, &payload);
        let (decoded, consumed) =
            ControlMessage::decode(&bytes).expect("適用外の既知 Detail は無視され、decode できる");
        assert_eq!(consumed, bytes.len(), "メッセージ全体を消費すること");
        let ControlMessage::RequestError(decoded) = decoded else {
            panic!("RequestError が期待された");
        };
        assert!(
            decoded.error_details.is_empty(),
            "適用外の UNSUPPORTED_PROPERTY Detail は保持しないこと"
        );
    }

    /// 適用外の Error Code に UNSUPPORTED_PROPERTY Detail を付けた RequestError は encode できない
    ///
    /// 受信側は適用外の既知 Detail を無視する (draft-ietf-moq-transport-23 §9.4.2) ため、
    /// 送信側で生成を許すと decode で Detail が落ちて往復が崩れる。
    #[test]
    fn encode_unsupported_property_for_other_error_code_is_protocol_violation() {
        let mut details = ErrorDetails::new();
        details.push(ErrorDetail::UnsupportedProperty(UnsupportedProperty {
            property_type: 0x4000,
        }));
        let msg = ControlMessage::RequestError(RequestError {
            error_code: shiguredo_moqt::error::REQUEST_NOT_SUPPORTED,
            retry_interval: 0,
            reason: ReasonPhrase::new("not supported").expect("正当な Reason Phrase である"),
            error_details: details,
        });
        let err = msg.encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE).unwrap_err();
        assert!(
            matches!(err, MessageError::ProtocolViolation(_)),
            "適用外 Detail の encode は ProtocolViolation であるべきだが {err:?}"
        );
    }

    /// Mandatory Track Property でない型を持つ UNSUPPORTED_PROPERTY は encode できない
    ///
    /// draft-ietf-moq-transport-23 §9.4.2.2 (UNSUPPORTED_PROPERTY) の値は
    /// "the Property Type (vi64) of a Mandatory Track Property" である。§3.7
    /// (Mandatory to Understand Track Properties) は 0x4000-0x7FFF を Mandatory Track
    /// Property の範囲とし、同範囲の GREASE 値 (§13 (Grease)) は Mandatory Track Property
    /// ではないと定める。範囲外と GREASE 値のどちらも送信側で拒否する。
    #[test]
    fn encode_unsupported_property_outside_mandatory_range_is_protocol_violation() {
        // 0x0001 は範囲の下限未満、0x3FFF は直前、0x8000 は上限の直後、
        // 0x409C は 0x7F * 129 + 0x9D で範囲内の GREASE 値
        for property_type in [0x0001_u64, 0x3FFF, 0x8000, 0x409C] {
            let mut details = ErrorDetails::new();
            details.push(ErrorDetail::UnsupportedProperty(UnsupportedProperty {
                property_type,
            }));
            let msg = ControlMessage::RequestError(RequestError {
                error_code: shiguredo_moqt::error::REQUEST_UNSUPPORTED_EXTENSION,
                retry_interval: 0,
                reason: ReasonPhrase::new("unsupported").expect("正当な Reason Phrase である"),
                error_details: details,
            });
            let err = msg.encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE).unwrap_err();
            assert!(
                matches!(err, MessageError::ProtocolViolation(_)),
                "型 {property_type:#x} の encode は ProtocolViolation であるべきだが {err:?}"
            );
        }
    }

    /// UNSUPPORTED_PROPERTY の値 (vi64) が途中終端すると MALFORMED_MESSAGE
    ///
    /// KVP の値はメッセージ Length の内側にあるため、値の varint が途中で切れた場合は
    /// 本文が Length より短いことになり、MALFORMED_MESSAGE になる
    /// (draft-ietf-moq-transport-23 §9 (Control Messages) / §8.3 (Key-Value-Pair Structure))。
    #[test]
    fn truncated_unsupported_property_detail_is_malformed_message() {
        let bytes = [
            MSG_REQUEST_ERROR as u8,
            0x05, // Length = 5 バイト
            0x33, // Error Code = UNSUPPORTED_EXTENSION
            0x00, // Retry Interval
            0x00, // Error Reason 長 = 0
            ERROR_DETAIL_UNSUPPORTED_PROPERTY as u8,
            // 3 バイト vi64 の先頭バイトだけで残りが無い
            0xC0,
        ];
        let err = ControlMessage::decode(&bytes).unwrap_err();
        assert_eq!(
            err,
            MessageError::MalformedMessage(
                "control message body is shorter than the declared length"
            ),
            "Detail 値の varint 途中終端は MALFORMED_MESSAGE であること"
        );
    }

    /// 未知の Error Detail は無視され、セッションを閉じない
    ///
    /// draft-ietf-moq-transport-23 §9.4.2: "Receivers MUST ignore unknown Error Detail types."
    /// 偶数 Type は vi64、奇数 Type は Length + bytes という KVP の形式だけで値を読み飛ばす
    /// (§8.3)。
    #[test]
    fn unknown_details_are_ignored() {
        let mut payload = vec![
            0x03, // Error Code = NOT_SUPPORTED
            0x00, // Retry Interval
            0x00, // Error Reason 長 = 0
        ];
        // 未知の奇数 Type 3: Delta Type=0x03, Length=2, 値 2 バイト
        payload.extend_from_slice(&[0x03, 0x02, 0xAA, 0xBB]);
        // 未知の偶数 Type 4: Delta Type=0x01 (4 - 3), 値 vi64=0x7F
        payload.extend_from_slice(&[0x01, 0x7F]);
        let bytes = frame(MSG_REQUEST_ERROR, &payload);
        let (decoded, consumed) =
            ControlMessage::decode(&bytes).expect("未知の Error Detail は無視され、decode できる");
        assert_eq!(consumed, bytes.len(), "メッセージ全体を消費すること");
        let ControlMessage::RequestError(decoded) = decoded else {
            panic!("RequestError が期待された");
        };
        assert!(
            decoded.error_details.is_empty(),
            "未知の Detail は保持しないこと"
        );
    }

    /// 未知の Error Detail の後ろにある REDIRECT Detail も受理される
    ///
    /// Error Details は順序を持たない KVP 列であり、REDIRECT Detail が先頭にある必要は
    /// ない。Delta Type 0 の未知の偶数 Type 0x00 を先に置き、Delta Type 1 で REDIRECT を
    /// 続けても、メッセージ末尾までの KVP 列として解釈できる。
    #[test]
    fn redirect_after_unknown_detail_is_accepted() {
        let mut payload = vec![
            0x34, // Error Code = REDIRECT
            0x00, // Retry Interval
            0x00, // Error Reason 長 = 0
        ];
        // 未知の偶数 Type 0x00: Delta Type=0x00, 値 vi64=0x7F
        payload.extend_from_slice(&[0x00, 0x7F]);
        // REDIRECT: Delta Type=0x01 (0x01 - 0x00), Length=3, 空の REDIRECT
        payload.extend_from_slice(&[0x01, 0x03, 0x00, 0x00, 0x00]);
        let bytes = frame(MSG_REQUEST_ERROR, &payload);
        let (decoded, consumed) =
            ControlMessage::decode(&bytes).expect("REDIRECT Detail は位置に依存せず受理される");
        assert_eq!(consumed, bytes.len());
        let ControlMessage::RequestError(decoded) = decoded else {
            panic!("RequestError が期待された");
        };
        assert!(
            decoded.error_details.redirect().is_some(),
            "未知 Detail の後ろの REDIRECT Detail が保持されること"
        );
    }

    /// 未知の Error Detail の重複は許可される
    ///
    /// draft-ietf-moq-transport-23 §9.4.2: "Receivers MUST allow duplicates of unknown Error
    /// Detail types." 2 つ目の Delta Type は 0 になる。
    #[test]
    fn duplicate_unknown_details_are_ignored() {
        let mut payload = vec![
            0x03, // Error Code = NOT_SUPPORTED
            0x00, // Retry Interval
            0x00, // Error Reason 長 = 0
        ];
        // 未知の奇数 Type 5
        payload.extend_from_slice(&[0x05, 0x01, 0xAA]);
        // 同じ Type 5 の重複 (Delta Type=0)
        payload.extend_from_slice(&[0x00, 0x02, 0xBB, 0xCC]);
        let bytes = frame(MSG_REQUEST_ERROR, &payload);
        let (decoded, consumed) =
            ControlMessage::decode(&bytes).expect("未知 Detail の重複は許可され、decode できる");
        assert_eq!(consumed, bytes.len());
        let ControlMessage::RequestError(decoded) = decoded else {
            panic!("RequestError が期待された");
        };
        assert!(decoded.error_details.is_empty());
    }

    /// 適用されない Error Code の REDIRECT Detail は無視される
    ///
    /// draft-ietf-moq-transport-23 §9.4.2: "Receivers MUST ... ignore a known Error Detail type
    /// that accompanies an Error Code it is not defined for." 非ゼロの Connect URI を持つ
    /// REDIRECT Detail でも、REDIRECT 以外の Error Code では役割検証に渡らない。
    #[test]
    fn redirect_detail_for_other_error_code_is_ignored() {
        let mut payload = vec![
            0x03, // Error Code = NOT_SUPPORTED
            0x00, // Retry Interval
            0x00, // Error Reason 長 = 0
        ];
        // Type 0x01, Length=4, Connect URI 長=1, URI="x", Namespace 0 フィールド, Track Name 長=0
        payload.extend_from_slice(&[0x01, 0x04, 0x01, b'x', 0x00, 0x00]);
        let bytes = frame(MSG_REQUEST_ERROR, &payload);
        let (decoded, consumed) =
            ControlMessage::decode(&bytes).expect("適用外の既知 Detail は無視され、decode できる");
        assert_eq!(consumed, bytes.len());
        let ControlMessage::RequestError(decoded) = decoded else {
            panic!("RequestError が期待された");
        };
        assert!(
            decoded.error_details.is_empty(),
            "適用外の REDIRECT Detail は保持しないこと"
        );
    }

    /// Error Details を読み切ったあとの余剰バイトは MALFORMED_MESSAGE
    ///
    /// Error Details はメッセージ末尾までを占める (§9.4.1) ため、Detail として解釈できない
    /// 残りは本文長の不一致として MALFORMED_MESSAGE になる (draft-ietf-moq-transport-23 §9)。
    #[test]
    fn trailing_garbage_after_details_is_malformed_message() {
        let mut payload = vec![
            0x34, // Error Code = REDIRECT
            0x00, // Retry Interval
            0x00, // Error Reason 長 = 0
        ];
        payload.extend_from_slice(&[ERROR_DETAIL_REDIRECT as u8, 0x03, 0x00, 0x00, 0x00]);
        // 0xAB は 2 バイト vi64 の先頭バイトであり、Length が揃わない未知 Detail になる
        payload.push(0xAB);
        let bytes = frame(MSG_REQUEST_ERROR, &payload);
        let err = ControlMessage::decode(&bytes).unwrap_err();
        assert!(
            matches!(err, MessageError::MalformedMessage(_)),
            "Detail のあとの余剰バイトは MALFORMED_MESSAGE であるべきだが {err:?}"
        );
    }

    /// Error Detail の Length が 2^16-1 を超えると FIELD_LENGTH_EXCEEDED
    ///
    /// draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure): "The maximum length of a
    /// value is 2^16-1 bytes. If an endpoint receives a length larger than the maximum, it MUST
    /// close the session with FIELD_LENGTH_EXCEEDED."
    #[test]
    fn detail_length_exceeding_maximum_is_field_length_exceeded() {
        let mut payload = vec![
            0x03, // Error Code = NOT_SUPPORTED
            0x00, // Retry Interval
            0x00, // Error Reason 長 = 0
        ];
        payload.push(0x05); // 未知の奇数 Type 5
        varint::encode(65_536, &mut payload); // 上限 65,535 を 1 バイト超える
        let bytes = frame(MSG_REQUEST_ERROR, &payload);
        let err = ControlMessage::decode(&bytes).unwrap_err();
        assert!(
            matches!(err, MessageError::FieldLengthExceeded(_)),
            "Detail の Length 上限超過は FIELD_LENGTH_EXCEEDED であるべきだが {err:?}"
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
        // draft-ietf-moq-transport-23 §9.11 (FETCH): 単一形式の往復
        let mut parameters = MessageParameters::new();
        parameters.push(MessageParameter {
            param_type: PARAM_LOCATION_FILTER,
            value: MessageParameterValue::LocationFilter(LocationFilter::AbsoluteRangeWithEnd {
                start: Location {
                    group_id: 2,
                    object_id: 3,
                },
                end_group_delta: 4,
                end_object: 5,
            }),
        });
        let msg = ControlMessage::Fetch(Fetch {
            request_id: 7,
            track_namespace: TrackNamespace::new(vec![b"live".to_vec()])
                .expect("テストフィクスチャの前提条件を満たす"),
            track_name: b"cam".to_vec(),
            parameters,
        });
        let encoded = msg
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
            .expect("正当なテスト入力の encode は成功する");
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
            let buf = frame(0x16, &payload); // MSG_FETCH

            assert!(
                ControlMessage::decode(&buf).is_err(),
                "旧 Fetch Type 形式は拒否されること: {payload:?}"
            );
        }
    }

    /// FETCH の初回成功応答 (REQUEST_OK、別名 FETCH_OK) は EXPIRES と LARGEST_OBJECT を
    /// 運べること
    ///
    /// draft-ietf-moq-transport-23 §9.3 (REQUEST_OK): "FETCH_OK: EXPIRES, LARGEST_OBJECT"。
    /// 両パラメータを持つ REQUEST_OK の往復と、FETCH_OK context で許可されない型
    /// (AUTHORIZATION_TOKEN) の拒否を固定する。
    #[test]
    fn fetch_ok_accepts_expires_and_largest_object() {
        use shiguredo_moqt::message_parameter::{
            MessageParameter, MessageParameterValue, PARAM_AUTHORIZATION_TOKEN, PARAM_EXPIRES,
            PARAM_LARGEST_OBJECT,
        };
        let mut parameters = MessageParameters::new();
        parameters.push(MessageParameter {
            param_type: PARAM_EXPIRES,
            value: MessageParameterValue::VarInt(100),
        });
        parameters.push(MessageParameter {
            param_type: PARAM_LARGEST_OBJECT,
            value: MessageParameterValue::Location {
                group: 3,
                object: 7,
            },
        });
        let msg = ControlMessage::RequestOk(RequestOk {
            parameters,
            track_properties: TrackProperties::new(),
        });
        let encoded = msg
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
            .expect("FETCH_OK の EXPIRES と LARGEST_OBJECT は encode できること");
        let (decoded, consumed) = ControlMessage::decode(&encoded)
            .expect("FETCH_OK の EXPIRES と LARGEST_OBJECT は decode できること");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, msg);

        // FETCH_OK context で許可されないパラメータは encode 時に拒否される
        let mut rejected = MessageParameters::new();
        rejected.push(MessageParameter {
            param_type: PARAM_AUTHORIZATION_TOKEN,
            value: MessageParameterValue::VarInt(1),
        });
        let msg = ControlMessage::RequestOk(RequestOk {
            parameters: rejected,
            track_properties: TrackProperties::new(),
        });
        assert!(matches!(
            msg.encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE),
            Err(MessageError::InvalidParameter)
        ));
    }

    /// 旧 FETCH_OK のメッセージ型 0x18 は未知の型として拒否されること
    ///
    /// draft-ietf-moq-transport-23 §9 Table 5: 0x18 は "RESERVED (FETCH_OK in <= 22)" で
    /// あり、FETCH_OK は REQUEST_OK (0x07) に統合された。§9 の "An endpoint that receives an
    /// unknown message type MUST close the session with UNEXPECTED_MESSAGE." に従い、
    /// 旧形式として解釈せず未知型として拒否する。
    #[test]
    fn legacy_fetch_ok_message_type_is_rejected() {
        // 旧 FETCH_OK (0x18) の本文: End of Track (0x00) + End Location (0, 0) +
        // Number of Parameters (0x00) + Track Properties (空)
        let payload = vec![0x00u8, 0x00, 0x00, 0x00];
        let encoded = frame(0x18, &payload);

        assert!(
            matches!(
                ControlMessage::decode(&encoded),
                Err(MessageError::InvalidMessageType(0x18))
            ),
            "旧 FETCH_OK の Type 0x18 は未知のメッセージ型として拒否されること"
        );
    }
}

mod redirect_full_track_name {
    use super::*;
    use shiguredo_moqt::message::{ErrorDetail, ErrorDetails, Redirect, RequestError};

    /// REQUEST_ERROR のメッセージ型 ID (draft-ietf-moq-transport-23 §9 Table 5)
    const MSG_REQUEST_ERROR: u64 = 0x05;

    /// REDIRECT Error Detail の Type 値 (draft-ietf-moq-transport-23 §9.4.2.1 (REDIRECT))
    const ERROR_DETAIL_REDIRECT: u64 = 0x01;

    /// 合計 4,096 バイト + `extra` の Full Track Name を持つ REDIRECT を組み立てる
    ///
    /// `extra` バイトだけ Track Name を伸ばす。
    fn request_error_with_redirect_total(extra: usize) -> RequestError {
        const NAMESPACE_LEN: usize = 4096 - 1;
        let ns = TrackNamespace::new(vec![vec![b'a'; NAMESPACE_LEN]])
            .expect("正当な Track Namespace である");
        let track_name = vec![b'b'; 1 + extra];
        let mut details = ErrorDetails::new();
        details.push(ErrorDetail::Redirect(Redirect {
            connect_uri: Vec::new(),
            track_namespace: ns,
            track_name,
        }));
        RequestError {
            error_code: shiguredo_moqt::error::REQUEST_REDIRECT,
            retry_interval: 0,
            reason: ReasonPhrase::new("redirect").expect("正当な Reason Phrase である"),
            error_details: details,
        }
    }

    /// 合計 4,096 バイトちょうどの Redirect target は encode / decode とも成功する (境界の非退行)
    #[test]
    fn redirect_at_limit_round_trips() {
        let msg = ControlMessage::RequestError(request_error_with_redirect_total(0));
        let bytes = msg
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
            .expect("4,096 バイトちょうどの Full Track Name は encode できること");
        let (decoded, _) = ControlMessage::decode(&bytes).expect("decode できること");
        assert_eq!(decoded, msg);
    }

    /// 合計 4,096 バイトを超える Redirect target は encode で拒否される
    ///
    /// draft-ietf-moq-transport-23 §8.7 (Track Namespace Structure): "If an endpoint receives a
    /// Track Namespace or a Full Track Name exceeding 4,096 bytes, it MUST close the session
    /// with FIELD_LENGTH_EXCEEDED."
    #[test]
    fn redirect_over_limit_is_rejected_on_encode() {
        let msg = ControlMessage::RequestError(request_error_with_redirect_total(1));
        assert!(matches!(
            msg.encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE),
            Err(MessageError::FieldLengthExceeded(_))
        ));
    }

    /// 合計 4,096 バイトを超える Redirect target は decode で拒否される
    ///
    /// Detail の Length は正しく設定し、Full Track Name の上限だけを超過させる。
    #[test]
    fn redirect_over_limit_is_rejected_on_decode() {
        const NAMESPACE_LEN: usize = 4096 - 1;
        let mut value = Vec::new();
        varint::encode(0, &mut value); // Connect URI 長
        varint::encode(1, &mut value); // Track Namespace フィールド数
        varint::encode(NAMESPACE_LEN as u64, &mut value);
        value.extend_from_slice(&vec![b'a'; NAMESPACE_LEN]);
        varint::encode(2, &mut value); // Track Name 長 (合計 4,097)
        value.extend_from_slice(b"bb");

        let mut payload = Vec::new();
        varint::encode(shiguredo_moqt::error::REQUEST_REDIRECT, &mut payload);
        payload.push(0); // Retry Interval
        varint::encode(1, &mut payload); // Error Reason 長
        payload.push(b'x'); // Error Reason
        varint::encode(ERROR_DETAIL_REDIRECT, &mut payload);
        varint::encode(value.len() as u64, &mut payload);
        payload.extend_from_slice(&value);

        let bytes = frame(MSG_REQUEST_ERROR, &payload);

        assert!(matches!(
            ControlMessage::decode(&bytes),
            Err(MessageError::FieldLengthExceeded(_))
        ));
    }
}

mod track_name_length {
    use super::*;
    use shiguredo_moqt::message::{ErrorDetail, ErrorDetails, Publish, Subscribe, TrackStatus};

    /// Track を名指す要求のメッセージ型 ID (draft-ietf-moq-transport-23 §9 Table 5)
    const MSG_SUBSCRIBE: u64 = 0x03;
    const MSG_TRACK_STATUS: u64 = 0x0D;
    const MSG_FETCH: u64 = 0x16;
    const MSG_PUBLISH: u64 = 0x1D;

    /// 0 フィールドの Track Namespace を作る
    ///
    /// draft-ietf-moq-transport-23 §2.4.1 (Track Naming) は Track Namespace を
    /// "an ordered set of between 0 and 32 Track Namespace Fields" と定義する。
    fn empty_namespace() -> TrackNamespace {
        TrackNamespace::new(Vec::new()).expect("0 フィールドの Track Namespace は正当である")
    }

    /// Track Namespace が空で、指定した Track Name を持つ 4 種の request を組み立てる
    fn track_named_requests(track_name: &[u8]) -> [(&'static str, ControlMessage); 4] {
        [
            (
                "SUBSCRIBE",
                ControlMessage::Subscribe(Subscribe {
                    request_id: 0,
                    track_namespace: empty_namespace(),
                    track_name: track_name.to_vec(),
                    parameters: MessageParameters::new(),
                }),
            ),
            (
                "PUBLISH",
                ControlMessage::Publish(Publish {
                    request_id: 0,
                    track_namespace: empty_namespace(),
                    track_name: track_name.to_vec(),
                    track_alias: 0,
                    parameters: MessageParameters::new(),
                    track_properties: TrackProperties::new(),
                }),
            ),
            (
                "FETCH",
                ControlMessage::Fetch(Fetch {
                    request_id: 0,
                    track_namespace: empty_namespace(),
                    track_name: track_name.to_vec(),
                    parameters: MessageParameters::new(),
                }),
            ),
            (
                "TRACK_STATUS",
                ControlMessage::TrackStatus(TrackStatus {
                    request_id: 0,
                    track_namespace: empty_namespace(),
                    track_name: track_name.to_vec(),
                    parameters: MessageParameters::new(),
                }),
            ),
        ]
    }

    /// 指定した Track Namespace と空の Track Name を持つ request の本文を組み立てる
    fn empty_track_name_payload(type_id: u64, namespace: &TrackNamespace) -> Vec<u8> {
        let mut payload = Vec::new();
        varint::encode(0, &mut payload); // Request ID
        varint::encode(namespace.fields().len() as u64, &mut payload);
        for field in namespace.fields() {
            varint::encode(field.len() as u64, &mut payload);
            payload.extend_from_slice(field);
        }
        varint::encode(0, &mut payload); // Track Name Length = 0
        if type_id == MSG_PUBLISH {
            varint::encode(0, &mut payload); // Track Alias
        }
        varint::encode(0, &mut payload); // Number of Parameters
        // PUBLISH の Track Properties は空のためバイトを追加しない
        payload
    }

    /// 4 種の request は空の Track Name を encode できない
    ///
    /// draft-ietf-moq-transport-23 §2.4.1 (Track Naming): "Each Track Name MUST contain at
    /// least one byte." 送信側は長さ 0 の Track Name を生成しない。
    #[test]
    fn request_messages_with_empty_track_name_are_rejected_on_encode() {
        for (label, msg) in track_named_requests(&[]) {
            let err = msg
                .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
                .expect_err("空の Track Name の encode は拒否されること");
            assert!(
                matches!(err, MessageError::ProtocolViolation(_)),
                "{label}: ProtocolViolation が期待されたが {err:?}"
            );
        }
    }

    /// 4 種の request は wire の空の Track Name を decode できない
    ///
    /// draft-ietf-moq-transport-23 §2.4.1 (Track Naming): 長さ 0 の Track Name を受信した
    /// endpoint は PROTOCOL_VIOLATION でセッションを閉じる (MUST)。通常名前空間と `.session`
    /// 名前空間のどちらでも同じ分類になる (`.session` は §6.5 の予約だが、Track Name の
    /// 長さ検証が先に成立する)。
    #[test]
    fn request_messages_with_empty_track_name_are_rejected_on_decode() {
        let namespaces = [
            ("normal", empty_namespace()),
            (
                ".session",
                TrackNamespace::new(vec![b".session".to_vec()])
                    .expect("正当な Track Namespace である"),
            ),
        ];
        for type_id in [MSG_SUBSCRIBE, MSG_PUBLISH, MSG_FETCH, MSG_TRACK_STATUS] {
            for (label, namespace) in &namespaces {
                let payload = empty_track_name_payload(type_id, namespace);
                let err = ControlMessage::decode(&frame(type_id, &payload))
                    .expect_err("空の Track Name は decode で拒否されること");
                assert!(
                    matches!(err, MessageError::ProtocolViolation(_)),
                    "type {type_id:#x} ({label}): ProtocolViolation が期待されたが {err:?}"
                );
            }
        }
    }

    /// 1 バイトの Track Name と 0 フィールドの Track Namespace は 4 種とも往復する
    ///
    /// draft-ietf-moq-transport-23 §2.4.1 (Track Naming) の下限 (Track Name 1 バイト、
    /// Track Namespace 0 フィールド) を同時に満たす要求が encode / decode を通ること。
    #[test]
    fn one_byte_track_name_with_empty_namespace_round_trips() {
        for (label, msg) in track_named_requests(b"c") {
            let encoded = msg
                .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
                .expect("1 バイトの Track Name は encode できること");
            let (decoded, consumed) = ControlMessage::decode(&encoded).expect("decode できること");
            assert_eq!(consumed, encoded.len());
            assert_eq!(decoded, msg, "{label}");
        }
    }

    /// namespace-scoped request 向けの空の Track Name は codec を通る
    ///
    /// draft-ietf-moq-transport-23 §9.4.2.1 (REDIRECT): namespace-scoped request
    /// (SUBSCRIBE_NAMESPACE / PUBLISH_NAMESPACE / SUBSCRIBE_TRACKS) の Redirect target は
    /// Track Name が空でなければならない (MUST)。REQUEST_ERROR の decode は応答先の request の
    /// 種別を知らないため、共有の Track Name デコードで一律に空を禁止せず、種別を知っている
    /// Session 層が検証する。ここでは codec が空の Track Name を保持したまま往復できることを
    /// 固定する。
    #[test]
    fn redirect_with_empty_track_name_round_trips() {
        let mut details = ErrorDetails::new();
        details.push(ErrorDetail::Redirect(Redirect {
            connect_uri: Vec::new(),
            track_namespace: empty_namespace(),
            track_name: Vec::new(),
        }));
        let msg = ControlMessage::RequestError(RequestError {
            error_code: shiguredo_moqt::error::REQUEST_REDIRECT,
            retry_interval: 0,
            reason: ReasonPhrase::new("redirect").expect("正当な Reason Phrase である"),
            error_details: details,
        });
        let encoded = msg
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
            .expect("namespace-scoped request 向けの Redirect は encode できること");
        let (decoded, consumed) = ControlMessage::decode(&encoded).expect("decode できること");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, msg, "空の Track Name が decode 後も保持されること");
    }
}

mod error_cases {
    use super::*;

    #[test]
    fn unknown_message_type() {
        // 0x3F は 1 バイト varint (< 64) だが有効なメッセージ型 ID ではない
        let buf = vec![0x3F, 0x00];
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
        // type=0x03 のあとに 2 バイト vi64 の先頭バイト (0x80) だけがある
        let buf = vec![0x03, 0x80];
        assert_eq!(
            ControlMessage::decode(&buf),
            Err(MessageError::UnexpectedEof)
        );
    }

    #[test]
    fn payload_shorter_than_length() {
        // type=0xAF, 0x00 (=0x2F00 SETUP), length=10, payload=5 bytes
        let buf = vec![0xAF, 0x00, 0x0A, 0x01, 0x02, 0x03, 0x04, 0x05];
        assert_eq!(
            ControlMessage::decode(&buf),
            Err(MessageError::UnexpectedEof)
        );
    }

    #[test]
    fn subscribe_with_kvp_length_exceeding_payload_is_key_value_formatting_error() {
        // payload 長は実際のバイト数と一致しているが、パラメータの KVP が宣言長 10 に対して
        // 3 バイトしか持たない。外側フレームは正常で、Type を理解したうえで Length/Value が
        // 直列化と一致しないため、draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure)
        // の MUST により MALFORMED_KEY_VALUE になる。
        let payload: Vec<u8> = vec![
            0x00, // Request ID
            0x01, 0x04, b'l', b'i', b'v', b'e', // Track Namespace (1 field)
            0x03, b'c', b'a', b'm', // Track Name
            0x01, // Number of Parameters
            0x03, // delta = AUTHORIZATION_TOKEN
            0x0A, // Length = 10
            0x01, 0x02, 0x03, // 3 バイトしかない
        ];
        let wire = frame(0x03, &payload); // MSG_SUBSCRIBE

        assert!(
            matches!(
                ControlMessage::decode(&wire),
                Err(MessageError::MalformedKeyValue(_))
            ),
            "KVP の宣言長超過は MALFORMED_KEY_VALUE であること"
        );
    }

    /// Track Properties の KVP 宣言長超過が MALFORMED_KEY_VALUE になること
    ///
    /// draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure) は既知 Type の
    /// Length/Value が定義と一致しない場合に MALFORMED_KEY_VALUE (0x6) でセッションを
    /// 閉じる MUST。SUBSCRIBE_OK の Track Properties は KVP 列であり、宣言長が残りバイトを
    /// 超える場合は TrackProperties::decode の分類がそのままメッセージ層へ返る。
    #[test]
    fn subscribe_ok_with_track_property_length_exceeding_payload_is_key_value_formatting_error() {
        // SUBSCRIBE_OK (0x04), Length = 7, Track Alias = 0,
        // Parameters: count=0,
        // Track Properties: delta=0x0B (既知の奇数型 IMMUTABLE_PROPERTIES) /
        // Length=10 / 実体 3 バイト
        let wire = [0x04, 0x07, 0x00, 0x00, 0x0B, 0x0A, 0x01, 0x02, 0x03];
        let err = ControlMessage::decode(&wire)
            .expect_err("Track Properties の宣言長超過は decode に失敗すること");
        assert!(
            matches!(err, MessageError::MalformedKeyValue(_)),
            "Track Properties の宣言長超過は MALFORMED_KEY_VALUE であること: {err:?}"
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

        let buf = frame(0x16, &inner); // MSG_FETCH

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
    fn decode_rejects_reason_phrase_longer_than_1024_as_field_length_exceeded() {
        // draft-ietf-moq-transport-23 §8.5 (Reason Phrase Structure) / §12.2
        // (Session Termination Codes): 1024 バイトを超える Reason Phrase の受信は
        // FIELD_LENGTH_EXCEEDED (0x401)。
        // PUBLISH_DONE (0x0B) のペイロードに長さ 1025 の Reason Phrase を載せる。
        let mut payload = vec![
            0x00, // status_code = 0
            0x00, // stream_count = 0
        ];
        varint::encode(1025, &mut payload); // reason phrase length (vi64)
        payload.extend_from_slice(&[b'x'; 1025]);

        let buf = frame(0x0B, &payload);

        assert!(
            matches!(
                ControlMessage::decode(&buf),
                Err(MessageError::ReasonPhraseTooLong)
            ),
            "1024 バイト超の Reason Phrase の decode は FIELD_LENGTH_EXCEEDED になること"
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

    // ─── パラメータスコープ検証 (draft-ietf-moq-transport-23 §9.19.1 (Parameter Scope)) ──────────────────

    /// RENDEZVOUS_TIMEOUT (0x04) は定義済みパラメータのため encode / decode で受理される
    ///
    /// draft-ietf-moq-transport-23 §9.19.6 (RENDEZVOUS TIMEOUT Parameter): "It MAY appear in
    /// a SUBSCRIBE message."
    /// §16.7 (Message Parameters) の Table 14 にも登録されており、§9.19 (Control Message
    /// Parameters) が INVALID_PARAMETER を要求する「未知のパラメータ」には該当しない。
    /// 本ライブラリは値を解釈しないが、受理して `MessageParameters` に残す。
    #[test]
    fn subscribe_with_rendezvous_timeout_is_accepted() {
        use shiguredo_moqt::message::Subscribe;
        use shiguredo_moqt::message_parameter::{
            MessageParameter, MessageParameterValue, PARAM_RENDEZVOUS_TIMEOUT,
        };
        let mut parameters = MessageParameters::new();
        parameters.push(MessageParameter {
            param_type: PARAM_RENDEZVOUS_TIMEOUT,
            value: MessageParameterValue::VarInt(500),
        });
        let msg = ControlMessage::Subscribe(Subscribe {
            request_id: 0,
            track_namespace: TrackNamespace::new(vec![b"example".to_vec()])
                .expect("正当な namespace である"),
            track_name: b"video".to_vec(),
            parameters,
        });
        let encoded = msg
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
            .expect("定義済みパラメータを含む SUBSCRIBE は encode できること");
        let (decoded, _) = ControlMessage::decode(&encoded)
            .expect("定義済みパラメータを含む SUBSCRIBE は decode できること");
        let ControlMessage::Subscribe(subscribe) = decoded else {
            panic!("Subscribe が期待された");
        };
        // 0x04 が decode 後も MessageParameters に残ること (値は解釈しない)
        let count = subscribe
            .parameters
            .as_slice()
            .iter()
            .filter(|p| p.param_type == PARAM_RENDEZVOUS_TIMEOUT)
            .count();
        assert_eq!(count, 1, "0x04 が decode 後も残ること");
    }

    /// Table 5 に定義済みで本ライブラリが実装しない型は Unsupported として decode できる
    ///
    /// draft-ietf-moq-transport-23 §1.6 (Modularity): "Limited endpoints SHOULD respond to any
    /// unsupported messages with the appropriate NOT_SUPPORTED error code, rather than ignoring
    /// them." decode した本体を encode で再構成できることも固定する。
    #[test]
    fn unsupported_control_messages_roundtrip() {
        // request として届く型 (本体が Request ID で始まる)
        for (type_id, request_id) in [
            (0x06u64, Some(3u64)), // PUBLISH_NAMESPACE
            (0x50, Some(5)),       // SUBSCRIBE_NAMESPACE
            (0x51, Some(7)),       // SUBSCRIBE_TRACKS
        ] {
            let original = ControlMessage::Unsupported {
                type_id,
                request_id,
                // 本体は Request ID (vi64) で始まる生バイト列。decode は本体先頭の varint を
                // request_id として読むため、body の先頭を request_id と一致させる
                body: vec![request_id.expect("request_id は Some") as u8],
            };
            let bytes = original
                .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
                .expect("encode できること");
            let (decoded, _) = ControlMessage::decode(&bytes).expect("decode できること");
            assert_eq!(decoded, original, "roundtrip で一致すること: {type_id:#x}");
        }
        // 応答専用の型 (Request ID を持たない)
        for type_id in [0x08u64, 0x0E, 0x0F] {
            let original = ControlMessage::Unsupported {
                type_id,
                request_id: None,
                body: vec![0x01, 0x02],
            };
            let bytes = original
                .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
                .expect("encode できること");
            let (decoded, _) = ControlMessage::decode(&bytes).expect("decode できること");
            assert_eq!(decoded, original, "roundtrip で一致すること: {type_id:#x}");
        }
    }

    /// `Number of Parameters` が途中で終わる FILL_PARAMETERS を持つ SUBSCRIBE は MALFORMED_MESSAGE で拒否される
    ///
    /// draft-ietf-moq-transport-23 §9.19.15 (FILL PARAMETERS Parameter) の値は
    /// `Number of Parameters (vi64)` とその個数の Parameters であり、外側の byte length を持たない。
    /// 値が個数の varint の途中で切れている場合は、外側のパラメータ列と同じく varint の
    /// 途中終端になる (分類の根拠はパラメータ層の `decode_fill_parameters` の doc を参照)。
    /// 本文の解析中に生じた UnexpectedEof はメッセージ層が「本文が Length より短い」ことを
    /// 意味する MALFORMED_MESSAGE に写すため、その写像もここで固定する。
    /// エラー文言そのものはパラメータ層のテストが固定する。
    #[test]
    fn subscribe_with_truncated_fill_parameter_count_is_rejected() {
        // この wire は encode が生成しない不正形である (encode は最低 1 バイトの
        // Number of Parameters を書く)。
        // SUBSCRIBE (0x03), Length = 14, Request ID = 7,
        // Track Namespace: count=1 / "live", Track Name: "cam",
        // Parameters: count=1 / delta=0x23 (FILL_PARAMETERS) / Number of Parameters の 1 バイト目だけ
        let wire = [
            0x03, 0x0E, 0x07, 0x01, 0x04, 0x6C, 0x69, 0x76, 0x65, 0x03, 0x63, 0x61, 0x6D, 0x01,
            0x23, 0x80,
        ];
        // 本文の解析中に生じた UnexpectedEof は「本文が Length より短い」ことを意味するため、
        // メッセージ層は MALFORMED_MESSAGE に写す (§9 (Control Messages))。
        assert!(
            matches!(
                ControlMessage::decode(&wire),
                Err(MessageError::MalformedMessage(_))
            ),
            "Number of Parameters の途中終端を持つ SUBSCRIBE は MALFORMED_MESSAGE であること"
        );
    }

    /// 内側 0 個の FILL_PARAMETERS を持つ SUBSCRIBE は受理され、空の内側が保持される
    ///
    /// encode は内側 0 個を `Number of Parameters = 0` (`0x00` 1 バイト) として書く。
    /// 値に byte length は付かないため、外側のパラメータ列は delta の直後がその 1 バイトになる。
    /// 拒否テストと対になる受理側の固定であり、内側の count が保持されることも確認する。
    #[test]
    fn subscribe_with_count_zero_fill_parameters_is_accepted() {
        use shiguredo_moqt::message::Subscribe;
        use shiguredo_moqt::message_parameter::{
            MessageParameter, MessageParameterValue, PARAM_FILL_PARAMETERS,
        };
        let mut parameters = MessageParameters::new();
        parameters.push(MessageParameter {
            param_type: PARAM_FILL_PARAMETERS,
            value: MessageParameterValue::FillParameters(MessageParameters::new()),
        });
        let msg = ControlMessage::Subscribe(Subscribe {
            request_id: 7,
            track_namespace: TrackNamespace::new(vec![b"live".to_vec()])
                .expect("正当な namespace である"),
            track_name: b"cam".to_vec(),
            parameters,
        });
        let encoded = msg
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
            .expect("内側 0 個の FILL_PARAMETERS は encode できること");
        // 内側 0 個の値は count 0 の `0x00` 1 バイトだけであり、Length の varint は現れない。
        // SUBSCRIBE (0x03), Length = 14, Request ID = 7, Track Namespace: count=1 / "live",
        // Track Name: "cam", Parameters: count=1 / delta=0x23 / 内側 count=0
        assert_eq!(
            encoded,
            [
                0x03, 0x0E, 0x07, 0x01, 0x04, 0x6C, 0x69, 0x76, 0x65, 0x03, 0x63, 0x61, 0x6D, 0x01,
                0x23, 0x00,
            ],
            "encode の出力が count 付きの FILL_PARAMETERS を持つ SUBSCRIBE であること"
        );

        let (decoded, consumed) = ControlMessage::decode(&encoded)
            .expect("内側 0 個の FILL_PARAMETERS を持つ SUBSCRIBE は decode できること");
        assert_eq!(consumed, encoded.len());
        let ControlMessage::Subscribe(subscribe) = decoded else {
            panic!("Subscribe が期待された");
        };
        let fill = subscribe
            .parameters
            .fill_parameters()
            .expect("FILL_PARAMETERS が保持されること");
        assert!(
            fill.is_empty(),
            "内側 0 個の FILL_PARAMETERS は空の内側として保持されること"
        );
    }

    /// FILL_PARAMETERS を許可しないメッセージに届いた FILL_PARAMETERS の分類
    ///
    /// 値のデコードが scope 検証より先に走るため、内側の値が途中で切れている場合は
    /// scope 違反の INVALID_PARAMETER ではなく値の形式違反の MALFORMED_MESSAGE を返す。
    /// 値が最後まで読めれば scope 違反の INVALID_PARAMETER になる。
    /// どちらもセッションを閉じるが、報告される code が変わることを固定する。
    #[test]
    fn subscribe_ok_with_fill_parameters_reports_value_error_first() {
        // SUBSCRIBE_OK (0x04), Length = 6, Track Alias = 0,
        // Parameters: count=1 / delta=0x23 (FILL_PARAMETERS) / 内側 count=1 /
        //             delta=0x0A (FILL_TIMEOUT) / 値の varint が途中終端 (0x80)
        assert!(
            matches!(
                ControlMessage::decode(&[0x04, 0x06, 0x00, 0x01, 0x23, 0x01, 0x0A, 0x80]),
                Err(MessageError::MalformedMessage(_))
            ),
            "内側の値の途中終端は scope 検証より先に MALFORMED_MESSAGE になること"
        );

        // 対照: SUBSCRIBE_OK (0x04), Length = 4, Track Alias = 0,
        // Parameters: count=1 / delta=0x23 (FILL_PARAMETERS) / 内側 count=0
        assert!(
            matches!(
                ControlMessage::decode(&[0x04, 0x04, 0x00, 0x01, 0x23, 0x00]),
                Err(MessageError::InvalidParameter)
            ),
            "値が最後まで読めれば Table 7 外として INVALID_PARAMETER になること"
        );
    }

    /// Table 14 に無い未定義のパラメータ型は encode / decode で拒否される
    ///
    /// draft-ietf-moq-transport-23 §9.19 (Control Message Parameters): "An endpoint that
    /// receives an unknown Message Parameter MUST close the session with INVALID_PARAMETER."
    #[test]
    fn subscribe_with_undefined_parameter_type_is_rejected() {
        use shiguredo_moqt::message::Subscribe;
        use shiguredo_moqt::message_parameter::{MessageParameter, MessageParameterValue};
        // Table 14 に無い型 (0x7F) を使う
        let mut parameters = MessageParameters::new();
        parameters.push(MessageParameter {
            param_type: 0x7F,
            value: MessageParameterValue::VarInt(1),
        });
        let msg = ControlMessage::Subscribe(Subscribe {
            request_id: 0,
            track_namespace: TrackNamespace::new(vec![b"example".to_vec()])
                .expect("正当な namespace である"),
            track_name: b"video".to_vec(),
            parameters,
        });
        assert!(
            matches!(
                msg.encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE),
                Err(MessageError::InvalidParameter)
            ),
            "未定義のパラメータ型は encode で拒否されること"
        );
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
            msg.encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE),
            Err(MessageError::InvalidParameter)
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
            msg.encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE),
            Err(MessageError::InvalidParameter)
        ));
    }
}

mod publish {
    use super::*;
    use shiguredo_moqt::message::{
        Fetch, Publish, PublishStateNotify, RequestUpdate, Subscribe, TrackStatus,
    };
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_SUBSCRIBE_TRACKS_ID,
    };

    /// SUBSCRIBE_TRACKS_ID を持つ PUBLISH の golden バイト列と往復
    ///
    /// draft-ietf-moq-transport-23 §9.8 (PUBLISH) は PUBLISH が運べるパラメータに
    /// SUBSCRIBE_TRACKS_ID を含め、§9.19.21 (SUBSCRIBE_TRACKS_ID Parameter) は値を varint と
    /// 定める。値 300 は 2 バイト varint (`0x81 0x2C`) であり、uint8 との取り違えも検出できる。
    #[test]
    fn subscribe_tracks_id_golden_bytes_and_round_trip() {
        let mut parameters = MessageParameters::new();
        parameters.push(MessageParameter {
            param_type: PARAM_SUBSCRIBE_TRACKS_ID,
            value: MessageParameterValue::VarInt(300),
        });
        let msg = ControlMessage::Publish(Publish {
            request_id: 0,
            track_namespace: TrackNamespace::new(vec![b"live".to_vec()])
                .expect("正当な namespace である"),
            track_name: b"cam".to_vec(),
            track_alias: 1,
            parameters,
            track_properties: TrackProperties::new(),
        });
        // PUBLISH (0x1D), Length = 16, Request ID = 0,
        // Track Namespace: count=1 / "live", Track Name: "cam", Track Alias = 1,
        // Parameters: count=1 / delta=0x36 / 値=300 (0x81 0x2C)
        // Track Properties は空 (カウントプレフィックスを持たない)
        let golden = vec![
            0x1D, 0x10, 0x00, 0x01, 0x04, 0x6C, 0x69, 0x76, 0x65, 0x03, 0x63, 0x61, 0x6D, 0x01,
            0x01, 0x36, 0x81, 0x2C,
        ];
        let encoded = msg
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
            .expect("SUBSCRIBE_TRACKS_ID を持つ PUBLISH は encode できること");
        assert_eq!(
            encoded, golden,
            "SUBSCRIBE_TRACKS_ID を持つ PUBLISH の golden バイト列"
        );

        let (decoded, consumed) = ControlMessage::decode(&golden)
            .expect("SUBSCRIBE_TRACKS_ID を持つ PUBLISH は decode できること");
        assert_eq!(
            consumed,
            golden.len(),
            "golden バイト列をすべて消費すること"
        );
        assert_eq!(
            decoded, msg,
            "decode 結果が encode 前のメッセージと一致すること"
        );
        let ControlMessage::Publish(publish) = decoded else {
            panic!("Publish が期待された");
        };
        assert_eq!(
            publish.parameters.subscribe_tracks_id(),
            Some(300),
            "decode 後も varint の値が読めること"
        );
    }

    /// SUBSCRIBE_TRACKS_ID は PUBLISH 以外のメッセージでは拒否される
    ///
    /// draft-ietf-moq-transport-23 §9.19.21 (SUBSCRIBE_TRACKS_ID Parameter) は出現先を
    /// PUBLISH に限り、§9.19.1 (Parameter Scope) は許可されないメッセージ型での受信を
    /// INVALID_PARAMETER で閉じることを MUST とする。encode / decode の両経路を固定する。
    /// 実装しない SUBSCRIBE_TRACKS (0x51) は codec 層が本体を解析しないため、
    /// スコープ検証がセッション層にあることも併せて固定する。
    #[test]
    fn subscribe_tracks_id_rejected_outside_publish() {
        let parameters = || {
            let mut parameters = MessageParameters::new();
            parameters.push(MessageParameter {
                param_type: PARAM_SUBSCRIBE_TRACKS_ID,
                value: MessageParameterValue::VarInt(300),
            });
            parameters
        };
        let namespace =
            || TrackNamespace::new(vec![b"live".to_vec()]).expect("正当な namespace である");
        let messages = vec![
            ControlMessage::Subscribe(Subscribe {
                request_id: 0,
                track_namespace: namespace(),
                track_name: b"cam".to_vec(),
                parameters: parameters(),
            }),
            ControlMessage::Fetch(Fetch {
                request_id: 0,
                track_namespace: namespace(),
                track_name: b"cam".to_vec(),
                parameters: parameters(),
            }),
            ControlMessage::RequestUpdate(RequestUpdate {
                request_id: 1,
                parameters: parameters(),
            }),
            ControlMessage::TrackStatus(TrackStatus {
                request_id: 0,
                track_namespace: namespace(),
                track_name: b"cam".to_vec(),
                parameters: parameters(),
            }),
            ControlMessage::PublishStateNotify(PublishStateNotify {
                parameters: parameters(),
            }),
        ];
        for message in &messages {
            assert!(
                matches!(
                    message.encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE),
                    Err(MessageError::InvalidParameter)
                ),
                "SUBSCRIBE_TRACKS_ID は {message:?} で encode できないこと"
            );
        }

        // SUBSCRIBE (0x03), Length = 15, Request ID = 0, Track Namespace: count=1 / "live",
        // Track Name: "cam", Parameters: count=1 / delta=0x36 / 値=300 (0x81 0x2C)
        let subscribe = vec![
            0x03, 0x0F, 0x00, 0x01, 0x04, 0x6C, 0x69, 0x76, 0x65, 0x03, 0x63, 0x61, 0x6D, 0x01,
            0x01, 0x36, 0x81, 0x2C,
        ];
        assert!(
            matches!(
                ControlMessage::decode(&subscribe),
                Err(MessageError::InvalidParameter)
            ),
            "SUBSCRIBE に載った SUBSCRIBE_TRACKS_ID の decode は拒否されること"
        );
        // FETCH (0x16) は SUBSCRIBE と同じ本体構造である
        let fetch = vec![
            0x16, 0x0F, 0x00, 0x01, 0x04, 0x6C, 0x69, 0x76, 0x65, 0x03, 0x63, 0x61, 0x6D, 0x01,
            0x01, 0x36, 0x81, 0x2C,
        ];
        assert!(
            matches!(
                ControlMessage::decode(&fetch),
                Err(MessageError::InvalidParameter)
            ),
            "FETCH に載った SUBSCRIBE_TRACKS_ID の decode は拒否されること"
        );
        // REQUEST_UPDATE (0x02), Length = 5, Request ID = 1,
        // Parameters: count=1 / delta=0x36 / 値=300 (0x81 0x2C)
        let request_update = vec![0x02, 0x05, 0x01, 0x01, 0x36, 0x81, 0x2C];
        assert!(
            matches!(
                ControlMessage::decode(&request_update),
                Err(MessageError::InvalidParameter)
            ),
            "REQUEST_UPDATE に載った SUBSCRIBE_TRACKS_ID の decode は拒否されること"
        );
        // TRACK_STATUS (0x0D) は SUBSCRIBE と同じ本体構造である
        let track_status = vec![
            0x0D, 0x0F, 0x00, 0x01, 0x04, 0x6C, 0x69, 0x76, 0x65, 0x03, 0x63, 0x61, 0x6D, 0x01,
            0x01, 0x36, 0x81, 0x2C,
        ];
        assert!(
            matches!(
                ControlMessage::decode(&track_status),
                Err(MessageError::InvalidParameter)
            ),
            "TRACK_STATUS に載った SUBSCRIBE_TRACKS_ID の decode は拒否されること"
        );
        // PUBLISH_STATE_NOTIFY (0x22), Length = 4,
        // Parameters: count=1 / delta=0x36 / 値=300 (0x81 0x2C)
        let publish_state_notify = vec![0x22, 0x04, 0x01, 0x36, 0x81, 0x2C];
        assert!(
            matches!(
                ControlMessage::decode(&publish_state_notify),
                Err(MessageError::InvalidParameter)
            ),
            "PUBLISH_STATE_NOTIFY に載った SUBSCRIBE_TRACKS_ID の decode は拒否されること"
        );
        // SUBSCRIBE_TRACKS (0x51) は本ライブラリが実装しない request であり、codec は本体を
        // 解析せず `Unsupported` として保持する (§1.6 (Modularity) の SHOULD)。§9.19.1
        // (Parameter Scope) の MUST 検証はセッション層 (`handle_unsupported_request`) が
        // 行うため、ここでは decode が生バイト列をそのまま保つことだけを固定する。
        // 本体は Request ID = 0、Track Namespace Prefix: count=1 / "live"、
        // Parameters: count=1 / delta=0x36 / 値=300 (0x81 0x2C)
        let subscribe_tracks = vec![
            0x51, 0x0B, 0x00, 0x01, 0x04, 0x6C, 0x69, 0x76, 0x65, 0x01, 0x36, 0x81, 0x2C,
        ];
        let (decoded, consumed) = ControlMessage::decode(&subscribe_tracks)
            .expect("SUBSCRIBE_TRACKS は未対応型として decode できること");
        assert_eq!(
            consumed,
            subscribe_tracks.len(),
            "SUBSCRIBE_TRACKS のフレーム全体を消費すること"
        );
        assert_eq!(
            decoded,
            ControlMessage::Unsupported {
                type_id: 0x51,
                request_id: Some(0),
                body: subscribe_tracks[2..].to_vec(),
            },
            "SUBSCRIBE_TRACKS は本体を解析せず Unsupported として保持されること"
        );
    }
}

mod publish_state_notify {
    use super::*;
    use shiguredo_moqt::message::PublishStateNotify;
    use shiguredo_moqt::message_parameter::LocationFilter;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_EXPIRES, PARAM_LARGEST_OBJECT,
        PARAM_LOCATION_FILTER, PARAM_PAUSED,
    };

    /// 許可パラメータ群 (PAUSED / LOCATION_FILTER / LARGEST_OBJECT) を作る
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
            param_type: PARAM_PAUSED,
            value: MessageParameterValue::Uint8(1),
        });
        params.push(MessageParameter {
            param_type: PARAM_LOCATION_FILTER,
            value: MessageParameterValue::LocationFilter(LocationFilter::NextObject),
        });
        params
    }

    #[test]
    fn round_trip() {
        // draft-ietf-moq-transport-23 §9.10 (PUBLISH_STATE_NOTIFY) の往復
        let msg = ControlMessage::PublishStateNotify(PublishStateNotify {
            parameters: sample_params(),
        });
        let encoded = msg
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
            .expect("正当なテスト入力の encode は成功する");
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
            msg.encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE),
            Err(MessageError::InvalidParameter)
        ));
    }

    #[test]
    fn disallowed_parameter_rejected_on_decode() {
        // EXPIRES (0x08) を含むワイヤ形式は PROTOCOL_VIOLATION
        // payload: count=1, delta=0x08, value=100
        let payload = vec![0x01u8, 0x08, 0x64];
        let buf = frame(0x22, &payload); // MSG_PUBLISH_STATE_NOTIFY
        assert!(matches!(
            ControlMessage::decode(&buf),
            Err(MessageError::InvalidParameter)
        ));
    }
}

mod request_ok {
    use super::*;
    use shiguredo_moqt::message::RequestOk;
    use shiguredo_moqt::message_parameter::LocationFilter;
    use shiguredo_moqt::message_parameter::{
        MessageParameter, MessageParameterValue, PARAM_EXPIRES, PARAM_LARGEST_OBJECT,
        PARAM_LOCATION_FILTER, PARAM_NEW_GROUP_REQUEST, PARAM_OBJECT_DELIVERY_TIMEOUT,
        PARAM_OBJECT_PROPERTY_FILTER, PARAM_OBJECTID_FILTER, PARAM_PAUSED, PARAM_PRIORITY_FILTER,
        PARAM_SUBGROUP_DELIVERY_TIMEOUT, PARAM_SUBGROUP_FILTER, PARAM_SUBSCRIBE_TRACKS_ID,
        PARAM_SUBSCRIBER_PRIORITY,
    };

    /// 和集合外のパラメータを含む REQUEST_OK は codec 層で INVALID_PARAMETER になる
    ///
    /// draft-ietf-moq-transport-23 §9.3 (REQUEST_OK) が応答 context ごとに列挙する許可パラメータの
    /// 和集合は `EXPIRES` と `LARGEST_OBJECT` の 2 型であり、それ以外は §9.19.1 (Parameter Scope) の
    /// MUST により INVALID_PARAMETER で閉じる。encode と decode の両経路で同じ検証が働くことを
    /// 固定する。和集合外の 11 型すべてを対象にし、1 型だけ受理する退行も検出できるようにする。
    #[test]
    fn out_of_scope_params_are_protocol_violation() {
        // (Parameter Type, typed 値, wire 上の値バイト列)
        let out_of_scope: [(u64, MessageParameterValue, &[u8]); 11] = [
            (
                PARAM_OBJECT_DELIVERY_TIMEOUT,
                MessageParameterValue::VarInt(1),
                &[0x01],
            ),
            (
                PARAM_SUBGROUP_DELIVERY_TIMEOUT,
                MessageParameterValue::VarInt(1),
                &[0x01],
            ),
            (PARAM_PAUSED, MessageParameterValue::Uint8(1), &[0x01]),
            (
                PARAM_SUBSCRIBER_PRIORITY,
                MessageParameterValue::Uint8(1),
                &[0x01],
            ),
            (
                PARAM_LOCATION_FILTER,
                MessageParameterValue::LocationFilter(LocationFilter::NextObject),
                // Location Filter Type 0x05 (Next Object) は Type のみの 1 バイト
                &[0x05],
            ),
            (
                PARAM_SUBGROUP_FILTER,
                MessageParameterValue::LengthPrefixed(vec![0x00]),
                // Length 1 + 値 1 バイト
                &[0x01, 0x00],
            ),
            (
                PARAM_OBJECTID_FILTER,
                MessageParameterValue::LengthPrefixed(vec![0x00]),
                &[0x01, 0x00],
            ),
            (
                PARAM_PRIORITY_FILTER,
                MessageParameterValue::LengthPrefixed(vec![0x00]),
                &[0x01, 0x00],
            ),
            (
                PARAM_OBJECT_PROPERTY_FILTER,
                MessageParameterValue::LengthPrefixed(vec![0x00]),
                &[0x01, 0x00],
            ),
            (
                PARAM_NEW_GROUP_REQUEST,
                MessageParameterValue::VarInt(1),
                &[0x01],
            ),
            // SUBSCRIBE_TRACKS_ID は SUBSCRIBE_TRACKS 由来の PUBLISH にだけ出現し
            // (draft-ietf-moq-transport-23 §9.19.21 (SUBSCRIBE_TRACKS_ID Parameter))、
            // REQUEST_OK のどの context でも許可されない。
            (
                PARAM_SUBSCRIBE_TRACKS_ID,
                MessageParameterValue::VarInt(1),
                &[0x01],
            ),
        ];
        for (param_type, value, value_bytes) in out_of_scope {
            let mut params = MessageParameters::new();
            params.push(MessageParameter { param_type, value });
            let msg = ControlMessage::RequestOk(RequestOk {
                parameters: params,
                track_properties: TrackProperties::default(),
            });
            assert!(
                matches!(
                    msg.encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE),
                    Err(MessageError::InvalidParameter)
                ),
                "型 {param_type:#x} の encode は INVALID_PARAMETER になること"
            );

            // decode 経路: 同じパラメータを wire 形式で手組みする
            // (REQUEST_OK の Type は 0x07、Track Properties は空なのでペイロードは
            // Parameters ブロックのみになる)
            let mut payload = Vec::new();
            shiguredo_moqt::varint::encode(1, &mut payload);
            shiguredo_moqt::varint::encode(param_type, &mut payload);
            payload.extend_from_slice(value_bytes);
            let wire = frame(0x07, &payload);
            assert!(
                matches!(
                    ControlMessage::decode(&wire),
                    Err(MessageError::InvalidParameter)
                ),
                "型 {param_type:#x} の decode は INVALID_PARAMETER になること"
            );
        }
    }

    /// 和集合内の 2 型 (EXPIRES / LARGEST_OBJECT) は codec 層で受理される
    ///
    /// draft-ietf-moq-transport-23 §9.3 (REQUEST_OK) は PUBLISH_OK: EXPIRES、
    /// FETCH_OK / REQUEST_UPDATE_OK / TRACK_STATUS_OK: EXPIRES と LARGEST_OBJECT を列挙する。
    /// codec 層は応答 context を区別できないため和集合で検証するが、TRACK_STATUS_OK と
    /// FETCH_OK の EXPIRES がワイヤ上で受理されることはこのテストが固定する。
    /// 拒否側だけを固定すると、許可集合をさらに狭める退行 (正当なパラメータを拒否する) を
    /// 検出できないため、encode の成功も確認する。往復の網羅は PBT が担う。
    #[test]
    fn allowed_params_are_accepted() {
        for (param_type, value) in [
            (PARAM_EXPIRES, MessageParameterValue::VarInt(300)),
            (
                PARAM_LARGEST_OBJECT,
                MessageParameterValue::Location {
                    group: 4,
                    object: 5,
                },
            ),
        ] {
            let mut params = MessageParameters::new();
            params.push(MessageParameter { param_type, value });
            let msg = ControlMessage::RequestOk(RequestOk {
                parameters: params,
                track_properties: TrackProperties::default(),
            });
            assert!(
                msg.encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE).is_ok(),
                "型 {param_type:#x} の encode は成功すること"
            );
        }
    }
}
