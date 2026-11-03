//! `GroupSwitchGate` の保留判定の PBT
//!
//! 映像は Group ごとに別の Subgroup ストリームで届く (draft-ietf-moq-transport-23
//! §2.2 (Subgroups)) ため、前の Group の stream が終わる前に次の Group の Object が届く。
//! そのとき次の Group を保留し、前の Group の stream の終端か 50 ms の期限で解除する。
//!
//! 検証する性質は次の 4 つである。
//!
//! - 配信済みの最大 Group 以上で、判定する Group より小さい Group の開いている stream が
//!   ある間は、期限前の `Deliver` が起きない
//! - 保留の期限が最初の `Hold` の時刻 + 50 ms を超えない
//! - 同じ Group の保留中に `Hold` の期限が変わらない
//! - `Deliver` を返した Group 以下の `admit` は `Hold` にならない (配信済みの最大が単調で
//!   あることを `admit` の振る舞いで観測する)
//!
//! 期待値は実装とは独立に、事象列 (stream の開閉・世代の更新・配送の観測) だけから数える。
//! あわせて、世代の異なる呼び出しが状態を変えないこと (session 移行の直前に世代を進め、
//! 止めきれなかった旧い stream task の呼び出しを無視する) も確かめる。
//!
//! 判定の分岐ごとに到達を数え、どの性質も空振りしていないことを確かめる。

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use pbt::common::test_runner;
use shiguredo_moqt::group_switch_gate::{
    GROUP_SWITCH_HOLD_US, GroupSwitchDecision, GroupSwitchGate,
};

/// 生成する Group 数の境界
const GROUP_COUNT_BOUNDARIES: [usize; 3] = [1, 2, 4];

/// 1 Group の stream 数の境界 (1 本 / 2 本)
const STREAM_COUNT_BOUNDARIES: [usize; 2] = [1, 2];

/// 事象の時間の刻み (マイクロ秒) の最大
///
/// 保留の期限 (50 ms) の前後をまたぐように、刻みを 0〜30 ms で生成する。
const MAX_TIME_STEP_US: u64 = 30_000;

/// 境界値を選ぶ確率 (1/4)。残りは一様に生成する
fn boundary_ratio() -> noprop::Ratio {
    noprop::Ratio::one_nth(4)
}

/// gate に与える 1 つの事象
#[derive(Debug, Clone, Copy)]
enum Event {
    /// stream を開いた
    Begin {
        /// 世代
        generation: u64,
        /// Group ID
        group_id: u64,
        /// stream の識別子
        stream_id: u64,
    },
    /// stream が終端した
    End {
        /// 世代
        generation: u64,
        /// Group ID
        group_id: u64,
        /// stream の識別子
        stream_id: u64,
    },
    /// Object の判定
    Admit {
        /// 世代
        generation: u64,
        /// Object の Group ID
        group_id: u64,
        /// 現在時刻 (マイクロ秒)
        now_us: i64,
    },
    /// 購読を張り直し、世代を進めて状態を初期化した
    Reset {
        /// 新しい世代
        generation: u64,
    },
}

/// 事象列から独立に数える観測状態
///
/// 実装の判定 (Hold / Deliver) ではなく、事象列と判定結果だけから更新する。実装と同じ規則を
/// 書き写すと、規則自体の誤りを検出できなくなるためである。
#[derive(Default)]
struct Observed {
    /// 現在の世代
    generation: u64,
    /// 開いている stream (`(group_id, stream_id)`)
    open_streams: BTreeSet<(u64, u64)>,
    /// `Deliver` を返した最大の Group
    delivered_max: Option<u64>,
}

impl Observed {
    /// stream の開閉と世代の更新を反映する
    fn apply(&mut self, event: Event) {
        match event {
            Event::Begin {
                generation,
                group_id,
                stream_id,
            } if generation == self.generation => {
                self.open_streams.insert((group_id, stream_id));
            }
            Event::End {
                generation,
                group_id,
                stream_id,
            } if generation == self.generation => {
                self.open_streams.remove(&(group_id, stream_id));
            }
            Event::Reset { generation } => {
                self.generation = generation;
                self.open_streams.clear();
                // 新しい購読では配信済みの最大も消える
                self.delivered_max = None;
            }
            _ => {}
        }
    }

    /// 保留の対象になる stream の数
    ///
    /// 配信済みの最大 Group 以上で、`group_id` より小さい Group の stream を数える。
    fn blocking_stream_count(&self, group_id: u64) -> usize {
        let Some(delivered_max) = self.delivered_max else {
            // まだ 1 つも配送していない場合は待つ対象が無い
            return 0;
        };
        if group_id <= delivered_max {
            // 配信済み以下の Group は保留しない。範囲を取ると開始が終端を超える
            return 0;
        }
        self.open_streams
            .range((delivered_max, 0)..(group_id, 0))
            .count()
    }
}

/// 1 つの Group ぶんの事象列を作る
///
/// stream を開き、その Group の Object を判定し、次の Group の Object を前の stream が
/// 開いたまま判定し、stream を 1 本ずつ終端する。保留と解除の両方が起きる形にする。
fn sample_group_events(
    ctx: &mut noprop::TestCaseContext,
    generation: u64,
    group_id: u64,
    next_group_id: u64,
    now_us: &mut i64,
) -> Vec<Event> {
    let stream_count =
        noprop::sample_with_boundaries(ctx, &STREAM_COUNT_BOUNDARIES, boundary_ratio(), |ctx| {
            noprop::sample_usize_in(ctx, STREAM_COUNT_BOUNDARIES[0]..=STREAM_COUNT_BOUNDARIES[1])
        });
    let mut events = Vec::new();
    for stream_id in 0..stream_count as u64 {
        events.push(Event::Begin {
            generation,
            group_id,
            stream_id,
        });
    }
    // この Group の Object を判定する。最初の判定で配信済みの最大がこの Group になる
    for _ in 0..noprop::sample_usize_in(ctx, 1..=2) {
        *now_us += noprop::sample_u64_in(ctx, 0..=MAX_TIME_STEP_US) as i64;
        events.push(Event::Admit {
            generation,
            group_id,
            now_us: *now_us,
        });
    }
    // 前の Group の stream が開いている間に、次の Group の Object を判定する
    *now_us += noprop::sample_u64_in(ctx, 0..=MAX_TIME_STEP_US) as i64;
    events.push(Event::Admit {
        generation,
        group_id: next_group_id,
        now_us: *now_us,
    });
    // 前の Group の stream を開いたまま時間を進め、期限切れで解除される経路も作る
    for _ in 0..noprop::sample_usize_in(ctx, 0..=2) {
        *now_us += noprop::sample_u64_in(ctx, 0..=MAX_TIME_STEP_US) as i64;
        events.push(Event::Admit {
            generation,
            group_id: next_group_id,
            now_us: *now_us,
        });
    }
    // stream を 1 本ずつ終端し、そのつど次の Group を判定する
    for stream_id in 0..stream_count as u64 {
        *now_us += noprop::sample_u64_in(ctx, 0..=MAX_TIME_STEP_US) as i64;
        events.push(Event::End {
            generation,
            group_id,
            stream_id,
        });
        events.push(Event::Admit {
            generation,
            group_id: next_group_id,
            now_us: *now_us,
        });
    }
    events
}

/// 事象列を作る
///
/// 途中で session 移行に相当する `reset` を挟み、旧世代の呼び出しも混ぜる。
fn sample_events(ctx: &mut noprop::TestCaseContext) -> Vec<Event> {
    let group_count =
        noprop::sample_with_boundaries(ctx, &GROUP_COUNT_BOUNDARIES, boundary_ratio(), |ctx| {
            noprop::sample_usize_in(ctx, GROUP_COUNT_BOUNDARIES[0]..=GROUP_COUNT_BOUNDARIES[2])
        });
    let mut events = Vec::new();
    let mut now_us = 0i64;
    for group_id in 0..group_count as u64 {
        let next_group_id = group_id + noprop::sample_u64_in(ctx, 1..=2);
        events.extend(sample_group_events(
            ctx,
            0,
            group_id,
            next_group_id,
            &mut now_us,
        ));
        // 途中で世代を進める。reset のあとに旧世代の呼び出しを混ぜ、無視されることを見る
        if noprop::sample_bool(ctx) && group_id + 1 < group_count as u64 {
            events.push(Event::Reset { generation: 1 });
            now_us += noprop::sample_u64_in(ctx, 0..=MAX_TIME_STEP_US) as i64;
            events.push(Event::Begin {
                generation: 0,
                group_id: group_id + 1,
                stream_id: 99,
            });
            events.push(Event::Admit {
                generation: 0,
                group_id: group_id + 2,
                now_us,
            });
            events.push(Event::End {
                generation: 0,
                group_id: group_id + 1,
                stream_id: 99,
            });
            // 以降は新しい世代で進める
            events.extend(sample_group_events(
                ctx,
                1,
                group_id + 1,
                group_id + 3,
                &mut now_us,
            ));
            break;
        }
    }
    events
}

/// 判定の分岐ごとの到達数
#[derive(Default)]
struct Coverage {
    /// `Hold` を返した回数
    hold: usize,
    /// 期限切れで `Deliver` になった回数
    expire_deliver: usize,
    /// 同じ Group の保留中にもう一度 `Hold` を返した回数
    hold_again: usize,
    /// 世代が異なるため無視した `Admit` の回数
    stale_admit: usize,
    /// 保留の対象の stream が 2 本以上あった回数
    multi_stream_hold: usize,
}

/// 事象列を gate に通し、保留の規則を確かめる
#[test]
fn gate_decisions_follow_the_hold_rules() -> noprop::TestResult {
    let mut runner = test_runner()?;
    let coverage = RefCell::new(Coverage::default());
    runner.run(256, |ctx| {
        let events = sample_events(ctx);
        let mut gate = GroupSwitchGate::new();
        // 実装とは独立に、事象列と判定結果だけから数える観測状態
        let mut observed = Observed::default();
        // Group ごとの最初の Hold の時刻と期限
        let mut first_hold_at: BTreeMap<u64, i64> = BTreeMap::new();
        let mut first_deadline: BTreeMap<u64, i64> = BTreeMap::new();
        for event in &events {
            let Event::Admit {
                generation,
                group_id,
                now_us,
            } = *event
            else {
                // 判定を伴わない事象は、gate と観測状態の両方へ反映する
                observed.apply(*event);
                match *event {
                    Event::Begin {
                        generation,
                        group_id,
                        stream_id,
                    } => gate.begin_stream(generation, group_id, stream_id),
                    Event::End {
                        generation,
                        group_id,
                        stream_id,
                    } => gate.end_stream(generation, group_id, stream_id),
                    Event::Reset { generation } => gate.reset(generation),
                    Event::Admit { .. } => unreachable!("Admit は上の else で処理済み"),
                }
                continue;
            };
            // 判定の前に、独立に数えた保留対象の stream 数を控える
            let blocking_stream_count = observed.blocking_stream_count(group_id);
            let delivered_max_before = observed.delivered_max;
            let actual = gate.admit(generation, group_id, now_us);
            // 旧世代の admit は gate の状態を変えないため、観測値も進めない
            if actual == GroupSwitchDecision::Deliver && generation == observed.generation {
                observed.delivered_max =
                    Some(delivered_max_before.map_or(group_id, |max| max.max(group_id)));
            }

            // 性質 1 の前半: 保留の対象の stream が開いていて、まだ保留していない Group の
            // 最初の admit は必ず Hold になる
            if generation == observed.generation
                && blocking_stream_count > 0
                && !first_deadline.contains_key(&group_id)
            {
                assert!(
                    matches!(actual, GroupSwitchDecision::Hold { .. }),
                    "保留の対象の stream が開いている間は最初の admit で保留すること: \
                     event={event:?} blocking={blocking_stream_count}"
                );
            }

            // 性質 2 と 3: 期限は最初の Hold の時刻 + 50 ms で、同じ Group の保留中は変わらない
            if let GroupSwitchDecision::Hold { release_at_us } = actual {
                coverage.borrow_mut().hold += 1;
                let first = *first_hold_at.entry(group_id).or_insert(now_us);
                assert_eq!(
                    release_at_us,
                    first + GROUP_SWITCH_HOLD_US,
                    "保留の期限は最初の Hold の時刻 + 50 ms であること: \
                     event={event:?} first={first}"
                );
                let known = *first_deadline.entry(group_id).or_insert(release_at_us);
                assert_eq!(
                    release_at_us, known,
                    "同じ Group の保留中は期限が変わらないこと: event={event:?}"
                );
                if first < now_us {
                    coverage.borrow_mut().hold_again += 1;
                }
                if blocking_stream_count >= 2 {
                    coverage.borrow_mut().multi_stream_hold += 1;
                }
            }

            // 性質 1: 保留すべき stream が開いている間は、期限前の Deliver が起きない
            if let GroupSwitchDecision::Deliver = actual
                && blocking_stream_count > 0
                && let Some(deadline) = first_deadline.get(&group_id)
            {
                assert!(
                    now_us >= *deadline,
                    "保留の対象の stream が開いている間は期限前の Deliver を返さないこと: \
                     event={event:?} deadline={deadline}"
                );
                coverage.borrow_mut().expire_deliver += 1;
            }

            // 性質 4: Deliver を返した Group 以下の admit は Hold にならない
            if let GroupSwitchDecision::Hold { .. } = actual
                && let Some(delivered_max) = delivered_max_before
            {
                assert!(
                    group_id > delivered_max,
                    "配信済みの最大以下の admit は Hold にならないこと: \
                     event={event:?} delivered_max={delivered_max}"
                );
            }

            // 世代が異なる admit は状態を変えない
            if generation != observed.generation {
                coverage.borrow_mut().stale_admit += 1;
            }
        }
        Ok(())
    })?;
    // 分岐の到達を確かめ、性質が空振りしていないことを保証する
    let coverage = coverage.into_inner();
    assert!(coverage.hold > 0, "Hold の判定が観測されなかった\n{runner}");
    assert!(
        coverage.hold_again > 0,
        "同じ Group の保留中にもう一度 Hold を返す判定が観測されなかった\n{runner}"
    );
    assert!(
        coverage.expire_deliver > 0,
        "期限切れで Deliver になる判定が観測されなかった\n{runner}"
    );
    assert!(
        coverage.stale_admit > 0,
        "世代が異なるため無視する admit が観測されなかった\n{runner}"
    );
    assert!(
        coverage.multi_stream_hold > 0,
        "1 Group に 2 本以上の stream がある保留が観測されなかった\n{runner}"
    );
    Ok(())
}
