//! CAT (CTA-5007-B / draft-ietf-moq-c4m-01) のテスト

use shiguredo_moqt::c4m::cat::{
    CLAIM_CAT_VERSION, CLAIM_CONFIRMATION, CatClaims, CatError, CatToken, ClaimValidationError,
    ClaimValidationOptions, Confirmation, MOQT_AUTH_TOKEN_TYPE_CAT, TokenFormat,
};
use shiguredo_moqt::c4m::cbor::Value;
use shiguredo_moqt::c4m::{CatDpop, MoqtAction};

#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::crypto::{CoseKey, EcCurve};
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::{MoqtClaim, MoqtScope};

use super::helpers::{decode_hex, encode_hex};
use super::vectors::*;

#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::cat::{CatTokenBuilder, VerifyOptions};
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::cose::{Algorithm, CoseEncodingOptions, KeyId};
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::crypto::aws_lc_rs::AwsLcRsCrypto;
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::crypto::{CoseCrypto, CryptoError};
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::jwk::Jwk;

/// 付録 A.1 の HMAC-SHA256 鍵
#[cfg(feature = "aws-lc-rs")]
fn hmac_key() -> CoseKey {
    CoseKey::symmetric(decode_hex(HMAC_KEY_HEX))
}

/// 付録 A.1 の ES256 公開鍵
#[cfg(feature = "aws-lc-rs")]
fn es256_public_key() -> CoseKey {
    CoseKey::ec2(
        EcCurve::P256,
        decode_hex(ES256_PUBLIC_KEY_X_HEX),
        decode_hex(ES256_PUBLIC_KEY_Y_HEX),
    )
}

/// 付録 A.1 の ES256 秘密鍵
#[cfg(feature = "aws-lc-rs")]
fn es256_private_key() -> CoseKey {
    CoseKey::ec2_with_private_key(
        EcCurve::P256,
        decode_hex(ES256_PUBLIC_KEY_X_HEX),
        decode_hex(ES256_PUBLIC_KEY_Y_HEX),
        decode_hex(ES256_PRIVATE_KEY_HEX),
    )
}

#[test]
fn token_vectors_decode() {
    for vector in TOKEN_VECTORS {
        let token = CatToken::decode(vector.token.as_bytes())
            .unwrap_or_else(|error| panic!("ベクタ {} のデコードに失敗した: {error}", vector.id));
        assert_eq!(token.format(), TokenFormat::Compact, "ベクタ {}", vector.id);
        assert_eq!(
            encode_hex(token.protected_header()),
            vector.header_hex,
            "ベクタ {} の protected ヘッダ",
            vector.id
        );
        assert_eq!(
            encode_hex(token.payload()),
            vector.payload_hex,
            "ベクタ {} の claims",
            vector.id
        );
        assert_eq!(
            encode_hex(token.signature()),
            vector.signature_hex,
            "ベクタ {} の署名",
            vector.id
        );
        assert_eq!(
            token.header().algorithm_identifier,
            Some(vector.algorithm_id),
            "ベクタ {} のアルゴリズム",
            vector.id
        );
        assert_eq!(
            token.header().typ,
            Some(Value::TextString(String::from("CAT"))),
            "ベクタ {} の typ",
            vector.id
        );
        let claims = token.claims();
        assert_eq!(
            claims.issuer.as_deref(),
            vector.issuer,
            "ベクタ {}",
            vector.id
        );
        assert_eq!(
            claims.subject.as_deref(),
            vector.subject,
            "ベクタ {}",
            vector.id
        );
        assert_eq!(claims.audience, vector.audience, "ベクタ {}", vector.id);
        assert_eq!(claims.expiration, vector.expiration, "ベクタ {}", vector.id);
        assert_eq!(claims.not_before, vector.not_before, "ベクタ {}", vector.id);
        assert_eq!(claims.issued_at, vector.issued_at, "ベクタ {}", vector.id);
        assert_eq!(
            claims.cwt_id.as_deref(),
            vector.cwt_id.map(str::as_bytes),
            "ベクタ {} の cti",
            vector.id
        );
    }
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn token_vectors_verify() {
    let crypto = AwsLcRsCrypto::new();
    for vector in TOKEN_VECTORS {
        let token = CatToken::decode(vector.token.as_bytes()).expect("デコードできる");
        let key = match vector.algorithm_id {
            -7 => es256_public_key(),
            _ => hmac_key(),
        };
        token
            .verify(&crypto, &key)
            .unwrap_or_else(|error| panic!("ベクタ {} の検証に失敗した: {error}", vector.id));

        let expected = if vector.algorithm_id == -7 {
            Algorithm::Es256
        } else {
            Algorithm::HmacSha256
        };
        token
            .verify_with(
                &crypto,
                &key,
                &VerifyOptions {
                    expected_algorithm: Some(expected),
                },
            )
            .expect("トークンの alg と期待アルゴリズムは一致する");
    }
}

#[test]
fn dpop_vectors_decode() {
    for vector in DPOP_VECTORS {
        let token = CatToken::decode(vector.token.as_bytes())
            .unwrap_or_else(|error| panic!("ベクタ {} のデコードに失敗した: {error}", vector.id));
        assert_eq!(
            encode_hex(token.payload()),
            vector.payload_hex,
            "ベクタ {} の claims",
            vector.id
        );
        let confirmation = token
            .claims()
            .confirmation
            .as_ref()
            .unwrap_or_else(|| panic!("ベクタ {} に cnf が無い", vector.id));
        assert_eq!(
            confirmation.c4m_draft_jwk_thumbprint.as_deref(),
            Some(&decode_hex(vector.cnf_jkt_hex)[..]),
            "ベクタ {} の jkt (confirmation key 3)",
            vector.id
        );
        assert_eq!(
            confirmation.jkt(),
            Some(&decode_hex(vector.cnf_jkt_hex)[..]),
            "ベクタ {} の jkt",
            vector.id
        );
        match &token.claims().catdpop {
            Some(catdpop) => {
                assert_eq!(
                    catdpop.window_seconds, vector.window_seconds,
                    "ベクタ {} の catdpop ウィンドウ",
                    vector.id
                );
                assert_eq!(
                    catdpop.honor_jti, vector.honor_jti,
                    "ベクタ {} の catdpop honor_jti",
                    vector.id
                );
            }
            None => {
                assert!(
                    vector.window_seconds.is_none(),
                    "ベクタ {} に catdpop が無い",
                    vector.id
                );
            }
        }
    }
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn dpop_es256_real_binding_thumbprint_matches() {
    let vector = DPOP_VECTORS
        .iter()
        .find(|vector| vector.id == "dpop_es256_real_binding")
        .expect("ベクタがある");
    let jwk_input = vector.jwk_thumbprint_input.expect("サムプリント入力がある");
    let jwk = Jwk::decode(jwk_input).expect("JWK をデコードできる");
    assert_eq!(
        jwk.canonical_json().expect("正規化 JSON を作れる"),
        jwk_input
    );
    let crypto = AwsLcRsCrypto::new();
    assert_eq!(
        encode_hex(
            &jwk.thumbprint_sha256(&crypto)
                .expect("サムプリントを計算できる")
        ),
        vector.cnf_jkt_hex
    );
}

#[test]
fn validation_vectors_decode_and_validate() {
    for vector in VALIDATION_VECTORS {
        let token = CatToken::decode(vector.token.as_bytes())
            .unwrap_or_else(|error| panic!("ベクタ {} のデコードに失敗した: {error}", vector.id));
        let options = ClaimValidationOptions {
            reference_time_seconds: vector.reference_time.unwrap_or(0.0),
            clock_tolerance_seconds: 0.0,
            expected_issuers: vector.expected_issuers,
            expected_audiences: vector.expected_audiences,
        };
        let result = token.claims().validate(&options);
        match vector.expected_error {
            Some("TokenExpired") => {
                assert_eq!(
                    result,
                    Err(ClaimValidationError::Expired),
                    "ベクタ {}",
                    vector.id
                )
            }
            Some("TokenNotYetValid") => assert_eq!(
                result,
                Err(ClaimValidationError::NotYetValid),
                "ベクタ {}",
                vector.id
            ),
            Some("InvalidIssuer") => assert_eq!(
                result,
                Err(ClaimValidationError::IssuerMismatch),
                "ベクタ {}",
                vector.id
            ),
            Some("InvalidAudience") => assert_eq!(
                result,
                Err(ClaimValidationError::AudienceMismatch),
                "ベクタ {}",
                vector.id
            ),
            // 署名エラーのベクタはクレーム検証では正常になる
            _ => assert_eq!(result, Ok(()), "ベクタ {}", vector.id),
        }
    }
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn validation_vectors_verify_signature() {
    let crypto = AwsLcRsCrypto::new();
    for vector in VALIDATION_VECTORS {
        let token = CatToken::decode(vector.token.as_bytes()).expect("デコードできる");
        let Some(key_hex) = vector.key_hex else {
            continue;
        };
        let key = CoseKey::symmetric(decode_hex(key_hex));
        let result = token.verify(&crypto, &key);
        match vector.expected_error {
            Some("SignatureVerificationFailed") => assert_eq!(
                result,
                Err(CatError::Crypto(CryptoError::SignatureVerificationFailed)),
                "ベクタ {}",
                vector.id
            ),
            _ => unreachable!("署名エラーのベクタだけを対象にする"),
        }
    }
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn validation_vector_algorithm_mismatch() {
    let crypto = AwsLcRsCrypto::new();
    let vector = VALIDATION_VECTORS
        .iter()
        .find(|vector| vector.id == "invalid_algorithm_mismatch")
        .expect("ベクタがある");
    assert_eq!(vector.verifier_algorithm_id, Some(-7));
    let token = CatToken::decode(vector.token.as_bytes()).expect("デコードできる");
    // 期待アルゴリズムを指定しなければ署名は検証できる
    token
        .verify(&crypto, &hmac_key())
        .expect("正しい鍵では検証できる");
    // 期待アルゴリズムが違えば AlgorithmMismatch
    assert_eq!(
        token.verify_with(
            &crypto,
            &hmac_key(),
            &VerifyOptions {
                expected_algorithm: Some(Algorithm::Es256),
            },
        ),
        Err(CatError::AlgorithmMismatch {
            token: Algorithm::HmacSha256,
            expected: Algorithm::Es256,
        })
    );
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn compact_round_trip_with_hmac() {
    let crypto = AwsLcRsCrypto::new();
    let claims = CatClaims {
        issuer: Some(String::from("https://auth.example.com")),
        audience: vec![String::from("https://relay.example.com")],
        expiration: Some(1700086400.0),
        not_before: Some(1700000000.0),
        moqt: Some(MoqtClaim::new().scope(
            MoqtScope::new([MoqtAction::Publish, MoqtAction::Fetch]).namespace_match(
                shiguredo_moqt::c4m::NamespaceMatch::Match(shiguredo_moqt::c4m::Match::Prefix(
                    b"example.com".to_vec(),
                )),
            ),
        )),
        moqt_reval: Some(300.0),
        catdpop: Some(CatDpop::new(300.0, true)),
        confirmation: Some(Confirmation {
            jwk_thumbprint: Some(vec![0xab; 32]),
            ..Confirmation::default()
        }),
        ..CatClaims::default()
    };
    let builder = CatTokenBuilder {
        claims: claims.clone(),
        ..CatTokenBuilder::default()
    };
    let token_text = builder
        .build_compact(&crypto, &hmac_key())
        .expect("compact 形式を発行できる");
    let token = CatToken::decode(token_text.as_bytes()).expect("デコードできる");
    assert_eq!(token.format(), TokenFormat::Compact);
    token.verify(&crypto, &hmac_key()).expect("検証できる");
    assert_eq!(token.claims(), &claims);
    // 発行時の HMAC-SHA256 は RFC 9053 の HMAC 256/256 (5) を使う
    // (ドラフト付録 A のベクタが使う -4 は検証でのみ受理する)
    assert_eq!(token.header().algorithm_identifier, Some(5));
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn cose_round_trip_with_hmac_is_mac0() {
    let crypto = AwsLcRsCrypto::new();
    let token_bytes = CatTokenBuilder::new()
        .issuer("https://auth.example.com")
        .audience("https://relay.example.com")
        .expiration(1700086400.0)
        .build_cose(&crypto, &hmac_key())
        .expect("COSE 形式を発行できる");
    let token = CatToken::decode(&token_bytes).expect("デコードできる");
    assert_eq!(token.format(), TokenFormat::CoseMac0);
    assert_eq!(token.header().algorithm_identifier, Some(5));
    token.verify(&crypto, &hmac_key()).expect("検証できる");
    // CWT タグ (61) と COSE タグ (17) が付いている
    assert_eq!(token_bytes[0], 0xd8);
    assert_eq!(token_bytes[1], 0x3d);
    assert_eq!(token_bytes[2], 0xd1);
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn cose_round_trip_with_es256_is_sign1() {
    let crypto = AwsLcRsCrypto::new();
    let token_bytes = CatTokenBuilder::new()
        .issuer("https://auth.example.com")
        .audience("https://moq-relay.example.com")
        .expiration(1700086400.0)
        .not_before(1700000000.0)
        .key_id(KeyId::Text(String::from("key-1")))
        .build_cose(&crypto, &es256_private_key())
        .expect("COSE 形式を発行できる");
    let token = CatToken::decode(&token_bytes).expect("デコードできる");
    assert_eq!(token.format(), TokenFormat::CoseSign1);
    assert_eq!(token.header().algorithm, Some(Algorithm::Es256));
    assert_eq!(
        token.header().key_id,
        Some(KeyId::Text(String::from("key-1")))
    );
    token
        .verify(&crypto, &es256_public_key())
        .expect("検証できる");
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn cose_round_trip_with_ed25519() {
    use shiguredo_moqt::c4m::crypto::OkpCurve;

    let crypto = AwsLcRsCrypto::new();
    // Ed25519 の鍵はテストベクタに無いため、検証用の既知の鍵対を使う
    // (両方とも RFC 8032 §7.1 のテストベクタ 1 の値)
    let public_key = CoseKey::ed25519(decode_hex(
        "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
    ));
    let private_key = CoseKey::ed25519_with_private_key(
        decode_hex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"),
        decode_hex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60"),
    );
    let token_bytes = CatTokenBuilder::new()
        .issuer("https://auth.example.com")
        .build_cose(&crypto, &private_key)
        .expect("Ed25519 で発行できる");
    let token = CatToken::decode(&token_bytes).expect("デコードできる");
    assert_eq!(token.header().algorithm, Some(Algorithm::EdDsa));
    token.verify(&crypto, &public_key).expect("検証できる");
    // 曲線の識別子を確認する
    assert_eq!(OkpCurve::Ed25519.identifier(), 6);
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn base64url_wrapped_cose_token_decodes() {
    use base64ct::{Base64UrlUnpadded, Encoding};

    let crypto = AwsLcRsCrypto::new();
    let token_bytes = CatTokenBuilder::new()
        .issuer("https://auth.example.com")
        .build_cose(&crypto, &hmac_key())
        .expect("COSE 形式を発行できる");
    let text = Base64UrlUnpadded::encode_string(&token_bytes);
    let token = CatToken::decode(text.as_bytes()).expect("base64url のトークンをデコードできる");
    assert_eq!(token.format(), TokenFormat::CoseMac0);
    token.verify(&crypto, &hmac_key()).expect("検証できる");
}

#[test]
fn decode_moqt_auth_token_checks_type() {
    let token_text = TOKEN_VECTORS[0].token;
    assert_eq!(
        CatToken::decode_moqt_auth_token(MOQT_AUTH_TOKEN_TYPE_CAT, token_text.as_bytes())
            .map(|token| token.format()),
        Ok(TokenFormat::Compact)
    );
    assert_eq!(
        CatToken::decode_moqt_auth_token(2, token_text.as_bytes()).map(|token| token.format()),
        Err(CatError::InvalidAuthTokenType(2))
    );
}

#[test]
fn invalid_tokens_are_rejected() {
    // CBOR / base64url のどちらとしても解釈できない入力は CBOR のエラーになる
    assert!(matches!(
        CatToken::decode(b"not a token"),
        Err(CatError::Cose(_))
    ));
    assert!(matches!(
        CatToken::decode(b"aaaa.bbbb"),
        Err(CatError::Cose(_))
    ));
    // compact 形式のデコードを明示的に呼んだ場合は形式エラー
    assert_eq!(
        CatToken::decode_compact("ogEjEGNDQVQ.aaaa").map(|_| ()),
        Err(CatError::InvalidTokenFormat)
    );
    // 自動判別では 3 分割でない入力は COSE 形式として解釈する
    assert!(matches!(
        CatToken::decode(b"ogEjEGNDQVQ.aaaa"),
        Err(CatError::Cose(_))
    ));
    // base64url として不正な 3 分割
    assert_eq!(
        CatToken::decode(b"***.@@@.$$$").map(|_| ()),
        Err(CatError::InvalidBase64)
    );
    // 3 分割だが claims が CBOR ではない
    assert!(matches!(
        CatToken::decode(b"ogEjEGNDQVQ.aaaa.aaaa"),
        Err(CatError::Cbor(_))
    ));
}

#[test]
fn tokens_without_algorithm_are_rejected() {
    // compact 形式で alg が無い
    let protected =
        shiguredo_moqt::c4m::cbor::encode(&Value::Map(Vec::new())).expect("エンコードできる");
    let claims =
        shiguredo_moqt::c4m::cbor::encode(&Value::Map(Vec::new())).expect("エンコードできる");
    let text = format!(
        "{}.{}.{}",
        base64url(&protected),
        base64url(&claims),
        base64url(b"signature")
    );
    assert_eq!(
        CatToken::decode(text.as_bytes()).map(|_| ()),
        Err(CatError::MissingAlgorithm)
    );

    // COSE 形式で alg が無い
    let value = Value::Tag(
        18,
        Box::new(Value::Array(vec![
            Value::ByteString(Vec::new()),
            Value::Map(Vec::new()),
            Value::ByteString(claims),
            Value::ByteString(b"signature".to_vec()),
        ])),
    );
    let bytes = shiguredo_moqt::c4m::cbor::encode(&value).expect("エンコードできる");
    assert_eq!(
        CatToken::decode(&bytes).map(|_| ()),
        Err(CatError::MissingAlgorithm)
    );
}

/// パディング無しの base64url でエンコードする
fn base64url(bytes: &[u8]) -> String {
    use base64ct::{Base64UrlUnpadded, Encoding};
    Base64UrlUnpadded::encode_string(bytes)
}

#[test]
fn claims_duplicate_raw_key_is_rejected_on_encode() {
    let claims = CatClaims {
        issuer: Some(String::from("https://auth.example.com")),
        raw: vec![(
            Value::integer(shiguredo_moqt::c4m::cat::CLAIM_ISSUER),
            Value::TextString(String::from("other")),
        )],
        ..CatClaims::default()
    };
    assert_eq!(
        claims.encode(),
        Err(CatError::DuplicateClaim(
            shiguredo_moqt::c4m::cat::CLAIM_ISSUER
        ))
    );
}

#[test]
fn claims_raw_round_trip_and_get() {
    // 付録 A.2 の catv / catu は型付きで解釈せず raw に保持する
    let vector = CLAIM_VECTORS
        .iter()
        .find(|vector| vector.id == "cbor_cat_version_usage")
        .expect("ベクタがある");
    let value =
        shiguredo_moqt::c4m::cbor::decode(&decode_hex(vector.payload_hex)).expect("デコードできる");
    let claims = CatClaims::decode(&value).expect("claims をデコードできる");
    assert_eq!(
        claims.get(CLAIM_CAT_VERSION),
        Some(&Value::TextString(String::from("CAT-v1")))
    );
    assert_eq!(claims.encode().expect("エンコードできる"), value);
}

#[test]
fn confirmation_jkt_prefers_iana_key() {
    let confirmation = Confirmation {
        jwk_thumbprint: Some(vec![1; 32]),
        c4m_draft_jwk_thumbprint: Some(vec![2; 32]),
        raw: Vec::new(),
    };
    assert_eq!(confirmation.jkt(), Some(&[1u8; 32][..]));
    let value = confirmation.encode();
    assert_eq!(
        CatClaims {
            confirmation: Some(Confirmation::default()),
            ..CatClaims::default()
        }
        .encode()
        .expect("エンコードできる")
        .map_get(&Value::integer(CLAIM_CONFIRMATION))
        .expect("cnf がある"),
        &Value::Map(Vec::new())
    );
    let decoded = Confirmation::decode(&value).expect("デコードできる");
    assert_eq!(decoded, confirmation);
}

#[test]
fn validation_options_default_and_tolerance() {
    let options = ClaimValidationOptions::default();
    assert_eq!(options.reference_time_seconds, 0.0);
    let claims = CatClaims {
        expiration: Some(100.0),
        not_before: Some(50.0),
        ..CatClaims::default()
    };
    // 期限内
    assert_eq!(
        claims.validate(&ClaimValidationOptions {
            reference_time_seconds: 100.0,
            ..ClaimValidationOptions::default()
        }),
        Ok(())
    );
    // 期限切れ
    assert_eq!(
        claims.validate(&ClaimValidationOptions {
            reference_time_seconds: 101.0,
            ..ClaimValidationOptions::default()
        }),
        Err(ClaimValidationError::Expired)
    );
    // 許容ずれの範囲内
    assert_eq!(
        claims.validate(&ClaimValidationOptions {
            reference_time_seconds: 110.0,
            clock_tolerance_seconds: 10.0,
            ..ClaimValidationOptions::default()
        }),
        Ok(())
    );
    // nbf より前
    assert_eq!(
        claims.validate(&ClaimValidationOptions {
            reference_time_seconds: 49.0,
            ..ClaimValidationOptions::default()
        }),
        Err(ClaimValidationError::NotYetValid)
    );
    // nbf は許容ずれで吸収できる
    assert_eq!(
        claims.validate(&ClaimValidationOptions {
            reference_time_seconds: 45.0,
            clock_tolerance_seconds: 5.0,
            ..ClaimValidationOptions::default()
        }),
        Ok(())
    );
}

#[test]
fn claims_default_has_no_claims() {
    let claims = CatClaims::default();
    assert_eq!(claims.issuer, None);
    assert!(claims.audience.is_empty());
    assert_eq!(
        claims.encode().expect("エンコードできる"),
        Value::Map(Vec::new())
    );
    assert!(!claims.authorize(MoqtAction::Publish, &[&b"a"[..]], b"t"));
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn cose_tag_option_is_honored() {
    let crypto = AwsLcRsCrypto::new();
    let token_bytes = CatTokenBuilder::new()
        .issuer("https://auth.example.com")
        .build_cose_with(
            &crypto,
            &hmac_key(),
            &CoseEncodingOptions {
                cose_tag: false,
                cwt_tag: false,
            },
        )
        .expect("発行できる");
    let token = CatToken::decode(&token_bytes).expect("デコードできる");
    assert_eq!(token.format(), TokenFormat::CoseMac0);
    token.verify(&crypto, &hmac_key()).expect("検証できる");
}

/// ES384 / ES512 / HMAC 384 / HMAC 512 の署名と検証の往復
///
/// 付録 A のベクタは HMAC-SHA256 と ES256 だけなので、他の対応アルゴリズムは
/// openssl で生成した固定の鍵で往復を固定する。
#[cfg(feature = "aws-lc-rs")]
#[test]
fn round_trip_for_all_algorithms() {
    let crypto = AwsLcRsCrypto::new();

    // HMAC 384 / 512
    for (algorithm, key) in [
        (Algorithm::HmacSha384, CoseKey::symmetric(vec![0xcd; 48])),
        (Algorithm::HmacSha512, CoseKey::symmetric(vec![0xef; 64])),
    ] {
        let token_bytes = CatTokenBuilder::new()
            .issuer("https://auth.example.com")
            .algorithm(algorithm)
            .build_cose(&crypto, &key)
            .expect("COSE 形式を発行できる");
        let token = CatToken::decode(&token_bytes).expect("デコードできる");
        assert_eq!(token.header().algorithm, Some(algorithm));
        token.verify(&crypto, &key).expect("検証できる");
    }

    // ES384 / ES512 (鍵はテスト専用に生成した固定値)
    for (algorithm, curve, x, y, d) in [
        (
            Algorithm::Es384,
            EcCurve::P384,
            "ce7de2ef769603fc91f4682efeedc9e415b221a79067153a31d8f2f62d14044e18be87058146596041b2148233ed4073",
            "8a69f5b6b5bdb0200d39bf760420a3da5bf091b8e81557f5cb4d37f7ce36f7a07fb1119a736e2385d75e4c3f5ca604e7",
            "e3129db6c869a183e6ed6abc233723f7339b94e38c38bdde7c19d4674c5c2730eca7680ac6afa8d660810a8adead31e2",
        ),
        (
            Algorithm::Es512,
            EcCurve::P521,
            "00518dbde5592773706f05f885b3c70b5b55c8d5fb00ed301714176827b36464b3fa5547e2e5b39abb7addb9648f556ef5319d866a927de7cf9d127f0040a98601b9",
            "010ddbdde03abcbad3ea82185af075133e30babc436411af7cdb4503127df46d82f828d4e91692a821886e7ddc614e476dd467d907691e0c2e7910660725f9999876",
            "01d1498cdca88097c4f581b05475494b5676aeb926a07dbd26093f99f8136cc572fe9fc8d03238d42b2af0a5b9dce17e615534029fd358a848873aff02bb9a470f76",
        ),
    ] {
        let private_key =
            CoseKey::ec2_with_private_key(curve, decode_hex(x), decode_hex(y), decode_hex(d));
        let public_key = CoseKey::ec2(curve, decode_hex(x), decode_hex(y));
        let token_bytes = CatTokenBuilder::new()
            .issuer("https://auth.example.com")
            .build_cose(&crypto, &private_key)
            .expect("COSE 形式を発行できる");
        let token = CatToken::decode(&token_bytes).expect("デコードできる");
        assert_eq!(token.header().algorithm, Some(algorithm));
        token.verify(&crypto, &public_key).expect("検証できる");
    }
}

/// 付録 A の全ベクタトークンの署名を検証する
///
/// A.3 以外 (A.4 / A.5 / A.6) のトークンの署名も固定し、ベクタのドリフトを検出する。
#[cfg(feature = "aws-lc-rs")]
#[test]
fn all_vector_tokens_verify_signatures() {
    let crypto = AwsLcRsCrypto::new();
    for vector in DPOP_VECTORS {
        let token = CatToken::decode(vector.token.as_bytes())
            .unwrap_or_else(|error| panic!("ベクタ {} のデコードに失敗した: {error}", vector.id));
        let key = if vector.id == "dpop_es256_real_binding" {
            es256_public_key()
        } else {
            hmac_key()
        };
        token
            .verify(&crypto, &key)
            .unwrap_or_else(|error| panic!("ベクタ {} の検証に失敗した: {error}", vector.id));
    }
    for vector in SCOPE_VECTORS {
        let token = CatToken::decode(vector.token.as_bytes())
            .unwrap_or_else(|error| panic!("ベクタ {} のデコードに失敗した: {error}", vector.id));
        token
            .verify(&crypto, &hmac_key())
            .unwrap_or_else(|error| panic!("ベクタ {} の検証に失敗した: {error}", vector.id));
    }
    for vector in VALIDATION_VECTORS {
        let token = CatToken::decode(vector.token.as_bytes())
            .unwrap_or_else(|error| panic!("ベクタ {} のデコードに失敗した: {error}", vector.id));
        // 改ざん・誤鍵のベクタは失敗することが期待値
        if matches!(
            vector.id,
            "invalid_tampered_signature" | "invalid_wrong_key"
        ) {
            continue;
        }
        token
            .verify(&crypto, &hmac_key())
            .unwrap_or_else(|error| panic!("ベクタ {} の検証に失敗した: {error}", vector.id));
    }
}

/// 付録 A.4 の cnf_jkt_source の記載を固定する
#[cfg(feature = "aws-lc-rs")]
#[test]
fn dpop_no_jti_thumbprint_source_matches() {
    use shiguredo_moqt::c4m::crypto::DigestAlgorithm;

    let crypto = AwsLcRsCrypto::new();
    let vector = DPOP_VECTORS
        .iter()
        .find(|vector| vector.id == "dpop_no_jti")
        .expect("ベクタがある");
    let digest = crypto
        .digest(DigestAlgorithm::Sha256, b"test-public-key-material")
        .expect("ハッシュを計算できる");
    assert_eq!(encode_hex(&digest), vector.cnf_jkt_hex);
}

#[test]
fn non_finite_claim_numbers_are_rejected() {
    for (claim, value, name) in [
        (
            shiguredo_moqt::c4m::cat::CLAIM_EXPIRATION,
            Value::Float(f64::NAN),
            "exp",
        ),
        (
            shiguredo_moqt::c4m::cat::CLAIM_NOT_BEFORE,
            Value::Float(f64::INFINITY),
            "nbf",
        ),
        (
            shiguredo_moqt::c4m::cat::CLAIM_ISSUED_AT,
            Value::Float(f64::NEG_INFINITY),
            "iat",
        ),
        (
            shiguredo_moqt::c4m::CLAIM_MOQT_REVAL,
            Value::Float(f64::NAN),
            "moqt-reval",
        ),
    ] {
        let claims = Value::Map(vec![(Value::integer(claim), value)]);
        assert_eq!(
            CatClaims::decode(&claims),
            Err(CatError::NonFiniteNumber(name))
        );
    }
}

#[test]
fn non_finite_reference_time_is_rejected() {
    let claims = CatClaims {
        expiration: Some(100.0),
        ..CatClaims::default()
    };
    assert_eq!(
        claims.validate(&ClaimValidationOptions {
            reference_time_seconds: f64::NAN,
            ..ClaimValidationOptions::default()
        }),
        Err(ClaimValidationError::InvalidReferenceTime)
    );
    assert_eq!(
        claims.validate(&ClaimValidationOptions {
            reference_time_seconds: 1.0,
            clock_tolerance_seconds: -1.0,
            ..ClaimValidationOptions::default()
        }),
        Err(ClaimValidationError::InvalidReferenceTime)
    );
}

#[test]
fn typed_claim_keys_are_rejected_in_raw_even_when_unset() {
    // 型付きフィールドが未設定でも raw に置くと decode と encode で解釈が曖昧になる
    let claims = CatClaims {
        raw: vec![(
            Value::integer(shiguredo_moqt::c4m::cat::CLAIM_EXPIRATION),
            Value::TextString(String::from("x")),
        )],
        ..CatClaims::default()
    };
    assert_eq!(
        claims.encode(),
        Err(CatError::DuplicateClaim(
            shiguredo_moqt::c4m::cat::CLAIM_EXPIRATION
        ))
    );
}

#[test]
fn validate_rejects_non_finite_claims_built_by_hand() {
    let claims = CatClaims {
        expiration: Some(f64::NAN),
        ..CatClaims::default()
    };
    assert_eq!(
        claims.validate(&ClaimValidationOptions::default()),
        Err(ClaimValidationError::NonFiniteClaim("exp"))
    );
    let claims = CatClaims {
        catdpop: Some(CatDpop {
            window_seconds: Some(f64::INFINITY),
            honor_jti: None,
            raw: Vec::new(),
        }),
        ..CatClaims::default()
    };
    assert_eq!(
        claims.validate(&ClaimValidationOptions::default()),
        Err(ClaimValidationError::NonFiniteClaim("catdpop window"))
    );
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn builder_rejects_non_finite_timestamps() {
    let crypto = AwsLcRsCrypto::new();
    assert_eq!(
        CatTokenBuilder::new()
            .expiration(f64::NAN)
            .build_compact(&crypto, &hmac_key()),
        Err(CatError::NonFiniteNumber("exp"))
    );
    assert_eq!(
        CatTokenBuilder::new()
            .not_before(f64::INFINITY)
            .build_cose(&crypto, &hmac_key()),
        Err(CatError::NonFiniteNumber("nbf"))
    );
    assert_eq!(
        CatTokenBuilder::new()
            .moqt_reval(f64::NAN)
            .build_cose(&crypto, &hmac_key()),
        Err(CatError::NonFiniteNumber("moqt-reval"))
    );
    assert_eq!(
        CatTokenBuilder::new()
            .catdpop(f64::NAN, true)
            .build_cose(&crypto, &hmac_key()),
        Err(CatError::C4m(
            shiguredo_moqt::c4m::C4mError::NonFiniteNumber("catdpop window")
        ))
    );
}

#[test]
fn cat_claim_keys_match_the_iana_registry() {
    use shiguredo_moqt::c4m::cat::*;
    assert_eq!(CLAIM_CAT_REPLAY, 308);
    assert_eq!(CLAIM_CAT_PROBABILITY_OF_REJECTION, 309);
    assert_eq!(CLAIM_CAT_VERSION, 310);
    assert_eq!(CLAIM_CAT_NETWORK_IP, 311);
    assert_eq!(CLAIM_CAT_URI, 312);
    assert_eq!(CLAIM_CAT_METHOD, 313);
    assert_eq!(CLAIM_CAT_ALPN, 314);
    assert_eq!(CLAIM_CAT_HEADER, 315);
    assert_eq!(CLAIM_CAT_GEO_ISO3166, 316);
    assert_eq!(CLAIM_CAT_GEO_COORD, 317);
    assert_eq!(CLAIM_CAT_GEO_ALT, 318);
    assert_eq!(CLAIM_CAT_TLS_PUBLIC_KEY, 319);
    assert_eq!(CLAIM_CAT_IF_DATA, 320);
    assert_eq!(CLAIM_CAT_DPOP, 321);
    assert_eq!(CLAIM_CAT_IF, 322);
    assert_eq!(CLAIM_CAT_RENEWAL, 323);
    assert_eq!(CLAIM_ISSUER, 1);
    assert_eq!(CLAIM_SUBJECT, 2);
    assert_eq!(CLAIM_AUDIENCE, 3);
    assert_eq!(CLAIM_EXPIRATION, 4);
    assert_eq!(CLAIM_NOT_BEFORE, 5);
    assert_eq!(CLAIM_ISSUED_AT, 6);
    assert_eq!(CLAIM_CWT_ID, 7);
    assert_eq!(CLAIM_CONFIRMATION, 8);
    assert_eq!(CONFIRMATION_JWK_THUMBPRINT, 323);
    assert_eq!(CONFIRMATION_C4M_DRAFT_JWK_THUMBPRINT, 3);
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn builder_setters_are_covered() {
    use shiguredo_moqt::c4m::cose::KeyId;

    let crypto = AwsLcRsCrypto::new();
    let claims = CatTokenBuilder::new()
        .issuer("https://auth.example.com")
        .subject("user:alice")
        .audience("https://relay.example.com")
        .issued_at(1700000000.0)
        .cwt_id(b"id-1".to_vec())
        .c4m_draft_jwk_thumbprint(vec![0x01; 32])
        .typ("CAT")
        .key_id(KeyId::Text(String::from("key-1")))
        .claim(
            310,
            shiguredo_moqt::c4m::cbor::Value::TextString(String::from("CAT-v1")),
        )
        .build_cose(&crypto, &hmac_key())
        .expect("発行できる");
    let token = CatToken::decode(&claims).expect("デコードできる");
    let decoded = token.claims();
    assert_eq!(decoded.subject.as_deref(), Some("user:alice"));
    assert_eq!(decoded.issued_at, Some(1700000000.0));
    assert_eq!(decoded.cwt_id.as_deref(), Some(&b"id-1"[..]));
    assert_eq!(
        decoded
            .confirmation
            .as_ref()
            .and_then(|confirmation| confirmation.c4m_draft_jwk_thumbprint.as_deref()),
        Some(&[0x01u8; 32][..])
    );
    assert_eq!(
        decoded.get(shiguredo_moqt::c4m::cat::CLAIM_CAT_VERSION),
        Some(&shiguredo_moqt::c4m::cbor::Value::TextString(String::from(
            "CAT-v1"
        )))
    );
    token.verify(&crypto, &hmac_key()).expect("検証できる");
    assert_eq!(
        token.header().typ,
        Some(Value::TextString(String::from("CAT")))
    );
}

#[test]
fn cbor_auth_token_value_is_decoded() {
    // MOQT の Token Value に CBOR (COSE 形式) を直接渡した場合もデコードできる
    let token_text = TOKEN_VECTORS[0].token;
    let compact = CatToken::decode(token_text.as_bytes()).expect("デコードできる");
    // compact 形式の payload を COSE として渡すと形式エラーになる
    assert!(CatToken::decode_moqt_auth_token(MOQT_AUTH_TOKEN_TYPE_CAT, compact.payload()).is_err());
    assert_eq!(
        CatToken::decode(compact.raw_token())
            .expect("raw から再デコードできる")
            .claims(),
        compact.claims()
    );
}

#[test]
fn debug_hides_raw_token_and_signature() {
    let token = CatToken::decode(TOKEN_VECTORS[0].token.as_bytes()).expect("デコードできる");
    let debug = format!("{token:?}");
    assert!(
        !debug.contains(TOKEN_VECTORS[0].token),
        "生トークンを出さない"
    );
    assert!(
        !debug.contains(TOKEN_VECTORS[0].signature_hex),
        "署名を出さない"
    );
    assert!(debug.contains("raw_bytes"), "長さは表示する");
}
