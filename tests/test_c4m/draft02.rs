//! draft-ietf-moq-c4m-02 付録 A の公式ベクタのテスト
//!
//! `tests/test_c4m/vectors_draft02.rs` が保持する公式のバイト列を直接デコードし、
//! 実暗号 (aws-lc-rs) で MAC / 署名を検証する。自己生成したトークンだけの
//! ラウンドトリップにしないため、`payload_cbor_hex` / `cose_hex` / `cose_b64` と
//! 公式の期待値をそのまま使う。
//!
//! `claims_edn` は CBOR の Extended Diagnostic Notation であり JSON の claim 表現
//! ではないため、このテストでも JSON として解釈しない。規範的な claim set は
//! `payload_cbor_hex` である。

use base64ct::{Base64UrlUnpadded, Encoding};
use shiguredo_moqt::c4m::MoqtAction;
use shiguredo_moqt::c4m::cat::{
    CatClaims, CatError, CatToken, ClaimValidationError, ClaimValidationOptions, TokenFormat,
};
use shiguredo_moqt::c4m::cbor::{self, Value};
use shiguredo_moqt::c4m::cose::{
    Algorithm, CoseEncodingOptions, CoseError, CoseMessage, TAG_COSE_MAC0, TAG_COSE_SIGN1,
};

use super::helpers::{decode_hex, encode_hex};
use super::vectors_draft02::*;

#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::MoqtClaim;
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::cat::{CAT_CONTENT_TYPE, CatTokenBuilder, VerifyOptions};
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::crypto::aws_lc_rs::AwsLcRsCrypto;
#[cfg(feature = "aws-lc-rs")]
use shiguredo_moqt::c4m::crypto::{CoseKey, CryptoError, EcCurve};

/// 公式ベクタの payload は claims の CBOR なのでデコードして `CatClaims` にする
fn decode_claims(payload_cbor_hex: &str) -> CatClaims {
    let value =
        cbor::decode(&decode_hex(payload_cbor_hex)).expect("claims の CBOR をデコードできる");
    CatClaims::decode(&value).expect("claims をデコードできる")
}

/// claims を再エンコードした CBOR の hex を返す
fn reencode_claims(claims: &CatClaims) -> String {
    encode_hex(
        &cbor::encode(&claims.encode().expect("claims をエンコードできる"))
            .expect("claims の CBOR をエンコードできる"),
    )
}

/// テスト用の namespace をバイト列の配列へ変換する
fn namespace_bytes<'a>(namespace: &'a [&'a str]) -> Vec<&'a [u8]> {
    namespace.iter().map(|field| field.as_bytes()).collect()
}

/// 付録 A.1 の HMAC-SHA256 鍵
#[cfg(feature = "aws-lc-rs")]
fn hmac_key() -> CoseKey {
    CoseKey::symmetric(decode_hex(DRAFT_02_HMAC_SHA256_KEY_HEX))
}

/// 付録 A.1 の ES256 公開鍵
#[cfg(feature = "aws-lc-rs")]
fn es256_public_key() -> CoseKey {
    CoseKey::ec2(
        EcCurve::P256,
        decode_hex(DRAFT_02_ES256_PUBLIC_KEY_X_HEX),
        decode_hex(DRAFT_02_ES256_PUBLIC_KEY_Y_HEX),
    )
}

/// 公式ベクタの `algorithm_id` に対応する COSE アルゴリズムを返す
fn expected_algorithm(algorithm_id: i64) -> Algorithm {
    Algorithm::from_identifier(algorithm_id).expect("既知の COSE アルゴリズムである")
}

/// 公式ベクタの `algorithm` 名と COSE アルゴリズムの対応を検証する
fn assert_algorithm_name(name: &str, algorithm: Algorithm) {
    match (name, algorithm) {
        ("HMAC-SHA256", Algorithm::HmacSha256) | ("ES256", Algorithm::Es256) => {}
        _ => panic!("公式ベクタのアルゴリズム名と識別子が一致しない: {name} / {algorithm:?}"),
    }
}

/// A.2 の公式 payload が規範的な claim set としてデコードできることを固定する
#[test]
fn claim_vectors_decode_payload_cbor() {
    for vector in DRAFT_02_CLAIM_VECTORS {
        let claims = decode_claims(vector.payload_cbor_hex);
        // claims_edn は参考値であり JSON claim 表現ではないが、公式の記載を
        // 保持していることは確認する
        assert!(
            vector.claims_edn.starts_with('{') && vector.claims_edn.ends_with('}'),
            "ベクタ {} の claims_edn が EDN の map ではない",
            vector.id
        );

        // 明示的に型付きで解釈する moqt claim を持つのは cbor_catu_with_moqt だけである
        if vector.id == "cbor_catu_with_moqt" {
            let moqt = claims.moqt.as_ref().expect("moqt クレームがある");
            assert_eq!(moqt.scopes.len(), 1);
            assert_eq!(moqt.scopes[0].actions, [4, 7]);
            assert!(claims.authorize(
                MoqtAction::Subscribe,
                &namespace_bytes(&["example.com", "vod"]),
                b"stream",
            ));
            assert!(claims.authorize(
                MoqtAction::Fetch,
                &namespace_bytes(&["example.com", "vod"]),
                b"stream",
            ));
            assert!(!claims.authorize(
                MoqtAction::Publish,
                &namespace_bytes(&["example.com", "vod"]),
                b"stream",
            ));
        } else {
            assert!(
                claims.moqt.is_none(),
                "ベクタ {} に moqt クレームは無い",
                vector.id
            );
        }
    }
}

/// A.2 のうち既知の CWT claim が型付きでデコードされることを固定する
#[test]
fn claim_vectors_decode_known_cwt_claims() {
    let issuer_only = DRAFT_02_CLAIM_VECTORS
        .iter()
        .find(|vector| vector.id == "cbor_issuer_only")
        .expect("ベクタがある");
    let claims = decode_claims(issuer_only.payload_cbor_hex);
    assert_eq!(claims.issuer.as_deref(), Some("https://auth.example.com"));

    let core = DRAFT_02_CLAIM_VECTORS
        .iter()
        .find(|vector| vector.id == "cbor_core_claims")
        .expect("ベクタがある");
    let claims = decode_claims(core.payload_cbor_hex);
    assert_eq!(claims.issuer.as_deref(), Some("https://auth.example.com"));
    assert_eq!(
        claims.audience,
        vec![String::from("https://relay.example.com")]
    );
    assert_eq!(claims.expiration, Some(1700086400.0));
    assert_eq!(claims.not_before, Some(1700000000.0));
    assert_eq!(claims.cwt_id.as_deref(), Some(&b"test-token-001"[..]));

    // CAT 固有のクレームは型付きで解釈せず raw に保持する
    let alpn = DRAFT_02_CLAIM_VECTORS
        .iter()
        .find(|vector| vector.id == "cbor_alpn")
        .expect("ベクタがある");
    // cbor_catu_with_moqt は catu と moqt を同じ claims set に持つ
    let mixed = DRAFT_02_CLAIM_VECTORS
        .iter()
        .find(|vector| vector.id == "cbor_catu_with_moqt")
        .expect("ベクタがある");
    let claims = decode_claims(mixed.payload_cbor_hex);
    assert_eq!(
        claims.get(312),
        Some(&Value::Map(vec![
            (
                Value::integer(1),
                Value::Map(vec![(
                    Value::integer(0),
                    Value::TextString(String::from("example.com")),
                )]),
            ),
            (
                Value::integer(3),
                Value::Map(vec![(
                    Value::integer(1),
                    Value::TextString(String::from("/vod/")),
                )]),
            ),
        ]))
    );
    // moqt は型付きでデコードされ、action 4 / 7 が namespace (example.com, vod) で認可される
    let moqt = claims.moqt.as_ref().expect("moqt がある");
    let namespace: Vec<&[u8]> = vec![b"example.com", b"vod"];
    assert!(moqt.authorize(MoqtAction::Subscribe, &namespace, b""));
    assert!(moqt.authorize(MoqtAction::Fetch, &namespace, b""));

    let claims = decode_claims(alpn.payload_cbor_hex);
    assert!(claims.get(314).is_some(), "catalpn が raw に残る");
}

/// A.2 の draft-02 で修正された CAT 固有クレームの形を固定する
#[test]
fn claim_vectors_keep_draft_02_cat_encodings() {
    // catv (310) と catu (312) の組。draft-01 の旧い値表現ではない
    let version_uri = DRAFT_02_CLAIM_VECTORS
        .iter()
        .find(|vector| vector.id == "cbor_cat_version_uri")
        .expect("ベクタがある");
    let claims = decode_claims(version_uri.payload_cbor_hex);
    assert_eq!(claims.get(310), Some(&Value::integer(1)));
    assert_eq!(
        claims.get(312),
        Some(&Value::Map(vec![(
            Value::integer(1),
            Value::Map(vec![(
                Value::integer(0),
                Value::TextString(String::from("example.com")),
            )]),
        )])),
        "catu は host exact の map である"
    );

    // catnip (311) は IP / CIDR に CBOR タグ 52 を付け、ASN と ASN 範囲は整数の
    // 配列で表す
    let network = DRAFT_02_CLAIM_VECTORS
        .iter()
        .find(|vector| vector.id == "cbor_network_identifiers")
        .expect("ベクタがある");
    let claims = decode_claims(network.payload_cbor_hex);
    let items = claims
        .get(311)
        .expect("catnip がある")
        .as_array()
        .expect("catnip は配列である");
    assert_eq!(items.len(), 4);
    assert!(matches!(
        &items[0],
        Value::Tag(52, inner)
            if matches!(inner.as_ref(), Value::ByteString(bytes) if bytes.as_slice() == [0xc0, 0xa8, 0x01, 0x64])
    ));
    assert_eq!(
        items[1],
        Value::Tag(
            52,
            Box::new(Value::Array(vec![
                Value::integer(8),
                Value::ByteString(vec![0x0a]),
            ]))
        ),
        "CIDR は [prefix length, address] のタグ 52 である"
    );
    assert_eq!(items[2], Value::integer(64512));
    assert_eq!(
        items[3],
        Value::Array(vec![Value::integer(64512), Value::integer(64768)])
    );

    // catu (312) の host exact / path prefix / extension exact
    let uri_match = DRAFT_02_CLAIM_VECTORS
        .iter()
        .find(|vector| vector.id == "cbor_uri_match_rules")
        .expect("ベクタがある");
    let claims = decode_claims(uri_match.payload_cbor_hex);
    let rules = claims
        .get(312)
        .expect("catu がある")
        .as_map()
        .expect("catu は map である");
    assert_eq!(
        rules,
        &[
            (
                Value::integer(1),
                Value::Map(vec![(
                    Value::integer(0),
                    Value::TextString(String::from("example.com")),
                )]),
            ),
            (
                Value::integer(3),
                Value::Map(vec![(
                    Value::integer(1),
                    Value::TextString(String::from("/vod/")),
                )]),
            ),
            (
                Value::integer(8),
                Value::Map(vec![(
                    Value::integer(0),
                    Value::TextString(String::from(".m3u8")),
                )]),
            ),
        ]
    );

    // 地理クレームの label
    let geographic = DRAFT_02_CLAIM_VECTORS
        .iter()
        .find(|vector| vector.id == "cbor_geographic_claims")
        .expect("ベクタがある");
    let claims = decode_claims(geographic.payload_cbor_hex);
    assert_eq!(
        claims.get(282),
        Some(&Value::TextString(String::from("9q8yyk")))
    );
    assert_eq!(
        claims.get(316),
        Some(&Value::Array(vec![
            Value::TextString(String::from("US")),
            Value::TextString(String::from("CA")),
        ]))
    );
    assert_eq!(
        claims.get(317),
        Some(&Value::Array(vec![Value::Array(vec![
            Value::Float(37.7749),
            Value::Float(-122.4194),
            Value::integer(100),
        ])]))
    );
    assert_eq!(
        claims.get(318),
        Some(&Value::Array(vec![Value::integer(10), Value::integer(5),]))
    );

    // catalpn (314) はバイト文字列の配列である
    let alpn = DRAFT_02_CLAIM_VECTORS
        .iter()
        .find(|vector| vector.id == "cbor_alpn")
        .expect("ベクタがある");
    // cbor_catu_with_moqt は catu と moqt を同じ claims set に持つ
    let mixed = DRAFT_02_CLAIM_VECTORS
        .iter()
        .find(|vector| vector.id == "cbor_catu_with_moqt")
        .expect("ベクタがある");
    let claims = decode_claims(mixed.payload_cbor_hex);
    assert_eq!(
        claims.get(312),
        Some(&Value::Map(vec![
            (
                Value::integer(1),
                Value::Map(vec![(
                    Value::integer(0),
                    Value::TextString(String::from("example.com")),
                )]),
            ),
            (
                Value::integer(3),
                Value::Map(vec![(
                    Value::integer(1),
                    Value::TextString(String::from("/vod/")),
                )]),
            ),
        ]))
    );
    // moqt は型付きでデコードされ、action 4 / 7 が namespace (example.com, vod) で認可される
    let moqt = claims.moqt.as_ref().expect("moqt がある");
    let namespace: Vec<&[u8]> = vec![b"example.com", b"vod"];
    assert!(moqt.authorize(MoqtAction::Subscribe, &namespace, b""));
    assert!(moqt.authorize(MoqtAction::Fetch, &namespace, b""));

    let claims = decode_claims(alpn.payload_cbor_hex);
    assert_eq!(
        claims.get(314),
        Some(&Value::Array(vec![
            Value::ByteString(b"moq-00".to_vec()),
            Value::ByteString(b"h3".to_vec()),
        ]))
    );
}

/// A.3 の公式 COSE 構造をデコードし、ヘッダ・payload・MAC / 署名を固定する
#[test]
fn token_vectors_decode_cose() {
    for vector in DRAFT_02_TOKEN_VECTORS {
        assert!(vector.valid, "ベクタ {} は valid である", vector.id);

        let cose_bytes = decode_hex(vector.cose_hex);
        let token = CatToken::decode(&cose_bytes)
            .unwrap_or_else(|error| panic!("ベクタ {} のデコードに失敗した: {error}", vector.id));
        let expected_algorithm = expected_algorithm(vector.algorithm_id);
        assert_algorithm_name(vector.algorithm, expected_algorithm);

        let expected_format = if expected_algorithm.is_mac() {
            TokenFormat::CoseMac0
        } else {
            TokenFormat::CoseSign1
        };
        assert_eq!(
            token.format(),
            expected_format,
            "ベクタ {} の COSE 構造",
            vector.id
        );
        assert_eq!(
            token.header().algorithm,
            Some(expected_algorithm),
            "ベクタ {} の alg",
            vector.id
        );
        assert_eq!(
            token.header().algorithm_identifier,
            Some(vector.algorithm_id),
            "ベクタ {} の alg 識別子",
            vector.id
        );
        assert_eq!(
            token.header().typ,
            Some(Value::TextString(String::from("CAT"))),
            "ベクタ {} の typ",
            vector.id
        );
        assert_eq!(
            encode_hex(token.protected_header()),
            vector.header_cbor_hex,
            "ベクタ {} の protected ヘッダ",
            vector.id
        );
        assert_eq!(
            encode_hex(token.payload()),
            vector.payload_cbor_hex,
            "ベクタ {} の claims",
            vector.id
        );
        let expected_tag = match (vector.tag_hex, vector.signature_hex) {
            (Some(tag), None) => tag,
            (None, Some(signature)) => signature,
            _ => panic!("ベクタ {} の MAC / 署名の記載が不正", vector.id),
        };
        assert_eq!(
            token.signature(),
            &decode_hex(expected_tag)[..],
            "ベクタ {} の MAC / 署名",
            vector.id
        );

        // 公式の COSE 構造は COSE タグ (17 / 18) を持ち、CWT タグ (61) は持たない
        let message = CoseMessage::decode(&cose_bytes).expect("COSE メッセージをデコードできる");
        match (message, expected_format) {
            (CoseMessage::Mac0(message), TokenFormat::CoseMac0) => {
                assert!(message.cose_tagged, "ベクタ {} の COSE タグ", vector.id);
                assert!(!message.cwt_tagged, "ベクタ {} の CWT タグ", vector.id);
                assert_eq!(encode_hex(&message.tag), expected_tag);
                assert_eq!(
                    message.payload.as_deref(),
                    Some(&decode_hex(vector.payload_cbor_hex)[..])
                );
            }
            (CoseMessage::Sign1(message), TokenFormat::CoseSign1) => {
                assert!(message.cose_tagged, "ベクタ {} の COSE タグ", vector.id);
                assert!(!message.cwt_tagged, "ベクタ {} の CWT タグ", vector.id);
                assert_eq!(encode_hex(&message.signature), expected_tag);
                assert_eq!(
                    message.payload.as_deref(),
                    Some(&decode_hex(vector.payload_cbor_hex)[..])
                );
            }
            _ => panic!("ベクタ {} の COSE 構造と alg が一致しない", vector.id),
        }

        // 公式の payload を claim set として再エンコードしても同じバイト列になる
        // (claim の取り違えや label の読み替えが無いことの確認)
        assert_eq!(
            reencode_claims(token.claims()),
            vector.payload_cbor_hex,
            "ベクタ {} の claims の再エンコード",
            vector.id
        );

        // cose_b64 は cose_hex のパディング無し base64url である
        assert_eq!(
            Base64UrlUnpadded::decode_vec(vector.cose_b64)
                .expect("cose_b64 をデコードできる")
                .as_slice(),
            cose_bytes.as_slice(),
            "ベクタ {} の cose_b64",
            vector.id
        );
        let token_from_b64 = CatToken::decode(vector.cose_b64.as_bytes()).unwrap_or_else(|error| {
            panic!(
                "ベクタ {} の cose_b64 のデコードに失敗した: {error}",
                vector.id
            )
        });
        assert_eq!(token_from_b64.format(), expected_format);
        assert_eq!(
            encode_hex(token_from_b64.payload()),
            vector.payload_cbor_hex
        );
        assert_eq!(encode_hex(token_from_b64.signature()), expected_tag);

        // 公式の鍵は付録 A.1 の鍵と一致する
        match vector.algorithm {
            "HMAC-SHA256" => assert_eq!(
                vector.key_hex.expect("HMAC の鍵がある"),
                DRAFT_02_HMAC_SHA256_KEY_HEX
            ),
            "ES256" => {
                assert_eq!(
                    vector.private_key_hex.expect("ES256 の秘密鍵がある"),
                    DRAFT_02_ES256_PRIVATE_KEY_HEX
                );
                assert_eq!(
                    vector.public_key_x_hex.expect("ES256 の x 座標がある"),
                    DRAFT_02_ES256_PUBLIC_KEY_X_HEX
                );
                assert_eq!(
                    vector.public_key_y_hex.expect("ES256 の y 座標がある"),
                    DRAFT_02_ES256_PUBLIC_KEY_Y_HEX
                );
            }
            other => panic!("未知のアルゴリズム名: {other}"),
        }
    }
}

/// A.4 の DPoP バインディングの claim と COSE 構造を固定する
#[test]
fn dpop_vectors_decode_cose() {
    for vector in DRAFT_02_DPOP_VECTORS {
        assert!(
            vector.claims_edn.starts_with('{') && vector.claims_edn.ends_with('}'),
            "ベクタ {} の claims_edn が EDN の map ではない",
            vector.id
        );
        let token = CatToken::decode(&decode_hex(vector.cose_hex))
            .unwrap_or_else(|error| panic!("ベクタ {} のデコードに失敗した: {error}", vector.id));
        assert_eq!(
            encode_hex(token.payload()),
            vector.payload_cbor_hex,
            "ベクタ {} の claims",
            vector.id
        );
        assert_eq!(
            reencode_claims(token.claims()),
            vector.payload_cbor_hex,
            "ベクタ {} の claims の再エンコード",
            vector.id
        );

        // A.4 は IANA 登録の jkt (323) を使う。draft-01 の 3 には写さない
        let confirmation = token
            .claims()
            .confirmation
            .as_ref()
            .unwrap_or_else(|| panic!("ベクタ {} に cnf が無い", vector.id));
        assert_eq!(
            confirmation.jwk_thumbprint.as_deref(),
            Some(&decode_hex(vector.cnf_jkt_hex)[..]),
            "ベクタ {} の jkt (confirmation key 323)",
            vector.id
        );
        assert!(
            confirmation.c4m_draft_jwk_thumbprint.is_none(),
            "ベクタ {} は旧 label 3 を使わない",
            vector.id
        );
        assert_eq!(confirmation.ckt(), None);

        let catdpop = token
            .claims()
            .catdpop
            .as_ref()
            .unwrap_or_else(|| panic!("ベクタ {} に catdpop が無い", vector.id));
        assert_eq!(
            catdpop.window_seconds,
            Some(vector.window_seconds),
            "ベクタ {} の catdpop ウィンドウ",
            vector.id
        );
        assert_eq!(
            catdpop.honor_jti, vector.honor_jti,
            "ベクタ {} の catdpop honor_jti",
            vector.id
        );

        // サムプリントの計算方法の記載と入力の有無を固定する
        match vector.id {
            "dpop_jwk_binding" => {
                assert!(vector.cnf_jkt_source.is_some());
                assert!(vector.jwk_thumbprint_input.is_none());
                assert_eq!(token.format(), TokenFormat::CoseMac0);
            }
            "dpop_no_jti" => {
                assert!(vector.cnf_jkt_source.is_some());
                assert!(vector.jwk_thumbprint_input.is_none());
                assert_eq!(token.format(), TokenFormat::CoseMac0);
            }
            "dpop_es256_real_binding" => {
                assert!(vector.cnf_jkt_source.is_none());
                assert!(vector.jwk_thumbprint_input.is_some());
                assert_eq!(token.format(), TokenFormat::CoseSign1);
                // 公開鍵は付録 A.1 の ES256 鍵と一致する
                assert_eq!(
                    vector.public_key_x_hex.expect("x 座標がある"),
                    DRAFT_02_ES256_PUBLIC_KEY_X_HEX
                );
                assert_eq!(
                    vector.public_key_y_hex.expect("y 座標がある"),
                    DRAFT_02_ES256_PUBLIC_KEY_Y_HEX
                );
            }
            other => panic!("未知の DPoP ベクタ: {other}"),
        }

        assert_eq!(
            Base64UrlUnpadded::decode_vec(vector.cose_b64)
                .expect("cose_b64 をデコードできる")
                .as_slice(),
            decode_hex(vector.cose_hex).as_slice(),
            "ベクタ {} の cose_b64",
            vector.id
        );
    }
}

/// A.5 のスコープの claim をデコードし、公式の認可テストを固定する
#[test]
fn scope_vectors_authorize_from_cose_tokens() {
    for vector in DRAFT_02_SCOPE_VECTORS {
        let token = CatToken::decode(&decode_hex(vector.cose_hex))
            .unwrap_or_else(|error| panic!("ベクタ {} のデコードに失敗した: {error}", vector.id));
        assert_eq!(token.format(), TokenFormat::CoseMac0, "{}", vector.id);
        assert_eq!(
            encode_hex(token.payload()),
            vector.payload_cbor_hex,
            "ベクタ {} の claims",
            vector.id
        );
        let claims = token.claims();
        assert_eq!(
            claims.moqt_reval, vector.moqt_reval,
            "ベクタ {} の moqt-reval",
            vector.id
        );
        // draft-02 §2.1 の map 形式と Private Use の暫定 label をそのまま再現できる
        assert_eq!(
            reencode_claims(claims),
            vector.payload_cbor_hex,
            "ベクタ {} の claims の再エンコード",
            vector.id
        );

        // cose_b64 は cose_hex のパディング無し base64url である
        assert_eq!(
            Base64UrlUnpadded::decode_vec(vector.cose_b64)
                .expect("cose_b64 をデコードできる")
                .as_slice(),
            decode_hex(vector.cose_hex).as_slice(),
            "ベクタ {} の cose_b64",
            vector.id
        );
        // compact 形式の既存ベクタと同じ claim set であることを固定する
        let compact = super::vectors::SCOPE_VECTORS
            .iter()
            .find(|old| old.id == vector.id)
            .unwrap_or_else(|| panic!("compact ベクタ {} がある", vector.id));
        assert_eq!(
            decode_claims(&encode_hex(token.payload())),
            decode_claims(compact.payload_hex),
            "ベクタ {} の claim set が compact 形式のベクタと一致しない",
            vector.id
        );

        // cose_b64 からも同じ claim set をデコードできる
        let from_b64 = CatToken::decode(vector.cose_b64.as_bytes())
            .unwrap_or_else(|error| panic!("ベクタ {} の b64 デコード失敗: {error}", vector.id));
        assert_eq!(
            encode_hex(from_b64.payload()),
            vector.payload_cbor_hex,
            "ベクタ {} の b64 経由の claims",
            vector.id
        );

        for test in vector.authorization_tests {
            // action 0 は §2.1 Table 1 の既知 action ではないため
            // MoqtAction に変換せず、専用のテストで公式の保持だけを確認する
            if test.action == 0 {
                assert_eq!(vector.id, "moqt_admin_wildcard");
                assert!(
                    test.expected,
                    "公式の action 0 の期待値は true のまま保持する"
                );
                continue;
            }
            let action = MoqtAction::from_key(test.action).unwrap_or_else(|| {
                panic!("ベクタ {} のアクションが不正: {}", vector.id, test.action)
            });
            let namespace = namespace_bytes(test.namespace);
            assert_eq!(
                claims.authorize(action, &namespace, test.track.as_bytes()),
                test.expected,
                "ベクタ {} の認可判定 (action={}, namespace={:?}, track={})",
                vector.id,
                test.action,
                test.namespace,
                test.track
            );
        }
    }
}

/// A.5 の `moqt_admin_wildcard` に残る action 0 を既知 action から分離する
#[test]
fn scope_admin_wildcard_action_zero_is_not_mapped() {
    let vector = DRAFT_02_SCOPE_VECTORS
        .iter()
        .find(|vector| vector.id == "moqt_admin_wildcard")
        .expect("ベクタがある");
    // 公式の authorization_tests は action 0 の期待値 true を保持している
    assert!(
        vector
            .authorization_tests
            .iter()
            .any(|test| test.action == 0 && test.expected)
    );

    let token = CatToken::decode(&decode_hex(vector.cose_hex)).expect("デコードできる");
    let claims = token.claims();
    let moqt = claims.moqt.as_ref().expect("moqt クレームがある");
    assert_eq!(
        moqt.scopes[0].actions,
        [0, 1, 2, 3, 4, 5, 6, 7, 8],
        "公式 payload の action 0 を保持する"
    );
    assert!(moqt.scopes[0].namespace.is_empty());
    assert_eq!(moqt.scopes[0].track, None);

    // action 0 は §2.1 Table 1 に無い。SETUP の alias に戻さない
    assert_eq!(MoqtAction::from_key(0), None);
    assert_eq!(MoqtAction::Setup.key(), 1);

    // 既知 action 1 〜 8 は通常どおり認可する
    for key in 1..=8 {
        let action = MoqtAction::from_key(key).expect("既知のアクションである");
        assert!(
            claims.authorize(action, &namespace_bytes(&["any.namespace"]), b"any-track"),
            "action {key} は認可される"
        );
    }
    // 公式の admin ベクタは 9 (SUBSCRIBE_TRACKS) を含まないため認可しない
    assert!(!claims.authorize(
        MoqtAction::SubscribeTracks,
        &namespace_bytes(&["any.namespace"]),
        b"any-track",
    ));
}

/// A.5 に含まれない SUBSCRIBE_TRACKS=9 の許可を補助 fixture で固定する
///
/// 公式ベクタは変更しない。公式の admin ベクタが action 9 を認可しないことは
/// `scope_admin_wildcard_action_zero_is_not_mapped` が固定する。
#[cfg(feature = "aws-lc-rs")]
#[test]
fn subscribe_tracks_is_authorized_by_auxiliary_fixture() {
    // 既存の既知バイト列 {0: [[[9]]]} を claim として使い、公式ベクタとは
    // 独立した合成 token を作る。action 9 だけを持つ claim である
    let claim_bytes = decode_hex("a10081818109");
    let claim_value = cbor::decode(&claim_bytes).expect("CBOR をデコードできる");
    let claim = MoqtClaim::decode(&claim_value).expect("claim をデコードできる");
    assert_eq!(claim.scopes[0].actions, [9]);

    let crypto = AwsLcRsCrypto::new();
    let builder = CatTokenBuilder::new()
        .issuer("https://auth.example.com")
        .expiration(1700086400.0)
        .moqt(claim);
    let bytes = builder
        .build_cose(&crypto, &hmac_key())
        .expect("合成 token を発行できる");
    let token = CatToken::decode(&bytes).expect("合成 token をデコードできる");
    assert_eq!(token.format(), TokenFormat::CoseMac0);
    token
        .verify(&crypto, &hmac_key())
        .expect("合成 token の MAC を検証できる");

    let namespace = namespace_bytes(&["example.com", "alice"]);
    assert!(
        token
            .claims()
            .authorize(MoqtAction::SubscribeTracks, &namespace, b"video-hd")
    );
    assert!(
        !token
            .claims()
            .authorize(MoqtAction::Setup, &namespace, b"video-hd")
    );
    assert!(
        !token
            .claims()
            .authorize(MoqtAction::Publish, &namespace, b"video-hd")
    );
}

/// A.6 の Composite claim を raw claim として保持し、認可には使わない
#[test]
fn composite_vectors_are_kept_as_raw_claims() {
    for vector in DRAFT_02_COMPOSITE_VECTORS {
        // すべての Composite claim の key は暫定の 0 である
        assert_eq!(vector.claim_key, 0, "ベクタ {}", vector.id);
        assert!(
            matches!(vector.operator, "OR" | "AND" | "NOR"),
            "ベクタ {} の operator",
            vector.id
        );
        assert!(
            vector.claims_edn.starts_with('{') && vector.claims_edn.ends_with('}'),
            "ベクタ {} の claims_edn",
            vector.id
        );

        let token = CatToken::decode(&decode_hex(vector.cose_hex))
            .unwrap_or_else(|error| panic!("ベクタ {} のデコードに失敗した: {error}", vector.id));
        assert_eq!(token.format(), TokenFormat::CoseMac0, "{}", vector.id);
        assert_eq!(
            encode_hex(token.payload()),
            vector.payload_cbor_hex,
            "ベクタ {} の claims",
            vector.id
        );

        // Composite claim (key 0) は raw に保持される。型付きクレームの再エンコードは
        // キー順が変わるため、公式バイト列との一致は payload の比較で確認する
        assert_eq!(
            token
                .claims()
                .raw
                .iter()
                .filter(|(key, _)| key.as_int() == Some(0))
                .count(),
            1,
            "ベクタ {} の Composite claim が raw に残る",
            vector.id
        );
        // cose_b64 は cose_hex のパディング無し base64url である
        assert_eq!(
            Base64UrlUnpadded::decode_vec(vector.cose_b64)
                .expect("cose_b64 をデコードできる")
                .as_slice(),
            decode_hex(vector.cose_hex).as_slice(),
            "ベクタ {} の cose_b64",
            vector.id
        );
        let from_b64 = CatToken::decode(vector.cose_b64.as_bytes())
            .unwrap_or_else(|error| panic!("ベクタ {} の b64 デコード失敗: {error}", vector.id));
        assert_eq!(
            encode_hex(from_b64.payload()),
            vector.payload_cbor_hex,
            "ベクタ {} の b64 経由の claims",
            vector.id
        );

        let claims = token.claims();
        // key 0 は既知の claim key ではないため、raw にだけ保持する
        assert!(
            claims.moqt.is_none(),
            "ベクタ {} に型付き moqt は無い",
            vector.id
        );
        assert_eq!(claims.raw.len(), 1, "ベクタ {} の raw claim", vector.id);
        assert_eq!(claims.raw[0].0, Value::integer(vector.claim_key));
        let Value::Array(items) = &claims.raw[0].1 else {
            panic!("ベクタ {} の Composite claim が配列ではない", vector.id);
        };
        let expected_count = if vector.operator == "NOR" { 1 } else { 2 };
        assert_eq!(
            items.len(),
            expected_count,
            "ベクタ {} の Composite claim の要素数",
            vector.id
        );
        // raw に保持した値が公式 payload の key 0 の値と一致する
        let payload_value =
            cbor::decode(&decode_hex(vector.payload_cbor_hex)).expect("CBOR をデコードできる");
        let expected_raw = payload_value
            .as_map()
            .expect("claims は map である")
            .iter()
            .find_map(|(key, value)| {
                if key.as_int() == Some(vector.claim_key) {
                    Some(value)
                } else {
                    None
                }
            })
            .expect("公式 payload に Composite claim がある");
        assert_eq!(
            claims.get(vector.claim_key),
            Some(expected_raw),
            "ベクタ {} の raw claim の値",
            vector.id
        );

        // Composite の評価は実装しない。型付き scope が無いため、どの既知 action も
        // 認可されない (raw claim として保持できることと認可成功を混同しない)
        for action in MoqtAction::ALL {
            assert!(
                !claims.authorize(action, &namespace_bytes(&["any.namespace"]), b"any-track"),
                "ベクタ {} は Composite を評価せず {} を認可しない",
                vector.id,
                action.name()
            );
        }
    }
}

/// A.7 の検証ベクタを新しい入力で検証する
#[test]
fn validation_vectors_decode_and_validate() {
    for vector in DRAFT_02_VALIDATION_VECTORS {
        let token = CatToken::decode(vector.token.as_bytes())
            .unwrap_or_else(|error| panic!("ベクタ {} のデコードに失敗した: {error}", vector.id));
        assert_eq!(token.format(), TokenFormat::CoseMac0, "{}", vector.id);
        assert_eq!(
            token.header().algorithm,
            Some(Algorithm::HmacSha256),
            "ベクタ {} の alg",
            vector.id
        );
        assert_eq!(token.header().algorithm_identifier, Some(5));

        // 期待結果の記載とエラー名の有無は一致している
        if vector.expected_error.is_none() {
            assert_eq!(vector.expected_result, "valid", "ベクタ {}", vector.id);
        } else {
            assert_eq!(vector.expected_result, "error", "ベクタ {}", vector.id);
        }

        // reference_time を持たないベクタ (改ざん・誤鍵・アルゴリズム不一致) は
        // 署名 / 構造の異常を見るものであり、claim の時刻検証は対象外である
        let Some(reference_time) = vector.reference_time else {
            continue;
        };
        let options = ClaimValidationOptions {
            reference_time_seconds: reference_time,
            clock_tolerance_seconds: 0.0,
            expected_issuers: vector.expected_issuers,
            expected_audiences: vector.expected_audiences,
            // validation ベクタは moqt-reval を持たないため、再検証能力は結果に影響しない
            revalidation_interval_seconds: None,
        };
        let result = token.claims().validate(&options);
        match vector.expected_error {
            None => assert_eq!(result, Ok(()), "ベクタ {}", vector.id),
            Some("TokenExpired") => assert_eq!(
                result,
                Err(ClaimValidationError::Expired),
                "ベクタ {}",
                vector.id
            ),
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
            // 署名 / 構造の異常は claim の検証では検出しない
            Some("SignatureVerificationFailed" | "InvalidTokenFormat") => {
                assert_eq!(result, Ok(()), "ベクタ {}", vector.id)
            }
            Some(error) => panic!("未知の expected_error: {error} (ベクタ {})", vector.id),
        }
    }
}

/// A.7 の改ざんベクタが公式の MAC の 1 バイトだけを反転していることを固定する
#[test]
fn validation_tampered_signature_vector_flips_one_mac_byte() {
    let vector = DRAFT_02_VALIDATION_VECTORS
        .iter()
        .find(|vector| vector.id == "invalid_tampered_signature")
        .expect("ベクタがある");
    let original = decode_hex(vector.original_cose_hex.expect("改ざん前の COSE がある"));
    let tampered = Base64UrlUnpadded::decode_vec(vector.token).expect("token をデコードできる");

    // HMAC-SHA256 の MAC は末尾の 32 バイトである
    assert_eq!(tampered.len(), original.len());
    let mac_start = original.len() - 32;
    assert_eq!(&tampered[..mac_start], &original[..mac_start]);
    assert_eq!(tampered[mac_start] ^ 0xff, original[mac_start]);
    assert_eq!(&tampered[mac_start + 1..], &original[mac_start + 1..]);
}

/// A.7 のアルゴリズム不一致ベクタの記載を固定する
#[test]
fn validation_algorithm_mismatch_vector_expects_different_algorithm() {
    let vector = DRAFT_02_VALIDATION_VECTORS
        .iter()
        .find(|vector| vector.id == "invalid_algorithm_mismatch")
        .expect("ベクタがある");
    assert_eq!(vector.token_algorithm_id, Some(5));
    assert_eq!(vector.verifier_algorithm_id, Some(-7));
    assert_eq!(vector.expected_error, Some("InvalidTokenFormat"));

    let token = CatToken::decode(vector.token.as_bytes()).expect("デコードできる");
    assert_eq!(token.format(), TokenFormat::CoseMac0);
    assert_eq!(token.header().algorithm_identifier, Some(5));
}

/// A.3 の COSE_Mac0 のタグを COSE_Sign1 に変えると alg=5 と構造が食い違う
#[test]
fn cose_mac0_tag_with_mac_algorithm_is_rejected_as_sign1() {
    let vector = DRAFT_02_TOKEN_VECTORS
        .iter()
        .find(|vector| vector.id == "token_hmac_minimal")
        .expect("ベクタがある");
    let value = cbor::decode(&decode_hex(vector.cose_hex)).expect("CBOR をデコードできる");
    let Value::Tag(TAG_COSE_MAC0, inner) = value else {
        panic!("公式ベクタは COSE_Mac0 (タグ 17) である");
    };
    // タグだけを COSE_Sign1 (18) に差し替える。protected ヘッダの alg は 5 のまま
    // なので、COSE 構造とアルゴリズム種別が一致しない
    let sign1 = cbor::encode(&Value::Tag(TAG_COSE_SIGN1, inner)).expect("CBOR をエンコードできる");
    assert_eq!(
        CatToken::decode_cose(&sign1),
        Err(CatError::Cose(CoseError::AlgorithmClassMismatch))
    );
}

/// 公式の `claims_edn` を保持していることを確認する
///
/// `claims_edn` は CBOR の EDN であり JSON の claim 表現ではないため、
/// ここでも JSON としては解釈しない。
#[test]
fn official_claims_edn_is_retained() {
    // claims_edn は参考値であるため値の解釈はしないが、公式の構造が保持されている
    // ことを代表的な部分文字列で確認する
    for vector in DRAFT_02_CLAIM_VECTORS {
        assert!(
            vector.claims_edn.starts_with('{') && vector.claims_edn.ends_with('}'),
            "ベクタ {}: {}",
            vector.id,
            vector.claims_edn
        );
    }
    for vector in DRAFT_02_TOKEN_VECTORS {
        assert!(!vector.claims_edn.is_empty(), "ベクタ {}", vector.id);
    }
    for vector in DRAFT_02_DPOP_VECTORS {
        assert!(
            vector.claims_edn.contains("323"),
            "ベクタ {}: {}",
            vector.id,
            vector.claims_edn
        );
    }
    for vector in DRAFT_02_SCOPE_VECTORS {
        assert!(
            vector.claims_edn.contains("moqt"),
            "ベクタ {}: {}",
            vector.id,
            vector.claims_edn
        );
    }
    for vector in DRAFT_02_COMPOSITE_VECTORS {
        assert!(!vector.claims_edn.is_empty(), "ベクタ {}", vector.id);
    }
}

/// A.3 の公式トークンの MAC / 署名を実暗号で検証する
#[cfg(feature = "aws-lc-rs")]
#[test]
fn token_vectors_verify_with_real_crypto() {
    let crypto = AwsLcRsCrypto::new();
    for vector in DRAFT_02_TOKEN_VECTORS {
        let token = CatToken::decode(&decode_hex(vector.cose_hex))
            .unwrap_or_else(|error| panic!("ベクタ {} のデコードに失敗した: {error}", vector.id));
        let key = match vector.algorithm {
            "HMAC-SHA256" => hmac_key(),
            "ES256" => es256_public_key(),
            other => panic!("未知のアルゴリズム名: {other}"),
        };
        token
            .verify(&crypto, &key)
            .unwrap_or_else(|error| panic!("ベクタ {} の検証に失敗した: {error}", vector.id));

        let expected = expected_algorithm(vector.algorithm_id);
        token
            .verify_with(
                &crypto,
                &key,
                &VerifyOptions {
                    expected_algorithm: Some(expected),
                    expected_type: Some(CAT_CONTENT_TYPE),
                },
            )
            .unwrap_or_else(|error| {
                panic!(
                    "ベクタ {} の alg / typ 付き検証に失敗した: {error}",
                    vector.id
                )
            });
    }
}

/// A.4 の公式トークンの MAC / 署名を実暗号で検証する
#[cfg(feature = "aws-lc-rs")]
#[test]
fn dpop_vectors_verify_with_real_crypto() {
    let crypto = AwsLcRsCrypto::new();
    for vector in DRAFT_02_DPOP_VECTORS {
        let token = CatToken::decode(&decode_hex(vector.cose_hex))
            .unwrap_or_else(|error| panic!("ベクタ {} のデコードに失敗した: {error}", vector.id));
        let key = match vector.algorithm {
            Some("ES256") => CoseKey::ec2(
                EcCurve::P256,
                decode_hex(vector.public_key_x_hex.expect("x 座標がある")),
                decode_hex(vector.public_key_y_hex.expect("y 座標がある")),
            ),
            None => hmac_key(),
            Some(other) => panic!("未知のアルゴリズム名: {other}"),
        };
        token
            .verify(&crypto, &key)
            .unwrap_or_else(|error| panic!("ベクタ {} の検証に失敗した: {error}", vector.id));
    }
}

/// A.4 のサムプリントの記載と実計算が一致する
#[cfg(feature = "aws-lc-rs")]
#[test]
fn dpop_vectors_thumbprint_sources_match() {
    use shiguredo_moqt::c4m::crypto::{CoseCrypto, DigestAlgorithm};
    use shiguredo_moqt::c4m::jwk::Jwk;

    let crypto = AwsLcRsCrypto::new();

    // cnf_jkt_source が「SHA-256 of 'test-public-key-material'」であるベクタ
    let no_jti = DRAFT_02_DPOP_VECTORS
        .iter()
        .find(|vector| vector.id == "dpop_no_jti")
        .expect("ベクタがある");
    assert_eq!(
        no_jti.cnf_jkt_source,
        Some("SHA-256 of 'test-public-key-material'")
    );
    let digest = crypto
        .digest(DigestAlgorithm::Sha256, b"test-public-key-material")
        .expect("ハッシュを計算できる");
    assert_eq!(encode_hex(&digest), no_jti.cnf_jkt_hex);

    // jwk_thumbprint_input が RFC 7638 の正規化 JSON であるベクタ
    let real_binding = DRAFT_02_DPOP_VECTORS
        .iter()
        .find(|vector| vector.id == "dpop_es256_real_binding")
        .expect("ベクタがある");
    let input = real_binding
        .jwk_thumbprint_input
        .expect("サムプリント入力がある");
    let jwk = Jwk::decode(input).expect("JWK をデコードできる");
    assert_eq!(jwk.canonical_json().expect("正規化 JSON を作れる"), input);
    assert_eq!(
        encode_hex(
            &jwk.thumbprint_sha256(&crypto)
                .expect("サムプリントを計算できる")
        ),
        real_binding.cnf_jkt_hex
    );
}

/// A.5 の公式トークンの MAC を実暗号で検証する
#[cfg(feature = "aws-lc-rs")]
#[test]
fn scope_vectors_verify_with_real_crypto() {
    let crypto = AwsLcRsCrypto::new();
    for vector in DRAFT_02_SCOPE_VECTORS {
        let token = CatToken::decode(&decode_hex(vector.cose_hex))
            .unwrap_or_else(|error| panic!("ベクタ {} のデコードに失敗した: {error}", vector.id));
        token
            .verify(&crypto, &hmac_key())
            .unwrap_or_else(|error| panic!("ベクタ {} の検証に失敗した: {error}", vector.id));

        // cose_b64 経由でも同じ MAC を検証できる
        let from_b64 = CatToken::decode(vector.cose_b64.as_bytes())
            .unwrap_or_else(|error| panic!("ベクタ {} の b64 デコード失敗: {error}", vector.id));
        from_b64
            .verify(&crypto, &hmac_key())
            .unwrap_or_else(|error| panic!("ベクタ {} の b64 検証失敗: {error}", vector.id));
    }
}

/// A.6 の公式トークンの MAC を実暗号で検証し、認可とは分離する
#[cfg(feature = "aws-lc-rs")]
#[test]
fn composite_vectors_verify_with_real_crypto() {
    let crypto = AwsLcRsCrypto::new();
    for vector in DRAFT_02_COMPOSITE_VECTORS {
        let token = CatToken::decode(&decode_hex(vector.cose_hex))
            .unwrap_or_else(|error| panic!("ベクタ {} のデコードに失敗した: {error}", vector.id));
        // Composite claim を内包していても COSE_Mac0 として MAC は検証できる
        token
            .verify(&crypto, &hmac_key())
            .unwrap_or_else(|error| panic!("ベクタ {} の検証に失敗した: {error}", vector.id));
        // 署名検証の成功は Composite の評価・認可の成功ではない
        assert!(!token.claims().authorize(
            MoqtAction::Publish,
            &namespace_bytes(&["any.namespace"]),
            b"any-track"
        ));
    }
}

/// A.7 の公式トークンを実暗号で検証し、各異常系の期待値と対応させる
#[cfg(feature = "aws-lc-rs")]
#[test]
fn validation_vectors_verify_with_real_crypto() {
    let crypto = AwsLcRsCrypto::new();
    let official_key = CoseKey::symmetric(decode_hex(DRAFT_02_HMAC_SHA256_KEY_HEX));

    for vector in DRAFT_02_VALIDATION_VECTORS {
        let token = CatToken::decode(vector.token.as_bytes())
            .unwrap_or_else(|error| panic!("ベクタ {} のデコードに失敗した: {error}", vector.id));
        let result = match vector.id {
            "invalid_tampered_signature" => {
                let key = CoseKey::symmetric(decode_hex(vector.key_hex.expect("鍵がある")));
                assert_eq!(
                    token.verify(&crypto, &key),
                    Err(CatError::Crypto(CryptoError::SignatureVerificationFailed)),
                    "ベクタ {} は改ざんされた MAC を拒否する",
                    vector.id
                );
                continue;
            }
            "invalid_wrong_key" => {
                let correct =
                    CoseKey::symmetric(decode_hex(vector.correct_key_hex.expect("正しい鍵がある")));
                let wrong =
                    CoseKey::symmetric(decode_hex(vector.wrong_key_hex.expect("誤った鍵がある")));
                token
                    .verify(&crypto, &correct)
                    .expect("正しい鍵では MAC を検証できる");
                assert_eq!(
                    token.verify(&crypto, &wrong),
                    Err(CatError::Crypto(CryptoError::SignatureVerificationFailed)),
                    "ベクタ {} は誤った鍵を拒否する",
                    vector.id
                );
                continue;
            }
            "invalid_algorithm_mismatch" => {
                // 同じ COSE_Mac0 なので alg を指定しなければ MAC は検証できる
                token
                    .verify(&crypto, &official_key)
                    .expect("正しい鍵では MAC を検証できる");
                // 検証側が ES256 を期待すると、トークンの alg=5 と一致しない。
                // 公式の expected_error は "InvalidTokenFormat" だが、本実装は
                // 期待アルゴリズムの不一致を AlgorithmMismatch として返す。
                // 公式の期待値は定数側にそのまま保持し、ここでは本実装の
                // エラーを固定する
                let expected = Algorithm::from_identifier(
                    vector.verifier_algorithm_id.expect("検証側の alg がある"),
                )
                .expect("既知の COSE アルゴリズムである");
                assert_eq!(
                    token.verify_with(
                        &crypto,
                        &official_key,
                        &VerifyOptions {
                            expected_algorithm: Some(expected),
                            ..VerifyOptions::default()
                        },
                    ),
                    Err(CatError::AlgorithmMismatch {
                        token: Algorithm::HmacSha256,
                        expected: Algorithm::Es256,
                    })
                );
                continue;
            }
            _ => token.verify(&crypto, &official_key),
        };
        result.unwrap_or_else(|error| {
            panic!(
                "ベクタ {} の MAC 検証に失敗した (署名は有効なはず): {error}",
                vector.id
            )
        });
    }
}

/// 公式の COSE_Mac0 を改変した MAC と誤った鍵を新しい入力で拒否する
#[cfg(feature = "aws-lc-rs")]
#[test]
fn tampered_mac_and_wrong_key_are_rejected() {
    let crypto = AwsLcRsCrypto::new();
    let vector = DRAFT_02_TOKEN_VECTORS
        .iter()
        .find(|vector| vector.id == "token_hmac_minimal")
        .expect("ベクタがある");

    // 公式の MAC の末尾 1 ビットを反転しても COSE 構造としてはデコードできる
    let mut bytes = decode_hex(vector.cose_hex);
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    let tampered = CatToken::decode(&bytes).expect("改変後も COSE としてデコードできる");
    assert_eq!(
        tampered.verify(&crypto, &hmac_key()),
        Err(CatError::Crypto(CryptoError::SignatureVerificationFailed))
    );

    // 誤った鍵でも MAC は検証できない
    let token = CatToken::decode(&decode_hex(vector.cose_hex)).expect("デコードできる");
    let wrong_key = CoseKey::symmetric(vec![0xff; 32]);
    assert_eq!(
        token.verify(&crypto, &wrong_key),
        Err(CatError::Crypto(CryptoError::SignatureVerificationFailed))
    );
    token
        .verify(&crypto, &hmac_key())
        .expect("正しい鍵で検証できる");
}

/// 公式の COSE_Mac0 を COSE タグ付き・CWT タグ無しで再エンコードすると公式バイト列に戻ること
///
/// COSE_Sign1 への差し替えは `cose_mac0_tag_with_mac_algorithm_is_rejected_as_sign1` で
/// 別に確認する。
#[test]
fn cose_message_reencode_keeps_official_bytes() {
    let vector = DRAFT_02_TOKEN_VECTORS
        .iter()
        .find(|vector| vector.id == "token_hmac_minimal")
        .expect("ベクタがある");
    let bytes = decode_hex(vector.cose_hex);
    let message = CoseMessage::decode(&bytes).expect("COSE をデコードできる");
    // 公式のトークンは CWT タグ (61) を持たないため、COSE タグだけを付与して
    // 再エンコードすると公式のバイト列に戻る
    let options = CoseEncodingOptions {
        cose_tag: true,
        cwt_tag: false,
    };
    assert_eq!(
        encode_hex(&message.encode(&options).expect("COSE をエンコードできる")),
        vector.cose_hex
    );
}

/// A.4 の `dpop_es256_real_binding` の署名を実暗号で検証する
#[cfg(feature = "aws-lc-rs")]
#[test]
fn dpop_es256_real_binding_verifies_with_vector_public_key() {
    let crypto = AwsLcRsCrypto::new();
    let vector = DRAFT_02_DPOP_VECTORS
        .iter()
        .find(|vector| vector.id == "dpop_es256_real_binding")
        .expect("ベクタがある");
    let key = CoseKey::ec2(
        EcCurve::P256,
        decode_hex(vector.public_key_x_hex.expect("x 座標がある")),
        decode_hex(vector.public_key_y_hex.expect("y 座標がある")),
    );
    let token = CatToken::decode(&decode_hex(vector.cose_hex)).expect("デコードできる");
    token
        .verify(&crypto, &key)
        .expect("公式の ES256 署名を検証できる");
    assert_eq!(token.format(), TokenFormat::CoseSign1);
    assert_eq!(token.header().algorithm, Some(Algorithm::Es256));
}

/// A.1 の鍵が既存の draft-01 compact ベクタの鍵と一致すること
///
/// draft-02 の付録 A.1 と draft-01 の付録 A.1 は同じテスト鍵を使う。両者を別定義して
/// いるため、片方だけ更新されないことを固定する。
#[test]
fn draft_02_keys_match_the_compact_vectors() {
    assert_eq!(DRAFT_02_HMAC_SHA256_KEY_HEX, super::vectors::HMAC_KEY_HEX);
    assert_eq!(
        DRAFT_02_ES256_PRIVATE_KEY_HEX,
        super::vectors::ES256_PRIVATE_KEY_HEX
    );
    assert_eq!(
        DRAFT_02_ES256_PUBLIC_KEY_X_HEX,
        super::vectors::ES256_PUBLIC_KEY_X_HEX
    );
    assert_eq!(
        DRAFT_02_ES256_PUBLIC_KEY_Y_HEX,
        super::vectors::ES256_PUBLIC_KEY_Y_HEX
    );
}
