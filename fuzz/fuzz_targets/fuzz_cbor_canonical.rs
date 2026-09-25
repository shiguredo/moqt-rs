#![no_main]

use libfuzzer_sys::fuzz_target;
use shiguredo_moqt::cbor::decode::decode;
use shiguredo_moqt::cbor::value::Value;

fuzz_target!(|data: &[u8]| {
    // 決定論的エンコードが固定点になり、マップのキーが
    // 決定論的エンコードのバイト列の辞書順になっていることを検証する
    if let Ok(value) = decode(data) {
        let canonical = value.to_canonical_bytes();
        let decoded = decode(&canonical).expect("決定論的エンコードは必ずデコードできる");
        assert_eq!(
            decoded.to_canonical_bytes(),
            canonical,
            "決定論的エンコードが安定しない"
        );
        assert_map_keys_sorted(&decoded);
    }
});

/// 決定論的エンコードされた値のマップのキーが辞書順になっていることを検証する
fn assert_map_keys_sorted(value: &Value) {
    match value {
        Value::Array(items) => {
            for item in items {
                assert_map_keys_sorted(item);
            }
        }
        Value::Map(pairs) => {
            let mut previous: Option<Vec<u8>> = None;
            for (key, value) in pairs {
                let encoded = key.to_canonical_bytes();
                if let Some(previous) = &previous {
                    assert!(
                        previous <= &encoded,
                        "決定論的エンコードのマップのキーが辞書順になっていない"
                    );
                }
                previous = Some(encoded);
                assert_map_keys_sorted(value);
            }
        }
        Value::Tag(_, content) => assert_map_keys_sorted(content),
        _ => {}
    }
}
