#![no_main]

use libfuzzer_sys::fuzz_target;

use shiguredo_moqt::loc::{
    LocProperties, PROP_AUDIO_CONFIG, PROP_AUDIO_LEVEL, PROP_TIMESCALE, PROP_TIMESTAMP,
    PROP_VIDEO_CONFIG, PROP_VIDEO_FRAME_MARKING,
};

fuzz_target!(|data: &[u8]| {
    // decode できた入力に対しては、公開 accessor と IMMUTABLE_PROPERTIES 内側の探索 API が
    // panic しないことも確認する (draft-ietf-moq-transport-23 §10.7 (Immutable Properties))。
    let Ok((props, _)) = LocProperties::decode(data) else {
        return;
    };
    let _ = props.timestamp();
    let _ = props.timescale();
    let _ = props.video_frame_marking();
    let _ = props.audio_level();
    let _ = props.video_config();
    let _ = props.audio_config();
    let _ = props.find_varint(PROP_TIMESTAMP);
    let _ = props.find_varint(PROP_TIMESCALE);
    let _ = props.find_varint(PROP_AUDIO_LEVEL);
    let _ = props.find_bytes(PROP_VIDEO_CONFIG);
    let _ = props.find_bytes(PROP_AUDIO_CONFIG);
    let _ = props.find_bytes(PROP_VIDEO_FRAME_MARKING);
    let _ = props.len();
    let _ = props.iter().count();
});
