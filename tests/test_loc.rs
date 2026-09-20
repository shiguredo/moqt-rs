use shiguredo_moqt::{
    error::MessageError,
    loc::{
        LocProperties, LocProperty, LocPropertyValue, PROP_AUDIO_LEVEL, PROP_TIMESCALE,
        PROP_TIMESTAMP, PROP_VIDEO_CONFIG, PROP_VIDEO_FRAME_MARKING,
    },
};

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
        // draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header): 空のプロパティ列は Properties Length = 0
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
            Err(MessageError::KeyValueFormattingError(_))
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
            Err(MessageError::KeyValueFormattingError(_))
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
            Err(MessageError::KeyValueFormattingError(_))
        ));
    }

    #[test]
    fn decode_empty_buffer() {
        assert_eq!(LocProperties::decode(&[]), Err(MessageError::UnexpectedEof));
    }

    #[test]
    fn decode_zero_length_returns_empty() {
        // draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header): 空のプロパティ列は Properties Length = 0
        let (props, consumed) =
            LocProperties::decode(&[0x00]).expect("テストフィクスチャの前提条件を満たす");
        assert!(props.is_empty());
        assert_eq!(consumed, 1);
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
