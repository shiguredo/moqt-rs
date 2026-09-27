//! JWS compact (RFC 7515) のテスト

use base64ct::{Base64UrlUnpadded, Encoding};
use shiguredo_moqt::c4m::cose::Algorithm;
use shiguredo_moqt::c4m::jwk::Jwk;
use shiguredo_moqt::c4m::jwt::{JwsCompact, JwtError};

#[cfg(feature = "aws-lc-rs")]
use super::helpers::decode_hex;
#[cfg(feature = "aws-lc-rs")]
use super::vectors::{
    ES256_PRIVATE_KEY_HEX, ES256_PUBLIC_KEY_X_HEX, ES256_PUBLIC_KEY_Y_HEX, HMAC_KEY_HEX,
};
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::crypto::{CoseCrypto, CoseKey};

/// パディング無しの base64url で文字列をエンコードする
fn base64url(text: &str) -> String {
    Base64UrlUnpadded::encode_string(text.as_bytes())
}

/// JWS compact のトークンを組み立てる
fn token(header_json: &str, payload_json: &str, signature: &[u8]) -> String {
    format!(
        "{}.{}.{}",
        base64url(header_json),
        base64url(payload_json),
        Base64UrlUnpadded::encode_string(signature)
    )
}

#[test]
fn decode_reads_header_payload_and_signing_input() {
    let text = token(
        r#"{"alg":"ES256","typ":"dpop-proof+jwt","kid":"key-1","jwk":{"kty":"EC","crv":"P-256","x":"AQ","y":"AQ"}}"#,
        r#"{"jti":"id-1"}"#,
        &[0x01, 0x02],
    );
    let jws = JwsCompact::decode(&text).expect("デコードできる");
    assert_eq!(jws.header().algorithm, Algorithm::Es256);
    assert_eq!(jws.header().algorithm_name, "ES256");
    assert_eq!(jws.header().typ.as_deref(), Some("dpop-proof+jwt"));
    assert_eq!(jws.header().key_id.as_deref(), Some("key-1"));
    assert_eq!(
        jws.header().jwk,
        Some(Jwk::Ec {
            curve: String::from("P-256"),
            x: String::from("AQ"),
            y: String::from("AQ"),
        })
    );
    assert_eq!(jws.payload(), br#"{"jti":"id-1"}"#);
    assert_eq!(jws.signature(), &[0x01, 0x02]);
    let signing_input = format!(
        "{}.{}",
        base64url(
            r#"{"alg":"ES256","typ":"dpop-proof+jwt","kid":"key-1","jwk":{"kty":"EC","crv":"P-256","x":"AQ","y":"AQ"}}"#
        ),
        base64url(r#"{"jti":"id-1"}"#),
    );
    assert_eq!(jws.signing_input(), signing_input.as_bytes());
}

#[test]
fn decode_errors() {
    assert_eq!(
        JwsCompact::decode("a.b").map(|_| ()),
        Err(JwtError::InvalidTokenFormat)
    );
    assert_eq!(
        JwsCompact::decode("***.@@@.$$$").map(|_| ()),
        Err(JwtError::InvalidBase64)
    );
    assert!(matches!(
        JwsCompact::decode(&token("not json", "{}", &[0x01])),
        Err(JwtError::Json(_))
    ));
    assert!(matches!(
        JwsCompact::decode(&token(r#"{"typ":"JWT"}"#, "{}", &[0x01])),
        Err(JwtError::Json(_))
    ));
    assert!(matches!(
        JwsCompact::decode(&token(r#"{"alg":"PS256"}"#, "{}", &[0x01])),
        Err(JwtError::UnsupportedAlgorithm(name)) if name == "PS256"
    ));
    assert!(matches!(
        JwsCompact::decode(&token(r#"{"alg":"ES256"}"#, "{}", &[0x01])),
        Ok(jws) if jws.header().jwk.is_none()
    ));
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn verify_hmac_jwt() {
    use shiguredo_moqt::c4m::crypto::CryptoError;
    use shiguredo_moqt::c4m::crypto::aws_lc_rs::AwsLcRsCrypto;

    let crypto = AwsLcRsCrypto::new();
    let key = CoseKey::symmetric(decode_hex(HMAC_KEY_HEX));
    let header = r#"{"alg":"HS256","typ":"JWT"}"#;
    let payload = r#"{"iss":"https://auth.example.com"}"#;
    let signing_input = format!("{}.{}", base64url(header), base64url(payload));
    let signature = crypto
        .sign(Algorithm::HmacSha256, &key, signing_input.as_bytes())
        .expect("署名できる");
    let text = format!(
        "{signing_input}.{}",
        Base64UrlUnpadded::encode_string(&signature)
    );
    let jws = JwsCompact::decode(&text).expect("デコードできる");
    jws.verify(&crypto, &key).expect("検証できる");

    // 改ざんされた署名は拒否する
    let mut tampered = signature.clone();
    tampered[0] ^= 0xff;
    let text = format!(
        "{signing_input}.{}",
        Base64UrlUnpadded::encode_string(&tampered)
    );
    let jws = JwsCompact::decode(&text).expect("デコードできる");
    assert_eq!(
        jws.verify(&crypto, &key),
        Err(JwtError::Crypto(CryptoError::SignatureVerificationFailed))
    );
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn verify_es256_jwt() {
    use shiguredo_moqt::c4m::crypto::EcCurve;
    use shiguredo_moqt::c4m::crypto::aws_lc_rs::AwsLcRsCrypto;

    let crypto = AwsLcRsCrypto::new();
    let private_key = CoseKey::ec2_with_private_key(
        EcCurve::P256,
        decode_hex(ES256_PUBLIC_KEY_X_HEX),
        decode_hex(ES256_PUBLIC_KEY_Y_HEX),
        decode_hex(ES256_PRIVATE_KEY_HEX),
    );
    let public_key = CoseKey::ec2(
        EcCurve::P256,
        decode_hex(ES256_PUBLIC_KEY_X_HEX),
        decode_hex(ES256_PUBLIC_KEY_Y_HEX),
    );
    let header = r#"{"alg":"ES256","typ":"JWT"}"#;
    let payload = r#"{"iss":"https://auth.example.com"}"#;
    let signing_input = format!("{}.{}", base64url(header), base64url(payload));
    let signature = crypto
        .sign(Algorithm::Es256, &private_key, signing_input.as_bytes())
        .expect("署名できる");
    let text = format!(
        "{signing_input}.{}",
        Base64UrlUnpadded::encode_string(&signature)
    );
    let jws = JwsCompact::decode(&text).expect("デコードできる");
    jws.verify(&crypto, &public_key).expect("検証できる");
}
