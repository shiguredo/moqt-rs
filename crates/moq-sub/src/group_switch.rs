//! Group の切り替え gate の共有状態と待機の協調
//!
//! ライブラリの [`GroupSwitchGate`] は sans-I/O の状態機械であり、購読の世代、待機、通知は
//! I/O を持つ example 側の責務である。この module は、stream task 間で共有する gate と
//! [`tokio::sync::Notify`] をまとめ、保留の解除を待つ手順を 1 箇所に閉じる。
//!
//! 保留した Object の payload は、その Object を読んだ stream task が保持する。映像の
//! decoder は stream ごとに作られ、1 stream = 1 Group であるため、解除後も同じ task が同じ
//! decoder で復号する必要がある。この module は payload を保持しない。
//!
//! 判定は `mutex` で保護する。`admit` が同期で判定を返す必要があり、Object ごとに actor への
//! 往復を挟むと保留の判定が非同期になるためである。保持するのは判定の間だけで、待機中は
//! 持たない。
//!
//! 通知の登録は [`GroupSwitchGate::admit`] より前に行う。`admit` が
//! [`GroupSwitchDecision::Hold`] を返した直後に [`SharedGroupSwitchGate::end_stream`] が
//! 起きても、通知を取りこぼさないようにするためである
//! (`crates/tokio-moq` の `StopSendingSignal::wait` と同じ形)。

use std::sync::atomic::{AtomicU64, Ordering};

use shiguredo_moqt::group_switch_gate::{GroupSwitchDecision, GroupSwitchGate};

/// stream task 間で共有する Group の切り替え gate
///
/// 購読で 1 つ持ち、stream task へ [`std::sync::Arc`] で渡す。ロックは判定の間だけ持ち、
/// 待機中は持たない。
#[derive(Debug)]
pub struct SharedGroupSwitchGate {
    /// 保留の判定
    gate: std::sync::Mutex<GroupSwitchGate>,
    /// stream の終端を待っている task を起こすための通知
    notify: tokio::sync::Notify,
    /// 購読の世代。session 移行のたびに進める
    generation: AtomicU64,
    /// 保留の期限に使う単調時計の起点
    ///
    /// 壁時計は進め戻されることがあり、その場合は期限までの残り時間が大きく伸びる。
    /// 待機の sleep と同じ単調な時計 (`tokio::time`) を使う。tokio の時計を止めたテストでも
    /// 期限の判定と sleep が同じ時計で進む。
    origin: tokio::time::Instant,
}

impl SharedGroupSwitchGate {
    /// 世代 0 で作る
    pub fn new() -> Self {
        Self {
            gate: std::sync::Mutex::new(GroupSwitchGate::new()),
            notify: tokio::sync::Notify::new(),
            generation: AtomicU64::new(0),
            origin: tokio::time::Instant::now(),
        }
    }

    /// 世代を進めて状態を捨てる
    ///
    /// session 移行の直前に呼ぶ。旧 session の stream task を止めきれない場合があるため、
    /// 旧い task の呼び出しは世代の違いで無視する。
    pub fn reset(&self) {
        // 世代の更新と状態の初期化を同じロックの中で行う。新しい世代の stream task が
        // 初期化前の状態を読まないようにするためである
        let mut gate = self
            .gate
            .lock()
            .expect("group switch gate lock is not poisoned");
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        gate.reset(generation);
        // 待機中の task を起こす。世代が変わったため `admit` は配送を返し、期限まで
        // 待たずに解除される
        self.notify.notify_waiters();
    }

    /// 保留の期限に使う現在時刻 (単調時計の起点からのマイクロ秒)
    fn now_us(&self) -> i64 {
        i64::try_from(self.origin.elapsed().as_micros()).unwrap_or(i64::MAX)
    }

    /// 現在の世代
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// live の Subgroup stream を開いているものとして登録する
    ///
    /// `generation` は stream task を作った時点の世代である。呼び出しのたびに現在の世代を
    /// 読むと、session 移行で世代を進めた後に旧い task が新しい世代として扱われてしまう。
    pub fn begin_stream(&self, generation: u64, group_id: u64, stream_id: u64) {
        self.gate
            .lock()
            .expect("group switch gate lock is not poisoned")
            .begin_stream(generation, group_id, stream_id);
    }

    /// live の Subgroup stream が終端したものとして登録を外し、待っている task を起こす
    pub fn end_stream(&self, generation: u64, group_id: u64, stream_id: u64) {
        self.gate
            .lock()
            .expect("group switch gate lock is not poisoned")
            .end_stream(generation, group_id, stream_id);
        // 待機中の task はすべて起こす。どの Group の終端が解除の条件になったかは
        // `admit` が判定する
        self.notify.notify_waiters();
    }

    /// Object を配送してよいか判定する
    fn admit(&self, generation: u64, group_id: u64, now_us: i64) -> GroupSwitchDecision {
        self.gate
            .lock()
            .expect("group switch gate lock is not poisoned")
            .admit(generation, group_id, now_us)
    }

    /// 保留が解除されるまで待つ
    ///
    /// [`GroupSwitchDecision::Deliver`] になるまで [`GroupSwitchGate::admit`] を呼び直す。
    /// `end_stream` の通知か期限のどちらか早い方で起床する。
    pub async fn wait_until_deliver(&self, generation: u64, group_id: u64) {
        self.wait_until_deliver_with_clock(generation, group_id, || self.now_us())
            .await;
    }

    /// 現在時刻を渡して保留が解除されるまで待つ
    ///
    /// 時刻の取得を差し替えられるようにしてある。テストで時計を明示的に進めるためである。
    async fn wait_until_deliver_with_clock<F>(&self, generation: u64, group_id: u64, mut now_us: F)
    where
        F: FnMut() -> i64,
    {
        loop {
            // 通知の登録は admit より前に行う。admit が Hold を返した直後に end_stream が
            // 起きても取りこぼさないためである
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let now_us = now_us();
            match self.admit(generation, group_id, now_us) {
                GroupSwitchDecision::Deliver => return,
                GroupSwitchDecision::Hold { release_at_us } => {
                    // 期限までの残り時間を sleep で待つ。時刻の取得と sleep の時計が同じで
                    // あれば、期限を過ぎた起床で admit が Deliver を返す
                    let wait_us = release_at_us.saturating_sub(now_us).max(0) as u64;
                    let deadline =
                        tokio::time::Instant::now() + std::time::Duration::from_micros(wait_us);
                    tokio::select! {
                        _ = &mut notified => {}
                        _ = tokio::time::sleep_until(deadline) => {}
                    }
                }
            }
        }
    }
}

/// stream task が使う Group の切り替え gate と、その task の購読の世代
///
/// 世代は stream task を作った時点で固定する。session 移行で世代を進めた後に旧い task が
/// 呼んでも、新しい世代の判定に影響しないようにするためである。
#[derive(Debug, Clone)]
pub struct GroupSwitchHandle {
    /// 購読で共有する gate
    gate: std::sync::Arc<SharedGroupSwitchGate>,
    /// この task を作った時点の購読の世代
    generation: u64,
}

impl GroupSwitchHandle {
    /// 現在の世代を固定してハンドルを作る
    pub fn new(gate: std::sync::Arc<SharedGroupSwitchGate>) -> Self {
        let generation = gate.generation();
        Self { gate, generation }
    }

    /// live の Subgroup stream を開いているものとして登録する
    pub fn begin_stream(&self, group_id: u64, stream_id: u64) {
        self.gate.begin_stream(self.generation, group_id, stream_id);
    }

    /// live の Subgroup stream が終端したものとして登録を外す
    pub fn end_stream(&self, group_id: u64, stream_id: u64) {
        self.gate.end_stream(self.generation, group_id, stream_id);
    }

    /// 保留が解除されるまで待つ
    pub async fn wait_until_deliver(&self, group_id: u64) {
        self.gate
            .wait_until_deliver(self.generation, group_id)
            .await;
    }
}

impl Default for SharedGroupSwitchGate {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use shiguredo_moqt::group_switch_gate::{GROUP_SWITCH_HOLD_US, GroupSwitchDecision};

    use super::{GroupSwitchHandle, SharedGroupSwitchGate};

    /// テストで使う現在時刻 (マイクロ秒)
    ///
    /// tokio の時計から取る。`tokio::time::pause` と `advance` で制御できる。
    fn test_clock(origin: tokio::time::Instant) -> impl Fn() -> i64 + Clone {
        move || {
            let elapsed = tokio::time::Instant::now().saturating_duration_since(origin);
            i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX)
        }
    }

    /// 前の Group の Object を配送させ、その Group の stream を開いたままにする
    ///
    /// 戻り値はこのハンドルの世代である。
    fn open_previous_group(gate: &SharedGroupSwitchGate, group_id: u64, stream_id: u64) -> u64 {
        let generation = gate.generation();
        gate.begin_stream(generation, group_id, stream_id);
        // 同じ Group の Object は保留しない。ここで配送済みの最大が group_id になる。
        // まだ 1 つも配送していないため、判定は時刻を使わない
        assert!(
            matches!(
                gate.admit(generation, group_id, 0),
                GroupSwitchDecision::Deliver
            ),
            "前の Group の Object は配送すること"
        );
        generation
    }

    /// 待機が解除されていないことを、短い timeout で確かめる
    ///
    /// `is_finished` だけでは、待機がまだ poll されていなくても真になるため、時計を止めた
    /// 状態で timeout が切れることを解除されていない根拠にする。
    async fn assert_still_waiting(waiter: &mut tokio::task::JoinHandle<()>) {
        let result = tokio::time::timeout(std::time::Duration::from_millis(1), &mut *waiter).await;
        assert!(
            result.is_err(),
            "前の Group の stream が開いている間は保留を解除しないこと"
        );
    }

    /// 新しい Group の Object が、前の Group の `end_stream` で解除されること
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn releases_when_the_previous_stream_ends() {
        let origin = tokio::time::Instant::now();
        let clock = test_clock(origin);
        let gate = Arc::new(SharedGroupSwitchGate::new());
        let generation = open_previous_group(&gate, 10, 100);

        let mut waiter = {
            let gate = Arc::clone(&gate);
            let clock = clock.clone();
            tokio::spawn(async move {
                gate.wait_until_deliver_with_clock(generation, 11, clock)
                    .await
            })
        };
        assert_still_waiting(&mut waiter).await;

        gate.end_stream(generation, 10, 100);
        tokio::time::timeout(std::time::Duration::from_millis(1), waiter)
            .await
            .expect("前の Group の stream が終端したら保留を解除すること")
            .expect("待機 task が panic しないこと");
    }

    /// 前の Group の `end_stream` が来なければ期限で解除されること
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn releases_at_the_deadline_without_end_stream() {
        let origin = tokio::time::Instant::now();
        let clock = test_clock(origin);
        let gate = Arc::new(SharedGroupSwitchGate::new());
        let generation = open_previous_group(&gate, 10, 100);

        let mut waiter = {
            let gate = Arc::clone(&gate);
            let clock = clock.clone();
            tokio::spawn(async move {
                gate.wait_until_deliver_with_clock(generation, 11, clock)
                    .await
            })
        };
        assert_still_waiting(&mut waiter).await;

        // 保留の期限 (50 ms) を過ぎるまで時計を進める。期限を延ばす退行があれば、待機が
        // 終わらずこの timeout が切れて失敗する (無限ループにしない)
        tokio::time::advance(std::time::Duration::from_millis(60)).await;
        tokio::time::timeout(std::time::Duration::from_millis(200), waiter)
            .await
            .expect("期限を過ぎたら保留を解除すること")
            .expect("待機 task が panic しないこと");
    }

    /// 待機の登録が `admit` より前に行われ、保留の直後に `end_stream` が起きても解除されること
    ///
    /// 1 回目の通知を admit が Hold を返す前に、2 回目の通知を再判定の前に起こす。通知の
    /// 登録が `admit` より後だと 1 回目の通知を取りこぼし、期限 (50 ms) まで待ってしまう。
    /// 時計を進めずに解除されることで、通知で解除されたことを確かめる。
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn does_not_lose_a_notification_between_admit_and_waiting() {
        let origin = tokio::time::Instant::now();
        let gate = Arc::new(SharedGroupSwitchGate::new());
        // 前の Group に 2 本の stream を開く。1 本終端しても、もう 1 本が開いている間は保留
        let generation = gate.generation();
        gate.begin_stream(generation, 10, 100);
        gate.begin_stream(generation, 10, 101);
        let _ = gate.admit(generation, 10, 0);

        let waiter = {
            // 時計から stream を終端するため、待機に使う gate とは別に Arc を持つ
            let gate_for_wait = Arc::clone(&gate);
            let gate_for_clock = Arc::clone(&gate);
            tokio::spawn(async move {
                // 時刻を読むたびに stream を 1 本ずつ終端する。時刻の取得は `admit` の直前
                // であり、通知の登録より後である
                let mut ended = 0u64;
                let clock = move || {
                    if ended == 0 {
                        gate_for_clock.end_stream(generation, 10, 100);
                    } else if ended == 1 {
                        gate_for_clock.end_stream(generation, 10, 101);
                    }
                    ended += 1;
                    let elapsed = tokio::time::Instant::now().saturating_duration_since(origin);
                    i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX)
                };
                gate_for_wait
                    .wait_until_deliver_with_clock(generation, 11, clock)
                    .await;
            })
        };

        // 時計を進めずに解除されること (期限まで待っていたら 50 ms 進んでいる)
        tokio::time::timeout(std::time::Duration::from_millis(1), waiter)
            .await
            .expect("通知で解除されること (期限まで待たない)")
            .expect("待機 task が panic しないこと");
        let elapsed = tokio::time::Instant::now().saturating_duration_since(origin);
        assert!(
            elapsed
                < std::time::Duration::from_micros(
                    u64::try_from(GROUP_SWITCH_HOLD_US).unwrap_or(50_000)
                ),
            "通知で解除されること (期限まで待たない): elapsed={elapsed:?}"
        );
    }

    /// 複数の task が待っていても、`end_stream` の通知で同時に解除されること
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn wakes_every_waiter_on_end_stream() {
        let origin = tokio::time::Instant::now();
        let clock = test_clock(origin);
        let gate = Arc::new(SharedGroupSwitchGate::new());
        let generation = open_previous_group(&gate, 10, 100);

        let mut waiters = Vec::new();
        for _ in 0..3 {
            let gate = Arc::clone(&gate);
            let clock = clock.clone();
            waiters.push(tokio::spawn(async move {
                gate.wait_until_deliver_with_clock(generation, 11, clock)
                    .await
            }));
        }
        for waiter in &mut waiters {
            assert_still_waiting(waiter).await;
        }

        gate.end_stream(generation, 10, 100);
        for waiter in waiters {
            tokio::time::timeout(std::time::Duration::from_millis(1), waiter)
                .await
                .expect("1 回の通知で待っている task をすべて起こすこと")
                .expect("待機 task が panic しないこと");
        }
    }

    /// `reset` が待機中の task を起こし、旧世代の task を保留しないこと
    ///
    /// 移行の直前に世代を進めると、待っている旧世代の task は期限まで待たずに解除される。
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn reset_wakes_waiters_of_the_old_generation() {
        let gate = Arc::new(SharedGroupSwitchGate::new());
        let generation = open_previous_group(&gate, 10, 100);
        // 期限が来ない時計 (常に 0) を使い、期限ではなく reset の通知で解除されることを見る
        let waiter = {
            let gate = Arc::clone(&gate);
            tokio::spawn(async move {
                gate.wait_until_deliver_with_clock(generation, 11, || 0)
                    .await
            })
        };
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished(), "期限前は保留を解除しないこと");

        gate.reset();
        tokio::time::timeout(std::time::Duration::from_millis(1), waiter)
            .await
            .expect("reset で旧世代の待機を解除すること")
            .expect("待機 task が panic しないこと");
    }

    /// `reset` のあと、旧世代の stream task は新しい購読の判定に影響しないこと
    ///
    /// session 移行では旧 session の stream task を止めきれないことがある。旧い task が
    /// 世代を固定したハンドルで `begin_stream` / `end_stream` を呼んでも、新しい世代の
    /// 判定を変えてはならない。新しい世代の stream の終端で解除されることまで確かめる。
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn reset_makes_the_old_generation_handle_harmless() {
        let gate = Arc::new(SharedGroupSwitchGate::new());
        // 旧世代 (世代 0) の stream task
        let old_handle = GroupSwitchHandle::new(Arc::clone(&gate));
        old_handle.begin_stream(10, 100);
        old_handle.wait_until_deliver(10).await;
        assert_eq!(
            gate.generation(),
            0,
            "旧世代のハンドルを作った直後であること"
        );

        // session 移行の直前に世代を進めて状態を捨てる
        gate.reset();
        assert_eq!(gate.generation(), 1, "reset で世代を進めること");
        let new_handle = GroupSwitchHandle::new(Arc::clone(&gate));

        // 新しい世代で Group 10 を配送して stream を終端し、Group 11 の stream を登録する
        new_handle.begin_stream(10, 500);
        new_handle.wait_until_deliver(10).await;
        new_handle.end_stream(10, 500);
        new_handle.begin_stream(11, 700);

        // 旧世代の task が同じ stream を開閉しても、新しい世代の保留は変わらない
        old_handle.begin_stream(11, 700);
        old_handle.end_stream(11, 700);
        let mut waiter = {
            let handle = new_handle.clone();
            tokio::spawn(async move { handle.wait_until_deliver(12).await })
        };
        // 旧世代の end_stream で解除されず、新しい世代の stream が開いている間は保留する
        assert_still_waiting(&mut waiter).await;

        // 新しい世代の stream を終端すると解除される
        new_handle.end_stream(11, 700);
        tokio::time::timeout(std::time::Duration::from_millis(1), waiter)
            .await
            .expect("新しい世代の stream の終端で解除すること")
            .expect("待機 task が panic しないこと");
    }
}
