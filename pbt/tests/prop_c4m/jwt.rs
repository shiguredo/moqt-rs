//! JWS compact (RFC 7515) の property テスト
//!
//! - ヘッダのデコードが、生成した JSON の内容から導いたモデルと一致すること
//! - compact 形式のデコードが、3 分割と base64url の規則から導いたモデルと一致すること
//!
//! 署名の検証は `CoseCrypto` を必要とするため `tests/test_c4m/` 側で固定する。

use pbt::common::{sample_bytes, test_runner};
use shiguredo_moqt::c4m::cose::Algorithm;
use shiguredo_moqt::c4m::jwk::Jwk;
use shiguredo_moqt::c4m::jwt::{JwsCompact, JwsHeader, JwtError};

use super::common::{
    base64url_encode, base64url_encode_padded, json_object, json_string, sample_safe_text,
};

/// JWS のヘッダが取り得るアルゴリズム (RFC 7518 §3.1 / RFC 8037 §3.1)
const ALGORITHMS: &[Algorithm] = &[
    Algorithm::HmacSha256,
    Algorithm::HmacSha384,
    Algorithm::HmacSha512,
    Algorithm::Es256,
    Algorithm::Es384,
    Algorithm::Es512,
    Algorithm::EdDsa,
];

/// アルゴリズムを 1 つ生成する
fn sample_algorithm(ctx: &mut noprop::TestCaseContext) -> Algorithm {
    noprop::sample_choice(ctx, ALGORITHMS)
}

/// ヘッダのデコード結果のモデル
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeaderDecodeModel {
    /// デコードに成功する
    Ok,
    /// 必須の `alg` が無い
    MissingAlgorithm,
    /// 未知の `alg` である
    UnsupportedAlgorithm,
    /// `crit` に理解できない拡張ヘッダがある (RFC 7515 §4.1.11)
    CriticalHeader,
    /// メンバー名が重複している (RFC 7515 §4)
    DuplicateMember,
    /// `jwk` が JWK としてデコードできない
    InvalidJwk,
}

/// ヘッダの生成結果とモデル
#[derive(Debug, Clone)]
struct HeaderModel {
    /// ヘッダの JSON
    text: String,
    /// 期待するデコード結果
    model: HeaderDecodeModel,
    /// デコードに成功した場合の `alg`
    algorithm: Algorithm,
    /// デコードに成功した場合の `typ`
    typ: Option<String>,
    /// デコードに成功した場合の `kid`
    key_id: Option<String>,
    /// デコードに成功した場合の `jwk`
    jwk: Option<Jwk>,
}

/// モデルに対応するデコードエラーかどうかを返す
fn header_error_matches(error: &JwtError, model: HeaderDecodeModel) -> bool {
    match model {
        HeaderDecodeModel::Ok => false,
        // 必須メンバーの欠落は nojson のエラーとして返る
        HeaderDecodeModel::MissingAlgorithm => matches!(error, JwtError::Json(_)),
        HeaderDecodeModel::UnsupportedAlgorithm => {
            matches!(error, JwtError::UnsupportedAlgorithm(_))
        }
        HeaderDecodeModel::CriticalHeader => error == &JwtError::UnsupportedCriticalHeader,
        HeaderDecodeModel::DuplicateMember => {
            error == &JwtError::DuplicateMember(String::from("alg"))
        }
        HeaderDecodeModel::InvalidJwk => matches!(error, JwtError::Jwk(_)),
    }
}

/// ヘッダの JSON とモデルを生成する
///
/// 欠陥は 1 つだけ入れる (実装は重複 / 必須メンバー / 未知の alg / crit / jwk の
/// 順に検査するため、同時に入れるとモデルが複雑になる)。
fn sample_header_model(ctx: &mut noprop::TestCaseContext) -> HeaderModel {
    // 0 は欠陥無し、1 以降は欠陥の種類
    let defect = noprop::sample_weighted_index(ctx, &[6, 1, 1, 1, 1, 1]);
    let algorithm = sample_algorithm(ctx);
    let typ = noprop::sample_bool(ctx).then(|| sample_safe_text(ctx, 8));
    let key_id = noprop::sample_bool(ctx).then(|| sample_safe_text(ctx, 8));

    let model = match defect {
        0 => HeaderDecodeModel::Ok,
        1 => HeaderDecodeModel::MissingAlgorithm,
        2 => HeaderDecodeModel::UnsupportedAlgorithm,
        3 => HeaderDecodeModel::CriticalHeader,
        4 => HeaderDecodeModel::DuplicateMember,
        _ => HeaderDecodeModel::InvalidJwk,
    };

    // 埋め込む JWK は、埋め込まない / 正しい / 秘密鍵を含む の 3 種類。
    // 欠陥が InvalidJwk のときは秘密鍵を含む JWK を必ず使う
    let (jwk, jwk_json) = if model == HeaderDecodeModel::InvalidJwk {
        // 秘密鍵メンバーを含む JWK は拒否される (RFC 9449 §4.3)
        let json = json_object(&[
            ("kty", json_string("OKP")),
            ("crv", json_string("Ed25519")),
            ("x", json_string("AAAA")),
            ("d", json_string("AAAA")),
        ]);
        (None, Some(json))
    } else {
        match noprop::sample_weighted_index(ctx, &[2, 5]) {
            0 => (None, None),
            _ => {
                let x = base64url_encode(&sample_bytes(ctx, 4));
                let jwk = Jwk::Okp {
                    curve: String::from("Ed25519"),
                    x: x.clone(),
                };
                let json = json_object(&[
                    ("kty", json_string("OKP")),
                    ("crv", json_string("Ed25519")),
                    ("x", json_string(&x)),
                ]);
                (Some(jwk), Some(json))
            }
        }
    };

    let algorithm_name = match model {
        HeaderDecodeModel::UnsupportedAlgorithm => String::from("HS999"),
        _ => String::from(algorithm.jose_name()),
    };
    let mut members = Vec::new();
    if model != HeaderDecodeModel::MissingAlgorithm {
        members.push(("alg", json_string(&algorithm_name)));
    }
    if let Some(typ) = &typ {
        members.push(("typ", json_string(typ)));
    }
    if let Some(key_id) = &key_id {
        members.push(("kid", json_string(key_id)));
    }
    if let Some(jwk_json) = &jwk_json {
        members.push(("jwk", jwk_json.clone()));
    }
    if model == HeaderDecodeModel::CriticalHeader {
        // crit は理解できない拡張ヘッダを列挙する
        members.push(("crit", String::from("[\"exp\"]")));
    }
    if model == HeaderDecodeModel::DuplicateMember {
        members.push(("alg", json_string(&algorithm_name)));
    }

    HeaderModel {
        text: json_object(&members),
        model,
        algorithm,
        typ,
        key_id,
        jwk,
    }
}

/// ヘッダのデコードが生成した JSON の内容と一致する
#[test]
fn header_decode_matches_model() -> noprop::TestResult {
    let ok_seen = std::cell::Cell::new(0usize);
    let missing_algorithm_seen = std::cell::Cell::new(0usize);
    let unsupported_algorithm_seen = std::cell::Cell::new(0usize);
    let critical_header_seen = std::cell::Cell::new(0usize);
    let duplicate_member_seen = std::cell::Cell::new(0usize);
    let invalid_jwk_seen = std::cell::Cell::new(0usize);
    let with_jwk_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let model = sample_header_model(ctx);
        match JwsHeader::decode(&model.text) {
            Ok(header) => {
                assert_eq!(
                    model.model,
                    HeaderDecodeModel::Ok,
                    "デコードに成功した: text={}",
                    model.text
                );
                ok_seen.set(ok_seen.get() + 1);
                assert_eq!(header.algorithm, model.algorithm);
                assert_eq!(header.typ, model.typ);
                assert_eq!(header.key_id, model.key_id);
                assert_eq!(header.jwk, model.jwk);
                if model.jwk.is_some() {
                    with_jwk_seen.set(with_jwk_seen.get() + 1);
                }
            }
            Err(error) => {
                assert!(
                    header_error_matches(&error, model.model),
                    "デコードエラーがモデルと一致しない: model={:?} error={error} text={}",
                    model.model,
                    model.text
                );
                match model.model {
                    HeaderDecodeModel::MissingAlgorithm => {
                        missing_algorithm_seen.set(missing_algorithm_seen.get() + 1);
                    }
                    HeaderDecodeModel::UnsupportedAlgorithm => {
                        unsupported_algorithm_seen.set(unsupported_algorithm_seen.get() + 1);
                    }
                    HeaderDecodeModel::CriticalHeader => {
                        critical_header_seen.set(critical_header_seen.get() + 1);
                    }
                    HeaderDecodeModel::DuplicateMember => {
                        duplicate_member_seen.set(duplicate_member_seen.get() + 1);
                    }
                    HeaderDecodeModel::InvalidJwk => {
                        invalid_jwk_seen.set(invalid_jwk_seen.get() + 1);
                    }
                    HeaderDecodeModel::Ok => {}
                }
            }
        }
        Ok(())
    })?;
    assert!(
        ok_seen.get() > 0,
        "デコードに成功するケースが生成されなかった\n{runner}"
    );
    assert!(
        missing_algorithm_seen.get() > 0,
        "alg が無いケースが生成されなかった\n{runner}"
    );
    assert!(
        unsupported_algorithm_seen.get() > 0,
        "未知の alg のケースが生成されなかった\n{runner}"
    );
    assert!(
        critical_header_seen.get() > 0,
        "crit 付きのケースが生成されなかった\n{runner}"
    );
    assert!(
        duplicate_member_seen.get() > 0,
        "重複メンバーのケースが生成されなかった\n{runner}"
    );
    assert!(
        invalid_jwk_seen.get() > 0,
        "JWK が不正なケースが生成されなかった\n{runner}"
    );
    assert!(
        with_jwk_seen.get() > 0,
        "jwk 付きのケースが生成されなかった\n{runner}"
    );
    Ok(())
}

/// compact 形式のデコードが 3 分割と base64url の規則と一致する
#[test]
fn compact_decode_matches_model() -> noprop::TestResult {
    let ok_seen = std::cell::Cell::new(0usize);
    let padded_seen = std::cell::Cell::new(0usize);
    let unpadded_seen = std::cell::Cell::new(0usize);
    let header_error_seen = std::cell::Cell::new(0usize);
    let format_error_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        // 2 / 4 / 5 分割は常に形式エラーになる
        if noprop::sample_ratio(ctx, noprop::Ratio::one_nth(4)) {
            let count = noprop::sample_choice(ctx, &[2usize, 4, 5]);
            let text = (0..count)
                .map(|_| sample_safe_text(ctx, 4))
                .collect::<Vec<String>>()
                .join(".");
            assert_eq!(
                JwsCompact::decode(&text),
                Err(JwtError::InvalidTokenFormat),
                "3 分割以外は形式エラーになる: text={text}"
            );
            format_error_seen.set(format_error_seen.get() + 1);
            return Ok(());
        }

        let model = sample_header_model(ctx);
        let payload = sample_bytes(ctx, 16);
        let signature = sample_bytes(ctx, 16);
        // パディング付きの base64url も受理される
        let padded = noprop::sample_bool(ctx);
        let payload_segment = if padded {
            base64url_encode_padded(&payload)
        } else {
            base64url_encode(&payload)
        };
        let signature_segment = if padded {
            base64url_encode_padded(&signature)
        } else {
            base64url_encode(&signature)
        };
        let header_segment = base64url_encode(model.text.as_bytes());
        let text = format!("{header_segment}.{payload_segment}.{signature_segment}");

        match JwsCompact::decode(&text) {
            Ok(jws) => {
                assert_eq!(
                    model.model,
                    HeaderDecodeModel::Ok,
                    "デコードに成功した: text={text}"
                );
                ok_seen.set(ok_seen.get() + 1);
                if padded {
                    padded_seen.set(padded_seen.get() + 1);
                } else {
                    unpadded_seen.set(unpadded_seen.get() + 1);
                }
                assert_eq!(jws.header().algorithm, model.algorithm);
                assert_eq!(jws.header().typ, model.typ);
                assert_eq!(jws.header().key_id, model.key_id);
                assert_eq!(jws.header().jwk, model.jwk);
                // ペイロードと署名は base64url をデコードした結果になる
                assert_eq!(jws.payload(), payload.as_slice());
                assert_eq!(jws.signature(), signature.as_slice());
                // 署名対象は base64url のままの header と payload (RFC 7515 §7.1)
                assert_eq!(
                    jws.signing_input(),
                    format!("{header_segment}.{payload_segment}").as_bytes()
                );
            }
            Err(error) => {
                assert!(
                    header_error_matches(&error, model.model),
                    "デコードエラーがモデルと一致しない: model={:?} error={error} text={text}",
                    model.model
                );
                header_error_seen.set(header_error_seen.get() + 1);
            }
        }
        Ok(())
    })?;
    assert!(
        ok_seen.get() > 0,
        "デコードに成功するケースが生成されなかった\n{runner}"
    );
    assert!(
        padded_seen.get() > 0,
        "パディング付きのケースが生成されなかった\n{runner}"
    );
    assert!(
        unpadded_seen.get() > 0,
        "パディング無しのケースが生成されなかった\n{runner}"
    );
    assert!(
        header_error_seen.get() > 0,
        "ヘッダが不正なケースが生成されなかった\n{runner}"
    );
    assert!(
        format_error_seen.get() > 0,
        "3 分割以外のケースが生成されなかった\n{runner}"
    );
    Ok(())
}
