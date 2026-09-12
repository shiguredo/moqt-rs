use shiguredo_moqt::{
    error::MessageError,
    message_parameter::AuthorizationToken,
    parameter::{SetupOption, SetupOptionValue, SetupOptions},
};

/// Setup Option Type 定数が §15.4 レジストリ表と一致する
#[test]
fn setup_option_type_constants_match_registry() {
    use shiguredo_moqt::parameter::{
        SETUP_OPTION_AUTHORITY, SETUP_OPTION_AUTHORIZATION_TOKEN,
        SETUP_OPTION_MAX_AUTH_TOKEN_CACHE_SIZE, SETUP_OPTION_MAX_FILTER_RANGES,
        SETUP_OPTION_MAX_REQUEST_UPDATES, SETUP_OPTION_MOQT_IMPLEMENTATION, SETUP_OPTION_PATH,
    };
    assert_eq!(SETUP_OPTION_PATH, 0x01);
    assert_eq!(SETUP_OPTION_AUTHORIZATION_TOKEN, 0x03);
    assert_eq!(SETUP_OPTION_MAX_AUTH_TOKEN_CACHE_SIZE, 0x04);
    assert_eq!(SETUP_OPTION_AUTHORITY, 0x05);
    assert_eq!(SETUP_OPTION_MAX_FILTER_RANGES, 0x06);
    assert_eq!(SETUP_OPTION_MOQT_IMPLEMENTATION, 0x07);
    assert_eq!(SETUP_OPTION_MAX_REQUEST_UPDATES, 0x08);
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
/// draft-ietf-moq-transport-21 §9.1.7 (MAX_REQUEST_UPDATES)
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
/// draft-ietf-moq-transport-21 §9.1.6 / §3.3.2 (Range Filters)
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
    fn unexpected_eof_in_value() {
        // delta=1 (odd type), length=10, but only 3 bytes of data
        let buf = vec![0x01, 0x0A, 0x01, 0x02, 0x03];
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

/// draft-ietf-moq-transport-21 §8.3 (Key-Value-Pair Structure): Setup Option 値の 2^16-1 バイト上限
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
                    matches!(result, Err(MessageError::ProtocolViolation(_))),
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
            matches!(err, MessageError::ProtocolViolation(_)),
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
                    matches!(result, Err(MessageError::ProtocolViolation(_))),
                    "65536 バイトの Setup AuthorizationToken 値は ProtocolViolation で拒否される"
                );
            }
        }
    }
}
