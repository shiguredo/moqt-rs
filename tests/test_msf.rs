use shiguredo_moqt::{
    error::MessageError,
    msf::{
        MSF_VERSION, MsfAuthInfo, MsfBuffers, MsfCatalog, MsfCatalogDocument, MsfCloneTrack,
        MsfDeltaOperation, MsfDeltaUpdate, MsfEventIndex, MsfEventTimeline, MsfEventTimelineEntry,
        MsfInitData, MsfInitDataKind, MsfMediaTimeline, MsfMediaTimelineEntry, MsfPackaging,
        MsfRemoveTrack, MsfTemplate, MsfTrack, TimelineEncodingOptions, decode_event_timeline,
        decode_media_timeline, encode_event_timeline, encode_media_timeline, parse_fragment_pairs,
        resolve_catalog_variables,
    },
};

#[path = "test_msf/catalog_decode.rs"]
mod catalog_decode;

#[path = "test_msf/track_fields.rs"]
mod track_fields;

#[path = "test_msf/encode_decode_roundtrip.rs"]
mod encode_decode_roundtrip;

#[path = "test_msf/error_cases.rs"]
mod error_cases;

#[path = "test_msf/media_timeline.rs"]
mod media_timeline;

#[path = "test_msf/event_timeline.rs"]
mod event_timeline;

#[path = "test_msf/timeline_gzip.rs"]
mod timeline_gzip;

#[path = "test_msf/variable_substitution.rs"]
mod variable_substitution;

#[path = "test_msf/template.rs"]
mod template;

#[path = "test_msf/delta_apply.rs"]
mod delta_apply;

#[path = "test_msf/uri.rs"]
mod uri;
