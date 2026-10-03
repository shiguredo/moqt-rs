//! playout モジュールの単体テスト
//!
//! 本ファイルには integration test 共有の import / helper を置き、
//! 実際の公開 API 検証は `test_playout/` 配下の責務別サブモジュールへ分割する。

#[path = "test_playout/delay.rs"]
mod delay;
#[path = "test_playout/scheduler.rs"]
mod scheduler;
#[path = "test_playout/stretch.rs"]
mod stretch;
#[path = "test_playout/sync.rs"]
mod sync;
#[path = "test_playout/timeline.rs"]
mod timeline;
