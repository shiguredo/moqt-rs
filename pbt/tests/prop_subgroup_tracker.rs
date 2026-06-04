use pbt::common::test_runner;
use shiguredo_moqt::subgroup_tracker::{SubgroupStreamState, SubgroupTracker};

/// `SubgroupTracker` に対するオペレーション
enum Op {
    Open(u64, u64, u64),
    Fin(u64, u64, u64),
    Reset(u64, u64, u64, Option<u64>),
    Stop(u64, u64, u64),
}

/// オペレーションを生成する (キーは小さい範囲に限定し衝突を誘発する)
fn sample_op(ctx: &mut noprop::TestCaseContext) -> Op {
    let a = noprop::sample_usize_in(ctx, 0..=2) as u64;
    let g = noprop::sample_usize_in(ctx, 0..=2) as u64;
    let s = noprop::sample_usize_in(ctx, 0..=2) as u64;
    match noprop::sample_weighted_index(ctx, &[1, 1, 1, 1]) {
        0 => Op::Open(a, g, s),
        1 => Op::Fin(a, g, s),
        2 => {
            let sz = if noprop::sample_bool(ctx) {
                Some(noprop::sample_u64_in(ctx, 0..=1024))
            } else {
                None
            };
            Op::Reset(a, g, s, sz)
        }
        _ => Op::Stop(a, g, s),
    }
}

/// 任意のオペレーション列に対して、トラッカーの状態遷移が仕様通りであることを検証する
///
/// テスト側で独立した期待状態モデルを持ち、各操作の成否と `get()` 結果を突き合わせる。
/// - `open`: 未登録または再オープン可能状態 (Reset / StoppedByPeer) のみ成功する
/// - `mark_fin` / `mark_reset` / `mark_stop_sending`: 登録済みエントリのみ遷移させる
/// - FIN 最終 Object ID 不一致はエラーを返し状態を変えない
#[test]
fn tracker_state_matches_model_after_random_ops() -> noprop::TestResult {
    use std::collections::HashMap;
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Model {
        Open,
        ClosedFin { last_object_id: Option<u64> },
        Reset { reliable_size: Option<u64> },
        StoppedByPeer,
    }
    impl Model {
        fn can_reopen(&self) -> bool {
            matches!(self, Self::Reset { .. } | Self::StoppedByPeer)
        }
        fn as_state(&self) -> SubgroupStreamState {
            match self.clone() {
                Self::Open => SubgroupStreamState::Open,
                Self::ClosedFin { last_object_id } => {
                    SubgroupStreamState::ClosedFin { last_object_id }
                }
                Self::Reset { reliable_size } => SubgroupStreamState::Reset { reliable_size },
                Self::StoppedByPeer => SubgroupStreamState::StoppedByPeer,
            }
        }
    }
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut tracker = SubgroupTracker::new();
        let mut model: HashMap<(u64, u64, u64), Model> = HashMap::new();
        let mut fin_last: HashMap<(u64, u64, u64), Option<u64>> = HashMap::new();
        let op_count = noprop::sample_with_boundaries(
            ctx,
            &[0usize, 1, 40],
            noprop::Ratio::one_nth(4),
            |ctx| noprop::sample_usize_in(ctx, 0..=40),
        );
        for _ in 0..op_count {
            match sample_op(ctx) {
                Op::Open(a, g, s) => {
                    let key = (a, g, s);
                    let expect_ok = match model.get(&key) {
                        None => true,
                        Some(m) => m.can_reopen(),
                    };
                    let got = tracker.open(a, g, s);
                    assert_eq!(
                        got.is_ok(),
                        expect_ok,
                        "open の成否がモデルと一致すること: {key:?}"
                    );
                    if expect_ok {
                        model.insert(key, Model::Open);
                    }
                }
                Op::Fin(a, g, s) => {
                    let key = (a, g, s);
                    // 最終 Object ID もランダム化する (不一致エラーの経路を通すため)
                    let last = if noprop::sample_bool(ctx) {
                        Some(noprop::sample_u64_in(ctx, 0..=8))
                    } else {
                        None
                    };
                    let expect_err = matches!(fin_last.get(&key), Some(&prev) if prev != last);
                    let got = tracker.mark_fin(a, g, s, last);
                    assert_eq!(
                        got.is_err(),
                        expect_err,
                        "FIN 最終 Object 不一致の成否がモデルと一致すること: {key:?}"
                    );
                    if !expect_err {
                        fin_last.insert(key, last);
                        if model.contains_key(&key) {
                            model.insert(
                                key,
                                Model::ClosedFin {
                                    last_object_id: last,
                                },
                            );
                        }
                    }
                }
                Op::Reset(a, g, s, sz) => {
                    let key = (a, g, s);
                    tracker.mark_reset(a, g, s, sz);
                    if model.contains_key(&key) {
                        model.insert(key, Model::Reset { reliable_size: sz });
                    }
                }
                Op::Stop(a, g, s) => {
                    let key = (a, g, s);
                    tracker.mark_stop_sending(a, g, s);
                    if model.contains_key(&key) {
                        model.insert(key, Model::StoppedByPeer);
                    }
                }
            }
        }
        // 全キーの期待状態と get() 結果を突き合わせる
        for a in 0..=2 {
            for g in 0..=2 {
                for s in 0..=2 {
                    let key = (a, g, s);
                    match model.get(&key) {
                        None => assert_eq!(
                            tracker.get(a, g, s),
                            None,
                            "未 open キーは存在しないこと: {key:?}"
                        ),
                        Some(expected) => assert_eq!(
                            tracker.get(a, g, s),
                            Some(&expected.as_state()),
                            "状態がモデルと一致すること: {key:?}"
                        ),
                    }
                }
            }
        }
        Ok(())
    })?;
    Ok(())
}

// 再オープン可否 (FIN / RESET / STOP_SENDING) は状態機械の分岐がキー値に依存しないため
// 単体テスト (tests/test_subgroup_tracker.rs) が担う。ここではキー衝突を含むランダム操作列を
// 独立モデルと突き合わせる `tracker_state_matches_model_after_random_ops` のみを検証する。
