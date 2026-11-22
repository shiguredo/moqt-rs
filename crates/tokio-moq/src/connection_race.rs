//! Happy Eyeballs v3 の Sans I/O スケジューラを Tokio の接続試行へ接続するアダプタ
//!
//! 状態機械そのものは `happy-eyeballs` クレートにあり、このモジュールは名前解決済みの
//! 候補を独立した接続 future として起動し、タイマーと試行結果を状態機械へ渡す。

use std::fmt;
use std::future::Future;
use std::time::Duration;

use happy_eyeballs::scheduler::{Action, Event, Scheduler, Timestamp};
use happy_eyeballs::{Candidate, CandidateGroup, Config};
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep_until};

use crate::Transport;

/// 競争が失敗した理由
#[derive(Debug)]
pub(crate) enum RaceFailure<E> {
    /// 試す候補が 1 つも無かった
    NoCandidates,
    /// すべての試行が失敗した (候補順で最も先頭の失敗)
    Attempt(E),
    /// 接続試行のタスクが panic などで終了した
    Task(tokio::task::JoinError),
    /// 状態機械とアダプタの間の不整合 (実装バグ)
    Internal(&'static str),
}

impl<E: fmt::Display> fmt::Display for RaceFailure<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoCandidates => write!(f, "no connection candidates"),
            Self::Attempt(error) => write!(f, "{error}"),
            Self::Task(error) => write!(f, "connection attempt task failed: {error}"),
            Self::Internal(reason) => write!(f, "connection scheduler error: {reason}"),
        }
    }
}

/// 候補を Happy Eyeballs の手順で競争させる
///
/// `start` は候補ごとに 1 回呼ばれ、その候補の接続確立 future を返す。返された future は
/// `monitor` で計測した独立タスクで駆動する。いずれかの future が成功した時点で、残りの
/// タスクを中断し、中断したタスクの drop が完了するまで待ってから成功値を返す。中断した
/// タスクがすでに成功していた場合は `cleanup` で接続を破棄する。
pub(crate) async fn race<T, E, F, Fut, C>(
    groups: Vec<CandidateGroup<Transport>>,
    config: Config,
    monitor: &tokio_metrics::TaskMonitor,
    mut start: F,
    mut cleanup: C,
) -> Result<T, RaceFailure<E>>
where
    E: fmt::Display + Send + 'static,
    F: FnMut(Candidate<Transport>) -> Fut,
    Fut: Future<Output = Result<T, E>> + Send + 'static,
    C: FnMut(T),
    T: Send + 'static,
{
    let mut scheduler = Scheduler::new(&groups, config);
    let started_at = Instant::now();
    let mut errors: Vec<Option<E>> = Vec::new();
    for _ in 0..scheduler.candidate_count() {
        errors.push(None);
    }

    let mut action = match scheduler.start(Timestamp::from_millis(0)) {
        Ok(action) => action,
        Err(_) => return Err(RaceFailure::Internal("failed to start scheduler")),
    };
    let mut deadline = scheduler.next_deadline();
    let mut tasks: JoinSet<(usize, Candidate<Transport>, Result<T, E>)> = JoinSet::new();

    loop {
        // 前倒しや開始時刻の到来で、待たずにタイマーを処理できる場合
        if let Some(at) = deadline {
            let now = now(started_at);
            if at <= now {
                action = match scheduler.handle(Event::Timer { now }) {
                    Ok(action) => action,
                    Err(error) => {
                        cancel_all(&mut tasks, &mut cleanup).await;
                        return Err(RaceFailure::Internal(schedule_error_reason(error)));
                    }
                };
                deadline = scheduler.next_deadline();
                continue;
            }
        }

        match action {
            Action::Start { index, candidate } => {
                spawn_attempt(&mut tasks, &mut start, monitor, index, candidate);
                action = Action::None;
                deadline = scheduler.next_deadline();
                continue;
            }
            Action::WaitUntil { deadline: at } => {
                deadline = Some(at);
                action = Action::None;
                continue;
            }
            Action::None => {
                deadline = scheduler.next_deadline();
            }
            Action::Winner { .. } => {
                cancel_all(&mut tasks, &mut cleanup).await;
                return Err(RaceFailure::Internal("winner arrived without a value"));
            }
            Action::Failed { index, .. } => {
                let error = match errors.get_mut(index).and_then(Option::take) {
                    Some(error) => error,
                    None => {
                        cancel_all(&mut tasks, &mut cleanup).await;
                        return Err(RaceFailure::Internal("missing failure error"));
                    }
                };
                cancel_all(&mut tasks, &mut cleanup).await;
                return Err(RaceFailure::Attempt(error));
            }
            Action::NoCandidates => {
                return Err(RaceFailure::NoCandidates);
            }
        }

        if tasks.is_empty() && deadline.is_none() {
            return Err(RaceFailure::Internal("scheduler has no work"));
        }

        let timer =
            deadline.and_then(|at| started_at.checked_add(Duration::from_millis(at.as_millis())));
        if deadline.is_some() && timer.is_none() {
            cancel_all(&mut tasks, &mut cleanup).await;
            return Err(RaceFailure::Internal("timer deadline overflow"));
        }

        tokio::select! {
            biased;
            joined = tasks.join_next(), if !tasks.is_empty() => {
                let Some(joined) = joined else {
                    continue;
                };
                match joined {
                    Ok((index, _candidate, Ok(value))) => {
                        let action = match scheduler.handle(Event::Succeeded { index }) {
                            Ok(action) => action,
                            Err(error) => {
                                cancel_all(&mut tasks, &mut cleanup).await;
                                return Err(RaceFailure::Internal(schedule_error_reason(error)));
                            }
                        };
                        match action {
                            Action::Winner { .. } => {
                                cancel_all(&mut tasks, &mut cleanup).await;
                                return Ok(value);
                            }
                            _ => {
                                cancel_all(&mut tasks, &mut cleanup).await;
                                return Err(RaceFailure::Internal(
                                    "successful candidate was not accepted",
                                ));
                            }
                        }
                    }
                    Ok((index, candidate, Err(error))) => {
                        tracing::warn!(
                            transport = candidate.transport.as_str(),
                            addr = %candidate.addr,
                            error = %error,
                            "Happy Eyeballs: connection attempt failed"
                        );
                        if let Some(slot) = errors.get_mut(index) {
                            *slot = Some(error);
                        } else {
                            cancel_all(&mut tasks, &mut cleanup).await;
                            return Err(RaceFailure::Internal("failure index out of range"));
                        }
                        action = match scheduler.handle(Event::Failed {
                            index,
                            now: now(started_at),
                        }) {
                            Ok(action) => action,
                            Err(error) => {
                                cancel_all(&mut tasks, &mut cleanup).await;
                                return Err(RaceFailure::Internal(schedule_error_reason(error)));
                            }
                        };
                        deadline = scheduler.next_deadline();
                    }
                    Err(error) => {
                        tracing::error!(
                            error = %error,
                            "Happy Eyeballs: connection attempt task failed"
                        );
                        cancel_all(&mut tasks, &mut cleanup).await;
                        return Err(RaceFailure::Task(error));
                    }
                }
            }
            _ = async {
                match timer {
                    Some(at) => sleep_until(at).await,
                    None => std::future::pending().await,
                }
            }, if timer.is_some() => {
                action = match scheduler.handle(Event::Timer { now: now(started_at) }) {
                    Ok(action) => action,
                    Err(error) => {
                        cancel_all(&mut tasks, &mut cleanup).await;
                        return Err(RaceFailure::Internal(schedule_error_reason(error)));
                    }
                };
                deadline = scheduler.next_deadline();
            }
        }
    }
}

/// 現在時刻を状態機械の時刻へ変換する
fn now(started_at: Instant) -> Timestamp {
    let millis = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    Timestamp::from_millis(millis)
}

/// 状態機械のエラーを固定の理由へ変換する
fn schedule_error_reason(error: happy_eyeballs::scheduler::Error) -> &'static str {
    match error {
        happy_eyeballs::scheduler::Error::NotStarted => "scheduler is not started",
        happy_eyeballs::scheduler::Error::AlreadyStarted => "scheduler is already started",
        happy_eyeballs::scheduler::Error::AlreadyFinished => "scheduler is already finished",
        happy_eyeballs::scheduler::Error::InvalidIndex => "candidate index is out of range",
        happy_eyeballs::scheduler::Error::NotInFlight => "candidate is not in flight",
    }
}

/// 候補の接続確立 future を独立したタスクとして開始する
fn spawn_attempt<T, E, F, Fut>(
    tasks: &mut JoinSet<(usize, Candidate<Transport>, Result<T, E>)>,
    start: &mut F,
    monitor: &tokio_metrics::TaskMonitor,
    index: usize,
    candidate: Candidate<Transport>,
) where
    E: Send + 'static,
    F: FnMut(Candidate<Transport>) -> Fut,
    Fut: Future<Output = Result<T, E>> + Send + 'static,
    T: Send + 'static,
{
    tracing::info!(
        transport = candidate.transport.as_str(),
        addr = %candidate.addr,
        "Happy Eyeballs: starting connection attempt"
    );
    // start をタスクの外で呼ぶことで、start 自体が借用を保持していても future を
    // 'static にできる。候補ごとに独立した future を作る
    let attempt = start(candidate);
    let monitor = monitor.clone();
    tasks.spawn(monitor.instrument(async move { (index, candidate, attempt.await) }));
}

/// 残っている接続試行を中断し、drop が完了するまで待つ
///
/// 中断時点ですでに成功していた候補は `cleanup` で接続を閉じる。
async fn cancel_all<T, E, C>(
    tasks: &mut JoinSet<(usize, Candidate<Transport>, Result<T, E>)>,
    cleanup: &mut C,
) -> usize
where
    C: FnMut(T),
    E: Send + 'static,
    T: Send + 'static,
{
    let cancelled = tasks.len();
    tasks.abort_all();
    while let Some(joined) = tasks.join_next().await {
        if let Ok((_index, _candidate, Ok(value))) = joined {
            cleanup(value);
        }
    }
    cancelled
}
