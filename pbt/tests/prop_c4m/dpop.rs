//! DPoP proof (draft-nandakumar-moq-generic-dpop-proof-00) の property テスト
//!
//! - Authorization Context (actx) のデコードと検証が仕様のモデルと一致すること
//! - DPoP proof の JWT のデコードが、ヘッダとペイロードの内容から導いたモデルと
//!   一致すること
//! - 暗号を必要としない検証 (actx / 鮮度 / リプレイ) がモデルと一致すること
//!
//! 署名の検証は `CoseCrypto` を必要とするため `tests/test_c4m/` 側で固定する。

use pbt::common::{sample_bytes, sample_nonempty_bytes, test_runner};
use shiguredo_moqt::c4m::MoqtAction;
use shiguredo_moqt::c4m::cose::Algorithm;
use shiguredo_moqt::c4m::dpop::{
    AuthorizationContext, DPOP_PROOF_JWT_TYPE, DpopError, DpopProof, DpopProofClaims,
    DpopReplayCache, MOQT_AUTHORIZATION_CONTEXT_TYPE,
};
use shiguredo_moqt::c4m::jwt::JwtError;
use shiguredo_moqt::message::common::TrackNamespace;
use shiguredo_moqt::name;

use super::common::{base64url_encode, json_object, json_string, sample_safe_text};

/// 時刻とウィンドウの境界値 (非有限値を含む)
const TIME_BOUNDARIES: &[f64] = &[
    0.0,
    -0.0,
    1.0,
    -1.0,
    50.0,
    100.0,
    f64::NAN,
    f64::INFINITY,
    f64::NEG_INFINITY,
];

/// Track Namespace を生成する (フィールドは 0 〜 4 個)
fn sample_namespace(ctx: &mut noprop::TestCaseContext) -> TrackNamespace {
    let count = noprop::sample_usize_in(ctx, 0..=4);
    let fields = (0..count)
        .map(|_| sample_nonempty_bytes(ctx, 4))
        .collect::<Vec<Vec<u8>>>();
    TrackNamespace::new(fields).expect("テストフィクスチャの前提条件を満たす")
}

/// actx の生成結果と、その検証に使うモデル
#[derive(Debug, Clone)]
struct GeneratedContext {
    /// actx の JSON
    text: String,
    /// `type`
    context_type: String,
    /// `action`
    action_value: String,
    /// `tns`
    tns_value: String,
    /// `tn`
    tn_value: Option<String>,
    /// `resource`
    resource: Option<String>,
    /// 重複メンバーを含むかどうか
    duplicate: bool,
}

/// 仕様 (draft-ietf-moq-c4m-01 §3.1.2 / §3.1.3) の規則で `resource` の整合を判定するモデル
///
/// `verify_resource_consistency` と同じく、クエリを文字列として分割し、
/// パーセントエンコーディングは解釈しない。
fn resource_consistent(resource: &Option<String>, tns: &str, tn: &Option<String>) -> bool {
    let Some(resource) = resource else {
        return true;
    };
    let query = resource.split_once('?').map_or("", |(_, query)| query);
    let mut parsed_tns = None;
    let mut parsed_tn = None;
    for pair in query.split('&') {
        match pair.split_once('=') {
            Some(("tns", value)) => parsed_tns = Some(value),
            Some(("tn", value)) => parsed_tn = Some(value),
            _ => {}
        }
    }
    if parsed_tns != Some(tns) {
        return false;
    }
    match tn {
        Some(tn) => parsed_tn == Some(tn.as_str()),
        None => true,
    }
}

/// actx の値を生成する
///
/// `serialized_tns` / `serialized_tn` は対象の Full Track Name の正規シリアライズで、
/// 一致する値と一致しない値の両方を生成する。
fn sample_authorization_context(
    ctx: &mut noprop::TestCaseContext,
    action: MoqtAction,
    serialized_tns: &str,
    serialized_tn: &str,
) -> GeneratedContext {
    // 3/4 の確率で対象と一致する値を作り、許可と拒否の両方の経路を踏む
    let context_type = if noprop::sample_ratio(ctx, noprop::Ratio::new(3, 4)) {
        String::from(MOQT_AUTHORIZATION_CONTEXT_TYPE)
    } else {
        sample_safe_text(ctx, 6)
    };
    let action_value = if noprop::sample_ratio(ctx, noprop::Ratio::new(3, 4)) {
        String::from(action.authorization_context())
    } else {
        sample_safe_text(ctx, 6)
    };
    let tns_value = if noprop::sample_ratio(ctx, noprop::Ratio::new(3, 4)) {
        String::from(serialized_tns)
    } else {
        sample_safe_text(ctx, 8)
    };
    let tn_value = match noprop::sample_weighted_index(ctx, &[3, 2, 1]) {
        0 => Some(String::from(serialized_tn)),
        1 => Some(sample_safe_text(ctx, 8)),
        _ => None,
    };
    // 整合する resource / 順序と追加パラメータを変えた resource / クエリ無し /
    // 破損した resource / resource 無しを生成する
    let resource = match noprop::sample_weighted_index(ctx, &[3, 3, 1, 1, 2]) {
        0 => {
            let tn = tn_value.clone().unwrap_or_default();
            Some(format!("moqt://relay.example?tns={tns_value}&tn={tn}"))
        }
        1 => {
            let tn = tn_value.clone().unwrap_or_default();
            Some(format!(
                "moqt://relay.example?extra=1&tn={tn}&tns={tns_value}"
            ))
        }
        2 => Some(String::from("moqt://relay.example")),
        3 => Some(sample_safe_text(ctx, 12)),
        _ => None,
    };
    // 重複メンバーはデコード自体を失敗させる
    let duplicate = noprop::sample_ratio(ctx, noprop::Ratio::one_nth(8));
    let mut members = vec![
        ("type", json_string(&context_type)),
        ("action", json_string(&action_value)),
        ("tns", json_string(&tns_value)),
    ];
    if let Some(tn) = &tn_value {
        members.push(("tn", json_string(tn)));
    }
    if let Some(resource) = &resource {
        members.push(("resource", json_string(resource)));
    }
    if duplicate {
        members.push(("action", json_string(&action_value)));
    }
    GeneratedContext {
        text: json_object(&members),
        context_type,
        action_value,
        tns_value,
        tn_value,
        resource,
        duplicate,
    }
}

/// `verify_authorization_context` の判定を仕様から導くモデル
///
/// 検証の順序 (type / action / resource / target) も実装と同じにする。
fn expected_authorization_context_error(
    context: &GeneratedContext,
    action: MoqtAction,
    namespace: &TrackNamespace,
    track: &[u8],
) -> Option<DpopError> {
    if context.context_type != MOQT_AUTHORIZATION_CONTEXT_TYPE {
        return Some(DpopError::ContextTypeMismatch);
    }
    if context.action_value != action.authorization_context() {
        return Some(DpopError::ActionMismatch);
    }
    if !resource_consistent(&context.resource, &context.tns_value, &context.tn_value) {
        return Some(DpopError::ResourceInconsistent);
    }
    if context.tns_value != name::serialize_namespace(namespace) {
        return Some(DpopError::TargetMismatch);
    }
    let Some(tn) = &context.tn_value else {
        return Some(DpopError::MissingTrackName);
    };
    if *tn != name::serialize_track_name(track) {
        return Some(DpopError::TargetMismatch);
    }
    None
}

/// 有限な `iat` の JSON 表現を生成する
///
/// `format!("{value}")` は有限値なら JSON の数値として妥当な 10 進表現を返し、
/// パースすると元の値に戻る。
fn sample_iat_text(ctx: &mut noprop::TestCaseContext) -> String {
    let value = noprop::sample_with_boundaries(
        ctx,
        &[0.0, -0.0, 1.0, -1.0, 1_700_000_000.0, 1e-9, 1e15],
        noprop::Ratio::one_nth(4),
        |ctx| noprop::sample_f64_in(ctx, -1.0e12, 1.0e12),
    );
    format!("{value}")
}

/// proof のヘッダに埋め込む Ed25519 の JWK の JSON
fn ed25519_jwk_json(x: &str) -> String {
    json_object(&[
        ("kty", json_string("OKP")),
        ("crv", json_string("Ed25519")),
        ("x", json_string(x)),
    ])
}

/// JWS compact (RFC 7515 §7.1) を組み立てる
fn build_compact(header_json: &str, payload_json: &str, signature: &[u8]) -> String {
    format!(
        "{}.{}.{}",
        base64url_encode(header_json.as_bytes()),
        base64url_encode(payload_json.as_bytes()),
        base64url_encode(signature)
    )
}

/// デコードに成功する DPoP proof を組み立てる
fn build_valid_proof(jti: &str, iat_text: &str, actx_json: &str, signature: &[u8]) -> String {
    let header = json_object(&[
        ("typ", json_string(DPOP_PROOF_JWT_TYPE)),
        ("alg", json_string(Algorithm::Es256.jose_name())),
        (
            "jwk",
            ed25519_jwk_json(&base64url_encode(&proof_key_bytes())),
        ),
    ]);
    let payload = json_object(&[
        ("jti", json_string(jti)),
        ("iat", String::from(iat_text)),
        ("actx", String::from(actx_json)),
    ]);
    build_compact(&header, &payload, signature)
}

/// proof の JWK に埋め込む固定の公開鍵バイト列
fn proof_key_bytes() -> Vec<u8> {
    b"property-test-key".to_vec()
}

/// actx のデコードと 4 つの検証が仕様のモデルと一致する
#[test]
fn authorization_context_matches_model() -> noprop::TestResult {
    let decoded_seen = std::cell::Cell::new(0usize);
    let duplicate_seen = std::cell::Cell::new(0usize);
    let type_mismatch_seen = std::cell::Cell::new(0usize);
    let action_mismatch_seen = std::cell::Cell::new(0usize);
    let target_ok_seen = std::cell::Cell::new(0usize);
    let target_mismatch_seen = std::cell::Cell::new(0usize);
    let missing_track_name_seen = std::cell::Cell::new(0usize);
    let resource_ok_seen = std::cell::Cell::new(0usize);
    let resource_error_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let namespace = sample_namespace(ctx);
        let track = sample_bytes(ctx, 8);
        let action = MoqtAction::ALL[noprop::sample_usize_in(ctx, 0..MoqtAction::ALL.len())];
        let generated = sample_authorization_context(
            ctx,
            action,
            &name::serialize_namespace(&namespace),
            &name::serialize_track_name(&track),
        );

        let context = match AuthorizationContext::decode(&generated.text) {
            Ok(context) => {
                decoded_seen.set(decoded_seen.get() + 1);
                context
            }
            Err(error) => {
                // 重複メンバーだけがデコードを失敗させる
                assert!(generated.duplicate, "デコードに失敗した: {error}");
                assert_eq!(error, DpopError::DuplicateMember(String::from("action")));
                duplicate_seen.set(duplicate_seen.get() + 1);
                return Ok(());
            }
        };
        assert!(!generated.duplicate, "重複メンバーが受理された");

        // 型付きフィールドの取り出しが入力と一致する
        assert_eq!(context.context_type, generated.context_type);
        assert_eq!(context.action, generated.action_value);
        assert_eq!(context.track_namespace, generated.tns_value);
        assert_eq!(context.track_name, generated.tn_value);
        assert_eq!(context.resource, generated.resource);
        assert_eq!(context.raw, generated.text);

        // 検証の判定をモデルと比較する
        let expected_type = if generated.context_type == MOQT_AUTHORIZATION_CONTEXT_TYPE {
            Ok(())
        } else {
            Err(DpopError::ContextTypeMismatch)
        };
        assert_eq!(
            context.verify_context_type(MOQT_AUTHORIZATION_CONTEXT_TYPE),
            expected_type,
            "type の検証がモデルと一致しない: generated={generated:?}"
        );

        let expected_action = if generated.action_value == action.authorization_context() {
            Ok(())
        } else {
            Err(DpopError::ActionMismatch)
        };
        assert_eq!(
            context.verify_action(action),
            expected_action,
            "action の検証がモデルと一致しない: generated={generated:?} action={action:?}"
        );

        let serialized_tns = name::serialize_namespace(&namespace);
        let serialized_tn = name::serialize_track_name(&track);
        let expected_target = if generated.tns_value != serialized_tns {
            Err(DpopError::TargetMismatch)
        } else if generated.tn_value.is_none() {
            Err(DpopError::MissingTrackName)
        } else if generated.tn_value.as_deref() != Some(serialized_tn.as_str()) {
            Err(DpopError::TargetMismatch)
        } else {
            Ok(())
        };
        assert_eq!(
            context.verify_target(&namespace, &track),
            expected_target,
            "target の検証がモデルと一致しない: generated={generated:?}"
        );

        let expected_resource = if resource_consistent(
            &generated.resource,
            &generated.tns_value,
            &generated.tn_value,
        ) {
            Ok(())
        } else {
            Err(DpopError::ResourceInconsistent)
        };
        assert_eq!(
            context.verify_resource_consistency(),
            expected_resource,
            "resource の検証がモデルと一致しない: generated={generated:?}"
        );

        // 到達した分岐を数える
        if expected_type.is_err() {
            type_mismatch_seen.set(type_mismatch_seen.get() + 1);
        }
        if expected_action.is_err() {
            action_mismatch_seen.set(action_mismatch_seen.get() + 1);
        }
        match expected_target {
            Ok(()) => target_ok_seen.set(target_ok_seen.get() + 1),
            Err(DpopError::MissingTrackName) => {
                missing_track_name_seen.set(missing_track_name_seen.get() + 1);
            }
            Err(_) => target_mismatch_seen.set(target_mismatch_seen.get() + 1),
        }
        if expected_resource.is_err() {
            resource_error_seen.set(resource_error_seen.get() + 1);
        } else {
            resource_ok_seen.set(resource_ok_seen.get() + 1);
        }
        Ok(())
    })?;
    assert!(
        decoded_seen.get() > 0,
        "actx をデコードできたケースが生成されなかった\n{runner}"
    );
    assert!(
        duplicate_seen.get() > 0,
        "重複メンバーのケースが生成されなかった\n{runner}"
    );
    assert!(
        type_mismatch_seen.get() > 0,
        "type が一致しないケースが生成されなかった\n{runner}"
    );
    assert!(
        action_mismatch_seen.get() > 0,
        "action が一致しないケースが生成されなかった\n{runner}"
    );
    assert!(
        target_ok_seen.get() > 0,
        "target が一致するケースが生成されなかった\n{runner}"
    );
    assert!(
        target_mismatch_seen.get() > 0,
        "target が一致しないケースが生成されなかった\n{runner}"
    );
    assert!(
        missing_track_name_seen.get() > 0,
        "tn が無いケースが生成されなかった\n{runner}"
    );
    assert!(
        resource_ok_seen.get() > 0,
        "resource が整合するケースが生成されなかった\n{runner}"
    );
    assert!(
        resource_error_seen.get() > 0,
        "resource が整合しないケースが生成されなかった\n{runner}"
    );
    Ok(())
}

/// クレームのデコードがペイロードの JSON と一致する
#[test]
fn claims_decode_matches_json() -> noprop::TestResult {
    let decoded_seen = std::cell::Cell::new(0usize);
    let duplicate_seen = std::cell::Cell::new(0usize);
    let context_duplicate_seen = std::cell::Cell::new(0usize);
    let with_ath_seen = std::cell::Cell::new(0usize);
    let with_nonce_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let namespace = sample_namespace(ctx);
        let track = sample_bytes(ctx, 8);
        let action = MoqtAction::ALL[noprop::sample_usize_in(ctx, 0..MoqtAction::ALL.len())];
        let generated = sample_authorization_context(
            ctx,
            action,
            &name::serialize_namespace(&namespace),
            &name::serialize_track_name(&track),
        );
        let jti = sample_safe_text(ctx, 8);
        let iat_text = sample_iat_text(ctx);
        let ath = noprop::sample_bool(ctx).then(|| base64url_encode(&sample_bytes(ctx, 8)));
        let nonce = noprop::sample_bool(ctx).then(|| sample_safe_text(ctx, 8));
        // 重複メンバーはデコード自体を失敗させる
        let duplicate = noprop::sample_ratio(ctx, noprop::Ratio::one_nth(8));
        let mut members = vec![
            ("jti", json_string(&jti)),
            ("iat", iat_text.clone()),
            ("actx", generated.text.clone()),
        ];
        if let Some(ath) = &ath {
            members.push(("ath", json_string(ath)));
        }
        if let Some(nonce) = &nonce {
            members.push(("nonce", json_string(nonce)));
        }
        if duplicate {
            members.push(("jti", json_string(&jti)));
        }
        let text = json_object(&members);

        // ペイロードの重複が先に検査され、次に actx の重複が検査される
        let expected_error = if duplicate {
            Some(DpopError::DuplicateMember(String::from("jti")))
        } else if generated.duplicate {
            Some(DpopError::DuplicateMember(String::from("action")))
        } else {
            None
        };
        match DpopProofClaims::decode(&text) {
            Ok(claims) => {
                decoded_seen.set(decoded_seen.get() + 1);
                assert_eq!(expected_error, None, "重複メンバーが受理された");
                assert_eq!(claims.jti, jti);
                assert_eq!(
                    claims.issued_at,
                    iat_text
                        .parse::<f64>()
                        .expect("有限値の 10 進表現をパースできる"),
                    "iat が一致しない"
                );
                assert_eq!(claims.authorization_context.raw, generated.text);
                assert_eq!(
                    claims.authorization_context.context_type,
                    generated.context_type
                );
                assert_eq!(claims.authorization_context.action, generated.action_value);
                assert_eq!(
                    claims.authorization_context.track_namespace,
                    generated.tns_value
                );
                assert_eq!(claims.authorization_context.track_name, generated.tn_value);
                assert_eq!(claims.authorization_context.resource, generated.resource);
                assert_eq!(claims.access_token_hash, ath);
                assert_eq!(claims.nonce, nonce);
                if ath.is_some() {
                    with_ath_seen.set(with_ath_seen.get() + 1);
                }
                if nonce.is_some() {
                    with_nonce_seen.set(with_nonce_seen.get() + 1);
                }
            }
            Err(error) => {
                assert_eq!(
                    Some(error.clone()),
                    expected_error,
                    "デコードエラーがモデルと一致しない: text={text}"
                );
                if duplicate {
                    duplicate_seen.set(duplicate_seen.get() + 1);
                }
                if generated.duplicate {
                    context_duplicate_seen.set(context_duplicate_seen.get() + 1);
                }
            }
        }
        Ok(())
    })?;
    assert!(
        decoded_seen.get() > 0,
        "クレームをデコードできたケースが生成されなかった\n{runner}"
    );
    assert!(
        duplicate_seen.get() > 0,
        "重複メンバーのケースが生成されなかった\n{runner}"
    );
    assert!(
        with_ath_seen.get() > 0,
        "ath 付きのケースが生成されなかった\n{runner}"
    );
    assert!(
        with_nonce_seen.get() > 0,
        "nonce 付きのケースが生成されなかった\n{runner}"
    );
    assert!(
        context_duplicate_seen.get() > 0,
        "actx の重複メンバーのケースが生成されなかった\n{runner}"
    );
    Ok(())
}

/// proof のデコード結果のモデル
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProofDecodeModel {
    /// デコードに成功する
    Ok,
    /// `typ` が "dpop-proof+jwt" ではない
    InvalidType,
    /// 対称鍵 (MAC) アルゴリズムである
    UnsupportedAlgorithm,
    /// `jwk` ヘッダが無い
    MissingJwk,
    /// `jwk` が JWK としてデコードできない
    InvalidJwk,
    /// JWS のヘッダとしてデコードできない (未知の `alg` など)
    InvalidJws,
    /// actx に重複メンバーがある
    DuplicateActx,
}

/// proof のデコードがヘッダとペイロードの内容から導いたモデルと一致する
#[test]
fn proof_decode_matches_model() -> noprop::TestResult {
    let ok_seen = std::cell::Cell::new(0usize);
    let invalid_type_seen = std::cell::Cell::new(0usize);
    let unsupported_algorithm_seen = std::cell::Cell::new(0usize);
    let missing_jwk_seen = std::cell::Cell::new(0usize);
    let invalid_jwk_seen = std::cell::Cell::new(0usize);
    let invalid_jws_seen = std::cell::Cell::new(0usize);
    let duplicate_actx_seen = std::cell::Cell::new(0usize);
    let context_ok_seen = std::cell::Cell::new(0usize);
    let context_error_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let namespace = sample_namespace(ctx);
        let track = sample_bytes(ctx, 8);
        let action = MoqtAction::ALL[noprop::sample_usize_in(ctx, 0..MoqtAction::ALL.len())];
        let generated = sample_authorization_context(
            ctx,
            action,
            &name::serialize_namespace(&namespace),
            &name::serialize_track_name(&track),
        );
        let jti = sample_safe_text(ctx, 8);
        let iat_text = sample_iat_text(ctx);
        let iat = iat_text
            .parse::<f64>()
            .expect("有限値の 10 進表現をパースできる");
        let signature = sample_bytes(ctx, 8);
        let key_id = noprop::sample_bool(ctx).then(|| sample_safe_text(ctx, 8));
        let key = ed25519_jwk_json(&base64url_encode(&proof_key_bytes()));

        // ヘッダの種類ごとに、期待するデコード結果を決める
        let (header_model, typ, algorithm, jwk_json) =
            match noprop::sample_weighted_index(ctx, &[5, 1, 1, 1, 1, 1]) {
                0 => (
                    ProofDecodeModel::Ok,
                    String::from(DPOP_PROOF_JWT_TYPE),
                    String::from(Algorithm::Es256.jose_name()),
                    Some(key),
                ),
                1 => (
                    ProofDecodeModel::InvalidType,
                    sample_safe_text(ctx, 8),
                    String::from(Algorithm::Es256.jose_name()),
                    Some(key),
                ),
                2 => (
                    ProofDecodeModel::UnsupportedAlgorithm,
                    String::from(DPOP_PROOF_JWT_TYPE),
                    String::from(Algorithm::HmacSha256.jose_name()),
                    Some(key),
                ),
                3 => (
                    ProofDecodeModel::MissingJwk,
                    String::from(DPOP_PROOF_JWT_TYPE),
                    String::from(Algorithm::Es256.jose_name()),
                    None,
                ),
                4 => (
                    ProofDecodeModel::InvalidJwk,
                    String::from(DPOP_PROOF_JWT_TYPE),
                    String::from(Algorithm::Es256.jose_name()),
                    Some(json_object(&[
                        ("kty", json_string("OKP")),
                        ("crv", json_string("Ed25519")),
                        ("x", json_string("AAAA")),
                        // 秘密鍵メンバーは拒否される (RFC 9449 §4.3)
                        ("d", json_string("AAAA")),
                    ])),
                ),
                _ => (
                    ProofDecodeModel::InvalidJws,
                    String::from(DPOP_PROOF_JWT_TYPE),
                    String::from("HS999"),
                    Some(key),
                ),
            };
        // JWS ヘッダの欠陥が先に検出され、次に actx の重複メンバーが検出される
        let model = if header_model != ProofDecodeModel::Ok {
            header_model
        } else if generated.duplicate {
            ProofDecodeModel::DuplicateActx
        } else {
            ProofDecodeModel::Ok
        };
        let mut header_members = vec![("typ", json_string(&typ)), ("alg", json_string(&algorithm))];
        if let Some(key_id) = &key_id {
            header_members.push(("kid", json_string(key_id)));
        }
        if let Some(jwk_json) = &jwk_json {
            header_members.push(("jwk", jwk_json.clone()));
        }
        let header = json_object(&header_members);
        let payload = json_object(&[
            ("jti", json_string(&jti)),
            ("iat", iat_text.clone()),
            ("actx", generated.text.clone()),
        ]);
        let text = build_compact(&header, &payload, &signature);

        match DpopProof::decode(&text) {
            Ok(proof) => {
                assert_eq!(model, ProofDecodeModel::Ok, "デコードが成功した");
                ok_seen.set(ok_seen.get() + 1);
                assert_eq!(proof.header().typ, DPOP_PROOF_JWT_TYPE);
                assert_eq!(proof.header().algorithm, Algorithm::Es256);
                assert_eq!(proof.header().key_id, key_id);
                assert_eq!(
                    proof.header().jwk,
                    shiguredo_moqt::c4m::jwk::Jwk::Okp {
                        curve: String::from("Ed25519"),
                        x: base64url_encode(&proof_key_bytes()),
                    }
                );
                assert_eq!(proof.claims().jti, jti);
                assert_eq!(proof.claims().issued_at, iat);
                assert_eq!(proof.signature(), signature.as_slice());
                let first = base64url_encode(header.as_bytes());
                let second = base64url_encode(payload.as_bytes());
                assert_eq!(
                    proof.signing_input(),
                    format!("{first}.{second}").as_bytes()
                );

                // actx の一括検証もモデルと比較する
                let expected =
                    expected_authorization_context_error(&generated, action, &namespace, &track);
                let context_error = expected.is_some();
                assert_eq!(
                    proof.verify_authorization_context(action, &namespace, &track),
                    match expected {
                        Some(error) => Err(error),
                        None => Ok(()),
                    },
                    "actx の検証がモデルと一致しない: generated={generated:?} action={action:?}"
                );
                if context_error {
                    context_error_seen.set(context_error_seen.get() + 1);
                } else {
                    context_ok_seen.set(context_ok_seen.get() + 1);
                }
            }
            Err(error) => {
                let matched = match model {
                    ProofDecodeModel::Ok => false,
                    ProofDecodeModel::InvalidType => error == DpopError::InvalidType(typ.clone()),
                    ProofDecodeModel::UnsupportedAlgorithm => {
                        error == DpopError::UnsupportedAlgorithm
                    }
                    ProofDecodeModel::MissingJwk => error == DpopError::MissingHeader("jwk"),
                    // JWS ヘッダの `jwk` は JWT のエラーとして包まれて返る
                    ProofDecodeModel::InvalidJwk => {
                        matches!(error, DpopError::Jwt(JwtError::Jwk(_)))
                    }
                    ProofDecodeModel::InvalidJws => matches!(error, DpopError::Jwt(_)),
                    ProofDecodeModel::DuplicateActx => {
                        error == DpopError::DuplicateMember(String::from("action"))
                    }
                };
                assert!(
                    matched,
                    "デコードエラーがモデルと一致しない: model={model:?} error={error}"
                );
                match model {
                    ProofDecodeModel::InvalidType => {
                        invalid_type_seen.set(invalid_type_seen.get() + 1);
                    }
                    ProofDecodeModel::UnsupportedAlgorithm => {
                        unsupported_algorithm_seen.set(unsupported_algorithm_seen.get() + 1);
                    }
                    ProofDecodeModel::MissingJwk => {
                        missing_jwk_seen.set(missing_jwk_seen.get() + 1);
                    }
                    ProofDecodeModel::InvalidJwk => {
                        invalid_jwk_seen.set(invalid_jwk_seen.get() + 1);
                    }
                    ProofDecodeModel::InvalidJws => {
                        invalid_jws_seen.set(invalid_jws_seen.get() + 1);
                    }
                    ProofDecodeModel::DuplicateActx => {
                        duplicate_actx_seen.set(duplicate_actx_seen.get() + 1);
                    }
                    ProofDecodeModel::Ok => {}
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
        invalid_type_seen.get() > 0,
        "typ が違うケースが生成されなかった\n{runner}"
    );
    assert!(
        unsupported_algorithm_seen.get() > 0,
        "MAC アルゴリズムのケースが生成されなかった\n{runner}"
    );
    assert!(
        missing_jwk_seen.get() > 0,
        "jwk が無いケースが生成されなかった\n{runner}"
    );
    assert!(
        invalid_jwk_seen.get() > 0,
        "JWK が不正なケースが生成されなかった\n{runner}"
    );
    assert!(
        invalid_jws_seen.get() > 0,
        "JWS ヘッダが不正なケースが生成されなかった\n{runner}"
    );
    assert!(
        duplicate_actx_seen.get() > 0,
        "actx の重複メンバーのケースが生成されなかった\n{runner}"
    );
    assert!(
        context_ok_seen.get() > 0,
        "actx の検証に成功するケースが生成されなかった\n{runner}"
    );
    assert!(
        context_error_seen.get() > 0,
        "actx の検証に失敗するケースが生成されなかった\n{runner}"
    );
    Ok(())
}

/// 鮮度の検証が境界値込みのモデルと一致する
#[test]
fn proof_freshness_matches_model() -> noprop::TestResult {
    let ok_seen = std::cell::Cell::new(0usize);
    let expired_seen = std::cell::Cell::new(0usize);
    let not_yet_valid_seen = std::cell::Cell::new(0usize);
    let non_finite_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let iat_text = sample_iat_text(ctx);
        let iat = iat_text
            .parse::<f64>()
            .expect("有限値の 10 進表現をパースできる");
        let actx = json_object(&[
            ("type", json_string(MOQT_AUTHORIZATION_CONTEXT_TYPE)),
            ("action", json_string("PUBLISH")),
            ("tns", json_string("ns")),
            ("tn", json_string("track")),
        ]);
        let text = build_valid_proof("jti", &iat_text, &actx, &sample_bytes(ctx, 4));
        let proof = DpopProof::decode(&text).expect("組み立てた proof はデコードできる");

        let reference = noprop::sample_with_boundaries(
            ctx,
            TIME_BOUNDARIES,
            noprop::Ratio::one_nth(4),
            |ctx| noprop::sample_f64_in(ctx, -1.0e12, 1.0e12),
        );
        let window = noprop::sample_with_boundaries(
            ctx,
            &[0.0, 1.0, 10.0, 100.0, f64::NAN, f64::INFINITY],
            noprop::Ratio::one_nth(3),
            |ctx| noprop::sample_f64_in(ctx, 0.0, 1000.0),
        );

        // 参照時刻 / ウィンドウ / iat の有限性と差分から判定するモデル
        let expected: Result<(), DpopError> = if !reference.is_finite() {
            Err(DpopError::NonFiniteNumber("reference time"))
        } else if !window.is_finite() {
            Err(DpopError::NonFiniteNumber("freshness window"))
        } else if !iat.is_finite() {
            Err(DpopError::NonFiniteNumber("iat"))
        } else {
            let delta = reference - iat;
            if delta > window {
                Err(DpopError::ProofExpired)
            } else if -delta > window {
                Err(DpopError::ProofNotYetValid)
            } else {
                Ok(())
            }
        };
        assert_eq!(
            proof.verify_freshness(reference, window),
            expected,
            "鮮度の検証がモデルと一致しない: iat={iat} reference={reference} window={window}"
        );
        match expected {
            Ok(()) => ok_seen.set(ok_seen.get() + 1),
            Err(DpopError::ProofExpired) => expired_seen.set(expired_seen.get() + 1),
            Err(DpopError::ProofNotYetValid) => {
                not_yet_valid_seen.set(not_yet_valid_seen.get() + 1)
            }
            Err(_) => non_finite_seen.set(non_finite_seen.get() + 1),
        }
        Ok(())
    })?;
    assert!(
        ok_seen.get() > 0,
        "ウィンドウ内のケースが生成されなかった\n{runner}"
    );
    assert!(
        expired_seen.get() > 0,
        "古すぎるケースが生成されなかった\n{runner}"
    );
    assert!(
        not_yet_valid_seen.get() > 0,
        "未来すぎるケースが生成されなかった\n{runner}"
    );
    assert!(
        non_finite_seen.get() > 0,
        "非有限値のケースが生成されなかった\n{runner}"
    );
    Ok(())
}

/// `check_and_record` の結果のモデル
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplayResult {
    /// 記録した
    Recorded,
    /// 同じ jti がウィンドウ内にあった
    Replayed,
    /// 数値が有限でない
    NonFinite,
    /// 想定していないエラー
    Other,
}

/// リプレイキャッシュのモデル (保持期間と重複の規則を独立に実装する)
#[derive(Debug, Default)]
struct ReplayModel {
    /// 記録した (jti, iat) の一覧
    entries: Vec<(String, f64)>,
}

impl ReplayModel {
    /// 仕様どおりに確認して記録する
    fn check_and_record(
        &mut self,
        jti: &str,
        issued_at: f64,
        window_seconds: f64,
        reference_time_seconds: f64,
    ) -> ReplayResult {
        if !issued_at.is_finite()
            || !window_seconds.is_finite()
            || !reference_time_seconds.is_finite()
        {
            return ReplayResult::NonFinite;
        }
        // ウィンドウ外の記録は参照時刻で破棄する
        self.entries
            .retain(|(_, recorded_at)| reference_time_seconds - recorded_at <= window_seconds);
        if self.entries.iter().any(|(recorded, _)| recorded == jti) {
            return ReplayResult::Replayed;
        }
        self.entries.push((String::from(jti), issued_at));
        ReplayResult::Recorded
    }
}

/// リプレイキャッシュの保持と拒否がモデルと一致する
#[test]
fn replay_cache_matches_model() -> noprop::TestResult {
    let recorded_seen = std::cell::Cell::new(0usize);
    let replayed_seen = std::cell::Cell::new(0usize);
    let non_finite_seen = std::cell::Cell::new(0usize);
    let pruned_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut cache = DpopReplayCache::new();
        let mut model = ReplayModel::default();
        assert!(cache.is_empty(), "新しいキャッシュは空");
        // jti の衝突を起こすため、少数のプールから選ぶ
        let pool: Vec<String> = (0..3).map(|_| sample_safe_text(ctx, 4)).collect();
        let mut reference = noprop::sample_f64_in(ctx, 0.0, 100.0);
        let steps = noprop::sample_usize_in(ctx, 1..=12);
        for _ in 0..steps {
            let jti = if noprop::sample_ratio(ctx, noprop::Ratio::new(3, 4)) {
                pool[noprop::sample_usize_in(ctx, 0..pool.len())].clone()
            } else {
                sample_safe_text(ctx, 4)
            };
            let issued_at = noprop::sample_with_boundaries(
                ctx,
                TIME_BOUNDARIES,
                noprop::Ratio::one_nth(3),
                |ctx| noprop::sample_f64_in(ctx, -100.0, 100.0),
            );
            let window = noprop::sample_with_boundaries(
                ctx,
                &[0.0, 1.0, 10.0, 100.0, f64::NAN, f64::INFINITY],
                noprop::Ratio::one_nth(3),
                |ctx| noprop::sample_f64_in(ctx, 0.0, 100.0),
            );
            reference += noprop::sample_f64_in(ctx, 0.0, 30.0);
            let reference_now = if noprop::sample_ratio(ctx, noprop::Ratio::one_nth(8)) {
                noprop::sample_choice(ctx, &[f64::NAN, f64::INFINITY, f64::NEG_INFINITY])
            } else {
                reference
            };

            let before = model.entries.len();
            let expected = model.check_and_record(&jti, issued_at, window, reference_now);
            let actual = match cache.check_and_record(&jti, issued_at, window, reference_now) {
                Ok(()) => ReplayResult::Recorded,
                Err(DpopError::Replayed) => ReplayResult::Replayed,
                Err(DpopError::NonFiniteNumber(_)) => ReplayResult::NonFinite,
                Err(_) => ReplayResult::Other,
            };
            assert_eq!(
                actual, expected,
                "判定がモデルと一致しない: jti={jti} iat={issued_at} window={window} reference={reference_now}"
            );
            assert_eq!(
                cache.len(),
                model.entries.len(),
                "記録数がモデルと一致しない: jti={jti} iat={issued_at} window={window} reference={reference_now}"
            );
            assert_eq!(cache.is_empty(), model.entries.is_empty(), "空判定が一致しない");
            match expected {
                ReplayResult::Recorded => {
                    recorded_seen.set(recorded_seen.get() + 1);
                    if model.entries.len() < before {
                        pruned_seen.set(pruned_seen.get() + 1);
                    }
                }
                ReplayResult::Replayed => {
                    replayed_seen.set(replayed_seen.get() + 1);
                    if model.entries.len() < before {
                        pruned_seen.set(pruned_seen.get() + 1);
                    }
                }
                ReplayResult::NonFinite => {
                    non_finite_seen.set(non_finite_seen.get() + 1);
                    assert_eq!(model.entries.len(), before, "拒否したのに記録が変わった");
                }
                ReplayResult::Other => {}
            }
        }
        Ok(())
    })?;
    assert!(
        recorded_seen.get() > 0,
        "記録されるケースが生成されなかった\n{runner}"
    );
    assert!(
        replayed_seen.get() > 0,
        "リプレイと判定されるケースが生成されなかった\n{runner}"
    );
    assert!(
        non_finite_seen.get() > 0,
        "非有限値のケースが生成されなかった\n{runner}"
    );
    assert!(
        pruned_seen.get() > 0,
        "ウィンドウ外の記録が破棄されるケースが生成されなかった\n{runner}"
    );
    Ok(())
}
