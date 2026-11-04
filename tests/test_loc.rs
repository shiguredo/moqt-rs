use shiguredo_moqt::{
    error::MessageError,
    loc::{
        LocProperties, LocProperty, LocPropertyValue, PROP_AUDIO_CONFIG, PROP_AUDIO_LEVEL,
        PROP_CODEC_STRING, PROP_TIMESCALE, PROP_TIMESTAMP, PROP_VIDEO_CONFIG,
        PROP_VIDEO_FRAME_MARKING,
    },
    track_properties::PROP_IMMUTABLE_PROPERTIES,
};

/// `LocProperties::encode` が付ける Properties Length を除いた KVP 列を返す
///
/// IMMUTABLE_PROPERTIES (0x0B) の値は外側の Properties Length を伴わない delta-key
/// KVP 列である (draft-ietf-moq-transport-23 §10.7 (Immutable Properties) / §8.3
/// (Key-Value-Pair Structure))。内側のプロパティ列は `LocProperties` の encode で
/// 正規の delta-key 表現を作り、先頭の Properties Length だけを読み飛ばして取り出す。
fn kvp_sequence(props: &LocProperties) -> Vec<u8> {
    let encoded = props
        .encode()
        .expect("テストフィクスチャの内側 KVP の encode は成功する");
    let (_, length_field_len) =
        shiguredo_moqt::varint::decode(&encoded).expect("Properties Length の varint を読めること");
    encoded[length_field_len..].to_vec()
}

/// 外側に IMMUTABLE_PROPERTIES (0x0B) だけを持つ wire 形式を組み立てる
///
/// `inner` は 0x0B の値としてそのまま格納する delta-key KVP 列である。encode は 0x0B の
/// 内側の KVP を検証するため、破損した内側を decode に通すフィクスチャはこの helper で
/// 直接組み立てる。
fn wire_with_inner_bytes(inner: &[u8]) -> Vec<u8> {
    let mut kvp = Vec::new();
    shiguredo_moqt::varint::encode(PROP_IMMUTABLE_PROPERTIES, &mut kvp);
    shiguredo_moqt::varint::encode(inner.len() as u64, &mut kvp);
    kvp.extend_from_slice(inner);

    let mut wire = Vec::new();
    shiguredo_moqt::varint::encode(kvp.len() as u64, &mut wire);
    wire.extend_from_slice(&kvp);
    wire
}

mod accessors {
    use super::*;

    #[test]
    fn timestamp() {
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: LocPropertyValue::VarInt(123_456_789),
        });
        assert_eq!(props.timestamp(), Some(123_456_789));
        assert_eq!(props.timescale(), None);
        assert_eq!(props.video_frame_marking(), None);
        assert_eq!(props.audio_level(), None);
        assert_eq!(props.video_config(), None);
    }

    #[test]
    fn timescale() {
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_TIMESCALE,
            value: LocPropertyValue::VarInt(90000),
        });
        assert_eq!(props.timescale(), Some(90000));
        assert_eq!(props.timestamp(), None);
    }

    #[test]
    fn video_frame_marking() {
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_VIDEO_FRAME_MARKING,
            value: LocPropertyValue::Bytes(vec![0b0001_0001]),
        });
        assert_eq!(props.video_frame_marking(), Some(&[0b0001_0001u8][..]));
    }

    #[test]
    fn audio_level() {
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_AUDIO_LEVEL,
            value: LocPropertyValue::VarInt(0x50),
        });
        assert_eq!(props.audio_level(), Some(0x50));
    }

    #[test]
    fn video_config() {
        let cfg = vec![0x01u8, 0x64, 0x00, 0x28, 0xFF];
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_VIDEO_CONFIG,
            value: LocPropertyValue::Bytes(cfg.clone()),
        });
        assert_eq!(props.video_config(), Some(cfg.as_slice()));
    }

    #[test]
    fn missing_returns_none() {
        let props = LocProperties::new();
        assert_eq!(props.timestamp(), None);
        assert_eq!(props.timescale(), None);
        assert_eq!(props.video_frame_marking(), None);
        assert_eq!(props.audio_level(), None);
        assert_eq!(props.video_config(), None);
        assert_eq!(props.codec_string(), Ok(None));
    }
}

mod codec_string {
    use super::*;

    /// 最上位の CODEC_STRING (0x11) を任意のバイト列で持つ wire 形式を組み立てる
    ///
    /// encode は不正な UTF-8 を拒否するため、decode の検証用に入力を直接組み立てる。
    fn wire_with_top_level_codec_string(value: &[u8]) -> Vec<u8> {
        let mut inner = Vec::new();
        shiguredo_moqt::varint::encode(PROP_CODEC_STRING, &mut inner);
        shiguredo_moqt::varint::encode(value.len() as u64, &mut inner);
        inner.extend_from_slice(value);
        let mut wire = Vec::new();
        shiguredo_moqt::varint::encode(inner.len() as u64, &mut wire);
        wire.extend_from_slice(&inner);
        wire
    }

    /// IMMUTABLE_PROPERTIES (0x0B) の内側に CODEC_STRING を任意のバイト列で持つ wire 形式
    ///
    /// 外側の encode は内側の CODEC_STRING の UTF-8 も検証するため、decode の検証用入力を
    /// 直接組み立てる。
    fn wire_with_inner_codec_string(value: &[u8]) -> Vec<u8> {
        let mut inner = Vec::new();
        shiguredo_moqt::varint::encode(PROP_CODEC_STRING, &mut inner);
        shiguredo_moqt::varint::encode(value.len() as u64, &mut inner);
        inner.extend_from_slice(value);
        wire_with_inner_bytes(&inner)
    }

    #[test]
    fn codec_string_top_level() {
        // 最上位の CODEC_STRING (0x11) を構築・取得できること。UTF-8 のバイト長が共通 KVP の
        // Length フィールドに入り、独自の内側 length が追加されないことを wire で固定する。
        let codec = "av01.0.04M.08";
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_CODEC_STRING,
            value: LocPropertyValue::Bytes(codec.as_bytes().to_vec()),
        });
        assert_eq!(props.codec_string(), Ok(Some(codec)));

        let encoded = props
            .encode()
            .expect("正当なテスト入力の encode は成功する");
        // [Properties Length][delta=0x11][Length=13][UTF-8 の値]
        assert_eq!(encoded[0], 2 + codec.len() as u8);
        assert_eq!(encoded[1], PROP_CODEC_STRING as u8);
        assert_eq!(encoded[2], codec.len() as u8);
        assert_eq!(&encoded[3..], codec.as_bytes());

        let (decoded, consumed) =
            LocProperties::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded.codec_string(), Ok(Some(codec)));
        assert_eq!(
            decoded.find_bytes(PROP_CODEC_STRING),
            Some(codec.as_bytes()),
            "生バイト列の accessor でも同じ値が取得できること"
        );
    }

    #[test]
    fn codec_string_inside_immutable_properties() {
        // draft-ietf-moq-transport-23 §10.7 (Immutable Properties) は Property を mutable
        // リストと IMMUTABLE_PROPERTIES の内側のどちらに置いてもよいとし、LOC 側の
        // draft-ietf-moq-loc-05 §3.1.2 (Immutable Properties) は relay から見える不変の
        // LOC プロパティを内側に置くことを SHOULD とする。最上位には 0x0B だけを置き、
        // CODEC_STRING を内側にだけ持つ Object でも取得できることを固定する。
        let codec = "vp09.00.10.08";
        let mut inner = LocProperties::new();
        inner.push(LocProperty {
            prop_id: PROP_CODEC_STRING,
            value: LocPropertyValue::Bytes(codec.as_bytes().to_vec()),
        });
        let wire = wire_with_inner_bytes(&kvp_sequence(&inner));
        let (decoded, consumed) =
            LocProperties::decode(&wire).expect("IMMUTABLE_PROPERTIES の decode は成功すること");
        assert_eq!(consumed, wire.len());
        assert_eq!(decoded.len(), 1, "最上位は 0x0B だけであること");
        assert_eq!(decoded.codec_string(), Ok(Some(codec)));
        assert_eq!(
            decoded.find_bytes(PROP_CODEC_STRING),
            Some(codec.as_bytes())
        );
    }

    #[test]
    fn codec_string_missing_is_ok_none() {
        // CODEC_STRING が無い場合はエラーではなく Ok(None) であり、他のプロパティが
        // あっても欠落として扱うこと。
        let empty = LocProperties::new();
        assert_eq!(empty.codec_string(), Ok(None));

        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: LocPropertyValue::VarInt(1),
        });
        assert_eq!(props.codec_string(), Ok(None));
    }

    #[test]
    fn codec_string_invalid_utf8_is_error() {
        // 欠落 (Ok(None)) と不正 UTF-8 (Err) を区別すること。最上位と、push で組み立てた
        // IMMUTABLE_PROPERTIES の内側の両方で Err になることを固定する。
        let mut top_level = LocProperties::new();
        top_level.push(LocProperty {
            prop_id: PROP_CODEC_STRING,
            value: LocPropertyValue::Bytes(vec![0xFF, 0xFE]),
        });
        assert!(matches!(
            top_level.codec_string(),
            Err(MessageError::MalformedKeyValue(_))
        ));

        // 内側は encode を通すと検証で拒否されるため、KVP 列を直接組み立てる。push だけで
        // 組み立てた outer は encode を経由しないので、accessor の検証だけを観測できる。
        let mut inner_bytes = Vec::new();
        shiguredo_moqt::varint::encode(PROP_CODEC_STRING, &mut inner_bytes);
        shiguredo_moqt::varint::encode(1, &mut inner_bytes);
        inner_bytes.push(0x80);
        let mut outer = LocProperties::new();
        outer.push(LocProperty {
            prop_id: PROP_IMMUTABLE_PROPERTIES,
            value: LocPropertyValue::Bytes(inner_bytes),
        });
        assert!(matches!(
            outer.codec_string(),
            Err(MessageError::MalformedKeyValue(_))
        ));
    }

    #[test]
    fn codec_string_multibyte_roundtrip() {
        // UTF-8 のマルチバイト文字も、Length にバイト数が入って往復すること。WebCodecs の
        // codec 値は ASCII だが、値の型は UTF-8 文字列であり、バイト長と文字数の混同を
        // 防ぐためにマルチバイトを含む値で固定する。
        let codec = "av01.0.04M.08+日本語";
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_CODEC_STRING,
            value: LocPropertyValue::Bytes(codec.as_bytes().to_vec()),
        });
        let encoded = props
            .encode()
            .expect("正当なテスト入力の encode は成功する");
        assert_eq!(encoded[2], codec.len() as u8);
        let (decoded, _) =
            LocProperties::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(decoded.codec_string(), Ok(Some(codec)));
        assert_eq!(
            decoded.find_bytes(PROP_CODEC_STRING).map(<[u8]>::len),
            Some(codec.len()),
            "Length は文字数ではなく UTF-8 のバイト数であること"
        );
    }

    #[test]
    fn encode_rejects_invalid_codec_string_utf8() {
        // 最上位の CODEC_STRING が不正な UTF-8 なら、ローカル入力の encode でも
        // MALFORMED_KEY_VALUE の分類で拒否すること。
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_CODEC_STRING,
            value: LocPropertyValue::Bytes(vec![0xFF, 0xFE]),
        });
        assert!(matches!(
            props.encode(),
            Err(MessageError::MalformedKeyValue(_))
        ));
    }

    #[test]
    fn encode_rejects_invalid_codec_string_utf8_inside_immutable() {
        // IMMUTABLE_PROPERTIES の内側が解析できる場合、その中の CODEC_STRING の不正 UTF-8 も
        // encode で拒否すること。
        let mut inner = Vec::new();
        shiguredo_moqt::varint::encode(PROP_CODEC_STRING, &mut inner);
        shiguredo_moqt::varint::encode(2, &mut inner);
        inner.extend_from_slice(&[0xFF, 0xFE]);

        let mut outer = LocProperties::new();
        outer.push(LocProperty {
            prop_id: PROP_IMMUTABLE_PROPERTIES,
            value: LocPropertyValue::Bytes(inner),
        });
        assert!(matches!(
            outer.encode(),
            Err(MessageError::MalformedKeyValue(_))
        ));
    }

    #[test]
    fn encode_rejects_unparseable_immutable_properties() {
        // 内側 KVP が解析できない 0x0B は、ローカル入力の検証として encode が拒否すること。
        // decode の寛容な扱い (内側の構造破損を検出しない) とは別に、送信側は malformed な
        // Track を生成しない。
        //
        // (1) 入れ子の 0x0B は malformed Track であるため ProtocolViolation。
        let mut nested = Vec::new();
        shiguredo_moqt::varint::encode(PROP_IMMUTABLE_PROPERTIES, &mut nested);
        shiguredo_moqt::varint::encode(0, &mut nested);

        let mut outer = LocProperties::new();
        outer.push(LocProperty {
            prop_id: PROP_IMMUTABLE_PROPERTIES,
            value: LocPropertyValue::Bytes(nested),
        });
        let err = outer
            .encode()
            .expect_err("入れ子の 0x0B を持つ内側は encode で拒否されること");
        assert!(
            matches!(err, MessageError::ProtocolViolation(_)),
            "入れ子の 0x0B は ProtocolViolation であること: {err:?}"
        );

        // (2) 宣言長が残りバイトを超える内側 KVP も、内側の走査エラーとして拒否されること。
        let mut truncated = Vec::new();
        shiguredo_moqt::varint::encode(0x1001, &mut truncated); // 未知の奇数 ID
        shiguredo_moqt::varint::encode(5, &mut truncated); // 宣言長 5
        truncated.push(0xAA); // 実体は 1 バイトだけ (宣言長超過)

        let mut outer = LocProperties::new();
        outer.push(LocProperty {
            prop_id: PROP_IMMUTABLE_PROPERTIES,
            value: LocPropertyValue::Bytes(truncated),
        });
        assert!(
            matches!(outer.encode(), Err(MessageError::UnexpectedEof)),
            "宣言長が残りバイトを超える内側は切り詰めとして UnexpectedEof になること"
        );

        // (3) 内側に単一に限られる既知 ID の重複 (0x11 を 2 つ) がある場合も、ローカル入力の
        // 検証として ProtocolViolation で拒否されること (受信 Track の malformed 判定に使う
        // MalformedTrack を encode から返さない)。
        let mut duplicate = Vec::new();
        shiguredo_moqt::varint::encode(PROP_CODEC_STRING, &mut duplicate);
        shiguredo_moqt::varint::encode(4, &mut duplicate);
        duplicate.extend_from_slice(b"av01");
        shiguredo_moqt::varint::encode(0, &mut duplicate); // delta=0 → 同じ 0x11
        shiguredo_moqt::varint::encode(4, &mut duplicate);
        duplicate.extend_from_slice(b"vp09");

        let mut outer = LocProperties::new();
        outer.push(LocProperty {
            prop_id: PROP_IMMUTABLE_PROPERTIES,
            value: LocPropertyValue::Bytes(duplicate),
        });
        assert!(
            matches!(outer.encode(), Err(MessageError::ProtocolViolation(_))),
            "内側の既知 ID の重複は ProtocolViolation になること"
        );
    }

    #[test]
    fn decode_rejects_invalid_codec_string_utf8() {
        // 最上位の CODEC_STRING が不正な UTF-8 なら decode は MALFORMED_KEY_VALUE になること。
        let wire = wire_with_top_level_codec_string(&[0xFF, 0xFE]);
        assert!(matches!(
            LocProperties::decode(&wire),
            Err(MessageError::MalformedKeyValue(_))
        ));
    }

    #[test]
    fn decode_rejects_invalid_codec_string_utf8_inside_immutable() {
        // IMMUTABLE_PROPERTIES の内側が解析できる場合、内側の CODEC_STRING の不正 UTF-8 も
        // decode で拒否すること。0x80 は UTF-8 の継続バイト単独であり不正である。
        let wire = wire_with_inner_codec_string(&[0x80]);
        assert!(matches!(
            LocProperties::decode(&wire),
            Err(MessageError::MalformedKeyValue(_))
        ));
    }

    #[test]
    fn decode_keeps_unparseable_immutable_with_invalid_codec_string() {
        // 内側 KVP が解析できない場合、decode は内側の CODEC_STRING を検証せず成功すること
        // (内側の構造破損を decode では検出しない従来の方針の維持)。
        //
        // (1) 不正 UTF-8 の CODEC_STRING の後ろに切り詰めがあり、内側全体が解析できない。
        let mut truncated = Vec::new();
        shiguredo_moqt::varint::encode(PROP_CODEC_STRING, &mut truncated);
        shiguredo_moqt::varint::encode(2, &mut truncated);
        truncated.extend_from_slice(&[0xFF, 0xFE]);
        shiguredo_moqt::varint::encode(1, &mut truncated);
        shiguredo_moqt::varint::encode(5, &mut truncated);
        truncated.push(0xAA);

        let wire = wire_with_inner_bytes(&truncated);
        let (decoded, _) =
            LocProperties::decode(&wire).expect("内側が解析できない場合の decode は成功すること");
        assert_eq!(decoded.find_bytes(PROP_CODEC_STRING), None);
        assert_eq!(decoded.codec_string(), Ok(None));

        // (2) 入れ子の IMMUTABLE_PROPERTIES (0x0B) があると内側の走査は打ち切られるため、
        // 後続の CODEC_STRING が不正な UTF-8 でも decode は成功する。
        let mut nested = Vec::new();
        shiguredo_moqt::varint::encode(PROP_IMMUTABLE_PROPERTIES, &mut nested);
        shiguredo_moqt::varint::encode(0, &mut nested);
        shiguredo_moqt::varint::encode(PROP_CODEC_STRING - PROP_IMMUTABLE_PROPERTIES, &mut nested);
        shiguredo_moqt::varint::encode(1, &mut nested);
        nested.push(0xFF);

        let wire = wire_with_inner_bytes(&nested);
        let (decoded, _) = LocProperties::decode(&wire)
            .expect("入れ子の 0x0B がある内側も decode を失敗させないこと");
        assert_eq!(decoded.find_bytes(PROP_CODEC_STRING), None);
        assert_eq!(decoded.codec_string(), Ok(None));

        // (3) LOC 固有の値域違反 (Audio Level 256) も内側の走査を失敗させるため、後続の
        // CODEC_STRING が不正な UTF-8 でも decode は成功する。この残るギャップは
        // docs/missing-features.md §4.1 に記録している。
        let mut value_violation = Vec::new();
        shiguredo_moqt::varint::encode(PROP_AUDIO_LEVEL, &mut value_violation);
        shiguredo_moqt::varint::encode(256, &mut value_violation);
        shiguredo_moqt::varint::encode(PROP_CODEC_STRING - PROP_AUDIO_LEVEL, &mut value_violation);
        shiguredo_moqt::varint::encode(1, &mut value_violation);
        value_violation.push(0xFF);

        let wire = wire_with_inner_bytes(&value_violation);
        let (decoded, _) = LocProperties::decode(&wire)
            .expect("内側の値域違反がある場合も decode を失敗させないこと");
        assert_eq!(
            decoded.codec_string(),
            Ok(None),
            "値域違反で内側を解析できない場合は欠落として扱われること"
        );
    }

    #[test]
    fn codec_string_max_length_accepted() {
        // 奇数 ID の値長上限 2^16-1 バイト (draft-ietf-moq-transport-23 §8.3
        // (Key-Value-Pair Structure)) ちょうどの CODEC_STRING を往復できること。
        let codec = "a".repeat(65535);
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_CODEC_STRING,
            value: LocPropertyValue::Bytes(codec.as_bytes().to_vec()),
        });
        let encoded = props
            .encode()
            .expect("上限ちょうどの UTF-8 値は encode できること");
        let (decoded, _) =
            LocProperties::decode(&encoded).expect("上限ちょうどの UTF-8 値は decode できること");
        assert_eq!(decoded.codec_string(), Ok(Some(codec.as_str())));
        assert_eq!(
            decoded.find_bytes(PROP_CODEC_STRING).map(<[u8]>::len),
            Some(65535)
        );
    }

    #[test]
    fn codec_string_over_65535_rejected() {
        // 上限を 1 バイト超える CODEC_STRING は encode で PayloadTooLong になること
        // (既知の値域・長さ検証より汎用の長さ上限が先に効く)。
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_CODEC_STRING,
            value: LocPropertyValue::Bytes(vec![b'a'; 65536]),
        });
        assert_eq!(props.encode(), Err(MessageError::PayloadTooLong));
    }

    #[test]
    fn codec_string_varint_value_rejected() {
        // CODEC_STRING (0x11) は奇数 ID であり、値は Length + バイト列でなければならない。
        // VarInt 値を持つ in-memory 入力は encode で ProtocolViolation になること。
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_CODEC_STRING,
            value: LocPropertyValue::VarInt(0),
        });
        assert!(matches!(
            props.encode(),
            Err(MessageError::ProtocolViolation(_))
        ));
    }

    #[test]
    fn unknown_property_preserved_alongside_codec_string() {
        // CODEC_STRING を追加しても、未知 Property の値と複数インスタンスの保持が変わらない
        // こと (draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) の "MUST allow
        // multiple instances of the Property" / §16.8 (Properties) の "Endpoints MUST ignore
        // unknown Property types")。
        let codec = "av01.0.04M.08";
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_CODEC_STRING,
            value: LocPropertyValue::Bytes(codec.as_bytes().to_vec()),
        });
        props.push(LocProperty {
            prop_id: 0x1000,
            value: LocPropertyValue::VarInt(42),
        });
        props.push(LocProperty {
            prop_id: 0x1001,
            value: LocPropertyValue::Bytes(b"unknown".to_vec()),
        });
        props.push(LocProperty {
            prop_id: 0x1001,
            value: LocPropertyValue::Bytes(b"duplicate unknown".to_vec()),
        });

        let encoded = props
            .encode()
            .expect("未知 Property を含む encode は成功する");
        let (decoded, consumed) =
            LocProperties::decode(&encoded).expect("未知 Property を含む decode は成功する");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded.codec_string(), Ok(Some(codec)));
        assert_eq!(decoded.find_varint(0x1000), Some(42));
        assert_eq!(
            decoded.find_bytes(0x1001),
            Some(&b"unknown"[..]),
            "同じ未知 ID の最初のインスタンスを返すこと"
        );
        assert_eq!(
            decoded.iter().count(),
            4,
            "未知 Property の複数インスタンスを保持すること"
        );
    }

    #[test]
    fn duplicate_codec_string_rejected_on_encode() {
        // CODEC_STRING は単一インスタンスの既知型であるため、ローカル入力の encode では
        // ProtocolViolation で拒否すること (draft-ietf-moq-transport-23 §8.4 (Track and
        // Object Properties) / §12.1 (Malformed Tracks) 条件 9)。
        let mut props = LocProperties::new();
        for codec in ["av01.0.04M.08", "vp09.00.10.08"] {
            props.push(LocProperty {
                prop_id: PROP_CODEC_STRING,
                value: LocPropertyValue::Bytes(codec.as_bytes().to_vec()),
            });
        }
        assert!(matches!(
            props.encode(),
            Err(MessageError::ProtocolViolation(_))
        ));
    }

    #[test]
    fn duplicate_codec_string_is_malformed_on_decode() {
        // decode では単一に限られる既知 ID の重複を malformed Track として報告すること
        // (draft-ietf-moq-transport-23 §8.4 / §12.1 (Malformed Tracks) 条件 9)。
        let mut inner = Vec::new();
        shiguredo_moqt::varint::encode(PROP_CODEC_STRING, &mut inner);
        shiguredo_moqt::varint::encode(3, &mut inner);
        inner.extend_from_slice(b"av1");
        shiguredo_moqt::varint::encode(0, &mut inner); // delta=0 で同じ ID を繰り返す
        shiguredo_moqt::varint::encode(3, &mut inner);
        inner.extend_from_slice(b"av1");

        let mut wire = Vec::new();
        shiguredo_moqt::varint::encode(inner.len() as u64, &mut wire);
        wire.extend_from_slice(&inner);
        assert!(matches!(
            LocProperties::decode(&wire),
            Err(MessageError::MalformedTrack(_))
        ));
    }
}

mod delta_encoding {
    use super::*;

    #[test]
    fn delta_type_correctness() {
        // ソート順: prop_id=0x09 (VFM), prop_id=0x0D (VideoConfig), prop_id=0x10 (Timestamp)
        // delta: 9, 4, 3
        // varint 値は 63 以下の 1 バイト varint に収まる値を使う
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: LocPropertyValue::VarInt(50), // 50 < 64 → 1 バイト varint
        });
        props.push(LocProperty {
            prop_id: PROP_VIDEO_FRAME_MARKING,
            value: LocPropertyValue::Bytes(vec![0b11]),
        });
        props.push(LocProperty {
            prop_id: PROP_VIDEO_CONFIG,
            value: LocPropertyValue::Bytes(b"config".to_vec()),
        });

        let encoded = props
            .encode()
            .expect("正当なテスト入力の encode は成功する");

        // inner ブロックの構成 (prop_id 昇順にソート済):
        // delta=9 (1 byte), length=1 (1 byte), value=0b11 (1 byte) → 3 bytes (VFM, id=0x09)
        // delta=4 (1 byte), length=6 (1 byte), b"config" (6 bytes)  → 8 bytes (VideoConfig, id=0x0D)
        // delta=3 (1 byte), value=50 (1 byte)                       → 2 bytes (Timestamp, id=0x10)
        // inner length = 13, length フィールド = 1 byte
        assert_eq!(encoded[0], 13); // inner length = 13
        assert_eq!(encoded[1], 0x09); // delta=9 (prop_id=0x09, VFM)
        assert_eq!(encoded[2], 0x01); // length=1
        assert_eq!(encoded[3], 0b11); // VFM の値バイト
        assert_eq!(encoded[4], 0x04); // delta=4 (prop_id=0x0D, VideoConfig)
        assert_eq!(encoded[5], 0x06); // length=6
        assert_eq!(encoded[12], 0x03); // delta=3 (prop_id=0x10, Timestamp)
        assert_eq!(encoded[13], 50); // value=50

        let (decoded, consumed) =
            LocProperties::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded.video_frame_marking(), Some(&[0b11u8][..]));
        assert_eq!(decoded.timestamp(), Some(50));
        assert_eq!(decoded.video_config(), Some(b"config".as_ref()));
    }

    #[test]
    fn encode_sorts_by_id() {
        // 逆順に push してもソートされてエンコードされる
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_TIMESTAMP, // 0x10
            value: LocPropertyValue::VarInt(999),
        });
        props.push(LocProperty {
            prop_id: PROP_VIDEO_FRAME_MARKING, // 0x09
            value: LocPropertyValue::Bytes(vec![0xFF]),
        });

        let encoded = props
            .encode()
            .expect("正当なテスト入力の encode は成功する");
        let (decoded, _) =
            LocProperties::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(decoded.video_frame_marking(), Some(&[0xFFu8][..]));
        assert_eq!(decoded.timestamp(), Some(999));
    }

    #[test]
    fn timestamp_u64_max_roundtrip() {
        // Timestamp は vi64 1-9 bytes (u64 全域) のため u64::MAX もラウンドトリップする
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: LocPropertyValue::VarInt(u64::MAX),
        });
        let encoded = props
            .encode()
            .expect("正当なテスト入力の encode は成功する");
        let (decoded, _) =
            LocProperties::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(decoded.timestamp(), Some(u64::MAX));
    }

    #[test]
    fn unknown_property_id_roundtrip() {
        // 未知の偶数 ID (100) と奇数 ID (101) のラウンドトリップ
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: 100,
            value: LocPropertyValue::VarInt(42),
        });
        props.push(LocProperty {
            prop_id: 101,
            value: LocPropertyValue::Bytes(vec![0xDE, 0xAD]),
        });

        let encoded = props
            .encode()
            .expect("正当なテスト入力の encode は成功する");
        let (decoded, consumed) =
            LocProperties::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded.len(), 2);
        let entries: Vec<_> = decoded.iter().collect();
        assert_eq!(entries[0].prop_id, 100);
        assert_eq!(entries[0].value, LocPropertyValue::VarInt(42));
        assert_eq!(entries[1].prop_id, 101);
        assert_eq!(entries[1].value, LocPropertyValue::Bytes(vec![0xDE, 0xAD]));
    }
}

mod error_cases {
    use super::*;

    #[test]
    fn empty_encode_produces_zero_length() {
        // draft-ietf-moq-transport-23 §11.3.1 (Subgroup Header): 空のプロパティ列は Properties Length = 0
        let props = LocProperties::new();
        let encoded = props
            .encode()
            .expect("正当なテスト入力の encode は成功する");
        assert_eq!(encoded, vec![0x00]); // Properties Length = 0
    }

    #[test]
    fn even_id_with_bytes_value_error() {
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_TIMESTAMP, // 偶数 (0x10)
            value: LocPropertyValue::Bytes(vec![0x01]),
        });
        assert!(matches!(
            props.encode(),
            Err(MessageError::ProtocolViolation(_))
        ));
    }

    #[test]
    fn odd_id_with_varint_value_error() {
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_VIDEO_CONFIG, // 奇数 (13)
            value: LocPropertyValue::VarInt(42),
        });
        assert!(matches!(
            props.encode(),
            Err(MessageError::ProtocolViolation(_))
        ));
    }

    #[test]
    fn video_frame_marking_empty_returns_key_value_formatting_error() {
        // 長さ 0 は 1-4 bytes の範囲外
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_VIDEO_FRAME_MARKING,
            value: LocPropertyValue::Bytes(vec![]),
        });
        assert!(matches!(
            props.encode(),
            Err(MessageError::MalformedKeyValue(_))
        ));
    }

    #[test]
    fn video_frame_marking_5_bytes_returns_key_value_formatting_error() {
        // 長さ 5 は 1-4 bytes の範囲外
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_VIDEO_FRAME_MARKING,
            value: LocPropertyValue::Bytes(vec![0x01, 0x02, 0x03, 0x04, 0x05]),
        });
        assert!(matches!(
            props.encode(),
            Err(MessageError::MalformedKeyValue(_))
        ));
    }

    #[test]
    fn audio_level_exceeds_range_returns_key_value_formatting_error() {
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_AUDIO_LEVEL,
            value: LocPropertyValue::VarInt(256),
        });
        assert!(matches!(
            props.encode(),
            Err(MessageError::MalformedKeyValue(_))
        ));
    }

    #[test]
    fn decode_empty_buffer() {
        assert_eq!(LocProperties::decode(&[]), Err(MessageError::UnexpectedEof));
    }

    #[test]
    fn decode_zero_length_returns_empty() {
        // draft-ietf-moq-transport-23 §11.3.1 (Subgroup Header): 空のプロパティ列は Properties Length = 0
        let (props, consumed) =
            LocProperties::decode(&[0x00]).expect("テストフィクスチャの前提条件を満たす");
        assert!(props.is_empty());
        assert_eq!(consumed, 1);
    }

    #[test]
    fn decode_value_length_over_65535_is_field_length_exceeded() {
        // 奇数 ID (0x09 VIDEO_FRAME_MARKING) の宣言長 65,536 (2^16) は §8.3 の上限超過。
        // draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure) / §12.2 の優先規則により
        // FIELD_LENGTH_EXCEEDED で閉じる。
        let mut inner = Vec::new();
        inner.push(0x09u8); // delta=9 → prop_id=9 (奇数)
        inner.extend_from_slice(&[0xE0, 0x01, 0x00, 0x00]); // 65,536 の vi64
        let mut buf = vec![inner.len() as u8];
        buf.extend_from_slice(&inner);
        assert_eq!(
            LocProperties::decode(&buf),
            Err(MessageError::FieldLengthExceeded(
                "odd type key-value-pair length exceeds 65535"
            ))
        );
    }

    #[test]
    fn decode_truncated_inner() {
        // length=5 だが実際には 2 バイトしかない
        let buf = vec![0x05, 0x02, 0x01];
        assert_eq!(
            LocProperties::decode(&buf),
            Err(MessageError::UnexpectedEof)
        );
    }

    #[test]
    fn decode_truncated_bytes_value() {
        // delta=1 (奇数 ID)、value_len=10 だが値は 3 バイトしかない
        let mut inner = Vec::new();
        inner.push(0x01u8); // delta=1 → prop_id=1 (奇数)
        inner.push(0x0A); // value_len=10
        inner.extend_from_slice(&[0xAA, 0xBB, 0xCC]); // 3 バイトしかない

        let mut buf = vec![inner.len() as u8];
        buf.extend_from_slice(&inner);

        assert_eq!(
            LocProperties::decode(&buf),
            Err(MessageError::UnexpectedEof)
        );
    }
}

mod immutable_properties {
    use super::*;

    /// 外側に IMMUTABLE_PROPERTIES (0x0B) だけを持つ LOC Properties を wire 形式にする
    ///
    /// `inner` は 0x0B の値としてそのまま格納する delta-key KVP 列である。
    fn immutable_only_wire(inner: &LocProperties) -> Vec<u8> {
        let mut outer = LocProperties::new();
        outer.push(LocProperty {
            prop_id: PROP_IMMUTABLE_PROPERTIES,
            value: LocPropertyValue::Bytes(kvp_sequence(inner)),
        });
        outer
            .encode()
            .expect("テストフィクスチャの外側 KVP の encode は成功する")
    }

    #[test]
    fn finds_known_properties_only_inside_immutable_properties() {
        // draft-ietf-moq-transport-23 §10.7 (Immutable Properties) は Property を mutable
        // リストと IMMUTABLE_PROPERTIES (0x0B) の内側のどちらに置いてもよいとする。LOC 側の
        // draft-ietf-moq-loc-05 §3.1.2 (Immutable Properties) は、relay から見える不変の
        // LOC Properties を内側に置くことを SHOULD とする (LOC-05 自身は immutable を定義
        // しない)。外側の mutable リストには 0x0B だけを置き、7 つの既知 LOC プロパティを
        // 内側にだけ置いた Object でも find_varint / find_bytes と既知 accessor で値を
        // 取得できることを固定する。
        let video_config = vec![0x01u8, 0x64, 0x00, 0x1F];
        let audio_config = b"OpusHead".to_vec();
        let codec = "av01.0.04M.08";

        let mut inner = LocProperties::new();
        inner.push(LocProperty {
            prop_id: PROP_TIMESCALE,
            value: LocPropertyValue::VarInt(90_000),
        });
        inner.push(LocProperty {
            prop_id: PROP_VIDEO_FRAME_MARKING,
            value: LocPropertyValue::Bytes(vec![0x20]),
        });
        inner.push(LocProperty {
            prop_id: PROP_AUDIO_LEVEL,
            value: LocPropertyValue::VarInt(0x50),
        });
        inner.push(LocProperty {
            prop_id: PROP_VIDEO_CONFIG,
            value: LocPropertyValue::Bytes(video_config.clone()),
        });
        inner.push(LocProperty {
            prop_id: PROP_AUDIO_CONFIG,
            value: LocPropertyValue::Bytes(audio_config.clone()),
        });
        inner.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: LocPropertyValue::VarInt(7_200_000),
        });
        inner.push(LocProperty {
            prop_id: PROP_CODEC_STRING,
            value: LocPropertyValue::Bytes(codec.as_bytes().to_vec()),
        });

        let wire = immutable_only_wire(&inner);
        let (decoded, consumed) = LocProperties::decode(&wire)
            .expect("IMMUTABLE_PROPERTIES だけを持つ properties は decode できること");
        assert_eq!(consumed, wire.len());

        // iter() は最上位リストだけを返す (内側の 7 プロパティは列挙されない)
        assert_eq!(decoded.len(), 1);
        let top_level_ids: Vec<u64> = decoded.iter().map(|p| p.prop_id).collect();
        assert_eq!(top_level_ids, vec![PROP_IMMUTABLE_PROPERTIES]);

        // find_varint は内側の varint プロパティを取得する
        assert_eq!(decoded.find_varint(PROP_TIMESTAMP), Some(7_200_000));
        assert_eq!(decoded.find_varint(PROP_TIMESCALE), Some(90_000));
        assert_eq!(decoded.find_varint(PROP_AUDIO_LEVEL), Some(0x50));

        // find_bytes は外側の Bytes を借用したまま内側の値のスライスを返す
        assert_eq!(
            decoded.find_bytes(PROP_VIDEO_CONFIG),
            Some(video_config.as_slice())
        );
        assert_eq!(
            decoded.find_bytes(PROP_AUDIO_CONFIG),
            Some(audio_config.as_slice())
        );
        assert_eq!(
            decoded.find_bytes(PROP_VIDEO_FRAME_MARKING),
            Some(&[0x20u8][..])
        );
        assert_eq!(
            decoded.find_bytes(PROP_CODEC_STRING),
            Some(codec.as_bytes())
        );

        // 既知 accessor も同じ値に到達する
        assert_eq!(decoded.timestamp(), Some(7_200_000));
        assert_eq!(decoded.timescale(), Some(90_000));
        assert_eq!(decoded.audio_level(), Some(0x50));
        assert_eq!(decoded.video_config(), Some(video_config.as_slice()));
        assert_eq!(decoded.audio_config(), Some(audio_config.as_slice()));
        assert_eq!(decoded.video_frame_marking(), Some(&[0x20u8][..]));
        assert_eq!(decoded.codec_string(), Ok(Some(codec)));
    }

    #[test]
    fn top_level_properties_win_over_immutable_properties() {
        // 最上位と内側の両方に同じ ID のプロパティがある場合、既存の ObjectProperties /
        // TrackProperties の getter と同じく最上位を優先する (draft-ietf-moq-transport-23
        // §10.7 (Immutable Properties) の "MUST search both")。内側だけに別の値を置き、
        // 探索が内側へ落ちないことを固定する。なお §10.7 は複数の値を許すプロパティに限り
        // 両方への出現を許し、単一値プロパティが両方に現れると §8.4 / §12.1 条件 9 の
        // malformed になるが、その判定は本モジュールの範囲外であり、ここでは探索の優先規則
        // だけを固定する。
        let mut inner = LocProperties::new();
        inner.push(LocProperty {
            prop_id: PROP_TIMESCALE,
            value: LocPropertyValue::VarInt(48_000),
        });
        inner.push(LocProperty {
            prop_id: PROP_VIDEO_FRAME_MARKING,
            value: LocPropertyValue::Bytes(vec![0x00]),
        });
        inner.push(LocProperty {
            prop_id: PROP_AUDIO_LEVEL,
            value: LocPropertyValue::VarInt(1),
        });
        inner.push(LocProperty {
            prop_id: PROP_VIDEO_CONFIG,
            value: LocPropertyValue::Bytes(vec![0xBB]),
        });
        inner.push(LocProperty {
            prop_id: PROP_AUDIO_CONFIG,
            value: LocPropertyValue::Bytes(b"inner".to_vec()),
        });
        inner.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: LocPropertyValue::VarInt(200),
        });
        inner.push(LocProperty {
            prop_id: PROP_CODEC_STRING,
            value: LocPropertyValue::Bytes(b"vp09.00.10.08".to_vec()),
        });

        let mut outer = LocProperties::new();
        outer.push(LocProperty {
            prop_id: PROP_IMMUTABLE_PROPERTIES,
            value: LocPropertyValue::Bytes(kvp_sequence(&inner)),
        });
        outer.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: LocPropertyValue::VarInt(100),
        });
        outer.push(LocProperty {
            prop_id: PROP_TIMESCALE,
            value: LocPropertyValue::VarInt(90_000),
        });
        outer.push(LocProperty {
            prop_id: PROP_VIDEO_FRAME_MARKING,
            value: LocPropertyValue::Bytes(vec![0x20]),
        });
        outer.push(LocProperty {
            prop_id: PROP_AUDIO_LEVEL,
            value: LocPropertyValue::VarInt(2),
        });
        outer.push(LocProperty {
            prop_id: PROP_VIDEO_CONFIG,
            value: LocPropertyValue::Bytes(vec![0xAA]),
        });
        outer.push(LocProperty {
            prop_id: PROP_AUDIO_CONFIG,
            value: LocPropertyValue::Bytes(b"outer".to_vec()),
        });
        outer.push(LocProperty {
            prop_id: PROP_CODEC_STRING,
            value: LocPropertyValue::Bytes(b"av01.0.04M.08".to_vec()),
        });

        let wire = outer
            .encode()
            .expect("テストフィクスチャの外側 KVP の encode は成功する");
        let (decoded, _) = LocProperties::decode(&wire)
            .expect("最上位と内側の両方を持つ properties は decode できること");

        assert_eq!(decoded.timestamp(), Some(100));
        assert_eq!(decoded.timescale(), Some(90_000));
        assert_eq!(decoded.video_frame_marking(), Some(&[0x20u8][..]));
        assert_eq!(decoded.audio_level(), Some(2));
        assert_eq!(decoded.video_config(), Some(&[0xAAu8][..]));
        assert_eq!(decoded.audio_config(), Some(&b"outer"[..]));
        assert_eq!(decoded.codec_string(), Ok(Some("av01.0.04M.08")));

        // find_varint / find_bytes も accessor と同じく最上位を返す
        assert_eq!(decoded.find_varint(PROP_TIMESTAMP), Some(100));
        assert_eq!(decoded.find_varint(PROP_AUDIO_LEVEL), Some(2));
        assert_eq!(decoded.find_bytes(PROP_VIDEO_CONFIG), Some(&[0xAAu8][..]));
        assert_eq!(
            decoded.find_bytes(PROP_VIDEO_FRAME_MARKING),
            Some(&[0x20u8][..])
        );
        assert_eq!(
            decoded.find_bytes(PROP_CODEC_STRING),
            Some(b"av01.0.04M.08".as_slice())
        );
    }

    #[test]
    fn corrupt_inner_immutable_properties_returns_none() {
        // 内側の KVP が decode できない場合、find_varint / find_bytes は None を返す。
        // 内側の検証は decode 時ではなく探索時に行うため、外側の decode 自体は成功したまま
        // であることも合わせて固定する (decode の戻り値とエラー分類を変えない)。
        //
        // (1) Video Config の宣言長 5 に対して実バイトが 1 しかない (切り詰め)
        let mut truncated = Vec::new();
        shiguredo_moqt::varint::encode(PROP_VIDEO_CONFIG, &mut truncated);
        shiguredo_moqt::varint::encode(5, &mut truncated);
        truncated.push(0xAA);
        let wire = wire_with_inner_bytes(&truncated);
        let (decoded, _) =
            LocProperties::decode(&wire).expect("外側の decode は内側の切り詰めを検出しないこと");
        assert_eq!(decoded.find_bytes(PROP_VIDEO_CONFIG), None);
        assert_eq!(decoded.video_config(), None);

        // (2) Audio Level の値域違反 (256) は内側でも MalformedKeyValue 相当として扱い、
        // 探索結果を None にする
        let mut audio_level_out_of_range = Vec::new();
        shiguredo_moqt::varint::encode(PROP_AUDIO_LEVEL, &mut audio_level_out_of_range);
        shiguredo_moqt::varint::encode(256, &mut audio_level_out_of_range);
        let wire = wire_with_inner_bytes(&audio_level_out_of_range);
        let (decoded, _) =
            LocProperties::decode(&wire).expect("外側の decode は内側の値域違反を検出しないこと");
        assert_eq!(decoded.find_varint(PROP_AUDIO_LEVEL), None);
        assert_eq!(decoded.audio_level(), None);

        // (3) Video Frame Marking の宣言長 5 は 1-4 バイトの範囲外
        let mut vfm_too_long = Vec::new();
        shiguredo_moqt::varint::encode(PROP_VIDEO_FRAME_MARKING, &mut vfm_too_long);
        shiguredo_moqt::varint::encode(5, &mut vfm_too_long);
        vfm_too_long.extend_from_slice(&[0x01, 0x02, 0x03, 0x04, 0x05]);
        let wire = wire_with_inner_bytes(&vfm_too_long);
        let (decoded, _) =
            LocProperties::decode(&wire).expect("外側の decode は内側の長さ違反を検出しないこと");
        assert_eq!(decoded.find_bytes(PROP_VIDEO_FRAME_MARKING), None);
        assert_eq!(decoded.video_frame_marking(), None);

        // (4) 単一に限られる Timestamp の重複は内側でも malformed Track として扱い、
        // 探索結果を None にする
        let mut duplicate = Vec::new();
        shiguredo_moqt::varint::encode(PROP_TIMESTAMP, &mut duplicate);
        shiguredo_moqt::varint::encode(1, &mut duplicate);
        shiguredo_moqt::varint::encode(0, &mut duplicate);
        shiguredo_moqt::varint::encode(2, &mut duplicate);
        let wire = wire_with_inner_bytes(&duplicate);
        let (decoded, _) =
            LocProperties::decode(&wire).expect("外側の decode は内側の重複を検出しないこと");
        assert_eq!(decoded.find_varint(PROP_TIMESTAMP), None);
        assert_eq!(decoded.timestamp(), None);
    }

    #[test]
    fn nested_immutable_properties_inside_inner_returns_none() {
        // draft-ietf-moq-transport-23 §10.7 (Immutable Properties) は、IMMUTABLE_PROPERTIES
        // の内側に別の IMMUTABLE_PROPERTIES を含む Object を malformed Track とする。
        // 内側に有効な Timestamp が続いていても、入れ子を検出した時点で内側の探索を
        // 打ち切り、find_varint / find_bytes は None を返すことを固定する。
        let mut inner = Vec::new();
        shiguredo_moqt::varint::encode(PROP_IMMUTABLE_PROPERTIES, &mut inner); // delta = 0x0B
        shiguredo_moqt::varint::encode(0, &mut inner); // 入れ子の 0x0B (値は空)
        shiguredo_moqt::varint::encode(PROP_TIMESTAMP - PROP_IMMUTABLE_PROPERTIES, &mut inner); // delta = 5 (0x10 - 0x0B)
        shiguredo_moqt::varint::encode(42, &mut inner); // 入れ子の後ろの Timestamp

        let wire = wire_with_inner_bytes(&inner);
        let (decoded, _) =
            LocProperties::decode(&wire).expect("外側の decode は入れ子の 0x0B を検出しないこと");
        assert_eq!(decoded.find_varint(PROP_TIMESTAMP), None);
        assert_eq!(decoded.timestamp(), None);
        assert_eq!(decoded.find_bytes(PROP_VIDEO_CONFIG), None);
        assert_eq!(decoded.video_config(), None);
    }

    #[test]
    fn find_bytes_borrows_from_outer_immutable_properties() {
        // find_bytes は外側の 0x0B の Bytes を借用したまま内側の値のスライスを返し、
        // Vec へコピーしない。返したスライスが外側の Bytes の範囲内を指すことで固定する。
        let video_config = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let mut inner = LocProperties::new();
        inner.push(LocProperty {
            prop_id: PROP_VIDEO_CONFIG,
            value: LocPropertyValue::Bytes(video_config.clone()),
        });
        let wire = immutable_only_wire(&inner);
        let (decoded, _) =
            LocProperties::decode(&wire).expect("IMMUTABLE_PROPERTIES の decode は成功すること");

        let immutable_bytes = decoded
            .iter()
            .find(|p| p.prop_id == PROP_IMMUTABLE_PROPERTIES)
            .and_then(|p| match &p.value {
                LocPropertyValue::Bytes(b) => Some(b.as_slice()),
                LocPropertyValue::VarInt(_) => None,
            })
            .expect("外側の 0x0B が Bytes 値を持つこと");
        let found = decoded
            .find_bytes(PROP_VIDEO_CONFIG)
            .expect("内側の Video Config を取得できること");
        assert_eq!(found, video_config.as_slice());

        // ポインタの範囲で、返したスライスが外側の Bytes の内側を指していることを確認する
        let immutable_start = immutable_bytes.as_ptr() as usize;
        let immutable_end = immutable_start + immutable_bytes.len();
        let found_start = found.as_ptr() as usize;
        assert!(
            found_start >= immutable_start && found_start + found.len() <= immutable_end,
            "find_bytes は外側の IMMUTABLE_PROPERTIES の Bytes を指すスライスを返すこと"
        );
    }

    #[test]
    fn empty_immutable_properties_returns_none() {
        // IMMUTABLE_PROPERTIES の値が空 (長さ 0) の場合、内側には何も無いため探索結果は None。
        let wire = wire_with_inner_bytes(&[]);
        let (decoded, _) = LocProperties::decode(&wire)
            .expect("空の IMMUTABLE_PROPERTIES を持つ properties は decode できること");
        assert_eq!(decoded.find_varint(PROP_TIMESTAMP), None);
        assert_eq!(decoded.find_bytes(PROP_VIDEO_CONFIG), None);
        assert_eq!(decoded.timestamp(), None);
        assert_eq!(decoded.video_config(), None);
    }

    #[test]
    fn find_skips_mismatched_value_form_and_continues_to_inner() {
        // 最上位に同じ ID の値形式不一致 (偶数 ID に Bytes) があっても読み飛ばし、値形式が
        // 一致するインスタンスまで探す。これは ObjectProperties / TrackProperties の
        // getter と同じ規則であり、wire 経由では起こらない in-memory 入力だけの分岐である。
        let unknown_even_id = 0x1000;
        let mut inner = Vec::new();
        shiguredo_moqt::varint::encode(unknown_even_id, &mut inner);
        shiguredo_moqt::varint::encode(7, &mut inner);

        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_IMMUTABLE_PROPERTIES,
            value: LocPropertyValue::Bytes(inner),
        });
        props.push(LocProperty {
            prop_id: unknown_even_id,
            value: LocPropertyValue::Bytes(vec![0xFF]),
        });

        // 最上位の Bytes は varint 探索では読み飛ばされ、内側の varint 値が返る。
        assert_eq!(props.find_varint(unknown_even_id), Some(7));
        // バイト列探索は最上位の値を返す (値形式が一致するため内側へ落ちない)。
        assert_eq!(props.find_bytes(unknown_even_id), Some(&[0xFFu8][..]));
    }

    #[test]
    fn immutable_properties_that_is_not_bytes_stops_inner_search() {
        // 先頭の 0x0B がバイト列でない (不正な) 入力を push で構築した場合、後続の 0x0B の
        // 内側を探索せず None を返すこと (0x0B が単一である前提が破れた入力への防御)。
        let inner = kvp_sequence(&{
            let mut inner = LocProperties::new();
            inner.push(LocProperty {
                prop_id: PROP_VIDEO_CONFIG,
                value: LocPropertyValue::Bytes(vec![0x01]),
            });
            inner
        });
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_IMMUTABLE_PROPERTIES,
            value: LocPropertyValue::VarInt(1),
        });
        props.push(LocProperty {
            prop_id: PROP_IMMUTABLE_PROPERTIES,
            value: LocPropertyValue::Bytes(inner),
        });
        assert_eq!(props.find_bytes(PROP_VIDEO_CONFIG), None);
        assert_eq!(props.video_config(), None);
    }

    #[test]
    fn inner_declared_length_over_65535_returns_none() {
        // 内側の奇数 ID の宣言長が 65535 を超える場合、内側の走査は FieldLengthExceeded と
        // なり、探索結果は None になる (外側の decode は内側を検証しない)。
        let mut inner = Vec::new();
        shiguredo_moqt::varint::encode(0x1001, &mut inner); // 未知の奇数 ID (delta)
        shiguredo_moqt::varint::encode(65536, &mut inner); // 65535 超の宣言長
        let wire = wire_with_inner_bytes(&inner);
        let (decoded, _) =
            LocProperties::decode(&wire).expect("外側の decode は内側の長さ違反を検出しないこと");
        assert_eq!(decoded.find_bytes(0x1001), None);
        assert_eq!(decoded.find_bytes(PROP_VIDEO_CONFIG), None);
    }

    #[test]
    fn inner_truncated_and_delta_overflow_return_none() {
        // (1) 内側の varint が途中終端する場合、探索結果は None になる。
        let mut truncated = Vec::new();
        shiguredo_moqt::varint::encode(PROP_TIMESTAMP, &mut truncated);
        truncated.push(0x80); // 不完全な varint
        let wire = wire_with_inner_bytes(&truncated);
        let (decoded, _) =
            LocProperties::decode(&wire).expect("外側の decode は内側の切り詰めを検出しないこと");
        assert_eq!(decoded.find_varint(PROP_TIMESTAMP), None);

        // (2) 内側の delta-key がオーバーフローする場合、探索結果は None になる。
        let mut overflow = Vec::new();
        shiguredo_moqt::varint::encode(u64::MAX, &mut overflow); // delta = u64::MAX
        shiguredo_moqt::varint::encode(0, &mut overflow); // 奇数 ID の Length 0
        shiguredo_moqt::varint::encode(1, &mut overflow); // 次の delta でオーバーフロー
        let wire = wire_with_inner_bytes(&overflow);
        let (decoded, _) =
            LocProperties::decode(&wire).expect("外側の decode は内側の overflow を検出しないこと");
        assert_eq!(decoded.find_bytes(PROP_VIDEO_CONFIG), None);
        assert_eq!(decoded.find_varint(PROP_TIMESTAMP), None);
    }

    #[test]
    fn valid_match_before_later_inner_error_returns_none() {
        // 内側は 1 回の走査で全体を検証するため、要求 ID の有効な値が構造エラーより前に
        // あっても探索結果は None になる。一致時点で打ち切る実装に変わると検出できるよう、
        // この契約を固定する。
        let mut inner_bytes = Vec::new();
        shiguredo_moqt::varint::encode(PROP_TIMESTAMP, &mut inner_bytes); // 有効な Timestamp
        shiguredo_moqt::varint::encode(1234, &mut inner_bytes);
        shiguredo_moqt::varint::encode(0x1001 - PROP_TIMESTAMP, &mut inner_bytes); // 未知の奇数 ID
        shiguredo_moqt::varint::encode(5, &mut inner_bytes); // 宣言長 5
        inner_bytes.push(0xAA); // 実バイト 1 のみ (切り詰め)

        let wire = wire_with_inner_bytes(&inner_bytes);
        let (decoded, _) =
            LocProperties::decode(&wire).expect("外側の decode は内側の切り詰めを検出しないこと");
        assert_eq!(
            decoded.find_varint(PROP_TIMESTAMP),
            None,
            "後続の構造エラーがある場合は先頭の有効な値も返さないこと"
        );
        assert_eq!(decoded.timestamp(), None);
    }
}
