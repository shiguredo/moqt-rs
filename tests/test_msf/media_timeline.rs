use super::*;

#[test]
fn empty() {
    let tl = MsfMediaTimeline::new();
    let encoded = tl.encode();
    assert_eq!(encoded, b"[]");
    let decoded = MsfMediaTimeline::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded, tl);
}

#[test]
fn single_entry_vod_wallclock_zero() {
    // VOD の場合 wallclock_ms は 0
    let tl = MsfMediaTimeline(vec![MsfMediaTimelineEntry {
        pts_ms: 0,
        group_id: 0,
        object_id: 0,
        wallclock_ms: 0,
    }]);
    let encoded = tl.encode();
    let decoded = MsfMediaTimeline::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded, tl);
}

#[test]
fn entry_is_copy_eq_hash() {
    // MsfMediaTimelineEntry は全フィールド u64 のため Copy / Eq / Hash を実装する
    let entry = MsfMediaTimelineEntry {
        pts_ms: 0,
        group_id: 1,
        object_id: 2,
        wallclock_ms: 3,
    };
    // Copy による移動後も元の値が使える
    let copied = entry;
    assert_eq!(copied, entry);
    // Hash による集合への挿入と参照
    let mut set = std::collections::HashSet::new();
    set.insert(entry);
    assert!(set.contains(&copied));
}

#[test]
fn invalid_utf8_rejected() {
    let bytes = vec![0x80, 0x81];
    assert!(matches!(
        MsfMediaTimeline::decode(&bytes),
        Err(MessageError::InvalidCatalog(_))
    ));
}

#[test]
fn spec_example() {
    // draft-ietf-moq-msf-01 §7.1 (Media Timeline track payload) の例
    let json = b"[[0,[0,0],1759924158381],[2002,[1,0],1759924160383],[4004,[2,0],1759924162385]]";
    let tl = MsfMediaTimeline::decode(json).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(tl.0.len(), 3);
    assert_eq!(tl.0[2].pts_ms, 4004);
    assert_eq!(tl.0[2].group_id, 2);
    assert_eq!(tl.0[2].wallclock_ms, 1_759_924_162_385);

    let re_encoded = tl.encode();
    let re_decoded =
        MsfMediaTimeline::decode(&re_encoded).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(re_decoded, tl);
}
