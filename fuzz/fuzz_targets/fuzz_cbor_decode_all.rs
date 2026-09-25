#![no_main]

use libfuzzer_sys::fuzz_target;
use shiguredo_moqt::cbor::decode::{decode, decode_all};

fuzz_target!(|data: &[u8]| {
    // 任意入力の CBOR シーケンスのデコードがパニックしないことと、
    // デコードに成功した値の再エンコードと決定論的エンコードが安定することを検証する
    if let Ok(values) = decode_all(data) {
        let mut bytes = Vec::new();
        for value in &values {
            value.encode_into(&mut bytes);
        }
        let again = decode_all(&bytes).expect("エンコードした CBOR シーケンスは必ずデコードできる");
        assert_eq!(again, values, "再エンコードで値が変わった");

        for value in &values {
            let canonical = value.to_canonical_bytes();
            let decoded = decode(&canonical).expect("決定論的エンコードは必ずデコードできる");
            assert_eq!(
                decoded.to_canonical_bytes(),
                canonical,
                "決定論的エンコードが安定しない"
            );
        }
    }
});
