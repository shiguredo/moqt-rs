/// SubgroupTracker の単体テスト
///
/// draft-ietf-moq-transport-23 §2.2 (Subgroups) / draft-ietf-moq-transport-23 §11.3.2 (Closing Subgroup Streams) に基づく再オープン禁止検証の
/// 境界条件・エラーパスを PBT とは別に厳密化する。
use shiguredo_moqt::error::MessageError;
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
    assert!(
        matches!(err, MessageError::ProtocolViolation(_)),
        "再オープン禁止違反は §12.1 の条件ではなく ProtocolViolation になる: {err:?}"
    );
}

#[test]
fn reopen_after_reset_allowed() {
    // draft-ietf-moq-transport-23 §2.2 (Subgroups): "unless one of the streams was reset
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
    // draft-ietf-moq-transport-23 Appendix A.6 (Since draft-ietf-moq-transport-17) #1583
    // (REQUEST_UPDATE forward 0→1): StoppedByPeer は can_reopen() → 再オープン可能
    t.open(1, 10, 0)
        .expect("テストフィクスチャの前提条件を満たす");
    // 再オープン後は Open 状態になっている
    assert_eq!(t.get(1, 10, 0), Some(&SubgroupStreamState::Open));
    // 再度終端後は通常どおり再オープン禁止
    t.mark_fin(1, 10, 0, Some(7))
        .expect("テストフィクスチャの前提条件を満たす");
    let err = t.open(1, 10, 0).unwrap_err();
    assert!(
        matches!(err, MessageError::ProtocolViolation(_)),
        "再オープン禁止違反は §12.1 の条件ではなく ProtocolViolation になる: {err:?}"
    );
}

#[test]
fn concurrent_open_violation() {
    let mut t = SubgroupTracker::new();
    t.open(1, 10, 0)
        .expect("テストフィクスチャの前提条件を満たす");
    let err = t.open(1, 10, 0).unwrap_err();
    assert!(
        matches!(err, MessageError::ProtocolViolation(_)),
        "再オープン禁止違反は §12.1 の条件ではなく ProtocolViolation になる: {err:?}"
    );
}

#[test]
fn mark_without_open_is_noop() {
    let mut t = SubgroupTracker::new();
    // open 無しでの終端マークは状態エントリを作らない (FIN の最終 Object ID だけは記録する)
    t.mark_fin(1, 10, 0, None)
        .expect("テストフィクスチャの前提条件を満たす");
    t.mark_reset(1, 10, 0, None);
    t.mark_stop_sending(1, 10, 0);
    assert_eq!(t.get(1, 10, 0), None, "状態エントリは作られないこと");
    // open は成功する (状態エントリが無いため)
    t.open(1, 10, 0)
        .expect("テストフィクスチャの前提条件を満たす");
}

/// 状態エントリの無いキーの FIN は、状態エントリを作らずに最終 Object ID を記録すること
///
/// draft-ietf-moq-transport-23 §12.1 (Malformed Tracks) 条件 2 (FIN 済み Subgroup の最終 Object
/// 超過) と条件 3 (複数 FIN の最終 Object 不一致) の検出には、状態エントリが無くても最終
/// Object ID の記録が要る。
#[test]
fn mark_fin_on_unknown_key_records_only_final_object_id() {
    let mut t = SubgroupTracker::new();
    t.mark_fin(1, 10, 0, Some(5))
        .expect("状態エントリの無いキーの FIN は成功すること");
    assert_eq!(t.get(1, 10, 0), None, "状態エントリは作られないこと");
    assert!(
        t.check_object_after_fin(1, 10, 0, 6).is_some(),
        "記録した最終 Object ID を超える Object は条件 2 で Malformed になること"
    );
    assert!(
        t.check_object_after_fin(1, 10, 0, 5).is_none(),
        "最終 Object ID 以下の Object は条件 2 に当たらないこと"
    );
    // 過去の FIN と最終 Object ID が異なる FIN は、状態エントリが無くても条件 3 で拒否する
    assert!(
        matches!(
            t.mark_fin(1, 10, 0, Some(7)),
            Err(MessageError::MalformedTrack(_))
        ),
        "最終 Object ID の異なる FIN は MalformedTrack になること"
    );
    assert_eq!(t.get(1, 10, 0), None, "拒否時も状態エントリを作らないこと");
    assert!(
        t.check_object_after_fin(1, 10, 0, 6).is_some(),
        "拒否時は最終 Object ID の記録を更新しないこと"
    );
    // 状態エントリが無いため open は成功する
    t.open(1, 10, 0)
        .expect("状態エントリの無いキーは open できること");
}

/// FIN の後に RESET を記録しても ClosedFin のままで、再オープンを許さないこと
///
/// draft-ietf-moq-transport-23 §11.3.2 (Closing Subgroup Streams): "If the application
/// receives all data on the stream and the FIN, it can ignore any subsequent reset."
/// 正常終了の事実を reset で上書きすると、§2.2 (Subgroups) が禁じる FIN 後の再オープンと
/// 最終 Object ID 以下の Object の再受理を許してしまう。
#[test]
fn reset_after_fin_keeps_closed_fin() {
    let mut t = SubgroupTracker::new();
    t.open(1, 10, 0)
        .expect("テストフィクスチャの前提条件を満たす");
    t.mark_fin(1, 10, 0, Some(5))
        .expect("テストフィクスチャの前提条件を満たす");
    t.mark_reset(1, 10, 0, None);
    t.mark_reset(1, 10, 0, Some(64));
    assert_eq!(
        t.get(1, 10, 0),
        Some(&SubgroupStreamState::ClosedFin),
        "FIN 後の RESET は ClosedFin を上書きしないこと"
    );
    assert!(
        matches!(t.open(1, 10, 0), Err(MessageError::ProtocolViolation(_))),
        "FIN 後の RESET を挟んでも再オープンは拒否すること"
    );
}

/// FIN の後に STOP_SENDING を記録しても ClosedFin のままで、再オープンを許さないこと
#[test]
fn stop_sending_after_fin_keeps_closed_fin() {
    let mut t = SubgroupTracker::new();
    t.open(1, 10, 0)
        .expect("テストフィクスチャの前提条件を満たす");
    t.mark_fin(1, 10, 0, Some(5))
        .expect("テストフィクスチャの前提条件を満たす");
    t.mark_stop_sending(1, 10, 0);
    assert_eq!(
        t.get(1, 10, 0),
        Some(&SubgroupStreamState::ClosedFin),
        "FIN 後の STOP_SENDING は ClosedFin を上書きしないこと"
    );
    assert!(
        matches!(t.open(1, 10, 0), Err(MessageError::ProtocolViolation(_))),
        "FIN 後の STOP_SENDING を挟んでも再オープンは拒否すること"
    );
}

/// RESET と STOP_SENDING の相互の上書きは後勝ちであること
#[test]
fn reset_and_stop_sending_overwrite_each_other() {
    let mut t = SubgroupTracker::new();
    t.open(1, 10, 0)
        .expect("テストフィクスチャの前提条件を満たす");
    t.mark_stop_sending(1, 10, 0);
    t.mark_reset(1, 10, 0, Some(8));
    assert_eq!(
        t.get(1, 10, 0),
        Some(&SubgroupStreamState::Reset {
            reliable_size: Some(8)
        }),
        "STOP_SENDING の後の RESET は Reset にすること"
    );
    t.mark_stop_sending(1, 10, 0);
    assert_eq!(
        t.get(1, 10, 0),
        Some(&SubgroupStreamState::StoppedByPeer),
        "RESET の後の STOP_SENDING は StoppedByPeer にすること"
    );
}

#[test]
fn can_reopen_true_for_stopped_by_peer_and_reset() {
    assert!(!SubgroupStreamState::Open.can_reopen());
    assert!(!SubgroupStreamState::ClosedFin.can_reopen());
    // draft-ietf-moq-transport-23 §2.2: premature reset は再オープン可能
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
