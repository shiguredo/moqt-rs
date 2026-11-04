//! COSE (RFC 9052) の property テスト
//!
//! COSE_Key の CBOR マップ変換 (RFC 9052 §7 / RFC 8747 §3.2) と、
//! COSE Key Thumbprint (RFC 9679) の決定性・任意パラメータ非影響も含む。

use pbt::common::{sample_bytes, test_runner};
use shiguredo_moqt::c4m::cbor;
use shiguredo_moqt::c4m::cbor::Value;
use shiguredo_moqt::c4m::cose::{
    Algorithm, CoseEncodingOptions, CoseMac0, CoseMessage, CoseSign1, HEADER_KCCS, Header, KeyId,
};
use shiguredo_moqt::c4m::crypto::aws_lc_rs::AwsLcRsCrypto;
use shiguredo_moqt::c4m::crypto::{
    COSE_KEY_ALG, COSE_KEY_BASE_IV, COSE_KEY_KID, COSE_KEY_OPS, CoseKey, EcCurve,
    default_signing_algorithm,
};

/// `kccs` に埋め込む確認用の公開鍵の COSE_Key を生成する
fn sample_cose_key(ctx: &mut noprop::TestCaseContext) -> Value {
    // Ed25519 の公開鍵 (32 バイト) を固定の kty / crv で包む
    Value::Map(vec![
        (Value::integer(1), Value::integer(1)),
        (Value::integer(-1), Value::integer(6)),
        (Value::integer(-2), Value::ByteString(sample_bytes(ctx, 32))),
    ])
}

/// `kccs` を生成する
fn sample_kccs(ctx: &mut noprop::TestCaseContext) -> Value {
    Value::Map(vec![(
        Value::integer(8),
        Value::Map(vec![(Value::integer(1), sample_cose_key(ctx))]),
    )])
}

/// ヘッダを生成する
pub(super) fn sample_header(ctx: &mut noprop::TestCaseContext, algorithm: Algorithm) -> Header {
    let key_id = match noprop::sample_weighted_index(ctx, &[2, 2, 1]) {
        0 => None,
        1 => Some(KeyId::Bytes(sample_bytes(ctx, 4))),
        _ => {
            let len = noprop::sample_usize_in(ctx, 0..=8);
            Some(KeyId::Text(noprop::sample_ascii_printable_string(ctx, len)))
        }
    };
    // 3/4 の確率で kccs を置き、その一部で crit に label 14 を含める
    let kccs = if noprop::sample_ratio(ctx, noprop::Ratio::new(3, 4)) {
        Some(sample_kccs(ctx))
    } else {
        None
    };
    let critical = match &kccs {
        Some(_) if noprop::sample_ratio(ctx, noprop::Ratio::one_nth(2)) => {
            vec![Value::integer(HEADER_KCCS)]
        }
        _ => Vec::new(),
    };
    Header {
        algorithm: Some(algorithm),
        algorithm_identifier: Some(algorithm.identifier()),
        key_id,
        typ: Some(Value::TextString(String::from("CAT"))),
        content_type: None,
        kccs,
        critical,
        raw: Vec::new(),
    }
}

/// COSE Key Thumbprint の計算に使う EC2 / OKP の公開鍵を生成する
fn sample_public_key(ctx: &mut noprop::TestCaseContext) -> CoseKey {
    match noprop::sample_usize_in(ctx, 0..4) {
        // 座標長は曲線ごとの固定長である
        0 => CoseKey::ec2(
            EcCurve::P256,
            noprop::sample_bytes_vec(ctx, 32),
            noprop::sample_bytes_vec(ctx, 32),
        ),
        1 => CoseKey::ec2(
            EcCurve::P384,
            noprop::sample_bytes_vec(ctx, 48),
            noprop::sample_bytes_vec(ctx, 48),
        ),
        2 => CoseKey::ec2(
            EcCurve::P521,
            noprop::sample_bytes_vec(ctx, 66),
            noprop::sample_bytes_vec(ctx, 66),
        ),
        _ => CoseKey::ed25519(noprop::sample_bytes_vec(ctx, 32)),
    }
}

/// COSE_Sign1 / COSE_Mac0 の encode -> decode が一致する
#[test]
fn cose_message_roundtrip() -> noprop::TestResult {
    let mac_seen = std::cell::Cell::new(0usize);
    let signature_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let algorithm = noprop::sample_choice(
            ctx,
            &[
                Algorithm::HmacSha256,
                Algorithm::HmacSha384,
                Algorithm::HmacSha512,
                Algorithm::Es256,
                Algorithm::Es384,
                Algorithm::Es512,
                Algorithm::EdDsa,
            ],
        );
        let header = sample_header(ctx, algorithm);
        let protected = cbor::encode(&header.encode().expect("ヘッダをエンコードできる"))
            .expect("CBOR をエンコードできる");
        let payload = sample_bytes(ctx, 16);
        let signature = sample_bytes(ctx, 8);
        let message = if algorithm.is_mac() {
            mac_seen.set(mac_seen.get() + 1);
            CoseMessage::Mac0(CoseMac0 {
                protected,
                unprotected: Vec::new(),
                payload: Some(payload),
                tag: signature,
                cose_tagged: true,
                cwt_tagged: true,
            })
        } else {
            signature_seen.set(signature_seen.get() + 1);
            CoseMessage::Sign1(CoseSign1 {
                protected,
                unprotected: Vec::new(),
                payload: Some(payload),
                signature,
                cose_tagged: true,
                cwt_tagged: true,
            })
        };
        let encoded = message
            .encode(&CoseEncodingOptions::default())
            .expect("COSE メッセージをエンコードできる");
        let decoded = CoseMessage::decode(&encoded).expect("COSE メッセージをデコードできる");
        assert_eq!(decoded, message, "ラウンドトリップでメッセージが変わる");
        let decoded_header = decoded.header().expect("ヘッダを読める");
        assert_eq!(decoded_header.algorithm, Some(algorithm));
        assert_eq!(decoded_header.key_id, header.key_id);
        assert_eq!(
            decoded_header.kccs, header.kccs,
            "kccs のラウンドトリップで値が変わる"
        );
        assert_eq!(
            decoded_header.critical, header.critical,
            "crit のラウンドトリップで値が変わる"
        );
        assert!(decoded.signing_input().is_ok());
        Ok(())
    })?;
    assert!(
        mac_seen.get() > 0,
        "MAC のケースが生成されなかった\n{runner}"
    );
    assert!(
        signature_seen.get() > 0,
        "署名のケースが生成されなかった\n{runner}"
    );
    Ok(())
}

/// COSE_Key の CBOR マップ変換が公開鍵の情報を保つこと
#[test]
fn cose_key_map_roundtrip_preserves_public_key() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        let key = sample_public_key(ctx);
        let value = key.to_cose_key_map().expect("COSE_Key へ変換できる");
        // 秘密鍵パラメータは出力しない
        let entries = value.as_map().expect("COSE_Key は map である");
        assert!(
            !entries.iter().any(|(label, _)| label.as_int() == Some(-4)),
            "秘密鍵パラメータを出力した"
        );
        assert_eq!(
            CoseKey::from_cose_key_map(&value, None).expect("COSE_Key から復元できる"),
            key
        );
        Ok(())
    })?;
    Ok(())
}

/// COSE_Key に秘密鍵パラメータが現れたら常に拒否すること
#[test]
fn cose_key_map_rejects_private_key_parameter() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(64, |ctx| {
        let value = sample_cose_key(ctx);
        let Value::Map(mut entries) = value else {
            unreachable!("COSE_Key は map である");
        };
        entries.push((Value::integer(-4), Value::ByteString(sample_bytes(ctx, 32))));
        assert!(
            CoseKey::from_cose_key_map(&Value::Map(entries), None).is_err(),
            "秘密鍵パラメータを含む COSE_Key を受理した"
        );
        Ok(())
    })?;
    Ok(())
}

/// COSE Key Thumbprint が同じ鍵で安定し、異なる鍵で異なること
///
/// RFC 9679 §3 の計算は鍵の必須パラメータだけを入力にするため、同じ鍵からは常に
/// 同じサムプリントが得られ、公開パラメータが 1 バイトでも異なれば入力も変わる。
#[test]
fn cose_key_thumbprint_is_deterministic() -> noprop::TestResult {
    let crypto = AwsLcRsCrypto::new();
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        let key = sample_public_key(ctx);
        let first = key
            .cose_key_thumbprint_sha256(&crypto)
            .expect("サムプリントを計算できる");
        let second = key
            .cose_key_thumbprint_sha256(&crypto)
            .expect("サムプリントを計算できる");
        assert_eq!(first, second, "同じ鍵のサムプリントが変わった");
        assert_eq!(first.len(), 32, "SHA-256 のサムプリント長ではない");

        // 公開パラメータの 1 バイトだけを変えた鍵を作る。生成した鍵は曲線ごとの
        // 固定長を持つため、先頭バイトが必ず存在する
        let mut changed = key.clone();
        match &mut changed {
            CoseKey::Ec2 { x, .. } => x[0] ^= 0xff,
            CoseKey::Okp { public_key, .. } => public_key[0] ^= 0xff,
            CoseKey::Symmetric { .. } => unreachable!("公開鍵だけを生成する"),
        }
        assert_ne!(key, changed, "鍵の変更が反映されていない");
        assert_ne!(
            key.cose_key_thumbprint_sha256(&crypto)
                .expect("サムプリントを計算できる"),
            changed
                .cose_key_thumbprint_sha256(&crypto)
                .expect("サムプリントを計算できる"),
            "異なる鍵のサムプリントが一致した"
        );
        Ok(())
    })?;
    Ok(())
}

/// 任意の COSE_Key パラメータの有無が COSE Key Thumbprint を変えないこと
///
/// RFC 9679 §5.1 のとおり、`kid` / `alg` / `key_ops` などの任意パラメータは
/// サムプリントの入力に含まれない。COSE_Key マップからの解析を経由しても、
/// 必須パラメータだけの鍵と同じ値になることを確認する。
#[test]
fn cose_key_thumbprint_ignores_optional_parameters() -> noprop::TestResult {
    let crypto = AwsLcRsCrypto::new();
    let mut runner = test_runner()?;
    runner.run(64, |ctx| {
        let key = sample_public_key(ctx);
        let required = key.to_cose_key_map().expect("COSE_Key へ変換できる");
        let Value::Map(mut entries) = required else {
            unreachable!("COSE_Key は map である");
        };
        // 任意パラメータを加える。alg は鍵の種別と矛盾しない値にする
        entries.push((
            Value::integer(COSE_KEY_KID),
            Value::ByteString(noprop::sample_bytes_vec(ctx, 8)),
        ));
        entries.push((
            Value::integer(COSE_KEY_ALG),
            Value::integer(default_signing_algorithm(&key).identifier()),
        ));
        entries.push((
            Value::integer(COSE_KEY_OPS),
            Value::Array(vec![Value::TextString(String::from("verify"))]),
        ));
        entries.push((
            Value::integer(COSE_KEY_BASE_IV),
            Value::ByteString(noprop::sample_bytes_vec(ctx, 16)),
        ));
        entries.push((
            Value::integer(999),
            Value::TextString(noprop::sample_ascii_printable_string(ctx, 8)),
        ));
        let decorated = CoseKey::from_cose_key_map(&Value::Map(entries), None)
            .expect("任意パラメータ付きの COSE_Key から公開鍵を復元できる");
        assert_eq!(decorated, key, "任意パラメータの解析で公開鍵が変わった");
        assert_eq!(
            decorated
                .cose_key_thumbprint_sha256(&crypto)
                .expect("サムプリントを計算できる"),
            key.cose_key_thumbprint_sha256(&crypto)
                .expect("サムプリントを計算できる"),
            "任意パラメータの有無でサムプリントが変わった"
        );
        Ok(())
    })?;
    Ok(())
}
