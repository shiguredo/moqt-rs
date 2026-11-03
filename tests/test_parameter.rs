use shiguredo_moqt::{
    error::MessageError,
    message_parameter::AuthorizationToken,
    parameter::{SetupOption, SetupOptionValue, SetupOptions},
};

/// Setup Option Type 定数が §16.4 (Setup Options) レジストリ表と一致する
#[test]
fn setup_option_type_constants_match_registry() {
    use shiguredo_moqt::parameter::{
        SETUP_OPTION_AUTHORITY, SETUP_OPTION_AUTHORIZATION_TOKEN,
        SETUP_OPTION_MAX_AUTH_TOKEN_CACHE_SIZE, SETUP_OPTION_MAX_CONTROL_MESSAGE_SIZE,
        SETUP_OPTION_MAX_FILTER_RANGES, SETUP_OPTION_MAX_REQUEST_UPDATES,
        SETUP_OPTION_MOQT_IMPLEMENTATION, SETUP_OPTION_PATH,
    };
    assert_eq!(SETUP_OPTION_PATH, 0x01);
    assert_eq!(SETUP_OPTION_AUTHORIZATION_TOKEN, 0x03);
    assert_eq!(SETUP_OPTION_MAX_AUTH_TOKEN_CACHE_SIZE, 0x04);
    assert_eq!(SETUP_OPTION_AUTHORITY, 0x05);
    assert_eq!(SETUP_OPTION_MAX_FILTER_RANGES, 0x06);
    assert_eq!(SETUP_OPTION_MOQT_IMPLEMENTATION, 0x07);
    assert_eq!(SETUP_OPTION_MAX_REQUEST_UPDATES, 0x08);
    assert_eq!(SETUP_OPTION_MAX_CONTROL_MESSAGE_SIZE, 0x09);
}

/// MAX_CONTROL_MESSAGE_SIZE (Option Type 0x09) のエンコード / デコード
///
/// draft-ietf-moq-transport-23 §9.1.8 (MAX_CONTROL_MESSAGE_SIZE): 型番号 0x09 は奇数型の
/// ため §8.3 (Key-Value-Pair Structure) の Length 付きバイト列であり、その中身が varint で
/// ある。最小値は 65535、省略時の既定値も 65535 である。
mod max_control_message_size {
    use super::*;
    use shiguredo_moqt::parameter::{
        DEFAULT_MAX_CONTROL_MESSAGE_SIZE, MIN_MAX_CONTROL_MESSAGE_SIZE,
        SETUP_OPTION_MAX_CONTROL_MESSAGE_SIZE, SETUP_OPTION_PATH,
    };

    /// MAX_CONTROL_MESSAGE_SIZE を持つ SetupOptions を作る
    fn options_with(value: u64) -> SetupOptions {
        let mut options = SetupOptions::new();
        options.push(SetupOption {
            option_type: SETUP_OPTION_MAX_CONTROL_MESSAGE_SIZE,
            value: SetupOptionValue::VarInt(value),
        });
        options
    }

    /// 既定値と最小値が仕様 (65535) と一致する
    #[test]
    fn default_and_minimum_match_specification() {
        assert_eq!(DEFAULT_MAX_CONTROL_MESSAGE_SIZE, 65_535);
        assert_eq!(MIN_MAX_CONTROL_MESSAGE_SIZE, 65_535);
    }

    /// option を省略した場合に `max_control_message_size` が None を返す
    ///
    /// 省略時の既定値の適用は Session 層の責務であり、codec は省略をそのまま表す。
    #[test]
    fn omitted_option_returns_none() {
        assert_eq!(SetupOptions::new().max_control_message_size(), None);
    }

    /// 型番号 0x09 は Length 付きの varint で書かれる
    ///
    /// ワイヤは delta type (0x09) + Length + varint 値である
    ///
    /// draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure): 奇数型は Length を持つ。
    /// 65535 は 3 バイトの vi64 (0xC0 0xFF 0xFF) で表されるため、Length は 0x03 になる。
    #[test]
    fn wire_format_is_length_prefixed_varint() {
        let mut buf = Vec::new();
        options_with(65_535)
            .encode(&mut buf)
            .expect("正当なテスト入力の encode は成功する");
        assert_eq!(
            buf,
            vec![0x09, 0x03, 0xC0, 0xFF, 0xFF],
            "delta type + Length (3) + 3 バイト vi64 の値であること"
        );

        let (decoded, consumed) =
            SetupOptions::decode(&buf).expect("encode した option を decode できること");
        assert_eq!(consumed, buf.len());
        assert_eq!(
            decoded.max_control_message_size(),
            Some(65_535),
            "decode した値を取得できること"
        );
    }

    /// Length と varint のバイト数が一致しないワイヤは拒否される
    ///
    /// draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure): Type を理解したうえで
    /// Length/Value が直列化と一致しない場合は MALFORMED_KEY_VALUE である。
    #[test]
    fn decode_rejects_length_mismatch() {
        // Length は 2 だが、中身の varint は 3 バイト (65535) を要求する
        let wire = [0x09u8, 0x02, 0xC0, 0xFF, 0xFF];
        let err = SetupOptions::decode(&wire).expect_err("Length と varint 長の不一致は拒否される");
        assert!(
            matches!(err, MessageError::MalformedKeyValue(_)),
            "MalformedKeyValue を期待したが {err:?} になった"
        );
    }

    /// Length が 0 のワイヤは拒否される
    ///
    /// 空のバイト列は varint として復号できないため MALFORMED_KEY_VALUE とする。
    #[test]
    fn decode_rejects_empty_value() {
        let wire = [0x09u8, 0x00];
        let err = SetupOptions::decode(&wire).expect_err("Length 0 の値は拒否される");
        assert!(
            matches!(err, MessageError::MalformedKeyValue(_)),
            "MalformedKeyValue を期待したが {err:?} になった"
        );
    }

    /// Length が残りバッファを超えるワイヤは拒否される
    ///
    /// draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure) の MUST により
    /// MALFORMED_KEY_VALUE とする。
    #[test]
    fn decode_rejects_length_exceeding_buffer() {
        // Length は 3 だが、中身は 2 バイトしかない
        let wire = [0x09u8, 0x03, 0xC0, 0xFF];
        let err = SetupOptions::decode(&wire).expect_err("Length 超過は拒否される");
        assert!(
            matches!(err, MessageError::MalformedKeyValue(_)),
            "MalformedKeyValue を期待したが {err:?} になった"
        );
    }

    /// Length の範囲に varint の後続バイトが余っているワイヤは拒否される
    ///
    /// 値は単一の varint であり、Length のバイト数をすべて消費しなければならない。
    #[test]
    fn decode_rejects_trailing_bytes_in_value() {
        // Length は 3 だが、varint (65535) は 3 バイトで、さらに 0x00 が余る
        let wire = [0x09u8, 0x04, 0xC0, 0xFF, 0xFF, 0x00];
        let err = SetupOptions::decode(&wire).expect_err("値の余剰バイトは拒否される");
        assert!(
            matches!(err, MessageError::MalformedKeyValue(_)),
            "MalformedKeyValue を期待したが {err:?} になった"
        );
    }

    /// 最小値ちょうど (65535) とそれ以上の値はエンコードできる
    #[test]
    fn encode_accepts_minimum_and_larger_values() {
        for value in [65_535u64, 65_536, 1_000_000, u64::MAX] {
            let mut buf = Vec::new();
            options_with(value)
                .encode(&mut buf)
                .unwrap_or_else(|e| panic!("値 {value} の encode は成功すること: {e}"));
            let (decoded, _) = SetupOptions::decode(&buf)
                .unwrap_or_else(|e| panic!("値 {value} の decode は成功すること: {e}"));
            assert_eq!(
                decoded.max_control_message_size(),
                Some(value),
                "値 {value} が往復すること"
            );
        }
    }

    /// 最小値未満 (65534) はエンコードで拒否される
    #[test]
    fn encode_rejects_value_below_minimum() {
        let mut buf = Vec::new();
        let err = options_with(65_534)
            .encode(&mut buf)
            .expect_err("65535 未満の設定値は encode で拒否されること");
        assert!(
            matches!(err, MessageError::InvalidField(_)),
            "InvalidField を期待したが {err:?} になった"
        );
        assert!(
            buf.is_empty(),
            "拒否した option のバイトを書かないこと: {buf:?}"
        );
    }

    /// 最小値未満の値を含むワイヤはデコードで拒否される
    ///
    /// draft-ietf-moq-transport-23 §12.2 (Session Termination Codes) の INVALID_FIELD (0x301)。
    #[test]
    fn decode_rejects_value_below_minimum() {
        // delta type 0x09 + Length 3 + 3 バイト vi64 (65534 = 0xC0 0xFF 0xFE)
        let wire = [0x09u8, 0x03, 0xC0, 0xFF, 0xFE];
        let err =
            SetupOptions::decode(&wire).expect_err("65535 未満の設定値は decode で拒否される");
        assert!(
            matches!(err, MessageError::InvalidField(_)),
            "InvalidField を期待したが {err:?} になった"
        );
    }

    /// 型番号 0x09 に Bytes 値を与えると InvalidParameter になる
    ///
    /// 0x09 の値は varint として解釈するため、生のバイト列を保持する Bytes variant は
    /// エンコードできない。
    #[test]
    fn encode_rejects_bytes_value() {
        let mut options = SetupOptions::new();
        options.push(SetupOption {
            option_type: SETUP_OPTION_MAX_CONTROL_MESSAGE_SIZE,
            value: SetupOptionValue::Bytes(vec![0x00, 0x01]),
        });
        let mut buf = Vec::new();
        let err = options
            .encode(&mut buf)
            .expect_err("0x09 の Bytes 値は encode で拒否されること");
        assert!(
            matches!(err, MessageError::InvalidParameter),
            "InvalidParameter を期待したが {err:?} になった"
        );
    }

    /// 非最小符号化された値もデコードできる
    ///
    /// draft-ietf-moq-transport-23 §8.1 (Variable-Length Integers): decode 側は非最小
    /// エンコーディングをエラーにしない。Length は実際に書かれたバイト数と一致していればよい。
    #[test]
    fn decode_accepts_non_minimal_value() {
        // delta type 0x09 + Length 4 + 65535 を 4 バイト vi64 で表す (0xE0 0x00 0xFF 0xFF)
        let wire = [0x09u8, 0x04, 0xE0, 0x00, 0xFF, 0xFF];
        let (decoded, consumed) =
            SetupOptions::decode(&wire).expect("非最小符号化の値は受理されること");
        assert_eq!(consumed, wire.len());
        assert_eq!(decoded.max_control_message_size(), Some(65_535));
    }

    /// 0x09 の重複は encode / decode の双方で拒否される
    ///
    /// draft-ietf-moq-transport-23 §9.1 (SETUP): "Senders MUST NOT repeat the same Option Type
    /// in a message unless the option definition explicitly allows multiple instances."
    #[test]
    fn duplicate_option_is_rejected() {
        // encode: 同じ型を 2 つ積む
        let mut options = SetupOptions::new();
        options.push(SetupOption {
            option_type: SETUP_OPTION_MAX_CONTROL_MESSAGE_SIZE,
            value: SetupOptionValue::VarInt(65_535),
        });
        options.push(SetupOption {
            option_type: SETUP_OPTION_MAX_CONTROL_MESSAGE_SIZE,
            value: SetupOptionValue::VarInt(65_536),
        });
        let mut buf = Vec::new();
        let err = options
            .encode(&mut buf)
            .expect_err("0x09 の重複は encode で拒否されること");
        assert!(
            matches!(err, MessageError::InvalidParameter),
            "InvalidParameter を期待したが {err:?} になった"
        );

        // decode: 同じ型を delta 0 で 2 つ並べる
        let mut wire = Vec::new();
        wire.push(0x09); // delta type = 0x09
        wire.extend_from_slice(&[0x03, 0xC0, 0xFF, 0xFF]); // Length 3 + 65535
        wire.push(0x00); // delta = 0 (同じ型)
        wire.extend_from_slice(&[0x03, 0xC0, 0xFF, 0xFF]); // Length 3 + 65535
        let err = SetupOptions::decode(&wire).expect_err("0x09 の重複は decode で拒否される");
        assert!(
            matches!(err, MessageError::InvalidField(_)),
            "既知型の重複は ProtocolViolation であること: {err:?}"
        );
    }

    /// 未知の奇数型は従来どおり長さ付きバイト列として扱われる
    ///
    /// 0x09 も Length を持つため、他の奇数型と KV の構造は共通である。0x09 だけ値の中身を
    /// varint として解釈する。この区別を一般化しすぎていないことを固定する。
    #[test]
    fn other_odd_type_keeps_length_prefixed_form() {
        let mut options = SetupOptions::new();
        options.push(SetupOption {
            option_type: 0x0B,
            value: SetupOptionValue::Bytes(vec![0xAA, 0xBB]),
        });
        let mut buf = Vec::new();
        options
            .encode(&mut buf)
            .expect("未知の奇数型の encode は成功する");
        // delta type 0x0B + 長さ 2 + 値 2 バイト
        assert_eq!(buf, vec![0x0B, 0x02, 0xAA, 0xBB]);
        let (decoded, _) = SetupOptions::decode(&buf).expect("decode できること");
        // 未知型は保持され、`max_control_message_size` には影響しない
        assert_eq!(decoded.max_control_message_size(), None);
    }

    /// PATH と 0x09 を併用しても互いの解釈に影響しない
    ///
    /// 0x09 も Length 付き varint であり、既存の奇数型 (PATH) の長さ付きバイト列の解釈は
    /// 変わらない。
    #[test]
    fn mixed_with_path_option() {
        let mut options = SetupOptions::new();
        options.push(SetupOption {
            option_type: SETUP_OPTION_MAX_CONTROL_MESSAGE_SIZE,
            value: SetupOptionValue::VarInt(65_535),
        });
        options.push(SetupOption {
            option_type: SETUP_OPTION_PATH,
            value: SetupOptionValue::Bytes(b"/live".to_vec()),
        });
        let mut buf = Vec::new();
        options
            .encode(&mut buf)
            .expect("PATH と 0x09 の併用は encode できること");
        let (decoded, _) = SetupOptions::decode(&buf).expect("decode できること");
        assert_eq!(decoded.path(), Some(b"/live".as_slice()));
        assert_eq!(decoded.max_control_message_size(), Some(65_535));
    }
}

mod delta_encoding {
    use super::*;

    #[test]
    fn delta_type_correctness() {
        // type=2 (delta=2), type=5 (delta=3) の順でエンコードされるはず
        // 値として 1 バイト varint に収まる 50 を使う
        let mut options = SetupOptions::new();
        options.push(SetupOption {
            option_type: 0x02,
            value: SetupOptionValue::VarInt(50),
        });
        options.push(SetupOption {
            option_type: shiguredo_moqt::parameter::SETUP_OPTION_AUTHORITY,
            value: SetupOptionValue::Bytes(b"hello".to_vec()),
        });

        let mut buf = Vec::new();
        options
            .encode(&mut buf)
            .expect("正当なテスト入力の encode は成功する");

        // カウントプレフィックスなし
        // delta = 2 (type=2 - prev=0)
        assert_eq!(buf[0], 0x02);
        // varint 50 は 1 バイト (50 < 64)
        assert_eq!(buf[1], 50);
        // delta = 3 (type=5 - prev=2)
        assert_eq!(buf[2], 0x03);
        // length = 5
        assert_eq!(buf[3], 0x05);

        let (decoded, consumed) =
            SetupOptions::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, buf.len());
        assert_eq!(decoded.len(), 2);
        // type 0x05 は AUTHORITY オプション
        assert_eq!(decoded.authority(), Some(b"hello".as_ref()));
    }

    #[test]
    fn encode_sorts_by_type() {
        // 逆順に push しても delta が常に正になるようにソートしてエンコードする
        let mut options = SetupOptions::new();
        options.push(SetupOption {
            option_type: shiguredo_moqt::parameter::SETUP_OPTION_AUTHORITY,
            value: SetupOptionValue::Bytes(b"example.com".to_vec()),
        });
        options.push(SetupOption {
            option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_AUTH_TOKEN_CACHE_SIZE,
            value: SetupOptionValue::VarInt(42),
        });

        let mut buf = Vec::new();
        options
            .encode(&mut buf)
            .expect("正当なテスト入力の encode は成功する");

        let (decoded, _) =
            SetupOptions::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        // ソートされて type=4 が先に来る。両方のオプションが復元される
        assert_eq!(decoded.max_auth_token_cache_size(), Some(42));
        assert_eq!(decoded.authority(), Some(b"example.com".as_ref()));
    }
}

mod accessors {
    use super::*;

    #[test]
    fn path() {
        let mut options = SetupOptions::new();
        options.push(SetupOption {
            option_type: shiguredo_moqt::parameter::SETUP_OPTION_PATH,
            value: SetupOptionValue::Bytes(b"/moqt/session".to_vec()),
        });
        assert_eq!(options.path(), Some(b"/moqt/session".as_ref()));
    }

    #[test]
    fn authorization_token() {
        // USE_VALUE (Alias Type 0x03): Token Type + Token Value
        let token = AuthorizationToken::UseValue {
            token_type: 0,
            token_value: b"token123".to_vec(),
        };
        let mut options = SetupOptions::new();
        options.push(SetupOption {
            option_type: shiguredo_moqt::parameter::SETUP_OPTION_AUTHORIZATION_TOKEN,
            value: SetupOptionValue::AuthorizationToken(token.clone()),
        });
        assert_eq!(options.authorization_tokens(), vec![&token]);
    }

    #[test]
    fn authority() {
        let mut options = SetupOptions::new();
        options.push(SetupOption {
            option_type: shiguredo_moqt::parameter::SETUP_OPTION_AUTHORITY,
            value: SetupOptionValue::Bytes(b"example.com:4433".to_vec()),
        });
        assert_eq!(options.authority(), Some(b"example.com:4433".as_ref()));
    }

    #[test]
    fn max_auth_token_cache_size() {
        let mut options = SetupOptions::new();
        options.push(SetupOption {
            option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_AUTH_TOKEN_CACHE_SIZE,
            value: SetupOptionValue::VarInt(65536),
        });
        assert_eq!(options.max_auth_token_cache_size(), Some(65536));
    }

    #[test]
    fn missing_returns_none() {
        let options = SetupOptions::new();
        assert_eq!(options.path(), None);
        assert_eq!(options.authority(), None);
        assert_eq!(options.max_auth_token_cache_size(), None);
        assert_eq!(options.max_request_updates(), None);
        assert_eq!(options.max_filter_ranges(), None);
    }
}

/// MAX_REQUEST_UPDATES (type 0x08) のアクセサ
///
/// draft-ietf-moq-transport-23 §9.1.7 (MAX_REQUEST_UPDATES)
#[test]
fn max_request_updates() {
    let mut options = SetupOptions::new();
    options.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_REQUEST_UPDATES,
        value: SetupOptionValue::VarInt(3),
    });
    assert_eq!(options.max_request_updates(), Some(3));
}

/// MAX_FILTER_RANGES (type 0x06) のアクセサ
///
/// draft-ietf-moq-transport-23 §9.1.6 / §3.3.2 (Range Filters)
#[test]
fn max_filter_ranges() {
    let mut options = SetupOptions::new();
    options.push(SetupOption {
        option_type: shiguredo_moqt::parameter::SETUP_OPTION_MAX_FILTER_RANGES,
        value: SetupOptionValue::VarInt(5),
    });
    assert_eq!(options.max_filter_ranges(), Some(5));
}

#[test]
fn missing_returns_none() {
    let options = SetupOptions::new();
    assert_eq!(options.path(), None);
    assert_eq!(options.authority(), None);
    assert_eq!(options.max_auth_token_cache_size(), None);
    assert_eq!(options.max_request_updates(), None);
    assert_eq!(options.max_filter_ranges(), None);
}

mod error_cases {
    use super::*;

    #[test]
    fn decode_empty_is_ok() {
        // 空バッファはカウントプレフィックスなしなので正常に 0 個の SetupOptions を返す
        let (decoded, consumed) =
            SetupOptions::decode(&[]).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, 0);
        assert!(decoded.is_empty());
    }

    #[test]
    fn length_prefix_exceeding_remaining_is_key_value_formatting_error() {
        // delta=1 (奇数型)、宣言長 10、データは 3 バイトのみ。
        // Type を理解したうえで Length/Value が直列化と一致しないため MALFORMED_KEY_VALUE。
        let buf = vec![0x01, 0x0A, 0x01, 0x02, 0x03];
        assert_eq!(
            SetupOptions::decode(&buf),
            Err(MessageError::MalformedKeyValue(
                "setup option value length exceeds remaining buffer"
            ))
        );
    }

    #[test]
    fn truncated_length_varint_is_unexpected_eof() {
        // delta=1 (odd type) の直後で Length varint が途中で切れている。
        // Length を読めない段階の終端は UnexpectedEof のままである。
        let buf = vec![0x01, 0x80];
        assert_eq!(SetupOptions::decode(&buf), Err(MessageError::UnexpectedEof));
    }

    #[test]
    fn truncated_delta_key_varint_is_unexpected_eof() {
        // 先頭の delta key (Type) の varint が途中で切れている。
        let buf = vec![0x80];
        assert_eq!(SetupOptions::decode(&buf), Err(MessageError::UnexpectedEof));
    }

    #[test]
    fn truncated_even_type_value_varint_is_unexpected_eof() {
        // delta=2 (even type) の値 varint が途中で切れている。
        let buf = vec![0x02, 0x80];
        assert_eq!(SetupOptions::decode(&buf), Err(MessageError::UnexpectedEof));
    }

    /// malformed PATH の encode はセッション層で検証するため parameter 層では成功する
    #[test]
    fn path_without_leading_slash_encodes_successfully() {
        let mut options = SetupOptions::new();
        options.push(SetupOption {
            option_type: shiguredo_moqt::parameter::SETUP_OPTION_PATH,
            value: SetupOptionValue::Bytes(b"relative/path".to_vec()),
        });

        let mut buf = Vec::new();
        options
            .encode(&mut buf)
            .expect("正当なテスト入力の encode は成功する");
        // 出力も検証する (値が壊れず往復すること)
        let (decoded, consumed) =
            SetupOptions::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, buf.len());
        assert_eq!(decoded, options);
    }

    /// malformed PATH の decode はセッション層で検証するため parameter 層では成功する
    #[test]
    fn path_with_invalid_percent_encoding_decodes_successfully() {
        let buf = vec![0x01, 0x06, b'/', b'b', b'a', b'd', b'%', b'G'];
        let (decoded, consumed) =
            SetupOptions::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, buf.len());
        assert_eq!(
            decoded.len(),
            1,
            "不正な percent 符号でも値は保持されること"
        );
    }

    /// malformed AUTHORITY の encode はセッション層で検証するため parameter 層では成功する
    #[test]
    fn authority_with_space_encodes_successfully() {
        let mut options = SetupOptions::new();
        options.push(SetupOption {
            option_type: shiguredo_moqt::parameter::SETUP_OPTION_AUTHORITY,
            value: SetupOptionValue::Bytes(b"bad host".to_vec()),
        });

        let mut buf = Vec::new();
        options
            .encode(&mut buf)
            .expect("正当なテスト入力の encode は成功する");
        // 出力も検証する (値が壊れず往復すること)
        let (decoded, consumed) =
            SetupOptions::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, buf.len());
        assert_eq!(decoded, options);
    }
}

mod uri_validation_success_cases {
    use super::*;

    #[test]
    fn path_with_query_roundtrip() {
        let mut options = SetupOptions::new();
        options.push(SetupOption {
            option_type: shiguredo_moqt::parameter::SETUP_OPTION_PATH,
            value: SetupOptionValue::Bytes(b"/moqt/catalog?track=video/1".to_vec()),
        });

        let mut buf = Vec::new();
        options
            .encode(&mut buf)
            .expect("正当なテスト入力の encode は成功する");
        let (decoded, consumed) =
            SetupOptions::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, buf.len());
        assert_eq!(
            decoded.path(),
            Some(b"/moqt/catalog?track=video/1".as_ref())
        );
    }

    #[test]
    fn authority_with_ipv6_port_roundtrip() {
        let mut options = SetupOptions::new();
        options.push(SetupOption {
            option_type: shiguredo_moqt::parameter::SETUP_OPTION_AUTHORITY,
            value: SetupOptionValue::Bytes(b"[2001:db8::1]:4433".to_vec()),
        });

        let mut buf = Vec::new();
        options
            .encode(&mut buf)
            .expect("正当なテスト入力の encode は成功する");
        let (decoded, consumed) =
            SetupOptions::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, buf.len());
        assert_eq!(decoded.authority(), Some(b"[2001:db8::1]:4433".as_ref()));
    }
}

/// AUTHORIZATION_TOKEN (0x03) の値 variant の厳密性を検証する。
///
/// 0x03 は Token 構造専用であり、生の Bytes variant を encode すると decode 側が
/// Token 構造として解釈しようとして失敗する。encode 側で Bytes を拒否し、
/// AuthorizationToken variant のみを受け付けることを固定する。
mod authorization_token_encoding_strictness {
    use super::*;
    use shiguredo_moqt::parameter::SETUP_OPTION_AUTHORIZATION_TOKEN;

    #[test]
    fn auth_token_option_rejects_bytes_variant() {
        let mut options = SetupOptions::new();
        options.push(SetupOption {
            option_type: SETUP_OPTION_AUTHORIZATION_TOKEN,
            value: SetupOptionValue::Bytes(vec![0x03, 0x00]),
        });
        let mut buf = Vec::new();

        assert_eq!(
            options.encode(&mut buf),
            Err(MessageError::InvalidParameter),
            "AUTHORIZATION_TOKEN に Bytes は許可されないこと"
        );
    }

    #[test]
    fn auth_token_option_accepts_authorization_token() {
        let token = AuthorizationToken::UseValue {
            token_type: 0,
            token_value: b"token".to_vec(),
        };
        let mut options = SetupOptions::new();
        options.push(SetupOption {
            option_type: SETUP_OPTION_AUTHORIZATION_TOKEN,
            value: SetupOptionValue::AuthorizationToken(token.clone()),
        });
        let mut buf = Vec::new();
        options
            .encode(&mut buf)
            .expect("AuthorizationToken variant の encode は成功する");

        let (decoded, consumed) =
            SetupOptions::decode(&buf).expect("encode した AUTHORIZATION_TOKEN は decode できる");
        assert_eq!(consumed, buf.len());
        assert_eq!(decoded.authorization_tokens(), vec![&token]);
    }
}

/// draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure): Setup Option 値の 2^16-1 バイト上限
mod setup_option_value_length_limit {
    use super::*;
    use shiguredo_moqt::parameter::{SETUP_OPTION_AUTHORIZATION_TOKEN, SETUP_OPTION_PATH};

    /// 65535 バイトの Bytes 値はエンコードでき、65536 バイトは拒否される
    #[test]
    fn bytes_length_boundary() {
        for (len, expected_ok) in [(65535usize, true), (65536usize, false)] {
            let mut options = SetupOptions::new();
            options.push(SetupOption {
                option_type: SETUP_OPTION_PATH,
                value: SetupOptionValue::Bytes(vec![b'/'; len]),
            });
            let mut buf = Vec::new();
            let result = options.encode(&mut buf);
            if expected_ok {
                result.expect("65535 バイトの Setup Option 値はエンコードできる");
                let (decoded, consumed) =
                    SetupOptions::decode(&buf).expect("エンコード結果を decode できる");
                assert_eq!(consumed, buf.len());
                let path = decoded.path().expect("PATH がデコードできる");
                assert_eq!(path.len(), len);
                assert!(path.iter().all(|&b| b == b'/'));
            } else {
                assert!(
                    matches!(result, Err(MessageError::FieldLengthExceeded(_))),
                    "65536 バイトの Setup Option 値は ProtocolViolation で拒否される"
                );
            }
        }
    }

    /// 65536 バイト長の Setup Option 値は decode でも ProtocolViolation になる
    #[test]
    fn decode_65536_bytes_error() {
        use shiguredo_moqt::varint;
        // delta_key=SETUP_OPTION_PATH (prev=0), length=65536 (上限超過)
        let mut buf = Vec::new();
        varint::encode(SETUP_OPTION_PATH, &mut buf);
        varint::encode(65536, &mut buf);
        let err = SetupOptions::decode(&buf).unwrap_err();
        assert!(
            matches!(err, MessageError::FieldLengthExceeded(_)),
            "ProtocolViolation を期待したが {err:?} になった"
        );
    }

    /// 65535 バイトの AuthorizationToken 値はエンコードでき、65536 バイトは拒否される
    #[test]
    fn authorization_token_length_boundary() {
        // UseValue の値は alias type (1 バイト) + token_type (値 0 の 1 バイト varint)
        // + Token Value。KVP 値長 = 2 + token_value_len となり、65533 + 2 = 65535 (上限) /
        // 65534 + 2 = 65536 (超過) が境界になる。
        for (token_value_len, expected_ok) in [(65533usize, true), (65534usize, false)] {
            let token = AuthorizationToken::UseValue {
                token_type: 0,
                token_value: vec![0x01; token_value_len],
            };
            let mut options = SetupOptions::new();
            options.push(SetupOption {
                option_type: SETUP_OPTION_AUTHORIZATION_TOKEN,
                value: SetupOptionValue::AuthorizationToken(token.clone()),
            });
            let mut buf = Vec::new();
            let result = options.encode(&mut buf);
            if expected_ok {
                result.expect("65535 バイトの Setup AuthorizationToken 値はエンコードできる");
                let (decoded, consumed) =
                    SetupOptions::decode(&buf).expect("エンコード結果を decode できる");
                assert_eq!(consumed, buf.len());
                assert_eq!(decoded.authorization_tokens(), vec![&token]);
            } else {
                assert!(
                    matches!(result, Err(MessageError::FieldLengthExceeded(_))),
                    "65536 バイトの Setup AuthorizationToken 値は ProtocolViolation で拒否される"
                );
            }
        }
    }
}

/// draft-ietf-moq-transport-23 §9.1.4 (AUTHORIZATION TOKEN): SETUP で DELETE /
/// USE_ALIAS を受信した場合は PROTOCOL_VIOLATION で閉じる (MUST)。
mod setup_authorization_token_scope {
    use super::*;
    use shiguredo_moqt::parameter::SETUP_OPTION_AUTHORIZATION_TOKEN;

    /// SETUP の AUTHORIZATION_TOKEN を手組みのワイヤで作る
    ///
    /// ワイヤは delta key (0x03) + 値長 + Token 構造である。
    fn setup_option_wire(token_bytes: &[u8]) -> Vec<u8> {
        let mut buf = vec![
            SETUP_OPTION_AUTHORIZATION_TOKEN as u8,
            token_bytes.len() as u8,
        ];
        buf.extend_from_slice(token_bytes);
        buf
    }

    #[test]
    fn delete_is_protocol_violation() {
        // DELETE (0x0) + Token Alias
        let wire = setup_option_wire(&[0x00, 0x01]);
        assert_eq!(
            SetupOptions::decode(&wire),
            Err(MessageError::RoleViolation(
                "DELETE alias type is not allowed in SETUP"
            )),
            "SETUP の DELETE は PROTOCOL_VIOLATION であること"
        );
    }

    #[test]
    fn use_alias_is_protocol_violation() {
        // USE_ALIAS (0x2) + Token Alias
        let wire = setup_option_wire(&[0x02, 0x01]);
        assert_eq!(
            SetupOptions::decode(&wire),
            Err(MessageError::RoleViolation(
                "USE_ALIAS alias type is not allowed in SETUP"
            )),
            "SETUP の USE_ALIAS は PROTOCOL_VIOLATION であること"
        );
    }

    #[test]
    fn delete_encode_is_protocol_violation() {
        // encode 側でも同じ検証を行う (SETUP へ DELETE を書けない)
        let mut options = SetupOptions::new();
        options.push(SetupOption {
            option_type: SETUP_OPTION_AUTHORIZATION_TOKEN,
            value: SetupOptionValue::AuthorizationToken(AuthorizationToken::Delete { alias: 1 }),
        });
        let mut buf = Vec::new();
        assert_eq!(
            options.encode(&mut buf),
            Err(MessageError::RoleViolation(
                "DELETE alias type is not allowed in SETUP"
            )),
            "SETUP への DELETE の encode は拒否されること"
        );
    }

    #[test]
    fn use_alias_encode_is_protocol_violation() {
        let mut options = SetupOptions::new();
        options.push(SetupOption {
            option_type: SETUP_OPTION_AUTHORIZATION_TOKEN,
            value: SetupOptionValue::AuthorizationToken(AuthorizationToken::UseAlias { alias: 1 }),
        });
        let mut buf = Vec::new();
        assert_eq!(
            options.encode(&mut buf),
            Err(MessageError::RoleViolation(
                "USE_ALIAS alias type is not allowed in SETUP"
            )),
            "SETUP への USE_ALIAS の encode は拒否されること"
        );
    }

    #[test]
    fn register_and_use_value_are_accepted() {
        // REGISTER (0x1) と USE_VALUE (0x3) は SETUP で許可される
        for token in [
            AuthorizationToken::Register {
                alias: 1,
                token_type: 0,
                token_value: b"secret".to_vec(),
            },
            AuthorizationToken::UseValue {
                token_type: 0,
                token_value: b"secret".to_vec(),
            },
        ] {
            let mut options = SetupOptions::new();
            options.push(SetupOption {
                option_type: SETUP_OPTION_AUTHORIZATION_TOKEN,
                value: SetupOptionValue::AuthorizationToken(token.clone()),
            });
            let mut buf = Vec::new();
            options
                .encode(&mut buf)
                .expect("SETUP で許可される Token である");
            let (decoded, _consumed) =
                SetupOptions::decode(&buf).expect("encode した SETUP は decode できる");
            assert_eq!(decoded.authorization_tokens(), vec![&token]);
        }
    }

    #[test]
    fn delete_with_trailing_bytes_is_malformed_auth_token() {
        // Token 構造としてデコードできない場合は draft-ietf-moq-transport-23 §8.9 により
        // MALFORMED_AUTH_TOKEN (SETUP スコープ検証より前)
        let wire = setup_option_wire(&[0x00, 0x01, 0xFF]);
        assert!(
            matches!(
                SetupOptions::decode(&wire),
                Err(MessageError::MalformedAuthToken(_))
            ),
            "Token 構造のデコード失敗は MALFORMED_AUTH_TOKEN であること"
        );
    }
}
