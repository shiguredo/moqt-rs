use super::*;

// ─── initDataList ─────────────────────────────────────────────────────────────

#[test]
fn init_data_list_duplicate_id_rejected() {
    // draft-ietf-moq-msf-02 §5.1.7 (Initialization Data List): id は catalog 内で一意
    let json = br#"{"version":"draft-02","tracks":[],"initDataList":[
            {"id":"init1","type":"inline","data":"AAEC"},
            {"id":"init1","type":"inline","data":"AAED"}
        ]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn init_data_list_unknown_type_rejected() {
    let json = br#"{"version":"draft-02","tracks":[],"initDataList":[{"id":"init1","type":"unknown","data":"AAEC"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn init_ref_missing_id_rejected() {
    // draft-ietf-moq-msf-02 §5.2.14 (Initialization reference): initRef は
    // initDataList の id を指すため、一致する id が無ければ reject される
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"initRef":"missing"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn init_ref_matching_id_accepted() {
    // initRef が initDataList の id と一致すれば受理される
    let json = br#"{"version":"draft-02","tracks":[{"name":"t","packaging":"loc","isLive":true,"initRef":"init1"}],"initDataList":[{"id":"init1","type":"inline","data":"AAEC"}]}"#;
    assert!(MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None).is_ok());
}

#[test]
fn init_data_list_missing_id_rejected() {
    let json =
        br#"{"version":"draft-02","tracks":[],"initDataList":[{"type":"inline","data":"AAEC"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn init_data_list_missing_type_rejected() {
    let json =
        br#"{"version":"draft-02","tracks":[],"initDataList":[{"id":"init1","data":"AAEC"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn init_data_list_missing_data_rejected() {
    let json =
        br#"{"version":"draft-02","tracks":[],"initDataList":[{"id":"init1","type":"inline"}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json, shiguredo_moqt::msf::MsfCompression::None),
        Err(MessageError::InvalidCatalog(_))
    ));
}
