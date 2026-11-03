//! 前の Group の stream が開いている間、次の Group の映像 Object を保留する状態機械
//!
//! draft-ietf-moq-transport-22 §2.1.2 (Object States): "Since Objects can be delivered out
//! of order" であり、Group ごとに別の Subgroup ストリームで届くため、前の Group の stream が
//! 終わる前に次の Group のキーフレームが届くことがある。次の Group のキーフレームを先に
//! 復号してしまうと、遅れて届いた前の Group の末尾は [`crate::video_decode_order`] の
//! `VideoObjectAdmission::Skip` (`Stale`) になり、表示されない。GOP の末尾が 1〜数枚欠け、
//! 30 fps で 33〜100 ms の停止として見える。
//!
//! [`GroupSwitchGate`] は、より新しい Group の Object を、その Group より前の開いている
//! stream が終わるまで、または保留を始めてから [`GROUP_SWITCH_HOLD_US`] が経過するまで保留
//! する。stream の終端 (FIN / RESET_STREAM / STOP_SENDING / 読み取りエラー) は
//! [`GroupSwitchGate::end_stream`] で通知する。
//!
//! 判定に使うのは Object の位置と stream の識別子と時刻だけで、I/O も payload も扱わない
//! (sans I/O)。保留した Object の payload は呼び出し側が保持し、解除後に同じ経路で復号する。
//!
//! 保留の長さは、前の Group の末尾の欠落 (30 fps で 1〜3 枚、33〜100 ms の停止) を避けるため、
//! Group の切り替えごとに最大 50 ms の復号保留を許容する。moqt-js の `GROUP_SWITCH_HOLD_MS`
//! と同じ判断である。時刻は呼び出し側がマイクロ秒 (`i64`) で渡す。
//!
//! 世代番号で古い呼び出しを無視する。session 移行で stream task を止めきれないまま
//! [`GroupSwitchGate::reset`] した場合に、旧い task の [`GroupSwitchGate::begin_stream`] が
//! 新しい購読の判定へ影響しないようにするためである。
//!
//! 前の Group の stream が [`GroupSwitchGate::begin_stream`] で登録される前に次の Group の
//! Object が [`GroupSwitchGate::admit`] されると、その Group が配信済みの最大になり、待つ
//! 対象が無いため保留しない。stream の到着順が逆転した起動直後の 1 回に限られる。
//!
//! 根拠の仕様はドラフトであり、将来変更される可能性がある。

use alloc::collections::{BTreeMap, BTreeSet};

/// Group の切り替えで次の Group を保留する最大時間 (マイクロ秒)
///
/// 前の Group の stream の終端がこれを超えて遅れる場合は保留で覆えず、前の Group の末尾の
/// 欠落が残る。期限の計算には単調増加する時計から取った時刻を渡すこと
/// ([`GroupSwitchGate::admit`] を参照)。
pub const GROUP_SWITCH_HOLD_US: i64 = 50_000;

/// Object を配送してよいかの判定
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupSwitchDecision {
    /// 配送してよい
    Deliver,
    /// 保留する
    Hold {
        /// この時刻 (マイクロ秒) 以降に [`GroupSwitchGate::admit`] を呼び直す
        ///
        /// 同じ Group の保留中は同じ値を返す。
        release_at_us: i64,
    },
}

/// 前の Group の stream が開いている間、次の Group の Object を保留する状態機械
///
/// 購読ごとに 1 つ持つ。session 移行の直前に [`GroupSwitchGate::reset`] で初期状態に戻す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupSwitchGate {
    /// 現在の世代
    generation: u64,
    /// 配送を始めた最大の Group ID。まだ配送していなければ `None`
    delivered_group_id: Option<u64>,
    /// 開いている live の Subgroup stream。`(group_id, stream_id)` の組で追跡する
    ///
    /// 1 Group が複数の Subgroup stream で届き得るため (draft-ietf-moq-transport-22
    /// §2.2 (Subgroups))、Group ではなく stream で持つ。
    open_streams: BTreeSet<(u64, u64)>,
    /// 保留している Group ごとの解除時刻。同じ Group の保留中は期限を延ばさない
    hold_deadlines: BTreeMap<u64, i64>,
}

impl GroupSwitchGate {
    /// 世代 0 で初期状態を作る
    pub fn new() -> Self {
        Self {
            generation: 0,
            delivered_group_id: None,
            open_streams: BTreeSet::new(),
            hold_deadlines: BTreeMap::new(),
        }
    }

    /// 状態を初期化し、現在の世代を `generation` にする
    ///
    /// 購読を張り直したとき (session 移行の直前など) に呼ぶ。止めきれなかった旧い stream
    /// task が [`GroupSwitchGate::begin_stream`] / [`GroupSwitchGate::end_stream`] /
    /// [`GroupSwitchGate::admit`] を呼んでも、世代が異なるため無視される。
    pub fn reset(&mut self, generation: u64) {
        self.generation = generation;
        self.delivered_group_id = None;
        self.open_streams.clear();
        self.hold_deadlines.clear();
    }

    /// live の Subgroup stream を開いているものとして登録する
    ///
    /// `stream_id` は呼び出し側が stream ごとに一意に付ける識別子であり、Subgroup ID の
    /// 解決方法に依存しない。1 Group が複数の stream で届く場合は、それぞれを登録する。
    ///
    /// 現在の世代と異なる `generation` の呼び出しは無視する。
    pub fn begin_stream(&mut self, generation: u64, group_id: u64, stream_id: u64) {
        if generation != self.generation {
            return;
        }
        self.open_streams.insert((group_id, stream_id));
    }

    /// live の Subgroup stream が終端したものとして登録を外す
    ///
    /// stream の終端 (FIN / RESET_STREAM / STOP_SENDING / 読み取りエラー) で呼ぶ。保留は
    /// 呼び出し側が [`GroupSwitchGate::admit`] を呼び直して解除する。
    ///
    /// 現在の世代と異なる `generation` の呼び出しは無視する。
    pub fn end_stream(&mut self, generation: u64, group_id: u64, stream_id: u64) {
        if generation != self.generation {
            return;
        }
        self.open_streams.remove(&(group_id, stream_id));
    }

    /// Object を配送してよいかを判定し、配送する場合は状態を進める
    ///
    /// - 配信済みの最大 Group 以下の Object は [`GroupSwitchDecision::Deliver`]
    /// - より新しい Group の Object は、配信済みの最大 Group 以上でその Group より小さい
    ///   Group の開いている stream があれば [`GroupSwitchDecision::Hold`]。期限はその Group を
    ///   最初に保留した時刻 + [`GROUP_SWITCH_HOLD_US`] であり、同じ Group の保留中は同じ値を
    ///   返す
    /// - 開いている stream が無い場合と `now_us` が期限以上の場合、およびまだ 1 つも配送して
    ///   いない場合は [`GroupSwitchDecision::Deliver`] にして配信済みの最大 Group を更新する
    ///
    /// [`GroupSwitchDecision::Deliver`] を返したときは、その Group より前の Group の開いている
    /// stream を以後の判定に使わない (削除する)。
    ///
    /// 現在の世代と異なる `generation` の呼び出しは状態を変えずに
    /// [`GroupSwitchDecision::Deliver`] を返す。止めきれなかった旧い stream task を保留しない
    /// ためである。
    ///
    /// `now_us` は単調増加する時計から取ること。壁時計のように戻る時計を渡すと、期限までの
    /// 残り時間が実際より長くなり、[`GROUP_SWITCH_HOLD_US`] の上限を超えて保留しうる。
    pub fn admit(&mut self, generation: u64, group_id: u64, now_us: i64) -> GroupSwitchDecision {
        if generation != self.generation {
            return GroupSwitchDecision::Deliver;
        }

        // 配信済みの最大 Group 以下は、前の Group の末尾であっても配送してよい。Group の
        // 順序を守るのは Group をまたぐ切り替えだけであり、同じ Group の中は
        // [`crate::video_decode_order`] が判定する
        if let Some(delivered_group_id) = self.delivered_group_id
            && group_id <= delivered_group_id
        {
            return GroupSwitchDecision::Deliver;
        }

        // まだ 1 つも配送していない場合は待つ対象が無い。最初の Group から復号を始める
        let Some(delivered_group_id) = self.delivered_group_id else {
            self.deliver(group_id);
            return GroupSwitchDecision::Deliver;
        };

        // 配信済みの最大 Group 以上で、この Group より小さい Group の stream が開いていれば、
        // その終端まで待つ。Group ID が飛んでいる場合、飛んだ Group の stream は存在しない
        // ため待たない (飛ぶ前の Group の stream があればその終端までは待つ)
        // `(group_id, 0)` 未満 = Group ID が `group_id` より小さい全 stream。Group ID が
        // `delivered_group_id` 以上の stream だけを見る
        let blocking_stream_is_open = self
            .open_streams
            .range((delivered_group_id, 0)..(group_id, 0))
            .next()
            .is_some();
        if blocking_stream_is_open {
            // 同じ Group の保留中は期限を延ばさない。延ばすと、前の Group の終端が遅れ続ける
            // 限り保留も延びてしまう
            let release_at_us = *self
                .hold_deadlines
                .entry(group_id)
                .or_insert_with(|| now_us.saturating_add(GROUP_SWITCH_HOLD_US));
            if now_us < release_at_us {
                return GroupSwitchDecision::Hold { release_at_us };
            }
        }

        self.deliver(group_id);
        GroupSwitchDecision::Deliver
    }

    /// 配送したものとして状態を進める
    fn deliver(&mut self, group_id: u64) {
        self.delivered_group_id = Some(group_id);
        // この Group を配送したら、それより前の Group の stream は以後の保留の判定に使わない。
        // 前の Group の末尾が遅れて届いても、それは [`crate::video_decode_order`] が古いと
        // して捨てる
        self.open_streams
            .retain(|(open_group_id, _)| *open_group_id >= group_id);
        // 保留の期限も、配送した Group 以前の分は使わない
        self.hold_deadlines
            .retain(|held_group_id, _| *held_group_id > group_id);
    }
}

impl Default for GroupSwitchGate {
    fn default() -> Self {
        Self::new()
    }
}
