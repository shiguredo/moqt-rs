use shiguredo_moqt::{
    error::MessageError, track_properties::TrackProperties, track_properties::TrackProperty,
    track_properties::TrackPropertyValue,
};

mod delta_encoding {
    use super::*;

    #[test]
    fn delta_type_correctness() {
        // type=2 (delta=2), type=5 (delta=3) の順でエンコードされるはず
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: 0x02,
            value: TrackPropertyValue::VarInt(50),
        });
        props.push(TrackProperty {
            prop_type: 0x05,
            value: TrackPropertyValue::Bytes(b"test".to_vec()),
        });

        let mut buf = Vec::new();
        props
            .encode(&mut buf)
            .expect("正当なテスト入力の encode は成功する");

        // カウントプレフィックスなし
        // delta = 2 (type=2 - prev=0)
        assert_eq!(buf[0], 0x02);
        // varint 50
        assert_eq!(buf[1], 50);
        // delta = 3 (type=5 - prev=2)
        assert_eq!(buf[2], 0x03);

        let decoded = TrackProperties::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(decoded.len(), 2);
    }

    #[test]
    fn encode_sorts_by_type() {
        // 逆順に push してもソートされてエンコードされる
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: 0x05,
            value: TrackPropertyValue::Bytes(b"data".to_vec()),
        });
        props.push(TrackProperty {
            prop_type: 0x02,
            value: TrackPropertyValue::VarInt(42),
        });

        let mut buf = Vec::new();
        props
            .encode(&mut buf)
            .expect("正当なテスト入力の encode は成功する");

        let decoded = TrackProperties::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(decoded.find_varint(0x02), Some(42));
        assert!(decoded.as_slice().iter().any(|p| {
            p.prop_type == 0x05 && p.value == TrackPropertyValue::Bytes(b"data".to_vec())
        }));
    }
}

mod error_cases {
    use super::*;

    #[test]
    fn unexpected_eof_in_bytes_value() {
        // delta=1 (odd type), length=10, but only 3 bytes of data
        let buf = vec![0x01, 0x0A, 0x01, 0x02, 0x03];
        assert_eq!(
            TrackProperties::decode(&buf),
            Err(MessageError::UnexpectedEof)
        );
    }

    #[test]
    fn value_too_long() {
        // delta=1 (odd type), length=65536 (> 65535)
        // vi64 for 65536: 0xC1_00_00 (3 bytes)
        let mut buf = vec![0x01];
        // 65536 を vi64 エンコードする
        // 65536 = 0x10000, 3 バイト vi64: 110xxxxx xxxxxxxx xxxxxxxx
        // 0xC0 | (0x10000 >> 16) = 0xC1, (0x10000 >> 8) & 0xFF = 0x00, 0x10000 & 0xFF = 0x00
        buf.extend_from_slice(&[0xC1, 0x00, 0x00]);

        assert!(matches!(
            TrackProperties::decode(&buf),
            Err(MessageError::ProtocolViolation(_))
        ));
    }

    #[test]
    fn encode_rejects_bytes_value_over_65535() {
        // 奇数型の Bytes 値が 65536 バイトなら encode 時に PayloadTooLong
        // (object_properties / loc と同じ上限)
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: 0x05,
            value: TrackPropertyValue::Bytes(vec![0x01; 65536]),
        });
        let mut buf = Vec::new();

        assert_eq!(
            props.encode(&mut buf),
            Err(MessageError::PayloadTooLong),
            "65535 バイトを超える Bytes 値は PayloadTooLong になること"
        );
    }
}

mod duplicate_property_type {
    use super::*;

    #[test]
    fn decode_rejects_delta_zero_after_first_entry() {
        // type=14 (0x0E, delta=14), value=1, その後 delta=0 を読んだ時点で重複として reject する
        let buf = vec![0x0E, 0x01, 0x00];
        assert_eq!(
            TrackProperties::decode(&buf),
            Err(MessageError::ProtocolViolation(
                "duplicate track property type"
            ))
        );
    }

    #[test]
    fn decode_accepts_leading_delta_zero_as_prop_type_zero() {
        // 先頭エントリは props が空なので delta=0 (prop_type=0) を重複と見なさず受理する
        // type=0 は偶数型なので value は varint。buf = [delta=0, value=5]
        let buf = vec![0x00, 0x05];
        let decoded = TrackProperties::decode(&buf).expect("先頭の delta=0 は受理されるべき");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded.find_varint(0x00), Some(5));
    }

    #[test]
    fn encode_rejects_duplicate_prop_type() {
        // 同一 prop_type を 2 回 push して encode すると ProtocolViolation を返す
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: 0x02,
            value: TrackPropertyValue::VarInt(10),
        });
        props.push(TrackProperty {
            prop_type: 0x02,
            value: TrackPropertyValue::VarInt(20),
        });

        let mut buf = Vec::new();
        assert_eq!(
            props.encode(&mut buf),
            Err(MessageError::ProtocolViolation(
                "duplicate track property type"
            ))
        );
    }
}

mod accessors {
    use super::*;

    #[test]
    fn delivery_timeout_accessor() {
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: shiguredo_moqt::track_properties::PROP_OBJECT_DELIVERY_TIMEOUT,
            value: TrackPropertyValue::VarInt(250),
        });
        assert_eq!(props.object_delivery_timeout(), Some(250));
    }

    #[test]
    fn find_varint_missing() {
        let props = TrackProperties::new();
        assert_eq!(props.find_varint(0x02), None);
    }

    #[test]
    fn default_publisher_priority_maps_u8_range() {
        use shiguredo_moqt::track_properties::PROP_DEFAULT_PUBLISHER_PRIORITY;
        // 0 と 255 は u8 に収まるため Some になる
        for value in [0u64, 128, 255] {
            let mut props = TrackProperties::new();
            props.push(TrackProperty {
                prop_type: PROP_DEFAULT_PUBLISHER_PRIORITY,
                value: TrackPropertyValue::VarInt(value),
            });
            assert_eq!(props.default_publisher_priority(), Some(value as u8));
        }
    }

    #[test]
    fn default_publisher_priority_out_of_u8_range_returns_none() {
        use shiguredo_moqt::track_properties::PROP_DEFAULT_PUBLISHER_PRIORITY;
        // push による in-memory 構築では 0-255 外の値も積めるため、
        // u8 への切り詰めで誤った値を返さず None を返すこと。
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: PROP_DEFAULT_PUBLISHER_PRIORITY,
            value: TrackPropertyValue::VarInt(256),
        });
        assert_eq!(props.default_publisher_priority(), None);
    }

    #[test]
    fn default_publisher_group_order_out_of_u8_range_returns_none() {
        use shiguredo_moqt::track_properties::PROP_DEFAULT_PUBLISHER_GROUP_ORDER;
        // push による in-memory 構築では 1 / 2 以外の値も積めるため、
        // u8 に収まらない値は None を返すこと。
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: PROP_DEFAULT_PUBLISHER_GROUP_ORDER,
            value: TrackPropertyValue::VarInt(300),
        });
        assert_eq!(props.default_publisher_group_order(), None);
    }
}

mod mandatory_properties {
    use super::*;
    use shiguredo_moqt::track_properties::{
        MANDATORY_TRACK_PROPERTY_MAX, MANDATORY_TRACK_PROPERTY_MIN,
    };

    #[test]
    fn empty_has_no_unknown_mandatory() {
        let props = TrackProperties::new();
        assert!(!props.has_unknown_mandatory());
    }

    #[test]
    fn known_properties_not_mandatory() {
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: 0x02,
            value: TrackPropertyValue::VarInt(100),
        });
        props.push(TrackProperty {
            prop_type: 0x30,
            value: TrackPropertyValue::VarInt(1),
        });
        assert!(!props.has_unknown_mandatory());
    }

    #[test]
    fn unknown_mandatory_detected() {
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: MANDATORY_TRACK_PROPERTY_MIN,
            value: TrackPropertyValue::VarInt(1),
        });
        assert!(props.has_unknown_mandatory());
    }

    #[test]
    fn property_outside_mandatory_range_not_detected() {
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: 0x1000,
            value: TrackPropertyValue::Bytes(b"data".to_vec()),
        });
        assert!(!props.has_unknown_mandatory());
    }

    #[test]
    fn property_below_mandatory_range_not_detected() {
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: MANDATORY_TRACK_PROPERTY_MIN - 1,
            value: TrackPropertyValue::VarInt(42),
        });
        props.push(TrackProperty {
            prop_type: MANDATORY_TRACK_PROPERTY_MAX + 1,
            value: TrackPropertyValue::VarInt(42),
        });
        assert!(!props.has_unknown_mandatory());
    }

    #[test]
    fn mixed_known_and_unknown_mandatory() {
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: 0x02,
            value: TrackPropertyValue::VarInt(100),
        });
        props.push(TrackProperty {
            prop_type: 0x5000,
            value: TrackPropertyValue::VarInt(1),
        });
        assert!(props.has_unknown_mandatory());
    }

    /// GREASE の Property Type は 0x4000-0x7FFF に入っていても unknown mandatory にしない
    ///
    /// draft-ietf-moq-transport-21 §16.8 (Properties) の Table 14 は GREASE の Property Type
    /// (`0x7f * N + 0x9D`) を Scope Any として予約しており、N = 128 の 0x401D から N = 256 の
    /// 0x7F9D までは §3.6 (Mandatory Track Properties) の 0x4000-0x7FFF に入る。GREASE 値は
    /// IANA 登録された Property ではないため必須トラックプロパティとして扱わない
    /// (§13 (Grease) の "Endpoints MUST NOT close the session solely because they received an
    /// unknown value.")。
    #[test]
    fn grease_values_in_mandatory_range_not_detected() {
        // 範囲の下限 (0x401D) と上限 (0x7F9D) は奇数型、間の 0x409C は偶数型である
        for prop_type in [0x401D, 0x409C, 0x7F9D] {
            assert!(
                shiguredo_moqt::grease::is_grease(prop_type),
                "テストフィクスチャの前提条件を満たす ({prop_type:#x} は GREASE 値)"
            );
            let mut props = TrackProperties::new();
            props.push(TrackProperty {
                prop_type,
                // 奇数型は長さ付きバイト列、偶数型は varint
                // (draft-ietf-moq-transport-21 §8.3 (Key-Value-Pair Structure))
                value: if prop_type % 2 == 0 {
                    TrackPropertyValue::VarInt(1)
                } else {
                    TrackPropertyValue::Bytes(vec![0xAB])
                },
            });
            assert!(
                !props.has_unknown_mandatory(),
                "GREASE 値 {prop_type:#x} を unknown mandatory として扱わないこと"
            );
        }
    }

    /// 0x4000-0x7FFF 全域で、unknown mandatory の判定が GREASE 除外とだけ一致する
    ///
    /// GREASE 値の除外漏れ (一部の N だけ除外する実装ミス) と、GREASE 以外の未知
    /// Mandatory Track Property の検出漏れ (非退行) を同時に固定する。
    /// この等式は 0x4000-0x7FFF に既知の必須トラックプロパティが無い前提に依存するため、
    /// 将来 `TrackProperties::is_known_mandatory` に既知値を追加した場合は期待値を更新すること。
    #[test]
    fn mandatory_range_scan_matches_grease_exclusion() {
        for prop_type in MANDATORY_TRACK_PROPERTY_MIN..=MANDATORY_TRACK_PROPERTY_MAX {
            let mut props = TrackProperties::new();
            props.push(TrackProperty {
                prop_type,
                value: if prop_type % 2 == 0 {
                    TrackPropertyValue::VarInt(1)
                } else {
                    TrackPropertyValue::Bytes(vec![0xAB])
                },
            });
            assert_eq!(
                props.has_unknown_mandatory(),
                !shiguredo_moqt::grease::is_grease(prop_type),
                "{prop_type:#x} の判定が GREASE 除外と一致すること"
            );
        }
    }

    /// GREASE の Track Property は encode / decode を通しても unknown mandatory にならない
    ///
    /// 奇数型 (長さ付きバイト列) と偶数型 (varint) の両方で wire 表現を往復させる
    /// (draft-ietf-moq-transport-21 §8.3 (Key-Value-Pair Structure) / §16.8 (Properties) の
    /// Table 14)。
    #[test]
    fn grease_property_roundtrip_keeps_unknown_mandatory_false() {
        for (prop_type, value) in [
            (0x401D, TrackPropertyValue::Bytes(vec![0xAB])),
            (0x409C, TrackPropertyValue::VarInt(7)),
        ] {
            assert!(
                shiguredo_moqt::grease::is_grease(prop_type),
                "テストフィクスチャの前提条件を満たす ({prop_type:#x} は GREASE 値)"
            );
            let mut props = TrackProperties::new();
            props.push(TrackProperty { prop_type, value });
            let mut buf = Vec::new();
            props
                .encode(&mut buf)
                .expect("GREASE 値は encode できること");
            let decoded = TrackProperties::decode(&buf).expect("GREASE 値は decode できること");
            assert!(
                !decoded.has_unknown_mandatory(),
                "decode 後も {prop_type:#x} を unknown mandatory として扱わないこと"
            );
        }
    }

    /// GREASE 以外の未知 Mandatory Track Property は従来どおり検出する (非退行)
    #[test]
    fn non_grease_unknown_mandatory_still_detected() {
        // 0x401E / 0x7F9E は GREASE 値の隣接値である (どちらも偶数型のため varint で符号化する)
        for prop_type in [0x401E, 0x7F9E] {
            assert!(
                !shiguredo_moqt::grease::is_grease(prop_type),
                "テストフィクスチャの前提条件を満たす ({prop_type:#x} は GREASE 値ではない)"
            );
            let mut props = TrackProperties::new();
            props.push(TrackProperty {
                prop_type,
                value: TrackPropertyValue::VarInt(1),
            });
            assert!(
                props.has_unknown_mandatory(),
                "GREASE ではない {prop_type:#x} は必須トラックプロパティとして検出すること"
            );
        }
    }
}

/// draft-ietf-moq-transport-21 §10.7 (Immutable Properties): IMMUTABLE_PROPERTIES (0x0B) の Track scope 対応。
/// 入れ子禁止・内側 KVP 検証・統合検索 (MUST search both) を検証する。
mod immutable_properties {
    use super::*;
    use shiguredo_moqt::track_properties::PROP_IMMUTABLE_PROPERTIES;

    /// 内側 Track Property 列を encode してバイト列にする (正規の KVP 列)
    fn encode_inner(entries: &[(u64, TrackPropertyValue)]) -> Vec<u8> {
        let mut inner = TrackProperties::new();
        for (prop_type, value) in entries {
            inner.push(TrackProperty {
                prop_type: *prop_type,
                value: value.clone(),
            });
        }
        let mut buf = Vec::new();
        inner
            .encode(&mut buf)
            .expect("正規の内側 KVP 列は encode 可能");
        buf
    }

    /// IMMUTABLE_PROPERTIES (0x0B) 単一エントリを持つ TrackProperties を作る
    fn with_immutable(inner_bytes: Vec<u8>) -> TrackProperties {
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: PROP_IMMUTABLE_PROPERTIES,
            value: TrackPropertyValue::Bytes(inner_bytes),
        });
        props
    }

    #[test]
    fn nested_immutable_rejected_on_encode() {
        // 内側に 0x0B を含む IMMUTABLE は encode 時に拒否される (入れ子禁止)
        let nested =
            encode_inner(&[(PROP_IMMUTABLE_PROPERTIES, TrackPropertyValue::Bytes(vec![]))]);
        let props = with_immutable(nested);
        let mut buf = Vec::new();
        assert!(matches!(
            props.encode(&mut buf),
            Err(MessageError::ProtocolViolation(_))
        ));
    }

    #[test]
    fn nested_immutable_rejected_on_decode() {
        // 外側 0x0B (len=2) の内側に 0x0B (delta=11, len=0) を含む生バイト列
        let buf = vec![0x0B, 0x02, 0x0B, 0x00];
        assert!(matches!(
            TrackProperties::decode(&buf),
            Err(MessageError::ProtocolViolation(_))
        ));
    }

    #[test]
    fn inner_unparseable_rejected_on_decode() {
        // 内側が奇数型 len=5 を主張するが 1 バイトしかない (EOF)
        let buf = vec![0x0B, 0x03, 0x01, 0x05, 0x01];
        assert_eq!(
            TrackProperties::decode(&buf),
            Err(MessageError::UnexpectedEof)
        );
    }

    #[test]
    fn inner_value_range_violation_rejected_on_decode() {
        // 内側に DYNAMIC_GROUPS (0x30) = 2 (>1) を含む生バイト列
        let buf = vec![0x0B, 0x02, 0x30, 0x02];
        assert!(matches!(
            TrackProperties::decode(&buf),
            Err(MessageError::ProtocolViolation(_))
        ));
    }

    #[test]
    fn empty_immutable_accepted() {
        // 空の IMMUTABLE_PROPERTIES (length=0) は許容される
        let props = with_immutable(Vec::new());
        let mut buf = Vec::new();
        props.encode(&mut buf).expect("空 IMMUTABLE は encode 可能");
        let decoded = TrackProperties::decode(&buf).expect("空 IMMUTABLE は decode 可能");
        assert_eq!(decoded.len(), 1);
        // 内側は空なので統合検索しても見つからない
        assert_eq!(decoded.find_varint(0x02), None);
    }

    #[test]
    fn find_varint_searches_inside_immutable() {
        // OBJECT_DELIVERY_TIMEOUT (0x02) を内側に持つ IMMUTABLE
        let inner = encode_inner(&[(0x02, TrackPropertyValue::VarInt(250))]);
        let props = with_immutable(inner);
        // mutable リストには 0x02 が無いが、IMMUTABLE 内から見つける (MUST search both)
        assert_eq!(props.find_varint(0x02), Some(250));
        assert_eq!(props.object_delivery_timeout(), Some(250));
    }

    #[test]
    fn find_varint_searches_inside_immutable_after_roundtrip() {
        let inner = encode_inner(&[(0x02, TrackPropertyValue::VarInt(250))]);
        let props = with_immutable(inner);
        let mut buf = Vec::new();
        props
            .encode(&mut buf)
            .expect("IMMUTABLE を含む TrackProperties は encode 可能");
        let decoded = TrackProperties::decode(&buf).expect("encode した IMMUTABLE は decode 可能");
        assert_eq!(decoded.find_varint(0x02), Some(250));
    }

    #[test]
    fn mutable_preferred_over_immutable() {
        // 同一 prop_type が mutable と IMMUTABLE 内に跨って存在する場合、mutable を優先する
        let inner = encode_inner(&[(0x02, TrackPropertyValue::VarInt(250))]);
        let mut props = with_immutable(inner);
        props.push(TrackProperty {
            prop_type: 0x02,
            value: TrackPropertyValue::VarInt(100),
        });
        assert_eq!(props.find_varint(0x02), Some(100));
    }

    #[test]
    fn has_unknown_mandatory_searches_inside_immutable() {
        // 必須トラックプロパティ範囲 (0x4000) を内側に持つ IMMUTABLE
        let inner = encode_inner(&[(0x4000, TrackPropertyValue::VarInt(1))]);
        let props = with_immutable(inner);
        assert!(props.has_unknown_mandatory());
    }

    /// IMMUTABLE_PROPERTIES 内側の GREASE 値は unknown mandatory にしない
    ///
    /// draft-ietf-moq-transport-21 §10.7 (Immutable Properties) の「MUST search both」で
    /// 内側も探索するが、§16.8 (Properties) の Table 14 が GREASE の Property Type
    /// (`0x7f * N + 0x9D`) を Scope Any として予約しているため、内側でも unknown mandatory
    /// として扱わない。
    #[test]
    fn grease_inside_immutable_not_detected() {
        assert!(
            shiguredo_moqt::grease::is_grease(0x401D),
            "テストフィクスチャの前提条件を満たす"
        );
        let inner = encode_inner(&[(0x401D, TrackPropertyValue::Bytes(vec![0xAB]))]);
        let props = with_immutable(inner);
        assert!(
            !props.has_unknown_mandatory(),
            "IMMUTABLE 内側の GREASE 値を unknown mandatory として扱わないこと"
        );
    }

    #[test]
    fn mandatory_range_allowed_inside_immutable() {
        // Track scope では IMMUTABLE 内の 0x4000-0x7FFF を拒否しない (Object scope と非対称)
        let inner = encode_inner(&[(0x4000, TrackPropertyValue::VarInt(1))]);
        let props = with_immutable(inner);
        let mut buf = Vec::new();
        props
            .encode(&mut buf)
            .expect("内側の必須プロパティは拒否しない");
        let decoded = TrackProperties::decode(&buf).expect("内側の必須プロパティは拒否しない");
        assert!(decoded.has_unknown_mandatory());
    }

    #[test]
    fn find_bytes_returns_raw_immutable_bytes() {
        // find_bytes(0x0B) は mutable リスト内の IMMUTABLE 生バイト列をそのまま返す (拡張しない)
        let inner = encode_inner(&[(0x02, TrackPropertyValue::VarInt(250))]);
        let props = with_immutable(inner.clone());
        assert!(props.as_slice().iter().any(|p| {
            p.prop_type == PROP_IMMUTABLE_PROPERTIES
                && p.value == TrackPropertyValue::Bytes(inner.clone())
        }));
    }

    #[test]
    fn inner_value_too_long_rejected_on_decode() {
        // 内側に奇数型 len=65536 (>65535) を主張する生バイト列。
        // 外側 0x0B (len=4) の内側 = [delta=1, len=65536(vi64 0xC1,0x00,0x00)]
        let buf = vec![0x0B, 0x04, 0x01, 0xC1, 0x00, 0x00];
        assert!(matches!(
            TrackProperties::decode(&buf),
            Err(MessageError::ProtocolViolation(_))
        ));
    }

    #[test]
    fn find_varint_returns_none_for_corrupt_pushed_immutable() {
        // push で破損した IMMUTABLE バイト列を積んだ場合、統合検索は panic せず None を返す
        // (内側デコード失敗時は探索を打ち切る)。
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: PROP_IMMUTABLE_PROPERTIES,
            // delta=1 (奇数型) len=255 だが実体が 1 バイトしかない破損データ
            value: TrackPropertyValue::Bytes(vec![0x01, 0xFF]),
        });
        assert_eq!(props.find_varint(0x02), None);
        assert!(!props.has_unknown_mandatory());
    }
}
