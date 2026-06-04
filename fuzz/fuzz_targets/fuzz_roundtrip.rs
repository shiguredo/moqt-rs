#![no_main]

use libfuzzer_sys::fuzz_target;

use shiguredo_moqt::loc::LocProperties;
use shiguredo_moqt::message::ControlMessage;
use shiguredo_moqt::message_parameter::MessageParameters;
use shiguredo_moqt::msf::{MsfCatalogDocument, MsfEventTimeline, MsfMediaTimeline};
use shiguredo_moqt::object_properties::ObjectProperties;
use shiguredo_moqt::parameter::SetupOptions;
use shiguredo_moqt::stream::datagram::ObjectDatagram;
use shiguredo_moqt::stream::fetch::{FetchHeader, FetchPriorContext, FetchStreamEntry};
use shiguredo_moqt::stream::subgroup::{SubgroupHeader, SubgroupObject};
use shiguredo_moqt::track_properties::TrackProperties;

// decode に成功した値は encode しても panic しない (encode / decode の往復不変式)
fuzz_target!(|data: &[u8]| {
    if let Ok((msg, _)) = ControlMessage::decode(data) {
        let _ = msg.encode();
    }
    if let Ok((header, _)) = SubgroupHeader::decode(data) {
        let _ = header.encode();
    }
    if let Ok((object, _, _)) = SubgroupObject::decode(data, false) {
        let mut buf = Vec::new();
        let _ = object.encode(false, None, &mut buf);
    }
    if let Ok((object, _, _)) = SubgroupObject::decode(data, true) {
        let mut buf = Vec::new();
        let _ = object.encode(true, None, &mut buf);
    }
    if let Ok((header, _)) = FetchHeader::decode(data) {
        let _ = header.encode();
    }
    if let Ok((entry, _)) = FetchStreamEntry::decode(data, FetchPriorContext::First) {
        let mut buf = Vec::new();
        let _ = entry.encode(None, FetchPriorContext::First, &mut buf);
    }
    if let Ok((datagram, _)) = ObjectDatagram::decode(data) {
        let _ = datagram.encode();
    }
    if let Ok((options, _)) = SetupOptions::decode(data) {
        let mut buf = Vec::new();
        let _ = options.encode(&mut buf);
    }
    if let Ok((parameters, _)) = MessageParameters::decode(data) {
        let mut buf = Vec::new();
        let _ = parameters.encode(&mut buf);
    }
    if let Ok(props) = TrackProperties::decode(data) {
        let mut buf = Vec::new();
        let _ = props.encode(&mut buf);
    }
    if let Ok((props, _)) = ObjectProperties::decode(data) {
        let mut buf = Vec::new();
        let _ = props.encode(&mut buf);
    }
    if let Ok((props, _)) = LocProperties::decode(data) {
        let _ = props.encode();
    }
    if let Ok(document) = MsfCatalogDocument::decode(data) {
        let _ = document.encode();
    }
    if let Ok(timeline) = MsfMediaTimeline::decode(data) {
        let _ = timeline.encode();
    }
    if let Ok(timeline) = MsfEventTimeline::decode(data) {
        let _ = timeline.encode();
    }
});
