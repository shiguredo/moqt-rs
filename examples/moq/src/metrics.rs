//! tokio-metrics のタスクメトリクスを定期的にログ出力するヘルパー

use std::time::Duration;

/// タスクメトリクスのログ出力間隔
pub const TASK_METRICS_LOG_INTERVAL: Duration = Duration::from_secs(5);

/// `TaskMonitor` のメトリクスを一定間隔でログ出力するタスクを起動する。
///
/// `TaskMonitor::intervals()` が返す `TaskIntervals` は終端のない iterator で、
/// `next()` は現在の interval のメトリクスを即座に返す。`collect()` すると
/// 永久に回り続けて Vec が伸び続け、CPU とメモリを食い尽くす (moqt-rs 0101)。
/// そのため 1 件ずつ取り出してログ出力する。
pub fn spawn_task_metrics_logger(task_monitor: tokio_metrics::TaskMonitor) {
    tokio::spawn(async move {
        let mut intervals = task_monitor.intervals();
        loop {
            if let Some(m) = intervals.next() {
                tracing::info!(
                    "Task metrics: instrumented={}, dropped={}, first_poll={}, total_poll={}, idle_μs={}, scheduled_μs={}",
                    m.instrumented_count,
                    m.dropped_count,
                    m.first_poll_count,
                    m.total_poll_count,
                    m.total_idle_duration.as_micros(),
                    m.total_scheduled_duration.as_micros(),
                );
            }
            tokio::time::sleep(TASK_METRICS_LOG_INTERVAL).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// `TaskIntervals` が終端のない iterator であることを確認する。
    /// この性質があるため `collect()` してはいけない (moqt-rs 0101)
    #[test]
    fn test_task_intervals_never_ends() {
        let monitor = tokio_metrics::TaskMonitor::new();
        let mut intervals = monitor.intervals();
        for i in 0..1000 {
            assert!(
                intervals.next().is_some(),
                "{i} 回目の next() が None を返した"
            );
        }
    }

    /// `next()` が待ち合わせずに即座に値を返すことを確認する。
    /// 待ち合わせるなら 1 秒で 1000 回は進まない
    #[test]
    fn test_task_intervals_next_returns_immediately() {
        let monitor = tokio_metrics::TaskMonitor::new();
        let mut intervals = monitor.intervals();
        let start = Instant::now();
        for _ in 0..1000 {
            intervals.next();
        }
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "next() が待ち合わせている (経過 {:?})",
            start.elapsed()
        );
    }

    /// ログ出力タスクを起動しても interval の取得が 1 件ずつ進むことを確認する。
    /// `collect()` を使う実装だとここで返ってこない
    #[tokio::test]
    async fn test_spawn_task_metrics_logger_starts() {
        let monitor = tokio_metrics::TaskMonitor::new();
        spawn_task_metrics_logger(monitor.clone());
        let mut intervals = monitor.intervals();
        assert!(
            intervals.next().is_some(),
            "ログ出力タスクを起動した後もメトリクスを取得できない"
        );
    }
}
