#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use shiguredo_moqt::stream::decoder::SubgroupStreamDecoder;

#[derive(Arbitrary, Debug)]
struct Input {
    chunks: Vec<Vec<u8>>,
}

fuzz_target!(|input: Input| {
    let mut decoder = SubgroupStreamDecoder::new();
    let mut header_decoded = false;
    let mut awaiting_payload = false;

    for chunk in &input.chunks {
        decoder.push(chunk);

        if !header_decoded {
            match decoder.try_decode_header() {
                Ok(Some(_)) => {
                    header_decoded = true;
                }
                Ok(None) => continue,
                Err(_) => return,
            }
        }

        loop {
            if awaiting_payload {
                if decoder.try_read_payload().is_some() {
                    awaiting_payload = false;
                } else {
                    break;
                }
            }

            match decoder.try_decode_object() {
                Ok(Some(obj)) => {
                    if obj.payload_length > 0 && decoder.try_read_payload().is_none() {
                        awaiting_payload = true;
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => return,
            }
        }
    }

    let _ = decoder.finish();
});
