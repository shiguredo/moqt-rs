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
    fn unknown_type_bytes_value_length_exceeds_buffer_is_key_value_formatting_error() {
        // delta=1 (未知の奇数 Type), Length=10 に対して実体が 3 バイトしかない。
        // §8.3 (Key-Value-Pair Structure) の MUST は既知 Type が条件だが、未知 Type も
        // §16.8 (Properties) の length による skip が成立しないため、同じ分類の
        // MALFORMED_KEY_VALUE (0x6) で拒否する (本実装の方針)
        let buf = vec![0x01, 0x0A, 0x01, 0x02, 0x03];
        assert_eq!(
            TrackProperties::decode(&buf),
            Err(MessageError::MalformedKeyValue(
                "track property value length exceeds remaining buffer"
            ))
        );
    }

    #[test]
    fn length_varint_truncation_is_unexpected_eof() {
        // 奇数 Type の Length が 2 バイト必要な vi64 だが 1 バイトしかない場合は
        // フレーミングの切り詰めとして UnexpectedEof のまま
        assert_eq!(
            TrackProperties::decode(&[0x01, 0x80]),
            Err(MessageError::UnexpectedEof)
        );
    }

    #[test]
    fn length_equal_to_remaining_bytes_is_accepted() {
        // Length が残りバイトとちょうど一致する場合は正常に受理される
        let buf = vec![0x01, 0x03, 0xAA, 0xBB, 0xCC];
        let props = TrackProperties::decode(&buf).expect("宣言長ちょうどの値は受理されること");
        assert_eq!(
            props.as_slice(),
            &[TrackProperty {
                prop_type: 0x01,
                value: TrackPropertyValue::Bytes(vec![0xAA, 0xBB, 0xCC]),
            }]
        );
    }

    #[test]
    fn value_length_65535_with_no_remaining_bytes_is_key_value_formatting_error() {
        // 2^16-1 は上限内であるため ProtocolViolation ではなく、残りバイト超過として
        // MalformedKeyValue になる (65536 は ProtocolViolation のまま)
        let buf = vec![0x01, 0xC0, 0xFF, 0xFF];
        assert_eq!(
            TrackProperties::decode(&buf),
            Err(MessageError::MalformedKeyValue(
                "track property value length exceeds remaining buffer"
            ))
        );
    }

    #[test]
    fn malformed_second_kvp_is_key_value_formatting_error() {
        // 正常な 1 個目の後に宣言長超過の 2 個目が来ても、2 個目で同じ分類になる
        let buf = vec![0x02, 0x01, 0x01, 0x0A, 0x01, 0x02, 0x03];
        assert_eq!(
            TrackProperties::decode(&buf),
            Err(MessageError::MalformedKeyValue(
                "track property value length exceeds remaining buffer"
            ))
        );
    }

    #[test]
    fn known_type_bytes_value_length_exceeds_buffer_is_key_value_formatting_error() {
        // 既知の奇数 Type である IMMUTABLE_PROPERTIES (0x0B) が Length=5 を宣言するが
        // 実体が 1 バイトしかない場合も同じ分類になること
        let buf = vec![0x0B, 0x05, 0x01];
        assert_eq!(
            TrackProperties::decode(&buf),
            Err(MessageError::MalformedKeyValue(
                "track property value length exceeds remaining buffer"
            ))
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
            Err(MessageError::FieldLengthExceeded(_))
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

    /// 単一に限られる既知 Property が重複したときの理由
    const DUPLICATE: &str = "duplicate track property";

    #[test]
    fn decode_keeps_delta_zero_duplicate_and_validate_reports_malformed_track() {
        // type=14 (0x0E, delta=14), value=1, その後 delta=0 (同じ 0x0E) で value=2。
        // 構造 decode は重複を保持し、意味検証が malformed Track として報告する
        // (draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1
        // (Malformed Tracks) 条件 9)。
        let buf = vec![0x0E, 0x01, 0x00, 0x02];
        let decoded =
            TrackProperties::decode(&buf).expect("重複した Type は構造 decode を失敗させないこと");
        assert_eq!(decoded.len(), 2, "重複したインスタンスを保持すること");
        assert_eq!(decoded.as_slice()[0].prop_type, 0x0E);
        assert_eq!(decoded.as_slice()[1].prop_type, 0x0E);
        assert_eq!(
            decoded.validate_multiplicity(),
            Err(MessageError::MalformedTrack(DUPLICATE))
        );
        assert_eq!(
            decoded.validate(),
            Err(MessageError::MalformedTrack(DUPLICATE))
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
        assert_eq!(
            decoded.validate(),
            Ok(()),
            "型 0 は未知型であり、単一インスタンスなら多重違反にならないこと"
        );
    }

    #[test]
    fn encode_rejects_duplicate_known_prop_type() {
        // 単一に限られる既知 prop_type (OBJECT_DELIVERY_TIMEOUT) を 2 回 push して encode
        // すると、ローカル入力の検証として ProtocolViolation を返す
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
            Err(MessageError::ProtocolViolation(DUPLICATE))
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

    /// 未知の Mandatory Track Property の型を昇順・重複なしで列挙する
    ///
    /// draft-ietf-moq-transport-23 §9.4.2.2 (UNSUPPORTED_PROPERTY) は、理解できない
    /// Mandatory Track Property ごとに 1 つの Error Detail を送ることを求める (SHOULD)。
    /// 列挙には既知プロパティ (0x0E) と GREASE 予約値 (0x401D) を含めず、型の昇順に並べる。
    #[test]
    fn unknown_mandatory_types_lists_sorted_unique_types() {
        let mut props = TrackProperties::new();
        // 既知プロパティは未対応として報告しない
        props.push(TrackProperty {
            prop_type: 0x0E,
            value: TrackPropertyValue::VarInt(128),
        });
        // GREASE 予約値も未対応として報告しない
        props.push(TrackProperty {
            prop_type: 0x401D,
            value: TrackPropertyValue::Bytes(vec![0xAB]),
        });
        // 未知の必須プロパティを昇順でない順に積む
        props.push(TrackProperty {
            prop_type: 0x7FFF,
            value: TrackPropertyValue::VarInt(1),
        });
        props.push(TrackProperty {
            prop_type: 0x4000,
            value: TrackPropertyValue::VarInt(2),
        });
        props.push(TrackProperty {
            prop_type: 0x5001,
            value: TrackPropertyValue::Bytes(vec![0xCD]),
        });
        assert_eq!(
            props.unknown_mandatory_types(),
            vec![0x4000, 0x5001, 0x7FFF],
            "未知の必須プロパティの型だけを昇順で列挙すること"
        );
        assert!(props.has_unknown_mandatory());
    }

    /// 未知の必須プロパティが無ければ列挙は空になる
    #[test]
    fn unknown_mandatory_types_empty_when_no_unknown() {
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: 0x02,
            value: TrackPropertyValue::VarInt(100),
        });
        // GREASE 値 (0x401D) は 0x4000-0x7FFF に入るが未知の必須プロパティではない
        props.push(TrackProperty {
            prop_type: 0x401D,
            value: TrackPropertyValue::Bytes(vec![0xAB]),
        });
        assert!(
            props.unknown_mandatory_types().is_empty(),
            "既知プロパティと GREASE 値だけなら列挙は空であること"
        );
        assert!(!props.has_unknown_mandatory());
    }

    /// 未知の必須プロパティを 1 つも持たない空の TrackProperties では列挙が空になる
    #[test]
    fn unknown_mandatory_types_empty_for_empty_properties() {
        let props = TrackProperties::new();
        assert!(props.unknown_mandatory_types().is_empty());
    }

    /// GREASE の Track Property は 0x4000-0x7FFF に入っていても unknown mandatory にしない
    ///
    /// draft-ietf-moq-transport-23 §16.8 (Properties) の Table 15 は GREASE の Property Type
    /// (`0x7f * N + 0x9D`) を Scope Any として予約しており、N = 128 の 0x401D から N = 256 の
    /// 0x7F9D までは §3.7 (Mandatory to Understand Track Properties) の 0x4000-0x7FFF に入る。GREASE 値は
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
                // (draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure))
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
    /// bool 判定 (`has_unknown_mandatory`) と型の列挙 (`unknown_mandatory_types`) の
    /// 両方が同じ規則に従うことを確認する。
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
            let is_grease = shiguredo_moqt::grease::is_grease(prop_type);
            assert_eq!(
                props.has_unknown_mandatory(),
                !is_grease,
                "{prop_type:#x} の判定が GREASE 除外と一致すること"
            );
            let expected: Vec<u64> = if is_grease {
                Vec::new()
            } else {
                vec![prop_type]
            };
            assert_eq!(
                props.unknown_mandatory_types(),
                expected,
                "{prop_type:#x} の列挙が GREASE 除外と一致すること"
            );
        }
    }

    /// GREASE の Track Property は encode / decode を通しても unknown mandatory にならない
    ///
    /// 奇数型 (長さ付きバイト列) と偶数型 (varint) の両方で wire 表現を往復させる
    /// (draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure) / §16.8 (Properties) の
    /// Table 15)。
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

/// draft-ietf-moq-transport-23 §10.7 (Immutable Properties): IMMUTABLE_PROPERTIES (0x0B) の Track scope 対応。
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
    fn inner_length_exceeding_buffer_is_key_value_formatting_error() {
        // 内側が奇数型 len=5 を主張するが 1 バイトしかない (宣言長超過)
        let buf = vec![0x0B, 0x03, 0x01, 0x05, 0x01];
        assert_eq!(
            TrackProperties::decode(&buf),
            Err(MessageError::MalformedKeyValue(
                "track property value length exceeds remaining buffer"
            ))
        );
    }

    #[test]
    fn inner_value_range_violation_rejected_on_decode() {
        // 内側に DYNAMIC_GROUPS (0x30) = 2 (>1) を含む生バイト列。
        // 値域外の受信は draft-ietf-moq-transport-23 §10.6 の MUST により MALFORMED_KEY_VALUE
        let buf = vec![0x0B, 0x02, 0x30, 0x02];
        assert!(matches!(
            TrackProperties::decode(&buf),
            Err(MessageError::MalformedKeyValue(_))
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
    /// draft-ietf-moq-transport-23 §10.7 (Immutable Properties) の「MUST search both」で
    /// 内側も探索するが、§16.8 (Properties) の Table 15 が GREASE の Property Type
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

    /// IMMUTABLE_PROPERTIES の内側にある未知の必須プロパティも型として列挙する
    ///
    /// draft-ietf-moq-transport-23 §10.7 (Immutable Properties) の "MUST search both" に
    /// 従い、mutable リストと内側の両方を探索する。内側の GREASE 値 (0x401D) と既知
    /// プロパティ (0x0E) は列挙に含めない。
    #[test]
    fn unknown_mandatory_types_searches_inside_immutable() {
        let inner = encode_inner(&[
            (0x6000, TrackPropertyValue::VarInt(1)),
            (0x401D, TrackPropertyValue::Bytes(vec![0xAB])),
            (0x0E, TrackPropertyValue::VarInt(128)),
        ]);
        let mut props = with_immutable(inner);
        props.push(TrackProperty {
            prop_type: 0x5000,
            value: TrackPropertyValue::VarInt(2),
        });
        assert_eq!(
            props.unknown_mandatory_types(),
            vec![0x5000, 0x6000],
            "mutable と IMMUTABLE 内側の未知必須型を合わせて昇順で列挙すること"
        );
    }

    /// mutable と IMMUTABLE 内側に同じ未知必須型があっても列挙は 1 回だけになる
    ///
    /// draft-ietf-moq-transport-23 §9.4.2.2 (UNSUPPORTED_PROPERTY) は 1 型につき 1 Detail を
    /// 送ることを求める (SHOULD) ため、同じ型を重複して列挙しない。
    #[test]
    fn unknown_mandatory_types_deduplicates_across_mutable_and_immutable() {
        let inner = encode_inner(&[(0x4000, TrackPropertyValue::VarInt(1))]);
        let mut props = with_immutable(inner);
        props.push(TrackProperty {
            prop_type: 0x4000,
            value: TrackPropertyValue::VarInt(2),
        });
        assert_eq!(
            props.unknown_mandatory_types(),
            vec![0x4000],
            "同じ型は 1 回だけ列挙すること"
        );
    }

    /// IMMUTABLE_PROPERTIES を含む TrackProperties を往復しても列挙が変わらない
    #[test]
    fn unknown_mandatory_types_survives_roundtrip() {
        let inner = encode_inner(&[(0x4000, TrackPropertyValue::VarInt(1))]);
        let props = with_immutable(inner);
        let mut buf = Vec::new();
        props
            .encode(&mut buf)
            .expect("IMMUTABLE を含む TrackProperties は encode 可能");
        let decoded = TrackProperties::decode(&buf).expect("encode した IMMUTABLE は decode 可能");
        assert_eq!(
            decoded.unknown_mandatory_types(),
            vec![0x4000],
            "wire 経由でも内側の未知必須型を列挙できること"
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
    fn encode_rejects_known_type_range_violation() {
        use shiguredo_moqt::track_properties::PROP_DYNAMIC_GROUPS;
        // encode は既知 Type の値域も検証し、違反は MALFORMED_KEY_VALUE の分類になる
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: PROP_DYNAMIC_GROUPS,
            value: TrackPropertyValue::VarInt(2),
        });
        let mut buf = Vec::new();
        assert!(matches!(
            props.encode(&mut buf),
            Err(MessageError::MalformedKeyValue(
                "DYNAMIC_GROUPS must be 0 or 1"
            ))
        ));
    }

    #[test]
    fn encode_passes_through_inner_varint_truncation_as_unexpected_eof() {
        // 内側 KVP の Length varint が途中終端する場合、encode はローカル入力の
        // 切り詰めとして UnexpectedEof をそのまま返す (# Errors の記載どおり)
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: PROP_IMMUTABLE_PROPERTIES,
            // 内側: delta=1 (奇数型) の Length varint が 2 バイト必要だが 1 バイト
            value: TrackPropertyValue::Bytes(vec![0x01, 0x80]),
        });
        let mut buf = Vec::new();
        assert_eq!(props.encode(&mut buf), Err(MessageError::UnexpectedEof));
    }

    #[test]
    fn encode_maps_inner_length_exceeding_buffer_to_protocol_violation() {
        // encode はローカル入力の検証であるため、内側 KVP の宣言長超過
        // (decode 層では MalformedKeyValue) を ProtocolViolation に写す
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: PROP_IMMUTABLE_PROPERTIES,
            // 内側: delta=1 (奇数型), Length=5 だが実体は 1 バイト
            value: TrackPropertyValue::Bytes(vec![0x01, 0x05, 0x01]),
        });
        let mut buf = Vec::new();
        assert!(matches!(
            props.encode(&mut buf),
            Err(MessageError::ProtocolViolation(
                "immutable properties inner KVP length exceeds remaining buffer"
            ))
        ));
    }

    #[test]
    fn inner_value_too_long_rejected_on_decode() {
        // 内側に奇数型 len=65536 (>65535) を主張する生バイト列。
        // 外側 0x0B (len=4) の内側 = [delta=1, len=65536(vi64 0xC1,0x00,0x00)]
        let buf = vec![0x0B, 0x04, 0x01, 0xC1, 0x00, 0x00];
        assert!(matches!(
            TrackProperties::decode(&buf),
            Err(MessageError::FieldLengthExceeded(_))
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

/// Property のスコープ違反 (draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) /
/// §12.1 (Malformed Tracks) 条件 8) の検証。
///
/// Track Properties のスコープ検証は構造 decode と分離する。`TrackProperties::decode` は
/// スコープ違反だけを理由に失敗せず、`TrackProperties::validate_scope` が受信 Track の判定を
/// 行う。ローカル入力の `encode` は受信 Track の取消と混同しないよう `ProtocolViolation` で
/// 拒否する。
mod property_scope {
    use super::*;
    use shiguredo_moqt::object_properties::{PROP_PRIOR_GROUP_ID_GAP, PROP_PRIOR_OBJECT_ID_GAP};
    use shiguredo_moqt::track_properties::PROP_IMMUTABLE_PROPERTIES;

    /// 偶数型 Property の KVP 生バイト列 (先頭の delta は型そのもの)
    fn even_kvp(prop_type: u64, value: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        shiguredo_moqt::varint::encode(prop_type, &mut bytes);
        shiguredo_moqt::varint::encode(value, &mut bytes);
        bytes
    }

    /// IMMUTABLE_PROPERTIES の KVP 生バイト列 (内側の KVP 列を値に持つ)
    fn immutable_kvp(inner: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        shiguredo_moqt::varint::encode(PROP_IMMUTABLE_PROPERTIES, &mut bytes);
        shiguredo_moqt::varint::encode(inner.len() as u64, &mut bytes);
        bytes.extend_from_slice(inner);
        bytes
    }

    /// Object 専用の既知 Property を Track scope で検出したときの理由
    const OBJECT_ONLY: &str = "Object-only property in Track scope";

    /// 構造 decode はスコープ違反を理由に失敗せず、`validate_scope` が検出する
    ///
    /// decode 層は Track / request を特定できず §12.1 の cancel を発行できないため、
    /// スコープ検証は意味の検証として分離する。
    #[test]
    fn decode_keeps_object_only_property_and_validate_scope_detects_it() {
        let decoded = TrackProperties::decode(&even_kvp(PROP_PRIOR_GROUP_ID_GAP, 1))
            .expect("スコープ違反は構造 decode を失敗させないこと");
        assert_eq!(decoded.len(), 1, "Property は保持されること");
        assert_eq!(
            decoded.validate_scope(),
            Err(MessageError::MalformedTrack(OBJECT_ONLY))
        );
    }

    /// この実装が意味を理解する Object 専用型をすべて検出する
    ///
    /// draft-ietf-moq-transport-23 §16.8 (Properties) Table 15 の PRIOR_GROUP_ID_GAP /
    /// PRIOR_OBJECT_ID_GAP と、Table 16 (draft-ietf-moq-loc-05 §6.1) の TIMESTAMP /
    /// VIDEO_FRAME_MARKING / AUDIO_LEVEL が対象である。
    #[test]
    fn validate_scope_detects_object_only_types() {
        let cases = [
            (PROP_PRIOR_GROUP_ID_GAP, TrackPropertyValue::VarInt(1)),
            (PROP_PRIOR_OBJECT_ID_GAP, TrackPropertyValue::VarInt(1)),
            (0x10, TrackPropertyValue::VarInt(1)), // TIMESTAMP (偶数型)
            (0x09, TrackPropertyValue::Bytes(vec![0xAB])), // VIDEO_FRAME_MARKING (奇数型)
            (0x0C, TrackPropertyValue::VarInt(1)), // AUDIO_LEVEL (偶数型)
        ];
        for (prop_type, value) in cases {
            let mut props = TrackProperties::new();
            props.push(TrackProperty { prop_type, value });
            assert_eq!(
                props.validate_scope(),
                Err(MessageError::MalformedTrack(OBJECT_ONLY)),
                "{prop_type:#x} を Object 専用 Property として検出すること"
            );
        }
    }

    /// IMMUTABLE_PROPERTIES の内側も同じスコープ規則で検証する
    ///
    /// draft-ietf-moq-transport-23 §10.7 (Immutable Properties): "Properties MAY appear either
    /// in the mutable property list or inside Immutable Properties."
    #[test]
    fn validate_scope_searches_inside_immutable() {
        let inner = even_kvp(PROP_PRIOR_GROUP_ID_GAP, 1);
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: PROP_IMMUTABLE_PROPERTIES,
            value: TrackPropertyValue::Bytes(inner.clone()),
        });
        assert_eq!(
            props.validate_scope(),
            Err(MessageError::MalformedTrack(OBJECT_ONLY))
        );
        // wire を経由しても内側の違反を検出する
        let decoded = TrackProperties::decode(&immutable_kvp(&inner))
            .expect("内側のスコープ違反は構造 decode を失敗させないこと");
        assert_eq!(
            decoded.validate_scope(),
            Err(MessageError::MalformedTrack(OBJECT_ONLY))
        );
    }

    /// 未知型・GREASE・両スコープの型 (IMMUTABLE 内側を含む) は受理される
    ///
    /// draft-ietf-moq-transport-23 §8.4 は未知 Property の転送を MUST とし、§16.8 は
    /// "Endpoints MUST ignore unknown Property types" と定める。スコープを推測して
    /// 拒否しないことを固定する。
    #[test]
    fn validate_scope_accepts_track_and_both_scope_and_unknown() {
        // 0x02 (Track,Object) は単一に限られるため、mutable リストに置いたら内側には置かない
        // (draft-ietf-moq-transport-23 §10.7 (Immutable Properties) が同じ型を両方に置けるのは
        // 複数の値を許す Property に限る)。内側には未知型と GREASE だけを置く。
        let mut inner = TrackProperties::new();
        inner.push(TrackProperty {
            prop_type: 0x1000, // 未知 (偶数型)
            value: TrackPropertyValue::VarInt(1),
        });
        inner.push(TrackProperty {
            prop_type: 0x401D, // GREASE (奇数型)
            value: TrackPropertyValue::Bytes(vec![0xAB]),
        });
        let mut inner_bytes = Vec::new();
        inner
            .encode(&mut inner_bytes)
            .expect("内側の KVP 列は encode できる");
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: 0x02,
            value: TrackPropertyValue::VarInt(200),
        });
        props.push(TrackProperty {
            prop_type: 0x04, // MAX_CACHE_DURATION (Track)
            value: TrackPropertyValue::VarInt(1_000),
        });
        props.push(TrackProperty {
            prop_type: 0x08, // TIMESCALE (Track,Object)
            value: TrackPropertyValue::VarInt(90_000),
        });
        props.push(TrackProperty {
            prop_type: 0x0E, // DEFAULT_PUBLISHER_PRIORITY (Track)
            value: TrackPropertyValue::VarInt(128),
        });
        props.push(TrackProperty {
            prop_type: 0x22, // DEFAULT_PUBLISHER_GROUP_ORDER (Track)
            value: TrackPropertyValue::VarInt(1),
        });
        props.push(TrackProperty {
            prop_type: 0x30, // DYNAMIC_GROUPS (Track)
            value: TrackPropertyValue::VarInt(1),
        });
        props.push(TrackProperty {
            prop_type: 0x1001, // 未知 (奇数型)
            value: TrackPropertyValue::Bytes(vec![0xCD]),
        });
        props.push(TrackProperty {
            prop_type: PROP_IMMUTABLE_PROPERTIES,
            value: TrackPropertyValue::Bytes(inner_bytes),
        });
        let mut buf = Vec::new();
        props
            .encode(&mut buf)
            .expect("スコープ違反の無い Track Properties は encode できる");
        let decoded = TrackProperties::decode(&buf)
            .expect("スコープ違反の無い Track Properties は decode できる");
        assert_eq!(
            decoded.validate_scope(),
            Ok(()),
            "未知型・GREASE・両スコープの型はスコープ違反にしないこと"
        );
    }

    /// ローカル encode の Object 専用 Property は入力検証エラーになる
    ///
    /// 受信 Track の malformed 判定 (`MalformedTrack`) とは区別し、`ProtocolViolation` で
    /// 拒否する (§12.1 の cancel を起こさない)。
    #[test]
    fn encode_rejects_object_only_property() {
        let cases = [
            (PROP_PRIOR_GROUP_ID_GAP, TrackPropertyValue::VarInt(1)),
            (PROP_PRIOR_OBJECT_ID_GAP, TrackPropertyValue::VarInt(1)),
            (0x10, TrackPropertyValue::VarInt(1)),
            (0x09, TrackPropertyValue::Bytes(vec![0xAB])),
            (0x0C, TrackPropertyValue::VarInt(1)),
        ];
        for (prop_type, value) in cases {
            let mut props = TrackProperties::new();
            props.push(TrackProperty { prop_type, value });
            let mut buf = Vec::new();
            assert_eq!(
                props.encode(&mut buf),
                Err(MessageError::ProtocolViolation(OBJECT_ONLY)),
                "{prop_type:#x} の encode を入力検証エラーにすること"
            );
        }
    }

    /// IMMUTABLE_PROPERTIES の内側の Object 専用 Property も encode で拒否する
    #[test]
    fn encode_rejects_object_only_property_inside_immutable() {
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: PROP_IMMUTABLE_PROPERTIES,
            value: TrackPropertyValue::Bytes(even_kvp(PROP_PRIOR_GROUP_ID_GAP, 1)),
        });
        let mut buf = Vec::new();
        assert_eq!(
            props.encode(&mut buf),
            Err(MessageError::ProtocolViolation(OBJECT_ONLY))
        );
    }
}

/// Property の多重可否 (draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) /
/// §12.1 (Malformed Tracks) 条件 9) の検証。
///
/// 未知の Type は §8.4 の "MUST allow multiple instances of the Property" により複数
/// インスタンスを保持して往復できる。単一に限られる既知 Type の重複は構造 decode では保持し、
/// `TrackProperties::validate` が受信 Track の malformed として報告する。ローカル入力の
/// `encode` は受信 Track の取消と混同しないよう `ProtocolViolation` で拒否する。
mod property_multiplicity {
    use super::*;
    use shiguredo_moqt::track_properties::PROP_IMMUTABLE_PROPERTIES;

    /// 単一に限られる既知 Property が重複したときの理由
    const DUPLICATE: &str = "duplicate track property";

    /// 偶数型 Property の KVP 生バイト列 (先頭の delta は型そのもの)
    fn even_kvp(prop_type: u64, value: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        shiguredo_moqt::varint::encode(prop_type, &mut bytes);
        shiguredo_moqt::varint::encode(value, &mut bytes);
        bytes
    }

    /// 同じ型を delta=0 で繰り返す偶数型 Property の KVP 生バイト列
    fn repeated_even_kvp(prop_type: u64, values: &[u64]) -> Vec<u8> {
        let mut bytes = Vec::new();
        shiguredo_moqt::varint::encode(prop_type, &mut bytes);
        for (index, value) in values.iter().enumerate() {
            if index > 0 {
                // delta=0 で直前と同じ型を表す
                shiguredo_moqt::varint::encode(0, &mut bytes);
            }
            shiguredo_moqt::varint::encode(*value, &mut bytes);
        }
        bytes
    }

    /// 未知の Type の同値・異値の複数インスタンスが lossless に往復すること
    ///
    /// encode は型の昇順に安定ソートするため、同一 Type 内の入力順が保たれる。
    #[test]
    fn unknown_duplicate_types_roundtrip_losslessly() {
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: 0x1000,
            value: TrackPropertyValue::VarInt(7),
        });
        props.push(TrackProperty {
            prop_type: 0x1000,
            value: TrackPropertyValue::VarInt(8),
        });
        props.push(TrackProperty {
            prop_type: 0x1001,
            value: TrackPropertyValue::Bytes(vec![0xAA]),
        });
        props.push(TrackProperty {
            prop_type: 0x1001,
            value: TrackPropertyValue::Bytes(vec![0xBB]),
        });
        props.push(TrackProperty {
            prop_type: 0x1001,
            value: TrackPropertyValue::Bytes(vec![0xBB]),
        });

        let mut buf = Vec::new();
        props
            .encode(&mut buf)
            .expect("未知 Type の重複は encode できること");
        let decoded = TrackProperties::decode(&buf).expect("未知 Type の重複は decode できること");
        assert_eq!(
            decoded, props,
            "未知 Type の同値・異値の複数インスタンスが入力順のまま往復すること"
        );
        assert_eq!(decoded.validate(), Ok(()), "多重違反にならないこと");
        assert_eq!(
            decoded.find_varint(0x1000),
            Some(7),
            "重複した未知 Type は最初のインスタンスを返すこと"
        );

        // 受信した直列化を再 encode しても同じインスタンス列に戻ること
        let mut reencoded = Vec::new();
        decoded
            .encode(&mut reencoded)
            .expect("decode した未知 Type の重複を再 encode できること");
        assert_eq!(
            TrackProperties::decode(&reencoded),
            Ok(props),
            "再 encode しても lossless であること"
        );
    }

    /// 既知 Type の重複は構造 decode が保持し、`validate` が MalformedTrack にする
    #[test]
    fn validate_reports_duplicate_known_type_as_malformed_track() {
        let buf = repeated_even_kvp(0x02, &[1, 2]);
        let decoded = TrackProperties::decode(&buf)
            .expect("重複した既知 Type は構造 decode を失敗させないこと");
        assert_eq!(decoded.len(), 2, "重複したインスタンスを保持すること");
        assert_eq!(
            decoded.validate_multiplicity(),
            Err(MessageError::MalformedTrack(DUPLICATE))
        );
    }

    /// IMMUTABLE_PROPERTIES の内側の既知 Type の重複も MalformedTrack になる
    #[test]
    fn validate_reports_duplicate_known_type_inside_immutable() {
        let inner = repeated_even_kvp(0x02, &[1, 2]);
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: PROP_IMMUTABLE_PROPERTIES,
            value: TrackPropertyValue::Bytes(inner.clone()),
        });
        assert_eq!(
            props.validate_multiplicity(),
            Err(MessageError::MalformedTrack(DUPLICATE))
        );
        // wire を経由しても同じ判定になる
        let mut bytes = Vec::new();
        shiguredo_moqt::varint::encode(PROP_IMMUTABLE_PROPERTIES, &mut bytes);
        shiguredo_moqt::varint::encode(inner.len() as u64, &mut bytes);
        bytes.extend_from_slice(&inner);
        let decoded =
            TrackProperties::decode(&bytes).expect("内側の重複は構造 decode を失敗させないこと");
        assert_eq!(
            decoded.validate(),
            Err(MessageError::MalformedTrack(DUPLICATE))
        );
    }

    /// mutable リストと IMMUTABLE_PROPERTIES の内側にまたがる既知 Type の重複も検出する
    ///
    /// draft-ietf-moq-transport-23 §10.7 (Immutable Properties) は、複数の値を許す Property に
    /// 限り同じ型を両方に置けると定める。単一の型を両方に置くと 2 インスタンスである。
    #[test]
    fn validate_reports_duplicate_across_mutable_and_immutable() {
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: 0x02,
            value: TrackPropertyValue::VarInt(1),
        });
        props.push(TrackProperty {
            prop_type: PROP_IMMUTABLE_PROPERTIES,
            value: TrackPropertyValue::Bytes(even_kvp(0x02, 2)),
        });
        assert_eq!(
            props.validate_multiplicity(),
            Err(MessageError::MalformedTrack(DUPLICATE))
        );
    }

    /// IMMUTABLE_PROPERTIES が複数あると MalformedTrack になる
    #[test]
    fn validate_reports_duplicate_immutable_properties() {
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: PROP_IMMUTABLE_PROPERTIES,
            value: TrackPropertyValue::Bytes(Vec::new()),
        });
        props.push(TrackProperty {
            prop_type: PROP_IMMUTABLE_PROPERTIES,
            value: TrackPropertyValue::Bytes(Vec::new()),
        });
        assert_eq!(
            props.validate_multiplicity(),
            Err(MessageError::MalformedTrack(DUPLICATE))
        );
    }

    /// IMMUTABLE_PROPERTIES の内側の未知 Type の重複を受理し、生バイト列を書き換えないこと
    #[test]
    fn unknown_duplicates_inside_immutable_are_accepted_and_bytes_are_kept() {
        let inner = repeated_even_kvp(0x1000, &[3, 3]);
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: 0x02,
            value: TrackPropertyValue::VarInt(1),
        });
        props.push(TrackProperty {
            prop_type: PROP_IMMUTABLE_PROPERTIES,
            value: TrackPropertyValue::Bytes(inner.clone()),
        });
        assert_eq!(
            props.validate(),
            Ok(()),
            "既知 Type は mutable に 1 つ、未知 Type は内側に複数あり、多重違反にならないこと"
        );

        let mut buf = Vec::new();
        props
            .encode(&mut buf)
            .expect("未知 Type の重複を含む IMMUTABLE は encode できること");
        let decoded = TrackProperties::decode(&buf)
            .expect("未知 Type の重複を含む IMMUTABLE は decode できること");
        assert_eq!(
            decoded
                .as_slice()
                .iter()
                .find(|p| p.prop_type == PROP_IMMUTABLE_PROPERTIES)
                .map(|p| &p.value),
            Some(&TrackPropertyValue::Bytes(inner.clone())),
            "受信した IMMUTABLE の生バイト列を書き換えないこと"
        );
        assert_eq!(decoded.validate(), Ok(()));
    }

    /// ローカル encode は IMMUTABLE をまたぐ既知 Type の重複を入力検証エラーにする
    #[test]
    fn encode_rejects_duplicate_known_type_across_immutable() {
        let mut props = TrackProperties::new();
        props.push(TrackProperty {
            prop_type: 0x0E,
            value: TrackPropertyValue::VarInt(128),
        });
        props.push(TrackProperty {
            prop_type: PROP_IMMUTABLE_PROPERTIES,
            value: TrackPropertyValue::Bytes(even_kvp(0x0E, 1)),
        });
        let mut buf = Vec::new();
        assert_eq!(
            props.encode(&mut buf),
            Err(MessageError::ProtocolViolation(DUPLICATE))
        );
    }
}
