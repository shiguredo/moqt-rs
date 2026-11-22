use super::*;
use shiguredo_moqt::msf::MsfCatalogTracker;

fn full_catalog(json: &str) -> MsfCatalog {
    match MsfCatalogDocument::decode(json.as_bytes(), shiguredo_moqt::msf::MsfCompression::None)
        .expect("テストフィクスチャの前提条件を満たす")
    {
        MsfCatalogDocument::Full(catalog) => catalog,
        MsfCatalogDocument::Delta(_) => panic!("Full catalog がデコードされること"),
    }
}

#[test]
fn is_complete_must_not_be_removed() {
    let first = full_catalog(
        r#"{"version":"draft-02","isComplete":true,"tracks":[{"name":"v","packaging":"loc","isLive":false}]}"#,
    );
    let second = full_catalog(
        r#"{"version":"draft-02","tracks":[{"name":"v","packaging":"loc","isLive":false}]}"#,
    );
    let mut tracker = MsfCatalogTracker::new();
    tracker
        .observe(&first, None)
        .expect("最初のカタログは受理されること");
    assert!(matches!(
        tracker.observe(&second, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn new_track_after_complete_is_rejected() {
    let first = full_catalog(
        r#"{"version":"draft-02","isComplete":true,"tracks":[{"name":"v","packaging":"loc","isLive":false}]}"#,
    );
    let second = full_catalog(
        r#"{"version":"draft-02","isComplete":true,"tracks":[{"name":"v","packaging":"loc","isLive":false},{"name":"w","packaging":"loc","isLive":false}]}"#,
    );
    let mut tracker = MsfCatalogTracker::new();
    tracker
        .observe(&first, None)
        .expect("最初のカタログは受理されること");
    assert!(matches!(
        tracker.observe(&second, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn is_live_true_must_not_follow_false() {
    let vod = full_catalog(
        r#"{"version":"draft-02","tracks":[{"name":"v","packaging":"loc","isLive":false}]}"#,
    );
    let live = full_catalog(
        r#"{"version":"draft-02","tracks":[{"name":"v","packaging":"loc","isLive":true}]}"#,
    );
    let mut tracker = MsfCatalogTracker::new();
    tracker
        .observe(&vod, None)
        .expect("VOD カタログは受理されること");
    assert!(matches!(
        tracker.observe(&live, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn different_track_can_go_live() {
    let first = full_catalog(
        r#"{"version":"draft-02","tracks":[{"name":"a","packaging":"loc","isLive":false}]}"#,
    );
    let second = full_catalog(
        r#"{"version":"draft-02","tracks":[{"name":"b","packaging":"loc","isLive":true}]}"#,
    );
    let mut tracker = MsfCatalogTracker::new();
    tracker
        .observe(&first, None)
        .expect("最初のカタログは受理されること");
    tracker
        .observe(&second, None)
        .expect("別の track は isLive=true で登場できること");
}

#[test]
fn failed_observation_does_not_change_history() {
    let vod = full_catalog(
        r#"{"version":"draft-02","tracks":[{"name":"v","packaging":"loc","isLive":false}]}"#,
    );
    let live = full_catalog(
        r#"{"version":"draft-02","tracks":[{"name":"v","packaging":"loc","isLive":true}]}"#,
    );
    let mut tracker = MsfCatalogTracker::new();
    tracker
        .observe(&vod, None)
        .expect("VOD カタログは受理されること");
    assert!(tracker.observe(&live, None).is_err());
    // 失敗した観測で履歴が進んでいないため、同じ VOD カタログは再び受理される
    tracker
        .observe(&vod, None)
        .expect("失敗した観測は履歴を変更しないこと");
    assert!(tracker.observe(&live, None).is_err());
}

#[test]
fn publish_tracks_are_tracked() {
    let vod = full_catalog(
        r#"{"version":"draft-02","tracks":[],"publishTracks":[{"name":"v","packaging":"loc","isLive":false}]}"#,
    );
    let live = full_catalog(
        r#"{"version":"draft-02","tracks":[],"publishTracks":[{"name":"v","packaging":"loc","isLive":true}]}"#,
    );
    let mut tracker = MsfCatalogTracker::new();
    tracker
        .observe(&vod, None)
        .expect("VOD カタログは受理されること");
    assert!(matches!(
        tracker.observe(&live, None),
        Err(MessageError::InvalidCatalog(_))
    ));
}
