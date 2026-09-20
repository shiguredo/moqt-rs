//! COSE (RFC 9052) の property テスト

use pbt::common::{sample_bytes, test_runner};
use shiguredo_moqt::c4m::cbor;
use shiguredo_moqt::c4m::cose::{
    Algorithm, CoseEncodingOptions, CoseMac0, CoseMessage, CoseSign1, Header, KeyId,
};

/// ヘッダを生成する
fn sample_header(ctx: &mut noprop::TestCaseContext, algorithm: Algorithm) -> Header {
    let key_id = match noprop::sample_weighted_index(ctx, &[2, 2, 1]) {
        0 => None,
        1 => Some(KeyId::Bytes(sample_bytes(ctx, 4))),
        _ => {
            let len = noprop::sample_usize_in(ctx, 0..=8);
            Some(KeyId::Text(noprop::sample_ascii_printable_string(ctx, len)))
        }
    };
    Header {
        algorithm: Some(algorithm),
        algorithm_identifier: Some(algorithm.identifier()),
        key_id,
        typ: Some(shiguredo_moqt::c4m::cbor::Value::TextString(String::from(
            "CAT",
        ))),
        content_type: None,
        critical: Vec::new(),
        raw: Vec::new(),
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
