#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use shiguredo_moqt::stream::encoder::{FetchObjectInput, FetchStreamEncoder};

/// 1 オブジェクト分の入力
#[derive(Arbitrary, Debug)]
struct ObjectInput {
    group_id: u64,
    subgroup_id: u64,
    object_id: u64,
    publisher_priority: u8,
    has_properties: bool,
    is_datagram_origin: bool,
    payload_length: u64,
    properties: Vec<u8>,
}

/// エンコーダへ与える入力列
#[derive(Arbitrary, Debug)]
struct Input {
    request_id: u64,
    objects: Vec<ObjectInput>,
}

fuzz_target!(|input: Input| {
    let mut encoder = FetchStreamEncoder::new(input.request_id);
    let _ = encoder.encode_header();
    let mut buf = Vec::new();
    for object in &input.objects {
        let properties = object
            .has_properties
            .then_some(object.properties.as_slice());
        let object_input = FetchObjectInput {
            group_id: object.group_id,
            subgroup_id: object.subgroup_id,
            object_id: object.object_id,
            publisher_priority: object.publisher_priority,
            has_properties: object.has_properties,
            is_datagram_origin: object.is_datagram_origin,
            payload_length: object.payload_length,
        };
        let _ = encoder.encode_object(&object_input, properties, &mut buf);
    }
});
