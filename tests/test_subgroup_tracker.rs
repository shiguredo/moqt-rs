/// SubgroupTracker の単体テスト
///
/// draft-ietf-moq-transport-21 §2.2 (Subgroups) / draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams) に基づく再オープン禁止検証の
/// 境界条件・エラーパスを PBT とは別に厳密化する。
use shiguredo_moqt::error::SESSION_PROTOCOL_VIOLATION;
use shiguredo_moqt::subgroup_tracker::{SubgroupStreamState, SubgroupTracker};

#[test]
fn open_then_fin_then_different_subgroup_ok() {
    let mut t = SubgroupTracker::new();
    t.open(1, 10, 0)
        .expect("テストフィクスチャの前提条件を満たす");
    t.mark_fin(1, 10, 0, Some(5))
        .expect("テストフィクスチャの前提条件を満たす");
    // 異なる subgroup_id は独立
    t.open(1, 10, 1)
        .expect("テストフィクスチャの前提条件を満たす");
    // 異なる group_id は独立
    t.open(1, 11, 0)
        .expect("テストフィクスチャの前提条件を満たす");
    // 異なる track_alias は独立
    t.open(2, 10, 0)
        .expect("テストフィクスチャの前提条件を満たす");
}

#[test]
fn reopen_after_fin_violation() {
    let mut t = SubgroupTracker::new();
    t.open(1, 10, 0)
        .expect("テストフィクスチャの前提条件を満たす");
    t.mark_fin(1, 10, 0, Some(3))
        .expect("テストフィクスチャの前提条件を満たす");
    let err = t.open(1, 10, 0).unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

#[test]
fn reopen_after_reset_allowed() {
    // draft-ietf-moq-transport-21 §2.2 (Subgroups): "unless one of the streams was reset
    // prematurely" — Reset からの再オープンは許可される
    let mut t = SubgroupTracker::new();
    t.open(1, 10, 0)
        .expect("テストフィクスチャの前提条件を満たす");
    t.mark_reset(1, 10, 0, Some(1024));
    t.open(1, 10, 0)
        .expect("Reset 後の再オープンは許可されること");
    // 再オープン後は Open 状態になっている
    assert_eq!(t.get(1, 10, 0), Some(&SubgroupStreamState::Open));
}

#[test]
fn reopen_after_stop_sending_allowed() {
    let mut t = SubgroupTracker::new();
    t.open(1, 10, 0)
        .expect("テストフィクスチャの前提条件を満たす");
    t.mark_stop_sending(1, 10, 0);
    // draft-ietf-moq-transport-21 Appendix A.3 (Since draft-ietf-moq-transport-17) #1583
    // (REQUEST_UPDATE forward 0→1): StoppedByPeer は can_reopen() → 再オープン可能
    t.open(1, 10, 0)
        .expect("テストフィクスチャの前提条件を満たす");
    // 再オープン後は Open 状態になっている
    assert_eq!(t.get(1, 10, 0), Some(&SubgroupStreamState::Open));
    // 再度終端後は通常どおり再オープン禁止
    t.mark_fin(1, 10, 0, Some(7))
        .expect("テストフィクスチャの前提条件を満たす");
    let err = t.open(1, 10, 0).unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

#[test]
fn concurrent_open_violation() {
    let mut t = SubgroupTracker::new();
    t.open(1, 10, 0)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = t.open(1, 10, 0).unwrap_err();
    assert_eq!(err.code, SESSION_PROTOCOL_VIOLATION);
}

#[test]
fn mark_without_open_is_noop() {
    let mut t = SubgroupTracker::new();
    // open 無しでの終端マークは何も起きない (エントリが作られない)
    t.mark_fin(1, 10, 0, None)
        .expect("テストフィクスチャの前提条件を満たす");
    t.mark_reset(1, 10, 0, None);
    t.mark_stop_sending(1, 10, 0);
    // open は成功する (entry が無いため)
    t.open(1, 10, 0)
        .expect("テストフィクスチャの前提条件を満たす");
}

#[test]
fn can_reopen_true_for_stopped_by_peer_and_reset() {
    assert!(!SubgroupStreamState::Open.can_reopen());
    assert!(
        !SubgroupStreamState::ClosedFin {
            last_object_id: None
        }
        .can_reopen()
    );
    // draft-ietf-moq-transport-21 §2.2: premature reset は再オープン可能
    assert!(
        SubgroupStreamState::Reset {
            reliable_size: None
        }
        .can_reopen()
    );
    assert!(SubgroupStreamState::StoppedByPeer.can_reopen());
}

#[test]
fn mark_stop_sending_sets_stopped_by_peer_flag() {
    let mut t = SubgroupTracker::new();
    t.open(1, 5, 3)
        .expect("テストフィクスチャの前提条件を満たす");
    t.mark_stop_sending(1, 5, 3);
    assert_eq!(t.get(1, 5, 3), Some(&SubgroupStreamState::StoppedByPeer));
}
