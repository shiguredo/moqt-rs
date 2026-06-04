use super::*;

#[test]
fn template_roundtrip() {
    // draft-ietf-moq-msf-01 §5.6.10 準拠例の template が roundtrip すること
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"template":[0,2002,[0,0],[1,0],1759924158381,2002]}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc.clone() else {
        panic!("Full catalog がデコードされること");
    };
    let template = cat.tracks[0]
        .template
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(template.start_media_time, 0);
    assert_eq!(template.delta_media_time, 2002);
    assert_eq!((template.start_group_id, template.start_object_id), (0, 0));
    assert_eq!((template.delta_group_id, template.delta_object_id), (1, 0));
    assert_eq!(template.start_wallclock, 1759924158381);
    assert_eq!(template.delta_wallclock, 2002);
    let encoded = MsfCatalogDocument::Full(cat)
        .encode()
        .expect("encode に成功すること");
    let decoded =
        MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded, doc);
}

#[test]
fn template_too_many_elements_rejected() {
    // 要素数 7 は reject されること
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"template":[0,2002,[0,0],[1,0],1759924158381,2002,0]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn template_delta_location_arity_rejected() {
    // deltaLocation の要素数 1 は reject されること
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"template":[0,2002,[0,0],[1],1759924158381,2002]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn template_non_array_rejected() {
    // template 自体が配列でない場合は reject されること
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"template":0}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn template_in_clone_rejected_when_invalid() {
    // clone 経路の不正 template も reject されること
    let json = br#"{"deltaUpdate":[{"op":"clone","tracks":[{"name":"c","parentName":"p","template":[0,2002,[0,0],[1,0],1759924158381]}]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn template_location_non_array_rejected() {
    // startLocation が配列でない場合は reject されること
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"template":[0,2002,0,[1,0],1759924158381,2002]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn template_location_element_type_rejected() {
    // location 要素の型違いは reject されること
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"template":[0,2002,[0,"x"],[1,0],1759924158381,2002]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn template_distinct_values_not_swapped() {
    // 6 要素が全て異なる値でも正しく対応すること (start / delta 取り違えの回帰検出)
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"template":[100,2002,[5,7],[1,2],1759924158381,3003]}]}"#;
    let doc = MsfCatalogDocument::decode(json).expect("テストフィクスチャの前提条件を満たす");
    let MsfCatalogDocument::Full(cat) = doc else {
        panic!("Full catalog がデコードされること");
    };
    let template = cat.tracks[0]
        .template
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(template.start_media_time, 100);
    assert_eq!(template.delta_media_time, 2002);
    assert_eq!((template.start_group_id, template.start_object_id), (5, 7));
    assert_eq!((template.delta_group_id, template.delta_object_id), (1, 2));
    assert_eq!(template.start_wallclock, 1759924158381);
    assert_eq!(template.delta_wallclock, 3003);
    assert_eq!(
        template.resolve_entry(1),
        Some(MsfMediaTimelineEntry {
            pts_ms: 2102,
            group_id: 6,
            object_id: 9,
            wallclock_ms: 1759924161384,
        })
    );
}

#[test]
fn template_wrong_element_count_rejected() {
    // 要素数 5 は reject されること
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"template":[0,2002,[0,0],[1,0],1759924158381]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn template_wrong_location_arity_rejected() {
    // location の要素数 3 は reject されること
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"template":[0,2002,[0,0,0],[1,0],1759924158381,2002]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn template_wrong_type_rejected() {
    // 要素の型違いは reject されること
    let json = br#"{"version":"draft-01","tracks":[{"name":"t","packaging":"loc","isLive":true,"template":[0,"2002",[0,0],[1,0],1759924158381,2002]}]}"#;
    assert!(matches!(
        MsfCatalogDocument::decode(json),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn resolve_entry_first_and_overflow() {
    // n = 0 は先頭エントリを返し、overflow 時は None を返すこと
    let template = MsfTemplate {
        start_media_time: 0,
        delta_media_time: 2002,
        start_group_id: 0,
        start_object_id: 0,
        delta_group_id: 1,
        delta_object_id: 0,
        start_wallclock: 1759924158381,
        delta_wallclock: 2002,
    };
    assert_eq!(
        template.resolve_entry(0),
        Some(MsfMediaTimelineEntry {
            pts_ms: 0,
            group_id: 0,
            object_id: 0,
            wallclock_ms: 1759924158381,
        })
    );
    assert_eq!(
        template.resolve_entry(1),
        Some(MsfMediaTimelineEntry {
            pts_ms: 2002,
            group_id: 1,
            object_id: 0,
            wallclock_ms: 1759924160383,
        })
    );
    assert_eq!(template.resolve_entry(u64::MAX), None);
}

#[test]
fn resolve_entry_single_series_overflow() {
    // 1 系列のみ overflow しても全体が None になること
    let template = MsfTemplate {
        start_media_time: 0,
        delta_media_time: 0,
        start_group_id: 0,
        start_object_id: 0,
        delta_group_id: 0,
        delta_object_id: 0,
        start_wallclock: u64::MAX,
        delta_wallclock: 1,
    };
    assert_eq!(template.resolve_entry(1), None);
}

#[test]
fn resolve_entry_media_series_overflow() {
    // media 系列のみ overflow しても全体が None になること
    let template = MsfTemplate {
        start_media_time: u64::MAX,
        delta_media_time: 1,
        start_group_id: 0,
        start_object_id: 0,
        delta_group_id: 0,
        delta_object_id: 0,
        start_wallclock: 0,
        delta_wallclock: 0,
    };
    assert_eq!(template.resolve_entry(1), None);
}

#[test]
fn resolve_entry_zero_delta_wallclock() {
    // delta 0 の固定 template と wallclock 0 (VOD ・不明時) の解決
    let template = MsfTemplate {
        start_media_time: 4004,
        delta_media_time: 0,
        start_group_id: 2,
        start_object_id: 0,
        delta_group_id: 0,
        delta_object_id: 0,
        start_wallclock: 0,
        delta_wallclock: 0,
    };
    let expected = Some(MsfMediaTimelineEntry {
        pts_ms: 4004,
        group_id: 2,
        object_id: 0,
        wallclock_ms: 0,
    });
    assert_eq!(template.resolve_entry(0), expected);
    assert_eq!(template.resolve_entry(2), expected);
}
