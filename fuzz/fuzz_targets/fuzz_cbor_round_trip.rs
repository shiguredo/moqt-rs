#![no_main]

use libfuzzer_sys::fuzz_target;
use shiguredo_moqt::cbor::decode::{Decoder, decode, decode_all};

fuzz_target!(|data: &[u8]| {
    // 逐次デコードが任意入力でパニックせず、全てデコードできた場合は
    // decode_all と同じ結果になることを検証する
    let mut decoder = Decoder::new(data);
    let mut values = Vec::new();
    let mut complete = true;
    while !decoder.is_finished() {
        match decoder.decode() {
            Ok(value) => values.push(value),
            Err(_) => {
                complete = false;
                break;
            }
        }
    }

    if complete {
        let all = decode_all(data).expect("逐次デコードに成功した入力は decode_all でも成功する");
        assert_eq!(all, values, "Decoder と decode_all の結果が食い違った");
    }

    // decode は入力全体が 1 個のデータ項目のときだけ成功すること
    if let Ok(value) = decode(data) {
        assert_eq!(
            values.len(),
            1,
            "decode が成功したのにデータ項目が 1 個でない"
        );
        assert_eq!(value, values[0], "decode と Decoder の結果が食い違った");
    }
});
