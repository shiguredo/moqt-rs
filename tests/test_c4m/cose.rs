//! COSE (RFC 9052 / RFC 9053) のテスト

use shiguredo_moqt::c4m::cbor::{self, Value};
use shiguredo_moqt::c4m::cose::{
    Algorithm, AlgorithmClass, C4M_DRAFT_HMAC_SHA256_ALGORITHM_ID, CoseEncodingOptions, CoseError,
    CoseMac0, CoseMessage, CoseSign1, Header, KeyId, TAG_COSE_MAC0, TAG_COSE_SIGN1, TAG_CWT,
};

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
