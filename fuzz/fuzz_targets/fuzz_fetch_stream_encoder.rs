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
    /// Object の後に End of Gone Range を書くか
    ///
    /// draft-ietf-moq-transport-23 §11.4.2 (End of Range): Group ID Delta と Object ID Delta を
    /// 運び、直前の Location からその Location までが Gone であることを示す。
    end_of_gone_range: bool,
    /// Object の後に End of Group を書くか
    ///
    /// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields) の marker は直前の
    /// Object が Group の最終 Object であることを示す。
    end_of_group: bool,
    /// Object の後に End of Track を書くか
    end_of_track: bool,
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
        // End of Gone Range の終端は直前の Object の次の Object ID にする。Group Order と
        // 逆方向になる入力や overflow する入力はエンコーダが拒否するため、
        // ここでは panic しないことだけを確認する。
        if object.end_of_gone_range {
            let _ = encoder.encode_end_of_gone_range(
                object.group_id,
                object.object_id.wrapping_add(1),
                &mut buf,
            );
        }
        // End of Track は End of Group も意味するため、両方を書く入力もそのまま通す。
        // marker を書いた後の Object は前後関係の検証で拒否されることがあるが、
        // エンコーダが panic しないことだけを確認する。
        if object.end_of_group {
            let _ = encoder.encode_end_of_group(&mut buf);
        }
        if object.end_of_track {
            let _ = encoder.encode_end_of_track(&mut buf);
        }
    }
});
