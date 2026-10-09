//! JWK (RFC 7517) と JWK サムプリント (RFC 7638) の property テスト
//!
//! - `Jwk::decode` が生成した JSON の内容と一致すること
//! - `Jwk::canonical_json` が RFC 7638 §3.2 の正規化 (必須メンバーの辞書順・空白無し・
//!   パディング無し) と一致すること
//! - `Jwk::to_cose_key` / `Jwk::matches_public_key` / `default_signing_algorithm` が
//!   鍵の種別から導いたモデルと一致すること
//!
//! サムプリントは `CoseCrypto` を必要とするため `tests/test_c4m/` 側で固定する。

use pbt::common::{sample_bytes, test_runner};
use shiguredo_moqt::c4m::cose::Algorithm;
use shiguredo_moqt::c4m::crypto::{CoseKey, EcCurve, default_signing_algorithm};
use shiguredo_moqt::c4m::jwk::{Jwk, JwkError};

use super::common::{
    base64url_encode, base64url_encode_padded, json_object, json_string, sample_safe_text,
};

/// base64url の値の種類
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Base64Kind {
    /// パディング無しの正規形
    Unpadded,
    /// パディング付き
    Padded,
    /// base64url として不正
    Invalid,
}

/// base64url の値と、その正規化の期待値
#[derive(Debug, Clone)]
struct Base64Value {
    /// JSON に書く生の文字列
    text: String,
    /// 正規化 (パディング無し) した値。不正な場合は `None`
    canonical: Option<String>,
    /// デコードしたバイト列。不正な場合は `None`
    bytes: Option<Vec<u8>>,
    /// 生成した値の種類
    kind: Base64Kind,
}

/// base64url の値を生成する
///
/// 正規形 / パディング付き / 不正 (base64url のアルファベットに無い `*` を含む) の
/// 3 種類を生成する。
fn sample_base64_value(ctx: &mut noprop::TestCaseContext, max_len: usize) -> Base64Value {
    match noprop::sample_weighted_index(ctx, &[4, 2, 2]) {
        0 => {
            let bytes = sample_bytes(ctx, max_len);
            let text = base64url_encode(&bytes);
            Base64Value {
                canonical: Some(text.clone()),
                bytes: Some(bytes),
                text,
                kind: Base64Kind::Unpadded,
            }
        }
        1 => {
            let bytes = sample_bytes(ctx, max_len);
            Base64Value {
                canonical: Some(base64url_encode(&bytes)),
                text: base64url_encode_padded(&bytes),
                bytes: Some(bytes),
                kind: Base64Kind::Padded,
            }
        }
        _ => Base64Value {
            text: format!("{}*", sample_safe_text(ctx, max_len)),
            canonical: None,
            bytes: None,
            kind: Base64Kind::Invalid,
        },
    }
}

/// JWK に入れる欠陥
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JwkDefect {
    /// 欠陥無し
    None,
    /// 秘密鍵のメンバーを含む (RFC 9449 §4.3)
    PrivateMember,
    /// メンバー名が重複している (RFC 7517 §4)
    DuplicateMember,
}

/// JWK の生成結果とモデル
#[derive(Debug, Clone)]
struct JwkModel {
    /// JWK の JSON
    text: String,
    /// デコードに成功する場合の期待値
    decoded: Option<Jwk>,
    /// デコードに失敗する場合の期待エラー
    error: Option<JwkError>,
    /// `canonical_json` の期待値。base64url が不正な場合は `None`
    canonical: Option<String>,
    /// `to_cose_key` の期待値
    cose_key: Result<CoseKey, JwkError>,
    /// `default_signing_algorithm` の期待値 (`cose_key` が `Ok` のときだけ使う)
    algorithm: Option<Algorithm>,
    /// 名前空間を共有しない生成値の種類 (カバレッジゲート用)
    kinds: Vec<Base64Kind>,
}

/// EC の JWK とモデルを生成する
fn sample_ec_model(
    ctx: &mut noprop::TestCaseContext,
    defect: JwkDefect,
) -> (
    Vec<(&'static str, String)>,
    Base64Value,
    Base64Value,
    String,
) {
    let curve = noprop::sample_choice(ctx, &["P-256", "P-384", "P-521", "P-111"]);
    let x = sample_base64_value(ctx, 6);
    let y = sample_base64_value(ctx, 6);
    let mut members = vec![
        ("kty", json_string("EC")),
        ("crv", json_string(curve)),
        ("x", json_string(&x.text)),
        ("y", json_string(&y.text)),
        ("use", json_string("sig")),
    ];
    match defect {
        JwkDefect::None => {}
        JwkDefect::PrivateMember => members.push(("d", json_string("AAAA"))),
        JwkDefect::DuplicateMember => members.push(("x", json_string(&x.text))),
    }
    (members, x, y, String::from(curve))
}

/// OKP の JWK のメンバーを生成する
fn sample_okp_model(
    ctx: &mut noprop::TestCaseContext,
    defect: JwkDefect,
) -> (Vec<(&'static str, String)>, Base64Value, String) {
    let curve = noprop::sample_choice(ctx, &["Ed25519", "Ed448"]);
    let x = sample_base64_value(ctx, 6);
    let mut members = vec![
        ("kty", json_string("OKP")),
        ("crv", json_string(curve)),
        ("x", json_string(&x.text)),
        ("use", json_string("sig")),
    ];
    match defect {
        JwkDefect::None => {}
        JwkDefect::PrivateMember => members.push(("d", json_string("AAAA"))),
        JwkDefect::DuplicateMember => members.push(("x", json_string(&x.text))),
    }
    (members, x, String::from(curve))
}

/// RSA の JWK のメンバーを生成する
fn sample_rsa_model(
    ctx: &mut noprop::TestCaseContext,
    defect: JwkDefect,
) -> (Vec<(&'static str, String)>, Base64Value, Base64Value) {
    let n = sample_base64_value(ctx, 6);
    let e = sample_base64_value(ctx, 6);
    let mut members = vec![
        ("kty", json_string("RSA")),
        ("n", json_string(&n.text)),
        ("e", json_string(&e.text)),
    ];
    match defect {
        JwkDefect::None => {}
        JwkDefect::PrivateMember => members.push(("d", json_string("AAAA"))),
        JwkDefect::DuplicateMember => members.push(("n", json_string(&n.text))),
    }
    (members, n, e)
}

/// JWK の JSON とモデルを生成する
fn sample_jwk_model(ctx: &mut noprop::TestCaseContext) -> JwkModel {
    let defect = match noprop::sample_weighted_index(ctx, &[8, 1, 1]) {
        0 => JwkDefect::None,
        1 => JwkDefect::PrivateMember,
        _ => JwkDefect::DuplicateMember,
    };
    // EC / OKP / RSA の 3 種類を生成する
    let kind = noprop::sample_weighted_index(ctx, &[3, 3, 2]);

    let (members, decoded, canonical, cose_key, algorithm, kinds) = match kind {
        0 => {
            let (members, x, y, curve) = sample_ec_model(ctx, defect);
            let decoded = Jwk::Ec {
                curve: curve.clone(),
                x: x.text.clone(),
                y: y.text.clone(),
            };
            // RFC 7638 §3.2: 必須メンバー (crv / kty / x / y) を辞書順に並べる
            let canonical = match (&x.canonical, &y.canonical) {
                (Some(x), Some(y)) => Some(json_object(&[
                    ("crv", json_string(&curve)),
                    ("kty", json_string("EC")),
                    ("x", json_string(x)),
                    ("y", json_string(y)),
                ])),
                _ => None,
            };
            // 曲線の判定が base64url のデコードより先に来る
            let cose_key = match curve.as_str() {
                "P-256" | "P-384" | "P-521" => match (&x.bytes, &y.bytes) {
                    (Some(x), Some(y)) => {
                        let curve = match curve.as_str() {
                            "P-256" => EcCurve::P256,
                            "P-384" => EcCurve::P384,
                            _ => EcCurve::P521,
                        };
                        Ok(CoseKey::ec2(curve, x.clone(), y.clone()))
                    }
                    _ => Err(JwkError::InvalidBase64),
                },
                other => Err(JwkError::UnsupportedCurve(String::from(other))),
            };
            let algorithm = match &cose_key {
                Ok(CoseKey::Ec2 { curve, .. }) => Some(match curve {
                    EcCurve::P256 => Algorithm::Es256,
                    EcCurve::P384 => Algorithm::Es384,
                    EcCurve::P521 => Algorithm::Es512,
                }),
                _ => None,
            };
            (
                members,
                Some(decoded),
                canonical,
                cose_key,
                algorithm,
                vec![x.kind, y.kind],
            )
        }
        1 => {
            let (members, x, curve) = sample_okp_model(ctx, defect);
            let decoded = Jwk::Okp {
                curve: curve.clone(),
                x: x.text.clone(),
            };
            // RFC 7638 §3.2: 必須メンバー (crv / kty / x) を辞書順に並べる
            let canonical = x.canonical.as_ref().map(|x| {
                json_object(&[
                    ("crv", json_string(&curve)),
                    ("kty", json_string("OKP")),
                    ("x", json_string(x)),
                ])
            });
            let cose_key = if curve != "Ed25519" {
                Err(JwkError::UnsupportedCurve(curve.clone()))
            } else {
                match &x.bytes {
                    Some(bytes) => Ok(CoseKey::ed25519(bytes.clone())),
                    None => Err(JwkError::InvalidBase64),
                }
            };
            let algorithm = cose_key.is_ok().then_some(Algorithm::EdDsa);
            (
                members,
                Some(decoded),
                canonical,
                cose_key,
                algorithm,
                vec![x.kind],
            )
        }
        _ => {
            let (members, n, e) = sample_rsa_model(ctx, defect);
            let decoded = Jwk::Rsa {
                n: n.text.clone(),
                e: e.text.clone(),
            };
            // RFC 7638 §3.2: 必須メンバー (e / kty / n) を辞書順に並べる
            let canonical = match (&n.canonical, &e.canonical) {
                (Some(n), Some(e)) => Some(json_object(&[
                    ("e", json_string(e)),
                    ("kty", json_string("RSA")),
                    ("n", json_string(n)),
                ])),
                _ => None,
            };
            // RSA は CoseKey が表現を持たないため常にエラーになる
            (
                members,
                Some(decoded),
                canonical,
                Err(JwkError::UnsupportedOperation),
                None,
                vec![n.kind, e.kind],
            )
        }
    };

    // 欠陥がある場合はデコード自体が失敗し、後続のモデルは使われない
    let text = json_object(&members);
    let (decoded, error) = match defect {
        JwkDefect::None => (decoded, None),
        JwkDefect::PrivateMember => (
            None,
            Some(JwkError::PrivateKeyNotAllowed(String::from("d"))),
        ),
        JwkDefect::DuplicateMember => {
            let name = match kind {
                2 => "n",
                _ => "x",
            };
            (None, Some(JwkError::DuplicateMember(String::from(name))))
        }
    };
    let canonical = if defect == JwkDefect::None {
        canonical
    } else {
        None
    };
    let cose_key = if defect == JwkDefect::None {
        cose_key
    } else {
        // デコードに失敗するため to_cose_key も同じエラーになる
        Err(error
            .clone()
            .expect("欠陥がある場合はデコードエラーが決まっている"))
    };
    let algorithm = if defect == JwkDefect::None {
        algorithm
    } else {
        None
    };
    JwkModel {
        text,
        decoded,
        error,
        canonical,
        cose_key,
        algorithm,
        kinds,
    }
}

/// JWK のデコード・正規化・鍵変換がモデルと一致する
#[test]
fn jwk_matches_model() -> noprop::TestResult {
    let ec_seen = std::cell::Cell::new(0usize);
    let okp_seen = std::cell::Cell::new(0usize);
    let rsa_seen = std::cell::Cell::new(0usize);
    let padded_seen = std::cell::Cell::new(0usize);
    let invalid_base64_seen = std::cell::Cell::new(0usize);
    let private_member_seen = std::cell::Cell::new(0usize);
    let duplicate_seen = std::cell::Cell::new(0usize);
    let canonical_ok_seen = std::cell::Cell::new(0usize);
    let cose_key_ok_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let model = sample_jwk_model(ctx);
        // 生成値の種類を数える
        for kind in &model.kinds {
            match kind {
                Base64Kind::Padded => padded_seen.set(padded_seen.get() + 1),
                Base64Kind::Invalid => invalid_base64_seen.set(invalid_base64_seen.get() + 1),
                Base64Kind::Unpadded => {}
            }
        }
        match Jwk::decode(&model.text) {
            Ok(jwk) => {
                assert_eq!(
                    model.decoded.as_ref(),
                    Some(&jwk),
                    "デコード結果が一致しない"
                );
                match jwk.canonical_json() {
                    Ok(text) => {
                        assert_eq!(
                            Some(text),
                            model.canonical.clone(),
                            "正規化 JSON が一致しない: jwk={jwk:?}"
                        );
                        canonical_ok_seen.set(canonical_ok_seen.get() + 1);
                    }
                    Err(error) => {
                        assert_eq!(model.canonical, None, "正規化 JSON が失敗した: {error}");
                        assert_eq!(error, JwkError::InvalidBase64);
                    }
                }
                assert_eq!(
                    jwk.to_cose_key(),
                    model.cose_key,
                    "CoseKey への変換が一致しない: jwk={jwk:?}"
                );
                match &model.cose_key {
                    Ok(key) => {
                        assert_eq!(
                            jwk.matches_public_key(key),
                            Ok(true),
                            "同じ公開鍵と一致しない: jwk={jwk:?}"
                        );
                        // 公開鍵を 1 バイト変えた鍵とは一致しない
                        let mut different = key.clone();
                        match &mut different {
                            CoseKey::Ec2 { x, .. } => x.push(0x00),
                            CoseKey::Okp { public_key, .. } => public_key.push(0x00),
                            CoseKey::Symmetric { key } => key.push(0x00),
                        }
                        assert_eq!(
                            jwk.matches_public_key(&different),
                            Ok(false),
                            "異なる公開鍵と一致した: jwk={jwk:?}"
                        );
                        assert_eq!(
                            default_signing_algorithm(key),
                            model
                                .algorithm
                                .expect("鍵の種別から既定のアルゴリズムが決まる"),
                            "既定の署名アルゴリズムが一致しない: jwk={jwk:?}"
                        );
                        cose_key_ok_seen.set(cose_key_ok_seen.get() + 1);
                    }
                    Err(_) => {
                        assert!(
                            jwk.matches_public_key(&CoseKey::symmetric(Vec::new()))
                                .is_err(),
                            "CoseKey へ変換できない JWK が公開鍵として受理された: jwk={jwk:?}"
                        );
                    }
                }
            }
            Err(error) => {
                assert_eq!(model.decoded, None, "デコードに失敗した: {error}");
                assert_eq!(
                    model.error.as_ref(),
                    Some(&error),
                    "デコードエラーが一致しない: text={}",
                    model.text
                );
                match &error {
                    JwkError::PrivateKeyNotAllowed(_) => {
                        private_member_seen.set(private_member_seen.get() + 1);
                    }
                    JwkError::DuplicateMember(_) => {
                        duplicate_seen.set(duplicate_seen.get() + 1);
                    }
                    _ => {}
                }
            }
        }
        // JWK の種別を数える (デコードの成否に依らない)
        if model.text.contains("\"EC\"") {
            ec_seen.set(ec_seen.get() + 1);
        } else if model.text.contains("\"OKP\"") {
            okp_seen.set(okp_seen.get() + 1);
        } else {
            rsa_seen.set(rsa_seen.get() + 1);
        }
        Ok(())
    })?;
    assert!(ec_seen.get() > 0, "EC のケースが生成されなかった\n{runner}");
    assert!(
        okp_seen.get() > 0,
        "OKP のケースが生成されなかった\n{runner}"
    );
    assert!(
        rsa_seen.get() > 0,
        "RSA のケースが生成されなかった\n{runner}"
    );
    assert!(
        padded_seen.get() > 0,
        "パディング付きのケースが生成されなかった\n{runner}"
    );
    assert!(
        invalid_base64_seen.get() > 0,
        "不正な base64url のケースが生成されなかった\n{runner}"
    );
    assert!(
        private_member_seen.get() > 0,
        "秘密鍵メンバーのケースが生成されなかった\n{runner}"
    );
    assert!(
        duplicate_seen.get() > 0,
        "重複メンバーのケースが生成されなかった\n{runner}"
    );
    assert!(
        canonical_ok_seen.get() > 0,
        "正規化 JSON が成功するケースが生成されなかった\n{runner}"
    );
    assert!(
        cose_key_ok_seen.get() > 0,
        "CoseKey へ変換できるケースが生成されなかった\n{runner}"
    );
    Ok(())
}
