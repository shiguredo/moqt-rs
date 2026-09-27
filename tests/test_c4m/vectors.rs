//! draft-ietf-moq-c4m-01 付録 A のテストベクタ
//!
//! `tests/test_c4m.rs` のサブモジュールから共有する。値は refs/moq/draft-ietf-moq-c4m-01.txt
//! の付録 A の JSON をそのまま定数化したもので、改変しない。

#![allow(dead_code)]

/// 付録 A.1 の HMAC-SHA256 鍵
pub const HMAC_KEY_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
/// 付録 A.1 の ES256 秘密鍵 (スカラー)
pub const ES256_PRIVATE_KEY_HEX: &str =
    "c9afa9d845ba75166b5c215767b1d6934e50c3db36e89b127b8a622b120f6721";
/// 付録 A.1 の ES256 公開鍵の x 座標
pub const ES256_PUBLIC_KEY_X_HEX: &str =
    "60fed4ba255a9d31c961eb74c6356d68c049b8923b61fa6ce669622e60f29fb6";
/// 付録 A.1 の ES256 公開鍵の y 座標
pub const ES256_PUBLIC_KEY_Y_HEX: &str =
    "7903fe1008b8bc99a41ae9e95628bc64f2f1b20c2d7e9f5177a3c294d4462299";

/// 付録 A.2 の CBOR エンコードのベクタ
pub struct ClaimVector {
    /// ベクタの識別子
    pub id: &'static str,
    /// claims の CBOR バイト列 (hex)
    pub payload_hex: &'static str,
}

/// 付録 A.2 のベクタ一覧
pub const CLAIM_VECTORS: &[ClaimVector] = &[
    ClaimVector {
        id: "cbor_issuer_only",
        payload_hex: "a101781868747470733a2f2f617574682e6578616d706c652e636f6d",
    },
    ClaimVector {
        id: "cbor_core_claims",
        payload_hex: "a501781868747470733a2f2f617574682e6578616d706c652e636f6d0381781968747470733a2f2f72656c61792e6578616d706c652e636f6d041a65554280051a6553f100074e746573742d746f6b656e2d303031",
    },
    ClaimVector {
        id: "cbor_cat_version_usage",
        payload_hex: "a2190136664341542d763119013805",
    },
    ClaimVector {
        id: "cbor_network_identifiers",
        payload_hex: "a1190137846d3139322e3136382e312e313030a16869705f72616e67656a31302e302e302e302f38a16361736e19fc00a16961736e5f72616e67658219fc0019fd00",
    },
    ClaimVector {
        id: "cbor_geographic_claims",
        payload_hex: "a419011a6639713879796b19013c8262555362434119013da3636c6174fb4042e32fec56d5d0636c6f6efbc05e9ad77318fc50686163637572616379f9564019013e0a",
    },
    ClaimVector {
        id: "cbor_uri_patterns",
        payload_hex: "a119013b83782068747470733a2f2f6578616d706c652e636f6d2f6c6976652f73747265616d31a166707265666978781868747470733a2f2f6578616d706c652e636f6d2f766f642fa166737566666978652e6d337538",
    },
    ClaimVector {
        id: "cbor_alpn",
        payload_hex: "a119013a82666d6f712d3030626833",
    },
];

/// 付録 A.3 のトークン構造のベクタ
pub struct TokenVector {
    /// ベクタの識別子
    pub id: &'static str,
    /// protected ヘッダの CBOR バイト列 (hex)
    pub header_hex: &'static str,
    /// claims の CBOR バイト列 (hex)
    pub payload_hex: &'static str,
    /// 署名 (hex)
    pub signature_hex: &'static str,
    /// compact 形式のトークン
    pub token: &'static str,
    /// `alg` の識別子
    pub algorithm_id: i64,
    /// `iss`
    pub issuer: Option<&'static str>,
    /// `aud`
    pub audience: &'static [&'static str],
    /// `exp`
    pub expiration: Option<f64>,
    /// `nbf`
    pub not_before: Option<f64>,
    /// `iat`
    pub issued_at: Option<f64>,
    /// `sub`
    pub subject: Option<&'static str>,
    /// `cti`
    pub cwt_id: Option<&'static str>,
}

/// 付録 A.3 のベクタ一覧
pub const TOKEN_VECTORS: &[TokenVector] = &[
    TokenVector {
        id: "token_hmac_minimal",
        header_hex: "a201231063434154",
        payload_hex: "a301781868747470733a2f2f617574682e6578616d706c652e636f6d0381781968747470733a2f2f72656c61792e6578616d706c652e636f6d041a65554280",
        signature_hex: "5b5ec60fb1a3f81d18b5e8d7edf4702e55261248def8c13cd6809cf6865a6986",
        token: "ogEjEGNDQVQ.owF4GGh0dHBzOi8vYXV0aC5leGFtcGxlLmNvbQOBeBlodHRwczovL3JlbGF5LmV4YW1wbGUuY29tBBplVUKA.W17GD7Gj-B0YtejX7fRwLlUmEkje-ME81oCc9oZaaYY",
        algorithm_id: -4,
        issuer: Some("https://auth.example.com"),
        audience: &["https://relay.example.com"],
        expiration: Some(1700086400.0),
        not_before: None,
        issued_at: None,
        subject: None,
        cwt_id: None,
    },
    TokenVector {
        id: "token_hmac_full",
        header_hex: "a201231063434154",
        payload_hex: "aa01781a68747470733a2f2f6973737565722e6d6f712e6578616d706c650276757365723a616c696365406578616d706c652e636f6d0382781a68747470733a2f2f72656c6179312e6578616d706c652e636f6d781a68747470733a2f2f72656c6179322e6578616d706c652e636f6d041a65554280051a6553f100061a6553f100074a766563746f722d303032190136664341542d7631190137816c3230332e302e3131332e35301901380a",
        signature_hex: "02aa58a31e34ab53fab3c755b47cf08f458a3603da4d933d7c0b1ce4614f44da",
        token: "ogEjEGNDQVQ.qgF4Gmh0dHBzOi8vaXNzdWVyLm1vcS5leGFtcGxlAnZ1c2VyOmFsaWNlQGV4YW1wbGUuY29tA4J4Gmh0dHBzOi8vcmVsYXkxLmV4YW1wbGUuY29teBpodHRwczovL3JlbGF5Mi5leGFtcGxlLmNvbQQaZVVCgAUaZVPxAAYaZVPxAAdKdmVjdG9yLTAwMhkBNmZDQVQtdjEZATeBbDIwMy4wLjExMy41MBkBOAo.AqpYox40q1P6s8dVtHzwj0WKNgPaTZM9fAsc5GFPRNo",
        algorithm_id: -4,
        issuer: Some("https://issuer.moq.example"),
        audience: &["https://relay1.example.com", "https://relay2.example.com"],
        expiration: Some(1700086400.0),
        not_before: Some(1700000000.0),
        issued_at: Some(1700000000.0),
        subject: Some("user:alice@example.com"),
        cwt_id: Some("vector-002"),
    },
    TokenVector {
        id: "token_es256",
        header_hex: "a201261063434154",
        payload_hex: "a401781868747470733a2f2f617574682e6578616d706c652e636f6d0381781d68747470733a2f2f6d6f712d72656c61792e6578616d706c652e636f6d041a65554280051a6553f100",
        signature_hex: "fa3315e9de061fd77d814394428ae61da3d7a21fdffb19802b0c575c578098e7cd4b6b75a1690deed4c2baae994bfc462e0d8a2006f3e89780f3435738294d7a",
        token: "ogEmEGNDQVQ.pAF4GGh0dHBzOi8vYXV0aC5leGFtcGxlLmNvbQOBeB1odHRwczovL21vcS1yZWxheS5leGFtcGxlLmNvbQQaZVVCgAUaZVPxAA.-jMV6d4GH9d9gUOUQormHaPXoh_f-xmAKwxXXFeAmOfNS2t1oWkN7tTCuq6ZS_xGLg2KIAbz6JeA80NXOClNeg",
        algorithm_id: -7,
        issuer: Some("https://auth.example.com"),
        audience: &["https://moq-relay.example.com"],
        expiration: Some(1700086400.0),
        not_before: Some(1700000000.0),
        issued_at: None,
        subject: None,
        cwt_id: None,
    },
];

/// 付録 A.4 の DPoP バインディングのベクタ
pub struct DpopVector {
    /// ベクタの識別子
    pub id: &'static str,
    /// claims の CBOR バイト列 (hex)
    pub payload_hex: &'static str,
    /// compact 形式のトークン
    pub token: &'static str,
    /// `cnf` の JWK サムプリント (hex)
    pub cnf_jkt_hex: &'static str,
    /// `catdpop` のウィンドウ (秒)
    pub window_seconds: Option<f64>,
    /// `catdpop` の jti の扱い
    pub honor_jti: Option<bool>,
    /// サムプリント計算の入力となる正規化 JSON (ある場合)
    pub jwk_thumbprint_input: Option<&'static str>,
}

/// 付録 A.4 のベクタ一覧
pub const DPOP_VECTORS: &[DpopVector] = &[
    DpopVector {
        id: "dpop_jwk_binding",
        payload_hex: "a401781868747470733a2f2f617574682e6578616d706c652e636f6d041a6555428008a1035820a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1190141a200183c0101",
        token: "ogEjEGNDQVQ.pAF4GGh0dHBzOi8vYXV0aC5leGFtcGxlLmNvbQQaZVVCgAihA1ggoLHC0-T1prfI2eDxorPE1eb3qLnA0eLzpLXG1-j5oLEZAUGiABg8AQE.sN9kLIp64zIN9zDXoTLYC0xsJU_1FNF3kaO0CbdA_3M",
        cnf_jkt_hex: "a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1",
        window_seconds: Some(60.0),
        honor_jti: Some(true),
        jwk_thumbprint_input: None,
    },
    DpopVector {
        id: "dpop_no_jti",
        payload_hex: "a401781868747470733a2f2f617574682e6578616d706c652e636f6d041a6555428008a10358203c82dfd6358ba804bd90879c34e743bbe13aeab7980664944f37a0ec0063fe95190141a20019012c0100",
        token: "ogEjEGNDQVQ.pAF4GGh0dHBzOi8vYXV0aC5leGFtcGxlLmNvbQQaZVVCgAihA1ggPILf1jWLqAS9kIecNOdDu-E66reYBmSUTzeg7ABj_pUZAUGiABkBLAEA.M4lF5pQdxav6eIWqDjbchDkijVYOM7xa3oJR2IwWt9g",
        cnf_jkt_hex: "3c82dfd6358ba804bd90879c34e743bbe13aeab7980664944f37a0ec0063fe95",
        window_seconds: Some(300.0),
        honor_jti: Some(false),
        jwk_thumbprint_input: None,
    },
    DpopVector {
        id: "dpop_es256_real_binding",
        payload_hex: "a501781868747470733a2f2f617574682e6578616d706c652e636f6d0381781968747470733a2f2f72656c61792e6578616d706c652e636f6d041a6555428008a10358200cebf1bc9880748a95588905b79843b42ba75cb174055e3e246bf87fe00b4a6d190141a1001878",
        token: "ogEmEGNDQVQ.pQF4GGh0dHBzOi8vYXV0aC5leGFtcGxlLmNvbQOBeBlodHRwczovL3JlbGF5LmV4YW1wbGUuY29tBBplVUKACKEDWCAM6_G8mIB0ipVYiQW3mEO0K6dcsXQFXj4ka_h_4AtKbRkBQaEAGHg.5andoxOhWXQIKWR3EMHWT-WIMPBDMYQFc61nzlfZs8zmgzwcOARpmlaB3ZS5MbJ9iCYWykYAcIzJ81nMyyZoQw",
        cnf_jkt_hex: "0cebf1bc9880748a95588905b79843b42ba75cb174055e3e246bf87fe00b4a6d",
        window_seconds: Some(120.0),
        honor_jti: None,
        jwk_thumbprint_input: Some(
            "{\"crv\":\"P-256\",\"kty\":\"EC\",\"x\":\"YP7UuiVanTHJYet0xjVtaMBJuJI7Yfps5mliLmDyn7Y\",\"y\":\"eQP-EAi4vJmkGunpVii8ZPLxsgwtfp9Rd6PClNRGIpk\"}",
        ),
    },
];

/// 付録 A.5 の認可テスト
pub struct AuthorizationTest {
    /// アクションの整数値
    pub action: i64,
    /// Track Namespace のフィールド (UTF-8)
    pub namespace: &'static [&'static str],
    /// Track Name (UTF-8)
    pub track: &'static str,
    /// 期待する判定
    pub expected: bool,
}

/// 付録 A.5 のスコープのベクタ
pub struct ScopeVector {
    /// ベクタの識別子
    pub id: &'static str,
    /// claims の CBOR バイト列 (hex)
    pub payload_hex: &'static str,
    /// compact 形式のトークン
    pub token: &'static str,
    /// `moqt-reval` (ある場合)
    pub moqt_reval: Option<f64>,
    /// 認可テスト
    pub tests: &'static [AuthorizationTest],
}

/// 付録 A.5 のベクタ一覧
pub const SCOPE_VECTORS: &[ScopeVector] = &[
    ScopeVector {
        id: "moqt_publisher_exact",
        payload_hex: "a301781868747470733a2f2f617574682e6578616d706c652e636f6d041a655542801901478183820206824b6578616d706c652e636f6d45616c696365820146766964656f2d",
        token: "ogEjEGNDQVQ.owF4GGh0dHBzOi8vYXV0aC5leGFtcGxlLmNvbQQaZVVCgBkBR4GDggIGgktleGFtcGxlLmNvbUVhbGljZYIBRnZpZGVvLQ.oAPD24Wu_zHnDcuM6a-ePeGvRJjbCa6U7iswdsKzFDk",
        moqt_reval: None,
        tests: &[
            AuthorizationTest {
                action: 2,
                namespace: &["example.com", "alice"],
                track: "video-hd",
                expected: true,
            },
            AuthorizationTest {
                action: 6,
                namespace: &["example.com", "alice"],
                track: "video-sd",
                expected: true,
            },
            AuthorizationTest {
                action: 6,
                namespace: &["example.com", "alice"],
                track: "audio-main",
                expected: false,
            },
            AuthorizationTest {
                action: 4,
                namespace: &["example.com", "alice"],
                track: "video-hd",
                expected: false,
            },
            AuthorizationTest {
                action: 6,
                namespace: &["example.com", "bob"],
                track: "video-hd",
                expected: false,
            },
        ],
    },
    ScopeVector {
        id: "moqt_subscriber_prefix",
        payload_hex: "a301781868747470733a2f2f617574682e6578616d706c652e636f6d041a6555428019014781828303040781820152636f6e666572656e63652e6578616d706c65",
        token: "ogEjEGNDQVQ.owF4GGh0dHBzOi8vYXV0aC5leGFtcGxlLmNvbQQaZVVCgBkBR4GCgwMEB4GCAVJjb25mZXJlbmNlLmV4YW1wbGU.pfUPZultmyCm1GF2PvPXAYXzvK6d1D-OFNBLG1AwjDg",
        moqt_reval: None,
        tests: &[
            AuthorizationTest {
                action: 4,
                namespace: &["conference.example.room1"],
                track: "audio",
                expected: true,
            },
            AuthorizationTest {
                action: 7,
                namespace: &["conference.example.room2"],
                track: "video",
                expected: true,
            },
            AuthorizationTest {
                action: 4,
                namespace: &["other.domain"],
                track: "audio",
                expected: false,
            },
            AuthorizationTest {
                action: 6,
                namespace: &["conference.example.room1"],
                track: "audio",
                expected: false,
            },
        ],
    },
    ScopeVector {
        id: "moqt_multi_scope",
        payload_hex: "a401781868747470733a2f2f617574682e6578616d706c652e636f6d041a655542801901478282820206824c6c6976652e6578616d706c654873747564696f2d61828204078182014c6c6976652e6578616d706c65190148f95cb0",
        token: "ogEjEGNDQVQ.pAF4GGh0dHBzOi8vYXV0aC5leGFtcGxlLmNvbQQaZVVCgBkBR4KCggIGgkxsaXZlLmV4YW1wbGVIc3R1ZGlvLWGCggQHgYIBTGxpdmUuZXhhbXBsZRkBSPlcsA.byEzQmxc28UXFyFekHtgOtaVmWyIPl-63xNMOF0Q_IU",
        moqt_reval: Some(300.0),
        tests: &[
            AuthorizationTest {
                action: 6,
                namespace: &["live.example", "studio-a"],
                track: "cam1",
                expected: true,
            },
            AuthorizationTest {
                action: 4,
                namespace: &["live.example.studio-b"],
                track: "cam1",
                expected: true,
            },
            AuthorizationTest {
                action: 6,
                namespace: &["live.example", "studio-b"],
                track: "cam1",
                expected: false,
            },
            AuthorizationTest {
                action: 2,
                namespace: &["other.example", "studio-a"],
                track: "",
                expected: false,
            },
        ],
    },
    ScopeVector {
        id: "moqt_admin_wildcard",
        payload_hex: "a301781868747470733a2f2f617574682e6578616d706c652e636f6d041a65554280190147818189000102030405060708",
        token: "ogEjEGNDQVQ.owF4GGh0dHBzOi8vYXV0aC5leGFtcGxlLmNvbQQaZVVCgBkBR4GBiQABAgMEBQYHCA.XlNItz7OGqnNEbaqZ_bQh6TL-wV6SDr8hXyOLmtQkj4",
        moqt_reval: None,
        tests: &[
            AuthorizationTest {
                action: 0,
                namespace: &["any.namespace"],
                track: "any-track",
                expected: true,
            },
            AuthorizationTest {
                action: 6,
                namespace: &["any.namespace"],
                track: "any-track",
                expected: true,
            },
            AuthorizationTest {
                action: 8,
                namespace: &["any.namespace"],
                track: "status",
                expected: true,
            },
        ],
    },
    ScopeVector {
        id: "moqt_suffix_match",
        payload_hex: "a301781868747470733a2f2f617574682e6578616d706c652e636f6d041a65554280190147818381048182024c2e6578616d706c652e636f6d8202462d617564696f",
        token: "ogEjEGNDQVQ.owF4GGh0dHBzOi8vYXV0aC5leGFtcGxlLmNvbQQaZVVCgBkBR4GDgQSBggJMLmV4YW1wbGUuY29tggJGLWF1ZGlv.-eGYTPe_n1PeC0sgHdWCqgnKRHGYF-T89WTk269liBg",
        moqt_reval: None,
        tests: &[
            AuthorizationTest {
                action: 4,
                namespace: &["cdn.example.com"],
                track: "stream1-audio",
                expected: true,
            },
            AuthorizationTest {
                action: 4,
                namespace: &["cdn.example.com"],
                track: "stream1-video",
                expected: false,
            },
            AuthorizationTest {
                action: 4,
                namespace: &["cdn.other.org"],
                track: "stream1-audio",
                expected: false,
            },
        ],
    },
];

/// 付録 A.6 の検証ベクタ
pub struct ValidationVector {
    /// ベクタの識別子
    pub id: &'static str,
    /// compact 形式のトークン
    pub token: &'static str,
    /// 検証に使う現在時刻
    pub reference_time: Option<f64>,
    /// 期待する `iss`
    pub expected_issuers: &'static [&'static str],
    /// 期待する `aud`
    pub expected_audiences: &'static [&'static str],
    /// 期待するエラー (`None` は正常)
    pub expected_error: Option<&'static str>,
    /// 検証に使う鍵 (hex)
    pub key_hex: Option<&'static str>,
    /// 検証側が期待するアルゴリズム id
    pub verifier_algorithm_id: Option<i64>,
}

/// 付録 A.6 のベクタ一覧
pub const VALIDATION_VECTORS: &[ValidationVector] = &[
    ValidationVector {
        id: "valid_basic",
        token: "ogEjEGNDQVQ.pAF4GGh0dHBzOi8vYXV0aC5leGFtcGxlLmNvbQOBeBlodHRwczovL3JlbGF5LmV4YW1wbGUuY29tBBplVUKABRplU_EA.9SztgnG4xgw8U9zDFnqPIuPn6hLwuilSigQcfPsArSg",
        reference_time: Some(1700003600.0),
        expected_issuers: &["https://auth.example.com"],
        expected_audiences: &["https://relay.example.com"],
        expected_error: None,
        key_hex: None,
        verifier_algorithm_id: None,
    },
    ValidationVector {
        id: "invalid_expired",
        token: "ogEjEGNDQVQ.ogF4GGh0dHBzOi8vYXV0aC5leGFtcGxlLmNvbQQaX14QAA.lq8nGBiZm80yUwl1kH_Tv2prKu_nV20JvxVJW8ZGkho",
        reference_time: Some(1700000000.0),
        expected_issuers: &[],
        expected_audiences: &[],
        expected_error: Some("TokenExpired"),
        key_hex: None,
        verifier_algorithm_id: None,
    },
    ValidationVector {
        id: "invalid_not_yet_valid",
        token: "ogEjEGNDQVQ.owF4GGh0dHBzOi8vYXV0aC5leGFtcGxlLmNvbQQaZVaUAAUaZVVCgA.fPIUugY7_oSeHlheu83_8Yyljsk3iP2zGeWRUi7NtUs",
        reference_time: Some(1700000000.0),
        expected_issuers: &[],
        expected_audiences: &[],
        expected_error: Some("TokenNotYetValid"),
        key_hex: None,
        verifier_algorithm_id: None,
    },
    ValidationVector {
        id: "invalid_wrong_issuer",
        token: "ogEjEGNDQVQ.owF4GGh0dHBzOi8vZXZpbC5leGFtcGxlLmNvbQOBeBlodHRwczovL3JlbGF5LmV4YW1wbGUuY29tBBplVUKA.Xo7FCr_MGSyVX0C9sueeapSfboIHkrkysurn2VjC9PU",
        reference_time: Some(1700003600.0),
        expected_issuers: &["https://auth.example.com"],
        expected_audiences: &[],
        expected_error: Some("InvalidIssuer"),
        key_hex: None,
        verifier_algorithm_id: None,
    },
    ValidationVector {
        id: "invalid_wrong_audience",
        token: "ogEjEGNDQVQ.owF4GGh0dHBzOi8vYXV0aC5leGFtcGxlLmNvbQOBeB9odHRwczovL290aGVyLXJlbGF5LmV4YW1wbGUuY29tBBplVUKA.b8KxAKxJglzhELMuc9bYmsikrx3F9Y3YdvpfHLbsyk0",
        reference_time: Some(1700003600.0),
        expected_issuers: &["https://auth.example.com"],
        expected_audiences: &["https://relay.example.com"],
        expected_error: Some("InvalidAudience"),
        key_hex: None,
        verifier_algorithm_id: None,
    },
    ValidationVector {
        id: "invalid_tampered_signature",
        token: "ogEjEGNDQVQ.owF4GGh0dHBzOi8vYXV0aC5leGFtcGxlLmNvbQOBeBlodHRwczovL3JlbGF5LmV4YW1wbGUuY29tBBplVUKA.pF7GD7Gj-B0YtejX7fRwLlUmEkje-ME81oCc9oZaaYY",
        reference_time: None,
        expected_issuers: &[],
        expected_audiences: &[],
        expected_error: Some("SignatureVerificationFailed"),
        key_hex: Some("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"),
        verifier_algorithm_id: None,
    },
    ValidationVector {
        id: "invalid_wrong_key",
        token: "ogEjEGNDQVQ.ogF4GGh0dHBzOi8vYXV0aC5leGFtcGxlLmNvbQQaZVVCgA.zmbxdkvbWtGtX0DExLC2nIxPDDmgAVImqk4rRSCCkCY",
        reference_time: None,
        expected_issuers: &[],
        expected_audiences: &[],
        expected_error: Some("SignatureVerificationFailed"),
        key_hex: Some("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
        verifier_algorithm_id: None,
    },
    ValidationVector {
        id: "invalid_algorithm_mismatch",
        token: "ogEjEGNDQVQ.ogF4GGh0dHBzOi8vYXV0aC5leGFtcGxlLmNvbQQaZVVCgA.zmbxdkvbWtGtX0DExLC2nIxPDDmgAVImqk4rRSCCkCY",
        reference_time: None,
        expected_issuers: &[],
        expected_audiences: &[],
        expected_error: Some("AlgorithmMismatch"),
        key_hex: None,
        verifier_algorithm_id: Some(-7),
    },
];
