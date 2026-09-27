//! JWK (RFC 7517) と JWK サムプリント (RFC 7638) のテスト

use shiguredo_moqt::c4m::crypto::{CoseKey, EcCurve};
use shiguredo_moqt::c4m::jwk::{Jwk, JwkError};

use super::helpers::{decode_hex, encode_hex};
use super::vectors::{ES256_PUBLIC_KEY_X_HEX, ES256_PUBLIC_KEY_Y_HEX};

#[cfg(feature = "aws-lc-rs")]
use super::vectors::DPOP_VECTORS;

#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::crypto::aws_lc_rs::AwsLcRsCrypto;

/// パディング無しの base64url でエンコードする
fn base64url(bytes: &[u8]) -> String {
    use base64ct::{Base64UrlUnpadded, Encoding};
    Base64UrlUnpadded::encode_string(bytes)
}

#[test]
fn decode_ec_jwk_and_convert_to_cose_key() {
    let jwk = Jwk::decode(
        r#"{"kty":"EC","x":"l8tFrhx-34tV3hRICRDY9zCkDlpBhF42UQUfWVAWBFs","y":"9VE4jf_Ok_o64zbTTlcuNJajHmt6v9TDVrU0CdvGRDA","crv":"P-256"}"#,
    )
    .expect("JWK をデコードできる");
    assert_eq!(
        jwk,
        Jwk::Ec {
            curve: String::from("P-256"),
            x: String::from("l8tFrhx-34tV3hRICRDY9zCkDlpBhF42UQUfWVAWBFs"),
            y: String::from("9VE4jf_Ok_o64zbTTlcuNJajHmt6v9TDVrU0CdvGRDA"),
        }
    );
    let key = jwk.to_cose_key().expect("COSE 鍵へ変換できる");
    match key {
        CoseKey::Ec2 { curve, x, y, .. } => {
            assert_eq!(curve, EcCurve::P256);
            assert_eq!(
                encode_hex(&x),
                "97cb45ae1c7edf8b55de14480910d8f730a40e5a41845e3651051f595016045b"
            );
            assert_eq!(
                encode_hex(&y),
                "f551388dffce93fa3ae336d34e572e3496a31e6b7abfd4c356b53409dbc64430"
            );
        }
        other => panic!("EC2 鍵ではない: {other:?}"),
    }
}

#[test]
fn decode_okp_jwk() {
    let jwk = Jwk::decode(
        r#"{"kty":"OKP","crv":"Ed25519","x":"11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo"}"#,
    )
    .expect("JWK をデコードできる");
    let key = jwk.to_cose_key().expect("COSE 鍵へ変換できる");
    match key {
        CoseKey::Okp { public_key, .. } => {
            assert_eq!(
                encode_hex(&public_key),
                "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
            );
        }
        other => panic!("OKP 鍵ではない: {other:?}"),
    }
}

#[test]
fn canonical_json_sorts_members_and_normalizes_padding() {
    let jwk = Jwk::decode(r#"{"x":"aGVsbG8=","kty":"EC","crv":"P-256","y":"d29ybGQ="}"#)
        .expect("JWK をデコードできる");
    assert_eq!(
        jwk.canonical_json().expect("正規化 JSON を作れる"),
        r#"{"crv":"P-256","kty":"EC","x":"aGVsbG8","y":"d29ybGQ"}"#
    );
    let rsa = Jwk::decode(r#"{"kty":"RSA","n":"AQAB","e":"Aw"}"#).expect("JWK をデコードできる");
    assert_eq!(
        rsa.canonical_json().expect("正規化 JSON を作れる"),
        r#"{"e":"Aw","kty":"RSA","n":"AQAB"}"#
    );
    let okp = Jwk::decode(
        r#"{"kty":"OKP","crv":"Ed25519","x":"11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo"}"#,
    )
    .expect("JWK をデコードできる");
    assert_eq!(
        okp.canonical_json().expect("正規化 JSON を作れる"),
        r#"{"crv":"Ed25519","kty":"OKP","x":"11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo"}"#
    );
}

#[cfg(feature = "aws-lc-rs")]
#[test]
fn thumbprint_matches_the_draft_vector() {
    let vector = DPOP_VECTORS
        .iter()
        .find(|vector| vector.id == "dpop_es256_real_binding")
        .expect("ベクタがある");
    let jwk = Jwk::decode(vector.jwk_thumbprint_input.expect("サムプリント入力がある"))
        .expect("JWK をデコードできる");
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
fn matches_public_key_compares_coordinates() {
    let public_key = CoseKey::ec2(
        EcCurve::P256,
        decode_hex(ES256_PUBLIC_KEY_X_HEX),
        decode_hex(ES256_PUBLIC_KEY_Y_HEX),
    );
    let jwk = Jwk::Ec {
        curve: String::from("P-256"),
        x: base64url(&decode_hex(ES256_PUBLIC_KEY_X_HEX)),
        y: base64url(&decode_hex(ES256_PUBLIC_KEY_Y_HEX)),
    };
    assert_eq!(jwk.matches_public_key(&public_key), Ok(true));
    let other = Jwk::Ec {
        curve: String::from("P-256"),
        x: base64url(&[1u8; 32]),
        y: base64url(&[2u8; 32]),
    };
    assert_eq!(other.matches_public_key(&public_key), Ok(false));
    assert_eq!(
        Jwk::Rsa {
            n: String::from("AQAB"),
            e: String::from("Aw"),
        }
        .matches_public_key(&public_key),
        Err(JwkError::UnsupportedOperation)
    );
}

#[test]
fn jwk_errors() {
    assert!(matches!(
        Jwk::decode(r#"{"kty":"oct","k":"AQAB"}"#),
        Err(JwkError::UnsupportedKeyType(key_type)) if key_type == "oct"
    ));
    // 曲線の検証は COSE 鍵への変換時に行う
    let unsupported =
        Jwk::decode(r#"{"kty":"EC","crv":"P-999","x":"AQ","y":"AQ"}"#).expect("デコードできる");
    assert_eq!(
        unsupported.to_cose_key(),
        Err(JwkError::UnsupportedCurve(String::from("P-999")))
    );
    assert!(matches!(
        Jwk::decode(r#"{"kty":"EC","crv":"P-256","x":"AQ"}"#),
        Err(JwkError::Json(_))
    ));
    // base64url の検証は値を使うときに行う
    let invalid_base64 =
        Jwk::decode(r#"{"kty":"EC","crv":"P-256","x":"***","y":"AQ"}"#).expect("デコードできる");
    assert_eq!(invalid_base64.to_cose_key(), Err(JwkError::InvalidBase64));
    assert_eq!(
        invalid_base64.canonical_json(),
        Err(JwkError::InvalidBase64)
    );
    assert!(matches!(
        Jwk::decode(r#"{"kty":1}"#),
        Err(JwkError::Json(_))
    ));
    assert!(matches!(Jwk::decode("not json"), Err(JwkError::Json(_))));
    assert_eq!(
        Jwk::Rsa {
            n: String::from("AQAB"),
            e: String::from("Aw"),
        }
        .to_cose_key(),
        Err(JwkError::UnsupportedOperation)
    );
}
