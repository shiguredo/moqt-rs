#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use shiguredo_moqt::{stream::decoder::DecodedFetchEntry, stream::decoder::FetchStreamDecoder};

#[derive(Arbitrary, Debug)]
struct Input {
    group_order_ascending: bool,
    chunks: Vec<Vec<u8>>,
}

fuzz_target!(|input: Input| {
    // 0x01 = Ascending, 0x02 = Descending
    let group_order = if input.group_order_ascending {
        0x01
    } else {
        0x02
    };
    let mut decoder = FetchStreamDecoder::new_with_group_order(group_order)
        .expect("group_order は 0x01 または 0x02 のみを渡す");
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

            match decoder.try_decode_entry() {
                Ok(Some(entry)) => match entry {
                    DecodedFetchEntry::Object(obj) => {
                        if obj.payload_length > 0 && decoder.try_read_payload().is_none() {
                            awaiting_payload = true;
                            break;
                        }
                    }
                    DecodedFetchEntry::EndOfNonExistentRange { .. }
                    | DecodedFetchEntry::EndOfUnknownRange { .. }
                    | DecodedFetchEntry::EndOfTimedOutRange { .. } => {}
                },
                Ok(None) => break,
                Err(_) => return,
            }
        }
    }

    let _ = decoder.finish();
});
