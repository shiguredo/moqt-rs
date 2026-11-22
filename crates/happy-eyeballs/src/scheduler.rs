//! 接続試行の開始時刻と勝者を決める Sans I/O 状態機械
//!
//! 時刻は呼び出し側が渡す単調増加のミリ秒だけを使う。状態機械は現在時刻を読まず、
//! ソケット、タイマー、タスクを所有しない。

use alloc::vec::Vec;
use core::time::Duration;

use crate::{Candidate, CandidateGroup, Config, next_attempt_delay, order_candidates};

/// 呼び出し側が渡す単調増加の時刻 (ミリ秒)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(u64);

impl Timestamp {
    /// ミリ秒から時刻を作る
    pub const fn from_millis(millis: u64) -> Self {
        Self(millis)
    }

    /// ミリ秒表現を返す
    pub const fn as_millis(self) -> u64 {
        self.0
    }

    /// Duration を加算する。オーバーフローする場合は `u64::MAX` に飽和させる
    fn saturating_add(self, duration: Duration) -> Self {
        let millis = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        Self(self.0.saturating_add(millis))
    }
}

/// 状態機械への入力
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// タイマーが満了した
    Timer {
        /// 現在時刻
        now: Timestamp,
    },
    /// 候補のトランスポート確立に成功した
    Succeeded {
        /// 成功した候補のインデックス
        index: usize,
    },
    /// 候補のトランスポート確立に失敗した
    Failed {
        /// 失敗した候補のインデックス
        index: usize,
        /// 現在時刻
        now: Timestamp,
    },
}

/// 状態機械からの出力
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action<C> {
    /// 候補の試行を開始する
    Start {
        /// 開始する候補のインデックス
        index: usize,
        /// 開始する候補
        candidate: Candidate<C>,
    },
    /// 次回開始時刻まで待つ
    WaitUntil {
        /// 次回開始時刻
        deadline: Timestamp,
    },
    /// 勝者が確定した。残りの試行を中断する
    Winner {
        /// 勝者のインデックス
        index: usize,
        /// 勝者の候補
        candidate: Candidate<C>,
    },
    /// すべての候補が失敗した
    Failed {
        /// 候補順で最も先頭の失敗のインデックス
        index: usize,
        /// その候補
        candidate: Candidate<C>,
    },
    /// 候補が 1 つも無い
    NoCandidates,
    /// 次に取る行動は無い。進行中の試行の結果を待つ
    None,
}

/// 状態機械のエラー
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// 開始前にイベントを受け取った
    NotStarted,
    /// 開始済みの状態機械をもう一度開始しようとした
    AlreadyStarted,
    /// 終了済みの状態機械へイベントを渡した
    AlreadyFinished,
    /// 候補のインデックスが範囲外
    InvalidIndex,
    /// 進行中でない候補の結果を受け取った
    NotInFlight,
}

/// 競争の状態
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// まだ開始していない
    NotStarted,
    /// 競争中
    Running,
    /// 勝者が確定した
    Won,
    /// すべて失敗した
    Failed,
}

/// 候補の試行状態
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttemptState {
    /// まだ開始していない
    Pending,
    /// 進行中
    InFlight,
    /// 成功した
    Succeeded,
    /// 失敗した
    Failed,
}

/// Happy Eyeballs v3 の接続試行スケジューラ
///
/// `C` は呼び出し側が決めたトランスポート種別である。状態機械は `C` の意味を解釈せず、
/// `CandidateGroup` の順序をトランスポートの優先順として扱う。
#[derive(Debug)]
pub struct Scheduler<C> {
    config: Config,
    candidates: Vec<Candidate<C>>,
    states: Vec<AttemptState>,
    phase: Phase,
    next_index: usize,
    in_flight: usize,
    next_start_at: Option<Timestamp>,
    last_start: Option<Timestamp>,
    best_failure: Option<usize>,
}

impl<C: Copy> Scheduler<C> {
    /// 候補グループと設定からスケジューラを作る
    ///
    /// 設定は [`Config::sanitized`] で丸めてから保持する。候補が空の場合は
    /// [`Self::start`] が [`Action::NoCandidates`] を返す。
    pub fn new(groups: &[CandidateGroup<C>], config: Config) -> Self {
        let config = config.sanitized();
        let candidates = order_candidates(groups, &config);
        let mut states = Vec::new();
        states.resize(candidates.len(), AttemptState::Pending);
        Self {
            config,
            candidates,
            states,
            phase: Phase::NotStarted,
            next_index: 0,
            in_flight: 0,
            next_start_at: None,
            last_start: None,
            best_failure: None,
        }
    }

    /// 競争を開始し、最初の候補の試行を返す
    ///
    /// # Errors
    ///
    /// すでに開始済みの場合は [`Error::AlreadyStarted`] を返す。
    pub fn start(&mut self, now: Timestamp) -> Result<Action<C>, Error> {
        if self.phase != Phase::NotStarted {
            return Err(Error::AlreadyStarted);
        }
        if self.candidates.is_empty() {
            self.phase = Phase::Failed;
            return Ok(Action::NoCandidates);
        }
        self.phase = Phase::Running;
        Ok(self
            .start_next(now)
            .expect("a non-empty candidate list must start a candidate"))
    }

    /// イベントを処理し、次に取る行動を返す
    ///
    /// # Errors
    ///
    /// 開始前、終了後、範囲外のインデックス、進行中でない候補の結果を受け取った場合は
    /// エラーになる。
    pub fn handle(&mut self, event: Event) -> Result<Action<C>, Error> {
        match self.phase {
            Phase::NotStarted => return Err(Error::NotStarted),
            Phase::Won | Phase::Failed => return Err(Error::AlreadyFinished),
            Phase::Running => {}
        }
        match event {
            Event::Timer { now } => self.on_timer(now),
            Event::Succeeded { index } => self.on_succeeded(index),
            Event::Failed { index, now } => self.on_failed(index, now),
        }
    }

    /// 次回開始時刻を返す
    ///
    /// `None` の場合は、進行中の試行の結果を待つか、競争が終了している。
    pub fn next_deadline(&self) -> Option<Timestamp> {
        self.next_start_at
    }

    /// 候補の総数を返す
    pub fn candidate_count(&self) -> usize {
        self.candidates.len()
    }

    /// 次に開始する候補を開始する
    fn start_next(&mut self, now: Timestamp) -> Option<Action<C>> {
        if self.next_index >= self.candidates.len() {
            self.next_start_at = None;
            return None;
        }
        let index = self.next_index;
        self.states[index] = AttemptState::InFlight;
        self.next_index += 1;
        self.in_flight += 1;
        self.last_start = Some(now);
        self.schedule_next(now);
        Some(Action::Start {
            index,
            candidate: self.candidates[index],
        })
    }

    /// 次の候補の開始時刻を設定する
    fn schedule_next(&mut self, now: Timestamp) {
        if self.next_index >= self.candidates.len() {
            self.next_start_at = None;
            return;
        }
        let delay = next_attempt_delay(self.in_flight, &self.config);
        self.next_start_at = Some(now.saturating_add(delay));
    }

    /// タイマー満了を処理する
    fn on_timer(&mut self, now: Timestamp) -> Result<Action<C>, Error> {
        let Some(deadline) = self.next_start_at else {
            return Ok(Action::None);
        };
        if now < deadline {
            return Ok(Action::WaitUntil { deadline });
        }
        match self.start_next(now) {
            Some(action) => Ok(action),
            None => Ok(Action::None),
        }
    }

    /// 試行の成功を処理する
    fn on_succeeded(&mut self, index: usize) -> Result<Action<C>, Error> {
        self.ensure_in_flight(index)?;
        self.states[index] = AttemptState::Succeeded;
        self.in_flight -= 1;
        self.phase = Phase::Won;
        self.next_start_at = None;
        Ok(Action::Winner {
            index,
            candidate: self.candidates[index],
        })
    }

    /// 試行の失敗を処理する
    fn on_failed(&mut self, index: usize, now: Timestamp) -> Result<Action<C>, Error> {
        self.ensure_in_flight(index)?;
        self.states[index] = AttemptState::Failed;
        self.in_flight -= 1;
        self.best_failure = Some(match self.best_failure {
            Some(best) => best.min(index),
            None => index,
        });

        if self.next_index >= self.candidates.len() && self.in_flight == 0 {
            self.phase = Phase::Failed;
            self.next_start_at = None;
            let best = self
                .best_failure
                .expect("at least one failed candidate must be recorded");
            return Ok(Action::Failed {
                index: best,
                candidate: self.candidates[best],
            });
        }

        if self.next_index < self.candidates.len() {
            self.accelerate_next_start(now);
            let deadline = self
                .next_start_at
                .expect("a pending candidate must have a start deadline");
            return Ok(Action::WaitUntil { deadline });
        }

        self.next_start_at = None;
        Ok(Action::None)
    }

    /// 失敗を観測したため、次の候補の開始を前倒しする
    fn accelerate_next_start(&mut self, now: Timestamp) {
        if self.next_index >= self.candidates.len() {
            return;
        }
        let Some(last_start) = self.last_start else {
            return;
        };
        let earliest = last_start.saturating_add(self.config.minimum_connection_attempt_delay);
        let when = earliest.max(now);
        self.next_start_at = Some(match self.next_start_at {
            Some(current) => current.min(when),
            None => when,
        });
    }

    /// 候補が進行中であることを確認する
    fn ensure_in_flight(&self, index: usize) -> Result<(), Error> {
        let Some(state) = self.states.get(index) else {
            return Err(Error::InvalidIndex);
        };
        if *state != AttemptState::InFlight {
            return Err(Error::NotInFlight);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use core::net::{IpAddr, Ipv4Addr};

    fn addr(port: u16) -> core::net::SocketAddr {
        core::net::SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    fn groups(count: u16) -> Vec<CandidateGroup<u8>> {
        let mut addrs = Vec::new();
        for port in 1..=count {
            addrs.push(addr(port));
        }
        vec![CandidateGroup {
            transport: 0u8,
            addrs,
        }]
    }

    fn config() -> Config {
        Config {
            connection_attempt_delay: Duration::from_millis(250),
            minimum_connection_attempt_delay: Duration::from_millis(100),
            maximum_connection_attempt_delay: Duration::from_secs(2),
            ..Config::default()
        }
    }

    /// 空の候補では開始時に NoCandidates を返す
    #[test]
    fn empty_candidates_return_no_candidates() {
        let mut scheduler = Scheduler::<u8>::new(&[], Config::default());
        assert_eq!(
            scheduler.start(Timestamp::from_millis(0)),
            Ok(Action::NoCandidates)
        );
    }

    /// 最初の候補を即座に開始し、次の候補をタイマーで開始する
    #[test]
    fn starts_first_candidate_and_then_timer_candidate() {
        let mut scheduler = Scheduler::new(&groups(2), config());
        let action = scheduler.start(Timestamp::from_millis(0)).expect("start");
        assert_eq!(
            action,
            Action::Start {
                index: 0,
                candidate: Candidate {
                    transport: 0,
                    addr: addr(1)
                }
            }
        );
        assert_eq!(scheduler.next_deadline(), Some(Timestamp::from_millis(250)));

        let action = scheduler
            .handle(Event::Timer {
                now: Timestamp::from_millis(250),
            })
            .expect("timer");
        assert_eq!(
            action,
            Action::Start {
                index: 1,
                candidate: Candidate {
                    transport: 0,
                    addr: addr(2)
                }
            }
        );
        assert_eq!(scheduler.next_deadline(), None);
    }

    /// 失敗を観測すると次の開始を最小遅延まで前倒しする
    #[test]
    fn failure_accelerates_next_start() {
        let mut scheduler = Scheduler::new(&groups(2), config());
        scheduler.start(Timestamp::from_millis(0)).expect("start");
        let action = scheduler
            .handle(Event::Failed {
                index: 0,
                now: Timestamp::from_millis(20),
            })
            .expect("failed");
        assert_eq!(
            action,
            Action::WaitUntil {
                deadline: Timestamp::from_millis(100)
            }
        );
        assert_eq!(scheduler.next_deadline(), Some(Timestamp::from_millis(100)));

        let action = scheduler
            .handle(Event::Timer {
                now: Timestamp::from_millis(100),
            })
            .expect("timer");
        assert_eq!(
            action,
            Action::Start {
                index: 1,
                candidate: Candidate {
                    transport: 0,
                    addr: addr(2)
                }
            }
        );
    }

    /// 最初の成功を勝者として返す
    #[test]
    fn first_success_wins() {
        let mut scheduler = Scheduler::new(&groups(2), config());
        scheduler.start(Timestamp::from_millis(0)).expect("start");
        let action = scheduler
            .handle(Event::Succeeded { index: 0 })
            .expect("succeeded");
        assert_eq!(
            action,
            Action::Winner {
                index: 0,
                candidate: Candidate {
                    transport: 0,
                    addr: addr(1)
                }
            }
        );
        assert_eq!(
            scheduler.handle(Event::Timer {
                now: Timestamp::from_millis(250)
            }),
            Err(Error::AlreadyFinished)
        );
    }

    /// 全候補の失敗では候補順で先頭の失敗を返す
    #[test]
    fn all_failed_returns_preferred_failure() {
        let mut scheduler = Scheduler::new(&groups(2), config());
        scheduler.start(Timestamp::from_millis(0)).expect("start");
        scheduler
            .handle(Event::Failed {
                index: 0,
                now: Timestamp::from_millis(20),
            })
            .expect("failed");
        scheduler
            .handle(Event::Timer {
                now: Timestamp::from_millis(100),
            })
            .expect("timer");
        let action = scheduler
            .handle(Event::Failed {
                index: 1,
                now: Timestamp::from_millis(110),
            })
            .expect("failed");
        assert_eq!(
            action,
            Action::Failed {
                index: 0,
                candidate: Candidate {
                    transport: 0,
                    addr: addr(1)
                }
            }
        );
    }

    /// 早すぎるタイマーは同じ時刻で待ち直す
    #[test]
    fn early_timer_returns_same_deadline() {
        let mut scheduler = Scheduler::new(&groups(2), config());
        scheduler.start(Timestamp::from_millis(0)).expect("start");
        let action = scheduler
            .handle(Event::Timer {
                now: Timestamp::from_millis(10),
            })
            .expect("timer");
        assert_eq!(
            action,
            Action::WaitUntil {
                deadline: Timestamp::from_millis(250)
            }
        );
    }

    /// 範囲外のインデックスはエラーにする
    #[test]
    fn invalid_index_is_rejected() {
        let mut scheduler = Scheduler::new(&groups(1), config());
        scheduler.start(Timestamp::from_millis(0)).expect("start");
        assert_eq!(
            scheduler.handle(Event::Succeeded { index: 1 }),
            Err(Error::InvalidIndex)
        );
    }

    /// 開始前のイベントはエラーにする
    #[test]
    fn event_before_start_is_rejected() {
        let mut scheduler = Scheduler::new(&groups(1), config());
        assert_eq!(
            scheduler.handle(Event::Succeeded { index: 0 }),
            Err(Error::NotStarted)
        );
    }

    /// 開始済みの状態機械を再度開始できない
    #[test]
    fn double_start_is_rejected() {
        let mut scheduler = Scheduler::new(&groups(1), config());
        scheduler.start(Timestamp::from_millis(0)).expect("start");
        assert_eq!(
            scheduler.start(Timestamp::from_millis(1)),
            Err(Error::AlreadyStarted)
        );
    }
}
