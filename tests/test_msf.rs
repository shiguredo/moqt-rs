use shiguredo_moqt::{
    error::MessageError,
    message::common::TrackNamespace,
    msf::{
        MSF_VERSION, MsfAuthInfo, MsfBuffers, MsfCatalog, MsfCatalogDocument, MsfCloneTrack,
        MsfCompression, MsfDeltaOperation, MsfDeltaUpdate, MsfDependency, MsfEventIndex,
        MsfEventTimeline, MsfEventTimelineEntry, MsfInitData, MsfInitDataKind, MsfMediaTimeline,
        MsfMediaTimelineEntry, MsfPackaging, MsfRemoveTrack, MsfTemplate, MsfTrack, MsfUpdateTrack,
        StreamMapping, decode_event_timeline, decode_media_timeline, encode_event_timeline,
        encode_media_timeline, parse_catalog_namespace, parse_fragment_pairs,
        resolve_catalog_variables, serialize_catalog_namespace, validate_event_timeline_depends,
        validate_media_timeline_format,
    },
};

/// テスト用に v02 の `/` 区切り namespace を MOQT の Track Namespace へ変換する
fn test_catalog_namespace(value: &str) -> TrackNamespace {
    shiguredo_moqt::msf::parse_catalog_namespace(value)
        .expect("テスト用 namespace は v02 の制約を満たすこと")
}

#[path = "test_msf/catalog_decode.rs"]
mod catalog_decode;

#[path = "test_msf/compression.rs"]
mod compression;

#[path = "test_msf/tracker.rs"]
mod tracker;

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

#[path = "test_msf/debug_redaction.rs"]
mod debug_redaction;

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
