//! COSE (RFC 9052 / RFC 9053) と COSE Key Thumbprint (RFC 9679) のテスト

use shiguredo_moqt::c4m::cbor::{self, Value};
use shiguredo_moqt::c4m::cose::{
    Algorithm, AlgorithmClass, C4M_DRAFT_HMAC_SHA256_ALGORITHM_ID, CoseEncodingOptions, CoseError,
    CoseMac0, CoseMessage, CoseSign1, Header, KeyId, TAG_COSE_MAC0, TAG_COSE_SIGN1, TAG_CWT,
};
use shiguredo_moqt::c4m::crypto::{
    COSE_KEY_ALG, COSE_KEY_BASE_IV, COSE_KEY_CURVE, COSE_KEY_KID, COSE_KEY_OPS, COSE_KEY_PRIVATE,
    COSE_KEY_TYPE, COSE_KEY_TYPE_EC2, COSE_KEY_TYPE_OKP, COSE_KEY_TYPE_SYMMETRIC, COSE_KEY_X,
    COSE_KEY_Y, CoseKey, EcCurve,
};

#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::crypto::{CryptoError, aws_lc_rs::AwsLcRsCrypto};

use super::helpers::{decode_hex, encode_hex};

/// protected ヘッダ (`alg` のみ) の CBOR バイト列を作る
fn protected_header(algorithm_identifier: i64) -> Vec<u8> {
    cbor::encode(&Value::Map(vec![(
        Value::Unsigned(1),
        Value::integer(algorithm_identifier),
    )]))
    .expect("エンコードできる")
}

/// COSE 構造の配列を作る
fn cose_array(protected: &[u8], payload: Option<&[u8]>, signature: &[u8]) -> Value {
    Value::Array(vec![
        Value::ByteString(protected.to_vec()),
        Value::Map(Vec::new()),
        match payload {
            Some(payload) => Value::ByteString(payload.to_vec()),
            None => Value::Null,
        },
        Value::ByteString(signature.to_vec()),
    ])
}

#[test]
fn sign1_signing_input_matches_rfc9052() {
    let message = CoseMessage::Sign1(CoseSign1 {
        protected: protected_header(-7),
        unprotected: Vec::new(),
        payload: Some(b"hello".to_vec()),
        signature: vec![0x01],
        cose_tagged: false,
        cwt_tagged: false,
    });
    assert_eq!(
        encode_hex(&message.signing_input().expect("署名対象を組み立てられる")),
        "846a5369676e61747572653143a10126404568656c6c6f"
    );
    assert_eq!(
        message.header().expect("ヘッダを読める").algorithm,
        Some(Algorithm::Es256)
    );
}

#[test]
fn mac0_signing_input_matches_rfc9052() {
    let message = CoseMessage::Mac0(CoseMac0 {
        protected: protected_header(5),
        unprotected: Vec::new(),
        payload: Some(b"hello".to_vec()),
        tag: vec![0x01],
        cose_tagged: false,
        cwt_tagged: false,
    });
    assert_eq!(
        encode_hex(&message.signing_input().expect("署名対象を組み立てられる")),
        "84644d41433043a10105404568656c6c6f"
    );
}

#[test]
fn cwt_and_cose_tags_are_preserved() {
    let protected = protected_header(-7);
    let array = cose_array(&protected, Some(b"payload"), b"signature");
    let tagged = Value::Tag(
        TAG_CWT,
        Box::new(Value::Tag(TAG_COSE_SIGN1, Box::new(array))),
    );
    let bytes = cbor::encode(&tagged).expect("エンコードできる");
    let message = CoseMessage::decode(&bytes).expect("デコードできる");
    match &message {
        CoseMessage::Sign1(sign1) => {
            assert!(sign1.cose_tagged);
            assert!(sign1.cwt_tagged);
            assert_eq!(sign1.protected, protected);
            assert_eq!(sign1.payload.as_deref(), Some(&b"payload"[..]));
            assert_eq!(sign1.signature, b"signature");
        }
        CoseMessage::Mac0(_) => panic!("COSE_Sign1 としてデコードされるべき"),
    }
    assert_eq!(
        message
            .encode(&CoseEncodingOptions::default())
            .expect("エンコードできる"),
        bytes
    );
}

#[test]
fn untagged_structures_are_classified_by_algorithm() {
    let sign1 = cbor::encode(&cose_array(&protected_header(-7), Some(b"p"), b"s"))
        .expect("エンコードできる");
    assert!(matches!(
        CoseMessage::decode(&sign1).expect("デコードできる"),
        CoseMessage::Sign1(_)
    ));

    let mac0 = cbor::encode(&cose_array(&protected_header(5), Some(b"p"), b"s"))
        .expect("エンコードできる");
    assert!(matches!(
        CoseMessage::decode(&mac0).expect("デコードできる"),
        CoseMessage::Mac0(_)
    ));

    // alg が無い場合は構造を判別できない
    let bytes = cbor::encode(&cose_array(&Vec::new(), Some(b"p"), b"s")).expect("エンコードできる");
    assert_eq!(
        CoseMessage::decode(&bytes),
        Err(CoseError::MissingAlgorithm)
    );
}

#[test]
fn tag_and_algorithm_class_must_match() {
    let sign1_array = cose_array(&protected_header(-7), Some(b"p"), b"s");
    let bytes =
        cbor::encode(&Value::Tag(TAG_COSE_MAC0, Box::new(sign1_array))).expect("エンコードできる");
    assert_eq!(
        CoseMessage::decode(&bytes),
        Err(CoseError::AlgorithmClassMismatch)
    );

    let mac0_array = cose_array(&protected_header(5), Some(b"p"), b"s");
    let bytes =
        cbor::encode(&Value::Tag(TAG_COSE_SIGN1, Box::new(mac0_array))).expect("エンコードできる");
    assert_eq!(
        CoseMessage::decode(&bytes),
        Err(CoseError::AlgorithmClassMismatch)
    );
}

#[test]
fn unsupported_algorithm_is_rejected() {
    let bytes = cbor::encode(&cose_array(&protected_header(42), Some(b"p"), b"s"))
        .expect("エンコードできる");
    assert_eq!(
        CoseMessage::decode(&bytes),
        Err(CoseError::UnsupportedAlgorithm(42))
    );
}

#[test]
fn c4m_draft_hmac_alias_is_accepted() {
    assert_eq!(
        Algorithm::from_identifier_with_c4m_draft_alias(C4M_DRAFT_HMAC_SHA256_ALGORITHM_ID),
        Some(Algorithm::HmacSha256)
    );
    assert_eq!(
        Algorithm::from_identifier(C4M_DRAFT_HMAC_SHA256_ALGORITHM_ID),
        None
    );
    let bytes = cbor::encode(&cose_array(
        &protected_header(C4M_DRAFT_HMAC_SHA256_ALGORITHM_ID),
        Some(b"p"),
        b"s",
    ))
    .expect("エンコードできる");
    let message = CoseMessage::decode(&bytes).expect("デコードできる");
    assert_eq!(
        message.header().expect("ヘッダを読める").algorithm,
        Some(Algorithm::HmacSha256)
    );
    // 生の識別子も保持する
    assert_eq!(
        message
            .header()
            .expect("ヘッダを読める")
            .algorithm_identifier,
        Some(C4M_DRAFT_HMAC_SHA256_ALGORITHM_ID)
    );
}

#[test]
fn crit_must_only_list_understood_parameters() {
    // 存在して理解できるラベルだけを列挙する
    let header = Value::Map(vec![
        (Value::Unsigned(1), Value::integer(5)),
        (Value::Unsigned(16), Value::TextString(String::from("CAT"))),
        (
            Value::Unsigned(2),
            Value::Array(vec![Value::Unsigned(1), Value::Unsigned(16)]),
        ),
    ]);
    let header = Header::decode_protected(&header).expect("protected ヘッダとして妥当である");
    assert_eq!(header.critical.len(), 2);

    // 理解できないラベルは拒否する
    let header = Value::Map(vec![
        (Value::Unsigned(1), Value::integer(5)),
        (Value::Unsigned(2), Value::Array(vec![Value::Unsigned(99)])),
        (Value::Unsigned(99), Value::Unsigned(1)),
    ]);
    assert_eq!(
        Header::decode_protected(&header),
        Err(CoseError::UnsupportedCriticalHeader)
    );

    // crit が指すラベルが protected ヘッダに無い場合は致命的エラー (RFC 9052 §3.1)
    let header = Value::Map(vec![
        (Value::Unsigned(1), Value::integer(5)),
        (Value::Unsigned(2), Value::Array(vec![Value::Unsigned(16)])),
    ]);
    assert_eq!(
        Header::decode_protected(&header),
        Err(CoseError::CriticalHeaderNotPresent)
    );

    // crit の配列は 1 要素以上でなければならない (RFC 9052 §3.1)
    let header = Value::Map(vec![
        (Value::Unsigned(1), Value::integer(5)),
        (Value::Unsigned(2), Value::Array(Vec::new())),
    ]);
    assert_eq!(
        Header::decode_protected(&header),
        Err(CoseError::EmptyCriticalHeader)
    );

    // crit は protected ヘッダに置かなければならない (RFC 9052 §3.1)
    let header = Value::Map(vec![
        (Value::Unsigned(1), Value::integer(5)),
        (Value::Unsigned(2), Value::Array(vec![Value::Unsigned(1)])),
    ]);
    assert_eq!(
        Header::decode_unprotected(&header),
        Err(CoseError::UnprotectedCriticalHeader)
    );
}

#[test]
fn unprotected_header_must_not_carry_alg_or_typ() {
    // alg は protected ヘッダで認証されなければならない (RFC 9052 §3.1)
    let protected = cbor::encode(&Value::Map(vec![(
        Value::Unsigned(16),
        Value::TextString(String::from("CAT")),
    )]))
    .expect("エンコードできる");
    let unprotected = vec![(Value::Unsigned(1), Value::integer(5))];
    let bytes = cbor::encode(&Value::Array(vec![
        Value::ByteString(protected.clone()),
        Value::Map(unprotected.clone()),
        Value::ByteString(b"payload".to_vec()),
        Value::ByteString(b"signature".to_vec()),
    ]))
    .expect("エンコードできる");
    assert_eq!(
        CoseMessage::decode(&bytes).map(|_| ()),
        Err(CoseError::UnprotectedAlgorithm)
    );

    // alg が両方にある場合はエラー
    let protected = protected_header(5);
    let bytes = cbor::encode(&Value::Array(vec![
        Value::ByteString(protected),
        Value::Map(unprotected),
        Value::ByteString(b"payload".to_vec()),
        Value::ByteString(b"signature".to_vec()),
    ]))
    .expect("エンコードできる");
    assert_eq!(
        CoseMessage::decode(&bytes).map(|_| ()),
        Err(CoseError::DuplicateHeaderParameter(1))
    );

    // typ は unprotected ヘッダに置けない (RFC 9596 §2)
    let protected = protected_header(5);
    let unprotected = vec![(Value::Unsigned(16), Value::TextString(String::from("CAT")))];
    let bytes = cbor::encode(&Value::Array(vec![
        Value::ByteString(protected),
        Value::Map(unprotected),
        Value::ByteString(b"payload".to_vec()),
        Value::ByteString(b"signature".to_vec()),
    ]))
    .expect("エンコードできる");
    assert_eq!(
        CoseMessage::decode(&bytes).map(|_| ()),
        Err(CoseError::UnprotectedTypeHeader)
    );
}

#[test]
fn cwt_tag_requires_a_cose_tag() {
    // RFC 8392 §6: CWT タグは COSE のタグ付きオブジェクトにだけ前置できる
    let array = cose_array(&protected_header(5), Some(b"p"), b"s");
    let bytes = cbor::encode(&Value::Tag(TAG_CWT, Box::new(array))).expect("エンコードできる");
    assert!(matches!(
        CoseMessage::decode(&bytes),
        Err(CoseError::InvalidStructure(_))
    ));

    // CWT タグだけを付けてエンコードすることもできない
    let message = CoseMessage::Mac0(CoseMac0 {
        protected: protected_header(5),
        unprotected: Vec::new(),
        payload: Some(b"p".to_vec()),
        tag: vec![0x01],
        cose_tagged: false,
        cwt_tagged: false,
    });
    assert!(matches!(
        message.encode(&CoseEncodingOptions {
            cose_tag: false,
            cwt_tag: true,
        }),
        Err(CoseError::InvalidStructure(_))
    ));
}

#[test]
fn header_round_trips_unknown_parameters() {
    let header = Header {
        algorithm: Some(Algorithm::HmacSha256),
        algorithm_identifier: Some(C4M_DRAFT_HMAC_SHA256_ALGORITHM_ID),
        key_id: Some(KeyId::Bytes(vec![1, 2, 3])),
        typ: Some(Value::TextString(String::from("CAT"))),
        content_type: Some(Value::Unsigned(0)),
        kccs: None,
        critical: Vec::new(),
        raw: vec![(Value::Unsigned(99), Value::TextString(String::from("x")))],
    };
    let encoded = header.encode().expect("エンコードできる");
    assert_eq!(
        Header::decode_protected(&encoded).expect("デコードできる"),
        header
    );
}

#[test]
fn key_id_accepts_bytes_and_text() {
    let header = Header::decode_protected(&Value::Map(vec![(
        Value::Unsigned(4),
        Value::TextString(String::from("key-1")),
    )]))
    .expect("デコードできる");
    assert_eq!(header.key_id, Some(KeyId::Text(String::from("key-1"))));
    assert_eq!(
        header.key_id.as_ref().expect("kid がある").as_bytes(),
        b"key-1"
    );

    let header = Header::decode_protected(&Value::Map(vec![(
        Value::Unsigned(4),
        Value::ByteString(vec![0xab]),
    )]))
    .expect("デコードできる");
    assert_eq!(header.key_id, Some(KeyId::Bytes(vec![0xab])));
}

#[test]
fn header_type_errors() {
    assert_eq!(
        Header::decode_protected(&Value::Unsigned(1)),
        Err(CoseError::UnexpectedType("header map"))
    );
    assert_eq!(
        Header::decode_protected(&Value::Map(vec![(
            Value::Unsigned(1),
            Value::TextString(String::from("ES256"))
        )]))
        .map(|_| ()),
        Err(CoseError::UnexpectedType("alg"))
    );
    assert_eq!(
        Header::decode_protected(&Value::Map(vec![(Value::Unsigned(4), Value::Unsigned(1))]))
            .map(|_| ()),
        Err(CoseError::UnexpectedType("kid"))
    );
}

#[test]
fn detached_payload_is_rejected() {
    let message = CoseMessage::Sign1(CoseSign1 {
        protected: protected_header(-7),
        unprotected: Vec::new(),
        payload: None,
        signature: vec![0x01],
        cose_tagged: false,
        cwt_tagged: false,
    });
    assert_eq!(
        message.signing_input(),
        Err(CoseError::DetachedPayloadUnsupported)
    );
    let bytes = message
        .encode(&CoseEncodingOptions::default())
        .expect("エンコードできる");
    match CoseMessage::decode(&bytes).expect("デコードできる") {
        CoseMessage::Sign1(sign1) => assert_eq!(sign1.payload, None),
        CoseMessage::Mac0(_) => panic!("COSE_Sign1 としてデコードされるべき"),
    }
}

#[test]
fn encoding_options_control_tags() {
    let message = CoseMessage::Sign1(CoseSign1 {
        protected: protected_header(-7),
        unprotected: Vec::new(),
        payload: Some(b"p".to_vec()),
        signature: vec![0x01],
        cose_tagged: false,
        cwt_tagged: false,
    });
    let untagged = message
        .encode(&CoseEncodingOptions {
            cose_tag: false,
            cwt_tag: false,
        })
        .expect("エンコードできる");
    assert_eq!(
        CoseMessage::decode(&untagged)
            .expect("デコードできる")
            .header()
            .expect("ヘッダを読める")
            .algorithm,
        Some(Algorithm::Es256)
    );
    let tagged = message
        .encode(&CoseEncodingOptions {
            cose_tag: true,
            cwt_tag: true,
        })
        .expect("エンコードできる");
    let (value, _) = cbor::decode_partial(&tagged).expect("デコードできる");
    match value {
        Value::Tag(TAG_CWT, inner) => match *inner {
            Value::Tag(TAG_COSE_SIGN1, _) => {}
            other => panic!("COSE_Sign1 タグではない: {other:?}"),
        },
        other => panic!("CWT タグではない: {other:?}"),
    }
}

#[test]
fn algorithm_properties() {
    assert_eq!(Algorithm::Es256.identifier(), -7);
    assert_eq!(Algorithm::Es384.identifier(), -35);
    assert_eq!(Algorithm::Es512.identifier(), -36);
    assert_eq!(Algorithm::EdDsa.identifier(), -8);
    assert_eq!(Algorithm::HmacSha256.identifier(), 5);
    assert_eq!(Algorithm::HmacSha384.identifier(), 6);
    assert_eq!(Algorithm::HmacSha512.identifier(), 7);
    assert!(Algorithm::HmacSha384.is_mac());
    assert!(!Algorithm::Es256.is_mac());
    assert_eq!(AlgorithmClass::Mac.context(), "MAC0");
    assert_eq!(AlgorithmClass::Signature.context(), "Signature1");
    assert_eq!(AlgorithmClass::Mac.tag(), TAG_COSE_MAC0);
    assert_eq!(AlgorithmClass::Signature.tag(), TAG_COSE_SIGN1);
    assert_eq!(Algorithm::from_jose_name("ES256"), Some(Algorithm::Es256));
    assert_eq!(Algorithm::from_jose_name("EdDSA"), Some(Algorithm::EdDsa));
    assert_eq!(
        Algorithm::from_jose_name("HS256"),
        Some(Algorithm::HmacSha256)
    );
    assert_eq!(Algorithm::from_jose_name("PS256"), None);
    assert_eq!(Algorithm::Es256.jose_name(), "ES256");
}

#[test]
fn invalid_cose_arrays_are_rejected() {
    let bytes = cbor::encode(&Value::Array(vec![Value::Unsigned(1)])).expect("エンコードできる");
    assert_eq!(
        CoseMessage::decode(&bytes),
        Err(CoseError::InvalidStructure(
            "COSE array must have 4 elements"
        ))
    );
    let bytes = cbor::encode(&Value::Array(vec![
        Value::ByteString(Vec::new()),
        Value::Map(Vec::new()),
        Value::ByteString(Vec::new()),
    ]))
    .expect("エンコードできる");
    assert_eq!(
        CoseMessage::decode(&bytes),
        Err(CoseError::InvalidStructure(
            "COSE array must have 4 elements"
        ))
    );
    // タグ付きの構造は alg が無くても構造としてはデコードできる
    // (CAT として使う場合は CatToken が alg の存在を要求する)
    let bytes = cbor::encode(&Value::Tag(
        TAG_COSE_SIGN1,
        Box::new(Value::Array(vec![
            Value::ByteString(Vec::new()),
            Value::Map(Vec::new()),
            Value::ByteString(Vec::new()),
            Value::ByteString(Vec::new()),
        ])),
    ))
    .expect("エンコードできる");
    let message = CoseMessage::decode(&bytes).expect("デコードできる");
    assert_eq!(message.header().expect("ヘッダを読める").algorithm, None);
}

#[test]
fn decode_from_known_bytes() {
    // 付録 A.3 の protected ヘッダ (alg = -4、typ = "CAT")
    let protected = decode_hex("a201231063434154");
    let header = Header::decode_protected(&cbor::decode(&protected).expect("デコードできる"))
        .expect("ヘッダを読める");
    assert_eq!(header.algorithm, Some(Algorithm::HmacSha256));
    assert_eq!(header.typ, Some(Value::TextString(String::from("CAT"))));
}

#[test]
fn duplicate_parameters_across_buckets_are_rejected() {
    let protected = Value::Map(vec![
        (Value::Unsigned(1), Value::integer(5)),
        (Value::Unsigned(4), Value::ByteString(vec![1])),
        (Value::Unsigned(3), Value::TextString(String::from("cat"))),
    ]);
    let protected_bytes = cbor::encode(&protected).expect("エンコードできる");

    // kid の重複
    let unprotected = vec![(Value::Unsigned(4), Value::ByteString(vec![2]))];
    let bytes = cbor::encode(&Value::Array(vec![
        Value::ByteString(protected_bytes.clone()),
        Value::Map(unprotected),
        Value::ByteString(b"p".to_vec()),
        Value::ByteString(b"s".to_vec()),
    ]))
    .expect("エンコードできる");
    assert_eq!(
        CoseMessage::decode(&bytes).map(|_| ()),
        Err(CoseError::DuplicateHeaderParameter(4))
    );

    // content type の重複
    let unprotected = vec![(Value::Unsigned(3), Value::TextString(String::from("cat")))];
    let bytes = cbor::encode(&Value::Array(vec![
        Value::ByteString(protected_bytes),
        Value::Map(unprotected),
        Value::ByteString(b"p".to_vec()),
        Value::ByteString(b"s".to_vec()),
    ]))
    .expect("エンコードできる");
    assert_eq!(
        CoseMessage::decode(&bytes).map(|_| ()),
        Err(CoseError::DuplicateHeaderParameter(3))
    );
}

#[test]
fn kid_and_content_type_can_come_from_the_unprotected_bucket() {
    let unprotected = vec![
        (Value::Unsigned(4), Value::TextString(String::from("key-1"))),
        (Value::Unsigned(3), Value::TextString(String::from("cat"))),
    ];
    let bytes = cbor::encode(&Value::Array(vec![
        Value::ByteString(protected_header(5)),
        Value::Map(unprotected),
        Value::ByteString(b"p".to_vec()),
        Value::ByteString(b"s".to_vec()),
    ]))
    .expect("エンコードできる");
    let message = CoseMessage::decode(&bytes).expect("デコードできる");
    let header = message.header().expect("ヘッダを読める");
    assert_eq!(header.key_id, Some(KeyId::Text(String::from("key-1"))));
    assert_eq!(
        header.content_type,
        Some(Value::TextString(String::from("cat")))
    );
}

#[test]
fn header_encode_checks_critical_labels() {
    let header = Header {
        algorithm: Some(Algorithm::HmacSha256),
        algorithm_identifier: Some(5),
        key_id: None,
        typ: None,
        content_type: None,
        kccs: None,
        critical: vec![Value::Unsigned(16)],
        raw: Vec::new(),
    };
    assert_eq!(header.encode(), Err(CoseError::CriticalHeaderNotPresent));
}

#[test]
fn header_encode_accepts_present_and_understood_critical_labels() {
    let header = Header {
        algorithm: Some(Algorithm::HmacSha256),
        algorithm_identifier: Some(5),
        key_id: None,
        typ: Some(Value::TextString(String::from("CAT"))),
        content_type: None,
        kccs: None,
        critical: vec![Value::Unsigned(1), Value::Unsigned(16)],
        raw: Vec::new(),
    };
    let encoded = header.encode().expect("エンコードできる");
    assert_eq!(
        Header::decode_protected(&encoded).expect("デコードできる"),
        header
    );
    // crit 自身の列挙は許す
    let header = Header {
        critical: vec![Value::Unsigned(2)],
        ..header
    };
    assert!(header.encode().is_ok());
    // 理解できないラベルは encode でも拒否する
    let header = Header {
        critical: vec![Value::Unsigned(99)],
        raw: vec![(Value::Unsigned(99), Value::Unsigned(1))],
        ..header
    };
    assert_eq!(header.encode(), Err(CoseError::UnsupportedCriticalHeader));
}

#[test]
fn message_accessors_are_exposed() {
    let message = CoseMessage::Sign1(CoseSign1 {
        protected: protected_header(-7),
        unprotected: Vec::new(),
        payload: Some(b"p".to_vec()),
        signature: vec![0x01, 0x02],
        cose_tagged: false,
        cwt_tagged: false,
    });
    assert_eq!(message.signature(), &[0x01, 0x02]);
    assert_eq!(message.payload(), Some(&b"p"[..]));
    assert!(message.signing_input().is_ok());
}

/// COSE_Key の CBOR マップを組み立てる
fn cose_key_map(entries: Vec<(i64, Value)>) -> Value {
    Value::Map(
        entries
            .into_iter()
            .map(|(label, value)| (Value::integer(label), value))
            .collect(),
    )
}

/// 公開鍵の COSE_Key が EC2 / OKP でラウンドトリップする
#[test]
fn cose_key_round_trips_ec2_and_okp() {
    // EC2 の秘密鍵付きの鍵は、公開鍵だけの COSE_Key へ変換する
    let key = CoseKey::ec2_with_private_key(
        EcCurve::P256,
        vec![0x01; 32],
        vec![0x02; 32],
        vec![0x03; 32],
    );
    let value = key.to_cose_key_map().expect("COSE_Key へ変換できる");
    let Value::Map(entries) = &value else {
        panic!("COSE_Key はマップ");
    };
    assert!(
        !entries
            .iter()
            .any(|(label, _)| label.as_int() == Some(COSE_KEY_PRIVATE)),
        "秘密鍵パラメータを出力した"
    );
    assert_eq!(
        CoseKey::from_cose_key_map(&value, None).expect("COSE_Key から復元できる"),
        CoseKey::ec2(EcCurve::P256, vec![0x01; 32], vec![0x02; 32])
    );

    // OKP (Ed25519) もラウンドトリップする
    let key = CoseKey::ed25519_with_private_key(vec![0x04; 32], vec![0x05; 32]);
    let value = key.to_cose_key_map().expect("COSE_Key へ変換できる");
    assert_eq!(
        CoseKey::from_cose_key_map(&value, None).expect("COSE_Key から復元できる"),
        CoseKey::ed25519(vec![0x04; 32])
    );

    // P-384 / P-521 は曲線識別子を保ってラウンドトリップする
    for curve in [EcCurve::P384, EcCurve::P521] {
        let length = curve.coordinate_length();
        let key = CoseKey::ec2(curve, vec![0x11; length], vec![0x22; length]);
        let value = key.to_cose_key_map().expect("COSE_Key へ変換できる");
        assert_eq!(
            CoseKey::from_cose_key_map(&value, None).expect("COSE_Key から復元できる"),
            key
        );
    }
}

/// COSE_Key の秘密鍵・未知パラメータ・対称鍵を拒否する
#[test]
fn cose_key_rejects_private_unknown_and_symmetric_parameters() {
    // 秘密鍵パラメータ (d = -4) を含む COSE_Key
    let value = cose_key_map(vec![
        (COSE_KEY_TYPE, Value::integer(COSE_KEY_TYPE_EC2)),
        (COSE_KEY_CURVE, Value::integer(1)),
        (COSE_KEY_X, Value::ByteString(vec![0x01; 32])),
        (COSE_KEY_Y, Value::ByteString(vec![0x02; 32])),
        (COSE_KEY_PRIVATE, Value::ByteString(vec![0x03; 32])),
    ]);
    assert_eq!(
        CoseKey::from_cose_key_map(&value, None),
        Err(CoseError::PrivateKeyParameter(COSE_KEY_PRIVATE))
    );

    // kid (2) / Base IV (5) / 未知ラベルは公開鍵の復元に不要なため無視する
    let value = cose_key_map(vec![
        (COSE_KEY_TYPE, Value::integer(COSE_KEY_TYPE_EC2)),
        (COSE_KEY_CURVE, Value::integer(1)),
        (COSE_KEY_X, Value::ByteString(vec![0x01; 32])),
        (COSE_KEY_Y, Value::ByteString(vec![0x02; 32])),
        (COSE_KEY_KID, Value::ByteString(b"key-1".to_vec())),
        (COSE_KEY_BASE_IV, Value::ByteString(vec![0x03; 16])),
        (99, Value::TextString(String::from("ignored"))),
    ]);
    assert_eq!(
        CoseKey::from_cose_key_map(&value, None).expect("任意属性を無視して復元できる"),
        CoseKey::ec2(EcCurve::P256, vec![0x01; 32], vec![0x02; 32])
    );

    // alg (3) が期待と一致しない COSE_Key は拒否する
    let value = cose_key_map(vec![
        (COSE_KEY_TYPE, Value::integer(COSE_KEY_TYPE_EC2)),
        (COSE_KEY_CURVE, Value::integer(1)),
        (COSE_KEY_X, Value::ByteString(vec![0x01; 32])),
        (COSE_KEY_Y, Value::ByteString(vec![0x02; 32])),
        (COSE_KEY_ALG, Value::integer(-35)),
    ]);
    assert_eq!(
        CoseKey::from_cose_key_map(&value, Some(Algorithm::Es256)),
        Err(CoseError::KeyAlgorithmMismatch {
            key: Algorithm::Es384,
            expected: Algorithm::Es256,
        })
    );

    // key_ops (4) に verify が無い COSE_Key は拒否する
    let value = cose_key_map(vec![
        (COSE_KEY_TYPE, Value::integer(COSE_KEY_TYPE_EC2)),
        (COSE_KEY_CURVE, Value::integer(1)),
        (COSE_KEY_X, Value::ByteString(vec![0x01; 32])),
        (COSE_KEY_Y, Value::ByteString(vec![0x02; 32])),
        (
            COSE_KEY_OPS,
            Value::Array(vec![Value::TextString(String::from("sign"))]),
        ),
    ]);
    assert_eq!(
        CoseKey::from_cose_key_map(&value, None),
        Err(CoseError::KeyOperationNotAllowed("verify"))
    );

    // 対称鍵 (kty = 4)
    let value = cose_key_map(vec![
        (COSE_KEY_TYPE, Value::integer(COSE_KEY_TYPE_SYMMETRIC)),
        (-1, Value::ByteString(vec![0x11; 32])),
    ]);
    assert_eq!(
        CoseKey::from_cose_key_map(&value, None),
        Err(CoseError::SymmetricKeyNotAllowed)
    );

    // 対応していない kty
    let value = cose_key_map(vec![(COSE_KEY_TYPE, Value::integer(3))]);
    assert_eq!(
        CoseKey::from_cose_key_map(&value, None),
        Err(CoseError::UnsupportedKeyType(3))
    );

    // 対応していない crv
    let value = cose_key_map(vec![
        (COSE_KEY_TYPE, Value::integer(COSE_KEY_TYPE_OKP)),
        (COSE_KEY_CURVE, Value::integer(1)),
        (COSE_KEY_X, Value::ByteString(vec![0x01; 32])),
    ]);
    assert_eq!(
        CoseKey::from_cose_key_map(&value, None),
        Err(CoseError::UnsupportedCurve(1))
    );

    // 必須パラメータの欠落
    let value = cose_key_map(vec![(COSE_KEY_TYPE, Value::integer(COSE_KEY_TYPE_EC2))]);
    assert_eq!(
        CoseKey::from_cose_key_map(&value, None),
        Err(CoseError::MissingKeyParameter("crv"))
    );

    // 座標の長さ違反
    let value = cose_key_map(vec![
        (COSE_KEY_TYPE, Value::integer(COSE_KEY_TYPE_EC2)),
        (COSE_KEY_CURVE, Value::integer(1)),
        (COSE_KEY_X, Value::ByteString(vec![0x01; 3])),
        (COSE_KEY_Y, Value::ByteString(vec![0x02; 32])),
    ]);
    assert_eq!(
        CoseKey::from_cose_key_map(&value, None),
        Err(CoseError::InvalidKeyLength("x"))
    );

    // 重複したパラメータ
    let value = cose_key_map(vec![
        (COSE_KEY_TYPE, Value::integer(COSE_KEY_TYPE_EC2)),
        (COSE_KEY_TYPE, Value::integer(COSE_KEY_TYPE_EC2)),
        (COSE_KEY_CURVE, Value::integer(1)),
        (COSE_KEY_X, Value::ByteString(vec![0x01; 32])),
        (COSE_KEY_Y, Value::ByteString(vec![0x02; 32])),
    ]);
    assert_eq!(
        CoseKey::from_cose_key_map(&value, None),
        Err(CoseError::DuplicateKeyParameter(COSE_KEY_TYPE))
    );

    // 対称鍵は COSE_Key へ変換しない
    assert_eq!(
        CoseKey::symmetric(vec![0x11; 32]).to_cose_key_map(),
        Err(CoseError::SymmetricKeyNotAllowed)
    );
}

/// COSE_Key の tstr / int の両方の値表現を受理すること
#[test]
fn cose_key_accepts_registry_names_and_integer_operations() {
    // kty / crv / alg をレジストリ名で書いても受理する (RFC 9052 §7)
    let value = cose_key_map(vec![
        (COSE_KEY_TYPE, Value::TextString(String::from("EC2"))),
        (COSE_KEY_CURVE, Value::TextString(String::from("P-256"))),
        (COSE_KEY_X, Value::ByteString(vec![0x01; 32])),
        (COSE_KEY_Y, Value::ByteString(vec![0x02; 32])),
        (COSE_KEY_ALG, Value::TextString(String::from("ES256"))),
        (COSE_KEY_OPS, Value::Array(vec![Value::integer(2)])),
    ]);
    assert_eq!(
        CoseKey::from_cose_key_map(&value, Some(Algorithm::Es256))
            .expect("レジストリ名の COSE_Key を復元できる"),
        CoseKey::ec2(EcCurve::P256, vec![0x01; 32], vec![0x02; 32])
    );

    // OKP もレジストリ名で受理する
    let value = cose_key_map(vec![
        (COSE_KEY_TYPE, Value::TextString(String::from("OKP"))),
        (COSE_KEY_CURVE, Value::TextString(String::from("Ed25519"))),
        (COSE_KEY_X, Value::ByteString(vec![0x04; 32])),
    ]);
    assert_eq!(
        CoseKey::from_cose_key_map(&value, None).expect("OKP を復元できる"),
        CoseKey::ed25519(vec![0x04; 32])
    );

    // key_ops の整数 2 (verify) は署名検証を許可する
    let value = cose_key_map(vec![
        (COSE_KEY_TYPE, Value::integer(COSE_KEY_TYPE_EC2)),
        (COSE_KEY_CURVE, Value::integer(1)),
        (COSE_KEY_X, Value::ByteString(vec![0x01; 32])),
        (COSE_KEY_Y, Value::ByteString(vec![0x02; 32])),
        (COSE_KEY_OPS, Value::Array(vec![Value::integer(2)])),
    ]);
    assert!(
        CoseKey::from_cose_key_map(&value, None).is_ok(),
        "key_ops の整数 2 を拒否した"
    );

    // 未知の kty / crv / alg 名は拒否する。既定エントリと label が重複しないよう、
    // 対象の label だけを名前で置き換える
    let ec2_key = |key_type: Value, curve: Value| {
        cose_key_map(vec![
            (COSE_KEY_TYPE, key_type),
            (COSE_KEY_CURVE, curve),
            (COSE_KEY_X, Value::ByteString(vec![0x01; 32])),
            (COSE_KEY_Y, Value::ByteString(vec![0x02; 32])),
        ])
    };
    assert_eq!(
        CoseKey::from_cose_key_map(
            &ec2_key(Value::TextString(String::from("RSA")), Value::integer(1),),
            None,
        ),
        Err(CoseError::UnexpectedType("kty"))
    );
    assert_eq!(
        CoseKey::from_cose_key_map(
            &ec2_key(
                Value::integer(COSE_KEY_TYPE_EC2),
                Value::TextString(String::from("P-999")),
            ),
            None,
        ),
        Err(CoseError::UnexpectedType("crv"))
    );
    let entries = vec![
        (COSE_KEY_TYPE, Value::integer(COSE_KEY_TYPE_EC2)),
        (COSE_KEY_CURVE, Value::integer(1)),
        (COSE_KEY_X, Value::ByteString(vec![0x01; 32])),
        (COSE_KEY_Y, Value::ByteString(vec![0x02; 32])),
        (COSE_KEY_ALG, Value::TextString(String::from("unknown"))),
    ];
    assert_eq!(
        CoseKey::from_cose_key_map(&cose_key_map(entries), None),
        Err(CoseError::UnexpectedType("alg"))
    );
}

/// 圧縮点表現 (`y` が bool) の COSE_Key を拒否すること
#[test]
fn cose_key_rejects_compressed_point_representation() {
    let value = cose_key_map(vec![
        (COSE_KEY_TYPE, Value::integer(COSE_KEY_TYPE_EC2)),
        (COSE_KEY_CURVE, Value::integer(1)),
        (COSE_KEY_X, Value::ByteString(vec![0x01; 32])),
        (COSE_KEY_Y, Value::Bool(true)),
    ]);
    assert_eq!(
        CoseKey::from_cose_key_map(&value, None),
        Err(CoseError::UnexpectedType("y"))
    );
}

// ---------------------------------------------------------------------------
// COSE Key Thumbprint (RFC 9679)
// ---------------------------------------------------------------------------

/// RFC 9679 §6 の例の EC2 公開鍵の x 座標
#[cfg(feature = "aws-lc-rs")]
const RFC9679_X_HEX: &str = "65eda5a12577c2bae829437fe338701a10aaa375e1bb5b5de108de439c08551d";

/// RFC 9679 §6 の例の EC2 公開鍵の y 座標
#[cfg(feature = "aws-lc-rs")]
const RFC9679_Y_HEX: &str = "1e52ed75701163f7f9e40ddf9f341b3dc9ba860af7e0ca7ca7e9eecd0084d19c";

/// RFC 9679 §6 の例の COSE_Key からサムプリントを計算する
///
/// RFC に記載された決定論的 CBOR のバイト列と SHA-256 の値の両方を固定値として
/// 確認する。この例の COSE_Key は `kid` を含むが、サムプリントの入力には含まれない。
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cose_key_thumbprint_matches_rfc9679_example() {
    let crypto = AwsLcRsCrypto::new();
    let value = cose_key_map(vec![
        (COSE_KEY_TYPE, Value::integer(COSE_KEY_TYPE_EC2)),
        (COSE_KEY_CURVE, Value::integer(1)),
        (COSE_KEY_X, Value::ByteString(decode_hex(RFC9679_X_HEX))),
        (COSE_KEY_Y, Value::ByteString(decode_hex(RFC9679_Y_HEX))),
        // RFC 9679 §6 の例は kid を持つが、サムプリントには使わない
        (
            COSE_KEY_KID,
            Value::ByteString(decode_hex(
                "496bd8afadf307e5b08c64b0421bf9dc01528a344a43bda88fadd1669da253ec",
            )),
        ),
    ]);
    let key = CoseKey::from_cose_key_map(&value, None).expect("COSE_Key から公開鍵を復元できる");
    let required = key.to_cose_key_map().expect("COSE_Key へ変換できる");
    assert_eq!(
        encode_hex(&cbor::encode(&required).expect("決定論的 CBOR をエンコードできる")),
        concat!(
            "a40102200121582065eda5a12577c2bae829437fe338701a10aaa375e1bb5b5de108de439c08551d",
            "2258201e52ed75701163f7f9e40ddf9f341b3dc9ba860af7e0ca7ca7e9eecd0084d19c"
        )
    );
    assert_eq!(
        encode_hex(
            &key.cose_key_thumbprint_sha256(&crypto)
                .expect("サムプリントを計算できる")
        ),
        "496bd8afadf307e5b08c64b0421bf9dc01528a344a43bda88fadd1669da253ec"
    );
}

/// 任意の COSE_Key パラメータの有無で COSE Key Thumbprint が変わらないこと
///
/// RFC 9679 §5.1 のとおり、`kid` / `alg` / `key_ops` などの任意パラメータはハッシュ
/// 対象に含めない。PBT でも同じ性質を検証するが、固定の RFC 9679 §6 の鍵で統合経路を
/// 確認する。
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cose_key_thumbprint_ignores_optional_parameters() {
    let crypto = AwsLcRsCrypto::new();
    let required = cose_key_map(vec![
        (COSE_KEY_TYPE, Value::integer(COSE_KEY_TYPE_EC2)),
        (COSE_KEY_CURVE, Value::integer(1)),
        (COSE_KEY_X, Value::ByteString(decode_hex(RFC9679_X_HEX))),
        (COSE_KEY_Y, Value::ByteString(decode_hex(RFC9679_Y_HEX))),
    ]);
    let decorated = cose_key_map(vec![
        (COSE_KEY_TYPE, Value::integer(COSE_KEY_TYPE_EC2)),
        (COSE_KEY_CURVE, Value::integer(1)),
        (COSE_KEY_X, Value::ByteString(decode_hex(RFC9679_X_HEX))),
        (COSE_KEY_Y, Value::ByteString(decode_hex(RFC9679_Y_HEX))),
        (COSE_KEY_KID, Value::ByteString(b"key-1".to_vec())),
        (COSE_KEY_ALG, Value::integer(-7)),
        (
            COSE_KEY_OPS,
            Value::Array(vec![Value::TextString(String::from("verify"))]),
        ),
        (99, Value::TextString(String::from("application specific"))),
    ]);
    let plain = CoseKey::from_cose_key_map(&required, None).expect("COSE_Key から復元できる");
    let with_optional = CoseKey::from_cose_key_map(&decorated, Some(Algorithm::Es256))
        .expect("任意パラメータ付きの COSE_Key から復元できる");
    assert_eq!(plain, with_optional, "任意パラメータで公開鍵が変わった");
    assert_eq!(
        plain
            .cose_key_thumbprint_sha256(&crypto)
            .expect("サムプリントを計算できる"),
        with_optional
            .cose_key_thumbprint_sha256(&crypto)
            .expect("サムプリントを計算できる"),
        "任意パラメータの有無でサムプリントが変わった"
    );
}

/// RFC 9679 に OKP の例が無いため、独立に計算した固定値で確認する
///
/// RFC 8032 §7.1 のテストベクタ 1 の Ed25519 公開鍵について、RFC 9679 §3 / §4.1 の
/// 手順で計算した。CBOR は `{1: 1, -1: 6, -2: h'...'}` の決定論的エンコード
/// `a301012006215820d75a...` で、その SHA-256 が `866eefbd...` である。CBOR の
/// 固定値もテスト内で assert しているため、同じ手順で再計算できる。
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cose_key_thumbprint_matches_independent_okp_vector() {
    let crypto = AwsLcRsCrypto::new();
    let key = CoseKey::ed25519(decode_hex(super::vectors::ED25519_PUBLIC_KEY_HEX));
    let required = key.to_cose_key_map().expect("COSE_Key へ変換できる");
    assert_eq!(
        encode_hex(&cbor::encode(&required).expect("決定論的 CBOR をエンコードできる")),
        "a301012006215820d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
    );
    assert_eq!(
        encode_hex(
            &key.cose_key_thumbprint_sha256(&crypto)
                .expect("サムプリントを計算できる")
        ),
        "866eefbd6718c8846cd7ddfe43fc74ab1daac4538ff8514ea2ec2d410a415743"
    );
}

/// draft-ietf-moq-c4m-02 §4.2.1 の例の ckt と一致すること
///
/// ドラフトの ckt は付録 A の ES256 公開鍵の COSE Key Thumbprint である。
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cose_key_thumbprint_matches_c4m_example() {
    let crypto = AwsLcRsCrypto::new();
    let key = CoseKey::ec2(
        EcCurve::P256,
        decode_hex(super::vectors::ES256_PUBLIC_KEY_X_HEX),
        decode_hex(super::vectors::ES256_PUBLIC_KEY_Y_HEX),
    );
    assert_eq!(
        encode_hex(
            &key.cose_key_thumbprint_sha256(&crypto)
                .expect("サムプリントを計算できる")
        ),
        super::vectors::CKT_DRAFT_C4M_DOPP_PROOF_HEX
    );
}

/// COSE Key Thumbprint を計算できない鍵を拒否すること
#[cfg(feature = "aws-lc-rs")]
#[test]
fn cose_key_thumbprint_rejects_unusable_keys() {
    let crypto = AwsLcRsCrypto::new();
    // 対称鍵は公開鍵の COSE_Key として表現しない
    assert_eq!(
        CoseKey::symmetric(vec![0x11; 32]).cose_key_thumbprint_sha256(&crypto),
        Err(CryptoError::UnsupportedThumbprintKey)
    );
    // 座標 / 公開鍵の長さが曲線と一致しない鍵はハッシュしない
    assert_eq!(
        CoseKey::ec2(EcCurve::P256, vec![0x01; 31], vec![0x02; 32])
            .cose_key_thumbprint_sha256(&crypto),
        Err(CryptoError::InvalidKey)
    );
    assert_eq!(
        CoseKey::ec2(EcCurve::P256, vec![0x01; 32], vec![0x02; 33])
            .cose_key_thumbprint_sha256(&crypto),
        Err(CryptoError::InvalidKey)
    );
    assert_eq!(
        CoseKey::ed25519(vec![0x01; 31]).cose_key_thumbprint_sha256(&crypto),
        Err(CryptoError::InvalidKey)
    );
}
