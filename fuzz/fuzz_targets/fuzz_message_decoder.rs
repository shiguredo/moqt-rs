#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use shiguredo_moqt::decoder::MessageDecoder;

#[derive(Arbitrary, Debug)]
struct Input {
    chunks: Vec<Vec<u8>>,
}

fuzz_target!(|input: Input| {
    let mut decoder = MessageDecoder::new();
    let mut varint_decoded = false;

    for chunk in &input.chunks {
        decoder.push(chunk);

        if !varint_decoded {
            match decoder.try_decode_varint() {
                Ok(Some(_)) => {
                    varint_decoded = true;
                }
                Ok(None) => continue,
                Err(_) => return,
            }
        }

        loop {
            match decoder.try_decode_message() {
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(_) => return,
            }
        }
    }
});
