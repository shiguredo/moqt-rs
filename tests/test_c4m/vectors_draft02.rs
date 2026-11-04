//! draft-ietf-moq-c4m-02 付録 A の公式テストベクタ
//!
//! `refs/moq/draft-ietf-moq-c4m-02.txt` の付録 A.1 〜 A.7 が定義する JSON の
//! ベクタを定数化したものである。JSON はベクタの容器にすぎず、CAT claim の JSON
//! 表現ではない。`payload_cbor_hex` が規範的な claim set であり、`claims_edn` は
//! 同じ claim set を CBOR の Extended Diagnostic Notation で示した参考値である。
//! そのため `claims_edn` を独自の JSON claim 表現として解釈してはならない。
//!
//! 付録 A 冒頭の unfolding 規則に従い、hex / base64url / token の折り返しは改行と
//! 後続のインデントを取り除き、`claims_edn` の折り返しは 1 個の空白に置き換えて
//! いる。値そのものは変更していない。
//!
//! `tests/test_c4m/vectors.rs` の draft-01 由来の compact ベクタとは分離して
//! ある。compact 形式の汎用 API のテストはそちらを使い、このモジュールの
//! COSE_Mac0 / COSE_Sign1 のベクタと混ぜない。
//!
//! A.4 の `claims_edn` は折り返しを空白 1 個に置換した結果、hex literal の途中に
//! 空白を含む。これは付録の unfolding 規則どおりであり、EDN として parse しない。
//!
//! 公式 JSON の説明用フィールド (`description` と A.5 の `moqt_scopes`) はテストで
//! 使わないため保持しない。規範的な payload / COSE / MAC / 署名と検証入力だけを
//! 定義元のまま定数化する。
//!
//! A.1 の鍵は `tests/test_c4m/vectors.rs` の draft-01 compact ベクタと同じ値を
//! 重複定義している。片方だけ更新しないよう、両者の一致を
//! `draft_02_keys_match_the_compact_vectors` で固定する。
//!
//! draft-ietf-moq-c4m-02 は将来改訂され得る。改訂時はベクタの ID と値を突き合わせて
//! 更新すること。

/// 付録 A.1 の ES256 秘密鍵 (P-256 のスカラー)
pub const DRAFT_02_ES256_PRIVATE_KEY_HEX: &str =
    "c9afa9d845ba75166b5c215767b1d6934e50c3db36e89b127b8a622b120f6721";

/// 付録 A.1 の ES256 公開鍵の x 座標
pub const DRAFT_02_ES256_PUBLIC_KEY_X_HEX: &str =
    "60fed4ba255a9d31c961eb74c6356d68c049b8923b61fa6ce669622e60f29fb6";

/// 付録 A.1 の ES256 公開鍵の y 座標
pub const DRAFT_02_ES256_PUBLIC_KEY_Y_HEX: &str =
    "7903fe1008b8bc99a41ae9e95628bc64f2f1b20c2d7e9f5177a3c294d4462299";

/// 付録 A.1 の HMAC-SHA256 鍵
pub const DRAFT_02_HMAC_SHA256_KEY_HEX: &str =
    "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

/// 付録 A.2 の CBOR エンコードのベクタ
pub struct Draft02ClaimVector {
    /// ベクタの識別子
    pub id: &'static str,
    /// claims を CBOR の Extended Diagnostic Notation で示した参考値
    ///
    /// JSON の claim 表現ではないため、テストで JSON として解釈しない。
    pub claims_edn: &'static str,
    /// claims の規範的な CBOR バイト列 (hex)
    pub payload_cbor_hex: &'static str,
}

/// 付録 A.2 のベクタ一覧
pub const DRAFT_02_CLAIM_VECTORS: &[Draft02ClaimVector] = &[
    Draft02ClaimVector {
        id: "cbor_issuer_only",
        claims_edn: "{1 /iss/: \"https://auth.example.com\"}",
        payload_cbor_hex: "a101781868747470733a2f2f617574682e6578616d706c652e636f6d",
    },
    Draft02ClaimVector {
        id: "cbor_core_claims",
        claims_edn: "{1 /iss/: \"https://auth.example.com\", 3 /aud/: [\"https://relay.example.com\"], 4 /exp/: 1700086400, 5 /nbf/: 1700000000, 7 /cti/: h'746573742d746f6b656e2d303031'}",
        payload_cbor_hex: "a501781868747470733a2f2f617574682e6578616d706c652e636f6d0381781968747470733a2f2f72656c61792e6578616d706c652e636f6d041a65554280051a6553f100074e746573742d746f6b656e2d303031",
    },
    Draft02ClaimVector {
        id: "cbor_cat_version_uri",
        claims_edn: "{310 /catv/: 1, 312 /catu/: {1 /host/: {0 /exact/: \"example.com\"}}}",
        payload_cbor_hex: "a219013601190138a101a1006b6578616d706c652e636f6d",
    },
    Draft02ClaimVector {
        id: "cbor_network_identifiers",
        claims_edn: "{311 /catnip/: [52(h'c0a80164'), 52([8, h'0a']), 64512, [64512, 64768]]}",
        payload_cbor_hex: "a119013784d83444c0a80164d8348208410a19fc008219fc0019fd00",
    },
    Draft02ClaimVector {
        id: "cbor_geographic_claims",
        claims_edn: "{282 /geohash/: \"9q8yyk\", 316 /catgeoiso3166/: [\"US\", \"CA\"], 317 /catgeocoord/: [[37.7749, -122.4194, 100]], 318 /catgeoalt/: [10, 5]}",
        payload_cbor_hex: "a419011a6639713879796b19013c8262555362434119013d8183fb4042e32fec56d5d0fbc05e9ad77318fc50186419013e820a05",
    },
    Draft02ClaimVector {
        id: "cbor_uri_match_rules",
        claims_edn: "{312 /catu/: {1 /host/: {0 /exact/: \"example.com\"}, 3 /path/: {1 /prefix/: \"/vod/\"}, 8 /extension/: {0 /exact/: \".m3u8\"}}}",
        payload_cbor_hex: "a1190138a301a1006b6578616d706c652e636f6d03a101652f766f642f08a100652e6d337538",
    },
    Draft02ClaimVector {
        id: "cbor_catu_with_moqt",
        claims_edn: "{312 /catu/: {1 /host/: {0 /exact/: \"example.com\"}, 3 /path/: {1 /prefix/: \"/vod/\"}}, -65537 /moqt/: {0: [[[4, 7], [h'6578616d706c652e636f6d', h'766f64']]]}}",
        payload_cbor_hex: "a2190138a201a1006b6578616d706c652e636f6d03a101652f766f642f3a00010000a1008182820407824b6578616d706c652e636f6d43766f64",
    },
    Draft02ClaimVector {
        id: "cbor_alpn",
        claims_edn: "{314 /catalpn/: [h'6d6f712d3030', h'6833']}",
        payload_cbor_hex: "a119013a82466d6f712d3030426833",
    },
];

/// 付録 A.3 の COSE 構造のベクタ
pub struct Draft02TokenVector {
    /// ベクタの識別子
    pub id: &'static str,
    /// claims を CBOR の Extended Diagnostic Notation で示した参考値
    pub claims_edn: &'static str,
    /// 署名アルゴリズムの名前
    pub algorithm: &'static str,
    /// 署名アルゴリズムの COSE 識別子
    pub algorithm_id: i64,
    /// protected ヘッダの CBOR バイト列 (hex)
    pub header_cbor_hex: &'static str,
    /// claims の CBOR バイト列 (hex)
    pub payload_cbor_hex: &'static str,
    /// COSE_Mac0 の MAC (hex)。COSE_Sign1 のベクタでは `None`
    pub tag_hex: Option<&'static str>,
    /// COSE_Sign1 の署名 (hex)。COSE_Mac0 のベクタでは `None`
    pub signature_hex: Option<&'static str>,
    /// COSE_Mac0 の検証鍵 (hex)。COSE_Sign1 のベクタでは `None`
    pub key_hex: Option<&'static str>,
    /// COSE_Sign1 の秘密鍵 (hex)。COSE_Mac0 のベクタでは `None`
    pub private_key_hex: Option<&'static str>,
    /// COSE_Sign1 の公開鍵の x 座標 (hex)。COSE_Mac0 のベクタでは `None`
    pub public_key_x_hex: Option<&'static str>,
    /// COSE_Sign1 の公開鍵の y 座標 (hex)。COSE_Mac0 のベクタでは `None`
    pub public_key_y_hex: Option<&'static str>,
    /// COSE 構造全体の CBOR バイト列 (hex)
    pub cose_hex: &'static str,
    /// COSE 構造全体のパディング無し base64url
    pub cose_b64: &'static str,
    /// ベクタが期待する検証結果
    pub valid: bool,
}

/// 付録 A.3 のベクタ一覧
pub const DRAFT_02_TOKEN_VECTORS: &[Draft02TokenVector] = &[
    Draft02TokenVector {
        id: "token_hmac_minimal",
        claims_edn: "{1 /iss/: \"https://auth.example.com\", 3 /aud/: [\"https://relay.example.com\"], 4 /exp/: 1700086400}",
        algorithm: "HMAC-SHA256",
        algorithm_id: 5,
        header_cbor_hex: "a201051063434154",
        payload_cbor_hex: "a301781868747470733a2f2f617574682e6578616d706c652e636f6d0381781968747470733a2f2f72656c61792e6578616d706c652e636f6d041a65554280",
        tag_hex: Some("16ca16a2d4e2476528ae858d91b00f257a507fe57284bfb13d7d6ca1f065e230"),
        signature_hex: None,
        key_hex: Some("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"),
        private_key_hex: None,
        public_key_x_hex: None,
        public_key_y_hex: None,
        cose_hex: "d18448a201051063434154a0583fa301781868747470733a2f2f617574682e6578616d706c652e636f6d0381781968747470733a2f2f72656c61792e6578616d706c652e636f6d041a65554280582016ca16a2d4e2476528ae858d91b00f257a507fe57284bfb13d7d6ca1f065e230",
        cose_b64: "0YRIogEFEGNDQVSgWD-jAXgYaHR0cHM6Ly9hdXRoLmV4YW1wbGUuY29tA4F4GWh0dHBzOi8vcmVsYXkuZXhhbXBsZS5jb20EGmVVQoBYIBbKFqLU4kdlKK6FjZGwDyV6UH_lcoS_sT19bKHwZeIw",
        valid: true,
    },
    Draft02TokenVector {
        id: "token_hmac_full",
        claims_edn: "{1 /iss/: \"https://issuer.moq.example\", 2 /sub/: \"user:alice@example.com\", 3 /aud/: [\"https://relay1.example.com\", \"https://relay2.example.com\"], 4 /exp/: 1700086400, 5 /nbf/: 1700000000, 6 /iat/: 1700000000, 7 /cti/: h'766563746f722d303032', 310 /catv/: 1, 311 /catnip/: [52(h'cb007132')], 312 /catu/: {3 /path/: {1 /prefix/: \"/live/\"}}}",
        algorithm: "HMAC-SHA256",
        algorithm_id: 5,
        header_cbor_hex: "a201051063434154",
        payload_cbor_hex: "aa01781a68747470733a2f2f6973737565722e6d6f712e6578616d706c650276757365723a616c696365406578616d706c652e636f6d0382781a68747470733a2f2f72656c6179312e6578616d706c652e636f6d781a68747470733a2f2f72656c6179322e6578616d706c652e636f6d041a65554280051a6553f100061a6553f100074a766563746f722d3030321901360119013781d83444cb007132190138a103a101662f6c6976652f",
        tag_hex: Some("29c1a6612ce6d49273fd8c0116b9decbc287662d5ba5d67defafaf6ff0c11278"),
        signature_hex: None,
        key_hex: Some("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"),
        private_key_hex: None,
        public_key_x_hex: None,
        public_key_y_hex: None,
        cose_hex: "d18448a201051063434154a058abaa01781a68747470733a2f2f6973737565722e6d6f712e6578616d706c650276757365723a616c696365406578616d706c652e636f6d0382781a68747470733a2f2f72656c6179312e6578616d706c652e636f6d781a68747470733a2f2f72656c6179322e6578616d706c652e636f6d041a65554280051a6553f100061a6553f100074a766563746f722d3030321901360119013781d83444cb007132190138a103a101662f6c6976652f582029c1a6612ce6d49273fd8c0116b9decbc287662d5ba5d67defafaf6ff0c11278",
        cose_b64: "0YRIogEFEGNDQVSgWKuqAXgaaHR0cHM6Ly9pc3N1ZXIubW9xLmV4YW1wbGUCdnVzZXI6YWxpY2VAZXhhbXBsZS5jb20DgngaaHR0cHM6Ly9yZWxheTEuZXhhbXBsZS5jb214Gmh0dHBzOi8vcmVsYXkyLmV4YW1wbGUuY29tBBplVUKABRplU_EABhplU_EAB0p2ZWN0b3ItMDAyGQE2ARkBN4HYNETLAHEyGQE4oQOhAWYvbGl2ZS9YICnBpmEs5tSSc_2MARa53svCh2YtW6XWfe-vr2_wwRJ4",
        valid: true,
    },
    Draft02TokenVector {
        id: "token_es256",
        claims_edn: "{1 /iss/: \"https://auth.example.com\", 3 /aud/: [\"https://moq-relay.example.com\"], 4 /exp/: 1700086400, 5 /nbf/: 1700000000}",
        algorithm: "ES256",
        algorithm_id: -7,
        header_cbor_hex: "a201261063434154",
        payload_cbor_hex: "a401781868747470733a2f2f617574682e6578616d706c652e636f6d0381781d68747470733a2f2f6d6f712d72656c61792e6578616d706c652e636f6d041a65554280051a6553f100",
        tag_hex: None,
        signature_hex: Some(
            "ed8cf586f32ba410f2b3097642eb32255c8dc555e9e058c48084809df557887bc5ee4974a9ef5c4557c1557de799628b634cafaa1b74c936968e0c9c3a48f222",
        ),
        key_hex: None,
        private_key_hex: Some("c9afa9d845ba75166b5c215767b1d6934e50c3db36e89b127b8a622b120f6721"),
        public_key_x_hex: Some("60fed4ba255a9d31c961eb74c6356d68c049b8923b61fa6ce669622e60f29fb6"),
        public_key_y_hex: Some("7903fe1008b8bc99a41ae9e95628bc64f2f1b20c2d7e9f5177a3c294d4462299"),
        cose_hex: "d28448a201261063434154a05849a401781868747470733a2f2f617574682e6578616d706c652e636f6d0381781d68747470733a2f2f6d6f712d72656c61792e6578616d706c652e636f6d041a65554280051a6553f1005840ed8cf586f32ba410f2b3097642eb32255c8dc555e9e058c48084809df557887bc5ee4974a9ef5c4557c1557de799628b634cafaa1b74c936968e0c9c3a48f222",
        cose_b64: "0oRIogEmEGNDQVSgWEmkAXgYaHR0cHM6Ly9hdXRoLmV4YW1wbGUuY29tA4F4HWh0dHBzOi8vbW9xLXJlbGF5LmV4YW1wbGUuY29tBBplVUKABRplU_EAWEDtjPWG8yukEPKzCXZC6zIlXI3FVengWMSAhICd9VeIe8XuSXSp71xFV8FVfeeZYotjTK-qG3TJNpaODJw6SPIi",
        valid: true,
    },
];

/// 付録 A.4 の DPoP バインディングのベクタ
pub struct Draft02DpopVector {
    /// ベクタの識別子
    pub id: &'static str,
    /// claims を CBOR の Extended Diagnostic Notation で示した参考値
    pub claims_edn: &'static str,
    /// 署名アルゴリズムの名前。ベクタに記載が無い場合は `None`
    #[cfg_attr(not(feature = "aws-lc-rs"), expect(dead_code))]
    pub algorithm: Option<&'static str>,
    /// `cnf` の JWK サムプリント (hex)
    pub cnf_jkt_hex: &'static str,
    /// サムプリントの計算方法の記載
    pub cnf_jkt_source: Option<&'static str>,
    /// `catdpop` のウィンドウ (秒)
    pub window_seconds: f64,
    /// `catdpop` の jti の扱い。ベクタに記載が無い場合は `None`
    pub honor_jti: Option<bool>,
    /// サムプリント計算の入力となる正規化 JSON (ある場合)
    pub jwk_thumbprint_input: Option<&'static str>,
    /// ES256 の公開鍵の x 座標 (hex)。HMAC のベクタでは `None`
    pub public_key_x_hex: Option<&'static str>,
    /// ES256 の公開鍵の y 座標 (hex)。HMAC のベクタでは `None`
    pub public_key_y_hex: Option<&'static str>,
    /// claims の CBOR バイト列 (hex)
    pub payload_cbor_hex: &'static str,
    /// COSE 構造全体の CBOR バイト列 (hex)
    pub cose_hex: &'static str,
    /// COSE 構造全体のパディング無し base64url
    pub cose_b64: &'static str,
}

/// 付録 A.4 のベクタ一覧
pub const DRAFT_02_DPOP_VECTORS: &[Draft02DpopVector] = &[
    Draft02DpopVector {
        id: "dpop_jwk_binding",
        claims_edn: "{1 /iss/: \"https://auth.example.com\", 4 /exp/: 1700086400, 8 /cnf/: {323: h'0cebf1bc9880748a95588905b79843b42ba75cb174055 e3e246bf87fe00b4a6d'}, 321 /catdpop/: {0: 60, 1: 1}}",
        algorithm: None,
        cnf_jkt_hex: "0cebf1bc9880748a95588905b79843b42ba75cb174055e3e246bf87fe00b4a6d",
        cnf_jkt_source: Some("SHA-256(RFC 7638 canonical JWK of ES256 test key)"),
        window_seconds: 60.0,
        honor_jti: Some(true),
        jwk_thumbprint_input: None,
        public_key_x_hex: None,
        public_key_y_hex: None,
        payload_cbor_hex: "a401781868747470733a2f2f617574682e6578616d706c652e636f6d041a6555428008a119014358200cebf1bc9880748a95588905b79843b42ba75cb174055e3e246bf87fe00b4a6d190141a200183c0101",
        cose_hex: "d18448a201051063434154a05852a401781868747470733a2f2f617574682e6578616d706c652e636f6d041a6555428008a119014358200cebf1bc9880748a95588905b79843b42ba75cb174055e3e246bf87fe00b4a6d190141a200183c01015820cef11ccef3533e37884638bbff1e08cbcf3d6846afcc0257bf2530416b076242",
        cose_b64: "0YRIogEFEGNDQVSgWFKkAXgYaHR0cHM6Ly9hdXRoLmV4YW1wbGUuY29tBBplVUKACKEZAUNYIAzr8byYgHSKlViJBbeYQ7Qrp1yxdAVePiRr-H_gC0ptGQFBogAYPAEBWCDO8RzO81M-N4hGOLv_HgjLzz1oRq_MAle_JTBBawdiQg",
    },
    Draft02DpopVector {
        id: "dpop_no_jti",
        claims_edn: "{1 /iss/: \"https://auth.example.com\", 4 /exp/: 1700086400, 8 /cnf/: {323: h'3c82dfd6358ba804bd90879c34e743bbe13aeab798066 4944f37a0ec0063fe95'}, 321 /catdpop/: {0: 300, 1: 0}}",
        algorithm: None,
        cnf_jkt_hex: "3c82dfd6358ba804bd90879c34e743bbe13aeab7980664944f37a0ec0063fe95",
        cnf_jkt_source: Some("SHA-256 of 'test-public-key-material'"),
        window_seconds: 300.0,
        honor_jti: Some(false),
        jwk_thumbprint_input: None,
        public_key_x_hex: None,
        public_key_y_hex: None,
        payload_cbor_hex: "a401781868747470733a2f2f617574682e6578616d706c652e636f6d041a6555428008a119014358203c82dfd6358ba804bd90879c34e743bbe13aeab7980664944f37a0ec0063fe95190141a20019012c0100",
        cose_hex: "d18448a201051063434154a05853a401781868747470733a2f2f617574682e6578616d706c652e636f6d041a6555428008a119014358203c82dfd6358ba804bd90879c34e743bbe13aeab7980664944f37a0ec0063fe95190141a20019012c01005820ecb7a5b5667b95f20c673417718eac9f5d997fd427485942a61048cdd8c50e0c",
        cose_b64: "0YRIogEFEGNDQVSgWFOkAXgYaHR0cHM6Ly9hdXRoLmV4YW1wbGUuY29tBBplVUKACKEZAUNYIDyC39Y1i6gEvZCHnDTnQ7vhOuq3mAZklE83oOwAY_6VGQFBogAZASwBAFgg7LeltWZ7lfIMZzQXcY6sn12Zf9QnSFlCphBIzdjFDgw",
    },
    Draft02DpopVector {
        id: "dpop_es256_real_binding",
        claims_edn: "{1 /iss/: \"https://auth.example.com\", 3 /aud/: [\"https://relay.example.com\"], 4 /exp/: 1700086400, 8 /cnf/: {323: h'0cebf1bc9880748a95588905b79843b42ba75cb174055 e3e246bf87fe00b4a6d'}, 321 /catdpop/: {0: 120}}",
        algorithm: Some("ES256"),
        cnf_jkt_hex: "0cebf1bc9880748a95588905b79843b42ba75cb174055e3e246bf87fe00b4a6d",
        cnf_jkt_source: None,
        window_seconds: 120.0,
        honor_jti: None,
        jwk_thumbprint_input: Some(
            "{\"crv\":\"P-256\",\"kty\":\"EC\",\"x\":\"YP7UuiVanTHJYet0xjVtaMBJuJI7Yfps5mliLmDyn7Y\",\"y\":\"eQP-EAi4vJmkGunpVii8ZPLxsgwtfp9Rd6PClNRGIpk\"}",
        ),
        public_key_x_hex: Some("60fed4ba255a9d31c961eb74c6356d68c049b8923b61fa6ce669622e60f29fb6"),
        public_key_y_hex: Some("7903fe1008b8bc99a41ae9e95628bc64f2f1b20c2d7e9f5177a3c294d4462299"),
        payload_cbor_hex: "a501781868747470733a2f2f617574682e6578616d706c652e636f6d0381781968747470733a2f2f72656c61792e6578616d706c652e636f6d041a6555428008a119014358200cebf1bc9880748a95588905b79843b42ba75cb174055e3e246bf87fe00b4a6d190141a1001878",
        cose_hex: "d28448a201261063434154a0586da501781868747470733a2f2f617574682e6578616d706c652e636f6d0381781968747470733a2f2f72656c61792e6578616d706c652e636f6d041a6555428008a119014358200cebf1bc9880748a95588905b79843b42ba75cb174055e3e246bf87fe00b4a6d190141a10018785840cdb69dc3db876b1d05a8f808e5286a861026fc8b58b9a9aa97f23bdf0253ff56357f9129ee9b09667701357c8d700721b3029928d4fe573d56a88744dce04f82",
        cose_b64: "0oRIogEmEGNDQVSgWG2lAXgYaHR0cHM6Ly9hdXRoLmV4YW1wbGUuY29tA4F4GWh0dHBzOi8vcmVsYXkuZXhhbXBsZS5jb20EGmVVQoAIoRkBQ1ggDOvxvJiAdIqVWIkFt5hDtCunXLF0BV4-JGv4f-ALSm0ZAUGhABh4WEDNtp3D24drHQWo-AjlKGqGECb8i1i5qaqX8jvfAlP_VjV_kSnumwlmdwE1fI1wByGzApko1P5XPVaoh0Tc4E-C",
    },
];

/// 付録 A.5 の認可テスト
pub struct Draft02AuthorizationTest {
    /// アクションの整数値
    pub action: i64,
    /// Track Namespace のフィールド (UTF-8)
    pub namespace: &'static [&'static str],
    /// Track Name (UTF-8)
    pub track: &'static str,
    /// 期待する判定
    pub expected: bool,
}

/// 付録 A.5 の MOQT 認可スコープのベクタ
pub struct Draft02ScopeVector {
    /// ベクタの識別子
    pub id: &'static str,
    /// claims を CBOR の Extended Diagnostic Notation で示した参考値
    pub claims_edn: &'static str,
    /// `moqt-reval` (ある場合)
    pub moqt_reval: Option<f64>,
    /// claims の CBOR バイト列 (hex)
    pub payload_cbor_hex: &'static str,
    /// COSE 構造全体の CBOR バイト列 (hex)
    pub cose_hex: &'static str,
    /// COSE 構造全体のパディング無し base64url
    pub cose_b64: &'static str,
    /// 公式の認可テスト
    ///
    /// `moqt_admin_wildcard` は action 0 の期待値 `true` を含む。action 0 は
    /// draft-02 §2.1 Table 1 の 1 〜 9 に無いため、公式の期待値から削除せず
    /// そのまま保持する。
    pub authorization_tests: &'static [Draft02AuthorizationTest],
}

/// 付録 A.5 のベクタ一覧
pub const DRAFT_02_SCOPE_VECTORS: &[Draft02ScopeVector] = &[
    Draft02ScopeVector {
        id: "moqt_publisher_exact",
        claims_edn: "{1 /iss/: \"https://auth.example.com\", 4 /exp/: 1700086400, -65537 /moqt/: {0: [[[2, 6], [h'6578616d706c652e636f6d', h'616c696365'], [1, h'766964656f2d']]]}}",
        moqt_reval: None,
        payload_cbor_hex: "a301781868747470733a2f2f617574682e6578616d706c652e636f6d041a655542803a00010000a1008183820206824b6578616d706c652e636f6d45616c696365820146766964656f2d",
        cose_hex: "d18448a201051063434154a0584aa301781868747470733a2f2f617574682e6578616d706c652e636f6d041a655542803a00010000a1008183820206824b6578616d706c652e636f6d45616c696365820146766964656f2d5820467c57ade72d6be629b745ef33d6e28ec38ba1ca8bd3feb851f3832af94fe686",
        cose_b64: "0YRIogEFEGNDQVSgWEqjAXgYaHR0cHM6Ly9hdXRoLmV4YW1wbGUuY29tBBplVUKAOgABAAChAIGDggIGgktleGFtcGxlLmNvbUVhbGljZYIBRnZpZGVvLVggRnxXrecta-Ypt0XvM9bijsOLocqL0_64UfODKvlP5oY",
        authorization_tests: &[
            Draft02AuthorizationTest {
                action: 2,
                namespace: &["example.com", "alice"],
                track: "video-hd",
                expected: true,
            },
            Draft02AuthorizationTest {
                action: 6,
                namespace: &["example.com", "alice"],
                track: "video-sd",
                expected: true,
            },
            Draft02AuthorizationTest {
                action: 6,
                namespace: &["example.com", "alice"],
                track: "audio-main",
                expected: false,
            },
            Draft02AuthorizationTest {
                action: 4,
                namespace: &["example.com", "alice"],
                track: "video-hd",
                expected: false,
            },
            Draft02AuthorizationTest {
                action: 6,
                namespace: &["example.com", "bob"],
                track: "video-hd",
                expected: false,
            },
        ],
    },
    Draft02ScopeVector {
        id: "moqt_subscriber_prefix",
        claims_edn: "{1 /iss/: \"https://auth.example.com\", 4 /exp/: 1700086400, -65537 /moqt/: {0: [[[3, 4, 7], [[1, h'636f6e666572656e63652e6578616d706c65']]]]}}",
        moqt_reval: None,
        payload_cbor_hex: "a301781868747470733a2f2f617574682e6578616d706c652e636f6d041a655542803a00010000a10081828303040781820152636f6e666572656e63652e6578616d706c65",
        cose_hex: "d18448a201051063434154a05845a301781868747470733a2f2f617574682e6578616d706c652e636f6d041a655542803a00010000a10081828303040781820152636f6e666572656e63652e6578616d706c655820e0502202b8c01d5ab61508cd19b7dd943b287be0f26c3d37c12e583ca87a937b",
        cose_b64: "0YRIogEFEGNDQVSgWEWjAXgYaHR0cHM6Ly9hdXRoLmV4YW1wbGUuY29tBBplVUKAOgABAAChAIGCgwMEB4GCAVJjb25mZXJlbmNlLmV4YW1wbGVYIOBQIgK4wB1athUIzRm33ZQ7KHvg8mw9N8EuWDyoepN7",
        authorization_tests: &[
            Draft02AuthorizationTest {
                action: 4,
                namespace: &["conference.example.room1"],
                track: "audio",
                expected: true,
            },
            Draft02AuthorizationTest {
                action: 7,
                namespace: &["conference.example.room2"],
                track: "video",
                expected: true,
            },
            Draft02AuthorizationTest {
                action: 4,
                namespace: &["other.domain"],
                track: "audio",
                expected: false,
            },
            Draft02AuthorizationTest {
                action: 6,
                namespace: &["conference.example.room1"],
                track: "audio",
                expected: false,
            },
        ],
    },
    Draft02ScopeVector {
        id: "moqt_multi_scope",
        claims_edn: "{1 /iss/: \"https://auth.example.com\", 4 /exp/: 1700086400, -65537 /moqt/: {0: [[[2, 6], [h'6c6976652e6578616d706c65', h'73747564696f2d61']], [[4, 7], [[1, h'6c6976652e6578616d706c65']]]]}, -65538 /moqt-reval/: 300}",
        moqt_reval: Some(300.0),
        payload_cbor_hex: "a401781868747470733a2f2f617574682e6578616d706c652e636f6d041a655542803a00010000a1008282820206824c6c6976652e6578616d706c654873747564696f2d61828204078182014c6c6976652e6578616d706c653a0001000119012c",
        cose_hex: "d18448a201051063434154a05861a401781868747470733a2f2f617574682e6578616d706c652e636f6d041a655542803a00010000a1008282820206824c6c6976652e6578616d706c654873747564696f2d61828204078182014c6c6976652e6578616d706c653a0001000119012c5820f1080af59a719fbcb7a036aa6f9714f52ada467b8c1677c87ade0fcfccaaf50f",
        cose_b64: "0YRIogEFEGNDQVSgWGGkAXgYaHR0cHM6Ly9hdXRoLmV4YW1wbGUuY29tBBplVUKAOgABAAChAIKCggIGgkxsaXZlLmV4YW1wbGVIc3R1ZGlvLWGCggQHgYIBTGxpdmUuZXhhbXBsZToAAQABGQEsWCDxCAr1mnGfvLegNqpvlxT1KtpGe4wWd8h63g_PzKr1Dw",
        authorization_tests: &[
            Draft02AuthorizationTest {
                action: 6,
                namespace: &["live.example", "studio-a"],
                track: "cam1",
                expected: true,
            },
            Draft02AuthorizationTest {
                action: 4,
                namespace: &["live.example.studio-b"],
                track: "cam1",
                expected: true,
            },
            Draft02AuthorizationTest {
                action: 6,
                namespace: &["live.example", "studio-b"],
                track: "cam1",
                expected: false,
            },
            Draft02AuthorizationTest {
                action: 2,
                namespace: &["other.example", "studio-a"],
                track: "",
                expected: false,
            },
        ],
    },
    Draft02ScopeVector {
        id: "moqt_admin_wildcard",
        claims_edn: "{1 /iss/: \"https://auth.example.com\", 4 /exp/: 1700086400, -65537 /moqt/: {0: [[[0, 1, 2, 3, 4, 5, 6, 7, 8]]]}}",
        moqt_reval: None,
        payload_cbor_hex: "a301781868747470733a2f2f617574682e6578616d706c652e636f6d041a655542803a00010000a100818189000102030405060708",
        cose_hex: "d18448a201051063434154a05835a301781868747470733a2f2f617574682e6578616d706c652e636f6d041a655542803a00010000a1008181890001020304050607085820172ebe07f227a41c319684ef916fdbf142d391fac6902e38bcdcfb3966b46a50",
        cose_b64: "0YRIogEFEGNDQVSgWDWjAXgYaHR0cHM6Ly9hdXRoLmV4YW1wbGUuY29tBBplVUKAOgABAAChAIGBiQABAgMEBQYHCFggFy6-B_InpBwxloTvkW_b8ULTkfrGkC44vNz7OWa0alA",
        authorization_tests: &[
            Draft02AuthorizationTest {
                action: 0,
                namespace: &["any.namespace"],
                track: "any-track",
                expected: true,
            },
            Draft02AuthorizationTest {
                action: 6,
                namespace: &["any.namespace"],
                track: "any-track",
                expected: true,
            },
            Draft02AuthorizationTest {
                action: 8,
                namespace: &["any.namespace"],
                track: "status",
                expected: true,
            },
        ],
    },
    Draft02ScopeVector {
        id: "moqt_suffix_match",
        claims_edn: "{1 /iss/: \"https://auth.example.com\", 4 /exp/: 1700086400, -65537 /moqt/: {0: [[[4], [[2, h'2e6578616d706c652e636f6d']], [2, h'2d617564696f']]]}}",
        moqt_reval: None,
        payload_cbor_hex: "a301781868747470733a2f2f617574682e6578616d706c652e636f6d041a655542803a00010000a100818381048182024c2e6578616d706c652e636f6d8202462d617564696f",
        cose_hex: "d18448a201051063434154a05846a301781868747470733a2f2f617574682e6578616d706c652e636f6d041a655542803a00010000a100818381048182024c2e6578616d706c652e636f6d8202462d617564696f5820602c1ceb2729485a2b5eb09249e254a41d42abd0b63b2846fd677d7fbf593920",
        cose_b64: "0YRIogEFEGNDQVSgWEajAXgYaHR0cHM6Ly9hdXRoLmV4YW1wbGUuY29tBBplVUKAOgABAAChAIGDgQSBggJMLmV4YW1wbGUuY29tggJGLWF1ZGlvWCBgLBzrJylIWitesJJJ4lSkHUKr0LY7KEb9Z31_v1k5IA",
        authorization_tests: &[
            Draft02AuthorizationTest {
                action: 4,
                namespace: &["cdn.example.com"],
                track: "stream1-audio",
                expected: true,
            },
            Draft02AuthorizationTest {
                action: 4,
                namespace: &["cdn.example.com"],
                track: "stream1-video",
                expected: false,
            },
            Draft02AuthorizationTest {
                action: 4,
                namespace: &["cdn.other.org"],
                track: "stream1-audio",
                expected: false,
            },
        ],
    },
];

/// 付録 A.6 の Composite claim のベクタ
pub struct Draft02CompositeVector {
    /// ベクタの識別子
    pub id: &'static str,
    /// claims を CBOR の Extended Diagnostic Notation で示した参考値
    pub claims_edn: &'static str,
    /// 公式の operator 名 (OR / NOR / AND)
    ///
    /// 公式ベクタではすべての Composite claim の key が暫定の 0 であるため、
    /// wire の CBOR だけからは operator を区別できない。この値は JSON の
    /// `composite.operator` の記載であり、wire から復元したものではない。
    pub operator: &'static str,
    /// 公式の claim key (すべて暫定の 0)
    pub claim_key: i64,
    /// claims の CBOR バイト列 (hex)
    pub payload_cbor_hex: &'static str,
    /// COSE 構造全体の CBOR バイト列 (hex)
    pub cose_hex: &'static str,
    /// COSE 構造全体のパディング無し base64url
    pub cose_b64: &'static str,
}

/// 付録 A.6 のベクタ一覧
pub const DRAFT_02_COMPOSITE_VECTORS: &[Draft02CompositeVector] = &[
    Draft02CompositeVector {
        id: "composite_or_simple",
        claims_edn: "{1 /iss/: \"https://auth.example.com\", 4 /exp/: 1700086400, 0 /or/: [{1 /iss/: \"https://auth.example.com\", 4 /exp/: 1700086400, 310 /catv/: 1}, {1 /iss/: \"https://auth-backup.example.com\", 4 /exp/: 1700090000, 310 /catv/: 1}]}",
        operator: "OR",
        claim_key: 0,
        payload_cbor_hex: "a301781868747470733a2f2f617574682e6578616d706c652e636f6d041a655542800082a301781868747470733a2f2f617574682e6578616d706c652e636f6d041a6555428019013601a301781f68747470733a2f2f617574682d6261636b75702e6578616d706c652e636f6d041a6555509019013601",
        cose_hex: "d18448a201051063434154a05877a301781868747470733a2f2f617574682e6578616d706c652e636f6d041a655542800082a301781868747470733a2f2f617574682e6578616d706c652e636f6d041a6555428019013601a301781f68747470733a2f2f617574682d6261636b75702e6578616d706c652e636f6d041a655550901901360158208ebb4ac5a34771116903baff5e544f3047468faeb077820f26ea2df940b6c58d",
        cose_b64: "0YRIogEFEGNDQVSgWHejAXgYaHR0cHM6Ly9hdXRoLmV4YW1wbGUuY29tBBplVUKAAIKjAXgYaHR0cHM6Ly9hdXRoLmV4YW1wbGUuY29tBBplVUKAGQE2AaMBeB9odHRwczovL2F1dGgtYmFja3VwLmV4YW1wbGUuY29tBBplVVCQGQE2AVggjrtKxaNHcRFpA7r_XlRPMEdGj66wd4IPJuot-UC2xY0",
    },
    Draft02CompositeVector {
        id: "composite_and",
        claims_edn: "{1 /iss/: \"https://auth.example.com\", 4 /exp/: 1700086400, 0 /and/: [{4 /exp/: 1700086400, 310 /catv/: 1}, {4 /exp/: 1700086400, 311 /catnip/: [52(h'0a000000')]}]}",
        operator: "AND",
        claim_key: 0,
        payload_cbor_hex: "a301781868747470733a2f2f617574682e6578616d706c652e636f6d041a655542800082a2041a6555428019013601a2041a6555428019013781d834440a000000",
        cose_hex: "d18448a201051063434154a05841a301781868747470733a2f2f617574682e6578616d706c652e636f6d041a655542800082a2041a6555428019013601a2041a6555428019013781d834440a00000058208ca3213e71dfe3900fd910fcb7f6d365949e72e81ceba80f2ff24048502e9a1a",
        cose_b64: "0YRIogEFEGNDQVSgWEGjAXgYaHR0cHM6Ly9hdXRoLmV4YW1wbGUuY29tBBplVUKAAIKiBBplVUKAGQE2AaIEGmVVQoAZATeB2DRECgAAAFggjKMhPnHf45AP2RD8t_bTZZSecugc66gPL_JASFAumho",
    },
    Draft02CompositeVector {
        id: "composite_nor",
        claims_edn: "{1 /iss/: \"https://auth.example.com\", 4 /exp/: 1700086400, 0 /nor/: [{1 /iss/: \"https://revoked.example.com\", 4 /exp/: 1700086400}]}",
        operator: "NOR",
        claim_key: 0,
        payload_cbor_hex: "a301781868747470733a2f2f617574682e6578616d706c652e636f6d041a655542800081a201781b68747470733a2f2f7265766f6b65642e6578616d706c652e636f6d041a65554280",
        cose_hex: "d18448a201051063434154a05849a301781868747470733a2f2f617574682e6578616d706c652e636f6d041a655542800081a201781b68747470733a2f2f7265766f6b65642e6578616d706c652e636f6d041a655542805820a5419cd0db084ac9efc26c21ff2663b4ec0cff6622ae48719f65738cb93fd19e",
        cose_b64: "0YRIogEFEGNDQVSgWEmjAXgYaHR0cHM6Ly9hdXRoLmV4YW1wbGUuY29tBBplVUKAAIGiAXgbaHR0cHM6Ly9yZXZva2VkLmV4YW1wbGUuY29tBBplVUKAWCClQZzQ2whKye_CbCH_JmO07Az_ZiKuSHGfZXOMuT_Rng",
    },
    Draft02CompositeVector {
        id: "composite_nested",
        claims_edn: "{1 /iss/: \"https://auth.example.com\", 4 /exp/: 1700086400, 0 /or/: [{1 /iss/: \"https://primary.example.com\", 4 /exp/: 1700086400}, {0 /and/: [{1 /iss/: \"https://secondary.example.com\", 4 /exp/: 1700086400}, {4 /exp/: 1700086400, 310 /catv/: 1}]}]}",
        operator: "OR",
        claim_key: 0,
        payload_cbor_hex: "a301781868747470733a2f2f617574682e6578616d706c652e636f6d041a655542800082a201781b68747470733a2f2f7072696d6172792e6578616d706c652e636f6d041a65554280a10082a201781d68747470733a2f2f7365636f6e646172792e6578616d706c652e636f6d041a65554280a2041a6555428019013601",
        cose_hex: "d18448a201051063434154a0587ea301781868747470733a2f2f617574682e6578616d706c652e636f6d041a655542800082a201781b68747470733a2f2f7072696d6172792e6578616d706c652e636f6d041a65554280a10082a201781d68747470733a2f2f7365636f6e646172792e6578616d706c652e636f6d041a65554280a2041a65554280190136015820c60304bcd67f2f78bf9e83e84fe3a2d13f8e809659a32cc480c76cfe5ae1da06",
        cose_b64: "0YRIogEFEGNDQVSgWH6jAXgYaHR0cHM6Ly9hdXRoLmV4YW1wbGUuY29tBBplVUKAAIKiAXgbaHR0cHM6Ly9wcmltYXJ5LmV4YW1wbGUuY29tBBplVUKAoQCCogF4HWh0dHBzOi8vc2Vjb25kYXJ5LmV4YW1wbGUuY29tBBplVUKAogQaZVVCgBkBNgFYIMYDBLzWfy94v56D6E_jotE_joCWWaMsxIDHbP5a4doG",
    },
];

/// 付録 A.7 のトークン検証のベクタ
pub struct Draft02ValidationVector {
    /// ベクタの識別子
    pub id: &'static str,
    /// COSE_Mac0 構造全体のパディング無し base64url
    pub token: &'static str,
    /// 改ざん前の COSE バイト列 (hex)。改ざんベクタだけが持つ
    pub original_cose_hex: Option<&'static str>,
    /// 検証に使う現在時刻 (UNIX 秒)
    pub reference_time: Option<f64>,
    /// 検証側が期待する `iss`
    pub expected_issuers: &'static [&'static str],
    /// 検証側が期待する `aud`
    pub expected_audiences: &'static [&'static str],
    /// 公式が期待する結果 (`valid` または `error`)
    pub expected_result: &'static str,
    /// 公式が期待するエラー名
    pub expected_error: Option<&'static str>,
    /// 署名検証に使う鍵 (hex)。改ざんベクタだけが持つ
    #[cfg_attr(not(feature = "aws-lc-rs"), expect(dead_code))]
    pub key_hex: Option<&'static str>,
    /// 正しい鍵 (hex)。誤った鍵のベクタだけが持つ
    #[cfg_attr(not(feature = "aws-lc-rs"), expect(dead_code))]
    pub correct_key_hex: Option<&'static str>,
    /// 誤った鍵 (hex)。誤った鍵のベクタだけが持つ
    #[cfg_attr(not(feature = "aws-lc-rs"), expect(dead_code))]
    pub wrong_key_hex: Option<&'static str>,
    /// トークンのアルゴリズム識別子 (ある場合)
    pub token_algorithm_id: Option<i64>,
    /// 検証側が期待するアルゴリズム識別子 (ある場合)
    pub verifier_algorithm_id: Option<i64>,
}

/// 付録 A.7 のベクタ一覧
pub const DRAFT_02_VALIDATION_VECTORS: &[Draft02ValidationVector] = &[
    Draft02ValidationVector {
        id: "valid_basic",
        token: "0YRIogEFEGNDQVSgWEWkAXgYaHR0cHM6Ly9hdXRoLmV4YW1wbGUuY29tA4F4GWh0dHBzOi8vcmVsYXkuZXhhbXBsZS5jb20EGmVVQoAFGmVT8QBYIP30vfENhlwaLMuZjMPeYXjz8QWuqCDlyzStGQVQ4_xi",
        original_cose_hex: None,
        reference_time: Some(1700003600.0),
        expected_issuers: &["https://auth.example.com"],
        expected_audiences: &["https://relay.example.com"],
        expected_result: "valid",
        expected_error: None,
        key_hex: None,
        correct_key_hex: None,
        wrong_key_hex: None,
        token_algorithm_id: None,
        verifier_algorithm_id: None,
    },
    Draft02ValidationVector {
        id: "invalid_expired",
        token: "0YRIogEFEGNDQVSgWCKiAXgYaHR0cHM6Ly9hdXRoLmV4YW1wbGUuY29tBBpfXhAAWCDNInUDQ2qsHo8l8Gj0tzb1HIYIpBheVbVoItgT7Ua6aA",
        original_cose_hex: None,
        reference_time: Some(1700000000.0),
        expected_issuers: &[],
        expected_audiences: &[],
        expected_result: "error",
        expected_error: Some("TokenExpired"),
        key_hex: None,
        correct_key_hex: None,
        wrong_key_hex: None,
        token_algorithm_id: None,
        verifier_algorithm_id: None,
    },
    Draft02ValidationVector {
        id: "invalid_not_yet_valid",
        token: "0YRIogEFEGNDQVSgWCijAXgYaHR0cHM6Ly9hdXRoLmV4YW1wbGUuY29tBBplVpQABRplVUKAWCArREpqUH1KiIrMzevyeLr6t_y-L6oheaJGUU9hwhYRbg",
        original_cose_hex: None,
        reference_time: Some(1700000000.0),
        expected_issuers: &[],
        expected_audiences: &[],
        expected_result: "error",
        expected_error: Some("TokenNotYetValid"),
        key_hex: None,
        correct_key_hex: None,
        wrong_key_hex: None,
        token_algorithm_id: None,
        verifier_algorithm_id: None,
    },
    Draft02ValidationVector {
        id: "invalid_wrong_issuer",
        token: "0YRIogEFEGNDQVSgWD-jAXgYaHR0cHM6Ly9ldmlsLmV4YW1wbGUuY29tA4F4GWh0dHBzOi8vcmVsYXkuZXhhbXBsZS5jb20EGmVVQoBYIOmQtbjAMxCnCDlMbHiF-XZlN1SV6QSqkZWTguttCpaC",
        original_cose_hex: None,
        reference_time: Some(1700003600.0),
        expected_issuers: &["https://auth.example.com"],
        expected_audiences: &[],
        expected_result: "error",
        expected_error: Some("InvalidIssuer"),
        key_hex: None,
        correct_key_hex: None,
        wrong_key_hex: None,
        token_algorithm_id: None,
        verifier_algorithm_id: None,
    },
    Draft02ValidationVector {
        id: "invalid_wrong_audience",
        token: "0YRIogEFEGNDQVSgWEWjAXgYaHR0cHM6Ly9hdXRoLmV4YW1wbGUuY29tA4F4H2h0dHBzOi8vb3RoZXItcmVsYXkuZXhhbXBsZS5jb20EGmVVQoBYIOeCJJJNAkaNHBBkOdcSFVXKNNj0D8GdGkLmOriIB2pX",
        original_cose_hex: None,
        reference_time: Some(1700003600.0),
        expected_issuers: &["https://auth.example.com"],
        expected_audiences: &["https://relay.example.com"],
        expected_result: "error",
        expected_error: Some("InvalidAudience"),
        key_hex: None,
        correct_key_hex: None,
        wrong_key_hex: None,
        token_algorithm_id: None,
        verifier_algorithm_id: None,
    },
    Draft02ValidationVector {
        id: "invalid_tampered_signature",
        token: "0YRIogEFEGNDQVSgWD-jAXgYaHR0cHM6Ly9hdXRoLmV4YW1wbGUuY29tA4F4GWh0dHBzOi8vcmVsYXkuZXhhbXBsZS5jb20EGmVVQoBYIOnKFqLU4kdlKK6FjZGwDyV6UH_lcoS_sT19bKHwZeIw",
        original_cose_hex: Some(
            "d18448a201051063434154a0583fa301781868747470733a2f2f617574682e6578616d706c652e636f6d0381781968747470733a2f2f72656c61792e6578616d706c652e636f6d041a65554280582016ca16a2d4e2476528ae858d91b00f257a507fe57284bfb13d7d6ca1f065e230",
        ),
        reference_time: None,
        expected_issuers: &[],
        expected_audiences: &[],
        expected_result: "error",
        expected_error: Some("SignatureVerificationFailed"),
        key_hex: Some("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"),
        correct_key_hex: None,
        wrong_key_hex: None,
        token_algorithm_id: None,
        verifier_algorithm_id: None,
    },
    Draft02ValidationVector {
        id: "invalid_wrong_key",
        token: "0YRIogEFEGNDQVSgWCKiAXgYaHR0cHM6Ly9hdXRoLmV4YW1wbGUuY29tBBplVUKAWCCqD2TUB2jOEalC6kta2ptR7vx6flZ0I9L0RV3MCbZ-Aw",
        original_cose_hex: None,
        reference_time: None,
        expected_issuers: &[],
        expected_audiences: &[],
        expected_result: "error",
        expected_error: Some("SignatureVerificationFailed"),
        key_hex: None,
        correct_key_hex: Some("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"),
        wrong_key_hex: Some("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
        token_algorithm_id: None,
        verifier_algorithm_id: None,
    },
    Draft02ValidationVector {
        id: "invalid_algorithm_mismatch",
        token: "0YRIogEFEGNDQVSgWCKiAXgYaHR0cHM6Ly9hdXRoLmV4YW1wbGUuY29tBBplVUKAWCCqD2TUB2jOEalC6kta2ptR7vx6flZ0I9L0RV3MCbZ-Aw",
        original_cose_hex: None,
        reference_time: None,
        expected_issuers: &[],
        expected_audiences: &[],
        expected_result: "error",
        expected_error: Some("InvalidTokenFormat"),
        key_hex: None,
        correct_key_hex: None,
        wrong_key_hex: None,
        token_algorithm_id: Some(5),
        verifier_algorithm_id: Some(-7),
    },
];
