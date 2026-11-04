#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // decode できた入力に対しては、公開 accessor と IMMUTABLE_PROPERTIES 内側の探索 API が
    // panic しないことも確認する (draft-ietf-moq-transport-23 §10.7 (Immutable Properties))。
    let Ok(props) = shiguredo_moqt::track_properties::TrackProperties::decode(data) else {
        return;
    };
    let _ = props.codec_string();
    let _ = props.find_varint(shiguredo_moqt::loc::PROP_CODEC_STRING);
    let _ = props.find_bytes(shiguredo_moqt::loc::PROP_CODEC_STRING);
    let _ = props.find_bytes(shiguredo_moqt::track_properties::PROP_IMMUTABLE_PROPERTIES);
    let _ = props.as_slice().len();
});
