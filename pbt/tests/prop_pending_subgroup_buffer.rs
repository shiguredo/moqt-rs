//! `PendingSubgroupBuffer` の操作列に対する PBT
//!
//! Track Alias 未確立の Subgroup ストリームを保持するバッファへ、追加・チャンクの到着・
//! 購読の確立・タイムアウト・上限超過・削除を任意の順序で与え、テスト側の独立した
//! モデルと突き合わせる。
//!
//! draft-ietf-moq-transport-22 §3.1.3.1 (Unknown Track Alias) の "buffer it briefly" は
//! 上限を定めていないため、この実装は per-stream / per-session のバイト上限と
//! タイムアウトで保持量を抑える。検証する性質は次の 4 つである。
//!
//! - 集計バイト数が常に `per_session_max_bytes` 以下である (entry ごとのバイト数も
//!   `per_stream_max_bytes` 以下である)
//! - 集計バイト数が、実際に保持しているチャンクの合計と一致する
//!   (追加・破棄・引き取り・削除のどの操作の後でもずれない)
//! - 引き取った entry は 2 度引き取れない (バッファ全体の `take_ready` と、entry を
//!   指定する `take_ready_for` を混ぜても成り立つ)
//! - `Timeout` が返るのは追加から `timeout_us` を過ぎた entry だけである
//!
//! あわせて、操作と引き取り理由の分岐ごとに到達を数え、どの性質も空振りしていない
//! ことを確かめる。

use std::cell::Cell;
use std::collections::HashSet;

use pbt::common::test_runner;
use shiguredo_moqt::pending_subgroup_buffer::{
    PendingNotifyReason, PendingSubgroupBuffer, PendingSubgroupBufferOptions,
    PendingSubgroupEntryId, PendingSubgroupReady,
};

/// Track Alias の境界 (alias の取り違えを誘発するため小さい範囲に限定する)
const ALIAS_BOUNDARIES: [u64; 4] = [0, 1, 2, 7];

/// per-stream の上限の境界 (すぐ上限へ当たる小さい値と、当たらない大きい値)
const PER_STREAM_BOUNDARIES: [usize; 4] = [1, 4, 8, 64];

/// per-session の合計上限の境界
const PER_SESSION_BOUNDARIES: [usize; 4] = [1, 8, 16, 128];

/// タイムアウトの境界 (マイクロ秒)。0 は追加した瞬間に期限切れになる
const TIMEOUT_BOUNDARIES: [i64; 4] = [0, 1, 10, 1000];

/// 1 チャンクの長さの境界 (空のチャンクも含める)
const CHUNK_BOUNDARIES: [usize; 4] = [0, 1, 3, 9];

/// 1 ケースのオペレーション数の境界 (短い列と長い列の両方を作る)
const STEP_BOUNDARIES: [usize; 3] = [1, 8, 24];

/// 1 オペレーションで進める時刻の境界 (マイクロ秒)
const TIME_ADVANCE_BOUNDARIES: [usize; 4] = [0, 1, 5, 50];

/// 引き取りの理由ごとの添字
const REASON_COUNT: usize = 6;

/// 引き取りの理由を添字へ写す (到達を数えるゲート用)
fn reason_index(reason: PendingNotifyReason) -> usize {
    match reason {
        PendingNotifyReason::Subscriber => 0,
        PendingNotifyReason::Timeout => 1,
        PendingNotifyReason::OverflowPerStream => 2,
        PendingNotifyReason::OverflowPerSession => 3,
        PendingNotifyReason::SessionClose => 4,
        PendingNotifyReason::EndOfStream => 5,
    }
}

/// ゲートのメッセージ用の理由名
const REASON_NAMES: [&str; REASON_COUNT] = [
    "Subscriber",
    "Timeout",
    "OverflowPerStream",
    "OverflowPerSession",
    "SessionClose",
    "EndOfStream",
];

/// 境界値から選ぶ確率 (1/4)
fn boundary_ratio() -> noprop::Ratio {
    noprop::Ratio::one_nth(4)
}

/// 到達を数えるゲートを 1 増やす
fn bump(cell: &Cell<usize>) {
    cell.set(cell.get() + 1);
}

/// モデルと SUT へ与える 1 オペレーション
#[derive(Debug, Clone, Copy)]
enum Command {
    /// entry を追加する
    Add { track_alias: u64 },
    /// チャンクを積む
    Push { index: usize, len: usize },
    /// 購読の確立を通知する
    NoteSubscriber { track_alias: u64 },
    /// ストリームの終端を通知する
    NoteEndOfStream { track_alias: u64 },
    /// session の終了を通知する
    NoteSessionClose,
    /// 引き取り可能な entry を取り出す (バッファ全体)
    Take,
    /// 指定した entry だけを取り出す
    TakeFor { index: usize },
    /// entry を削除する
    Remove { index: usize },
}

/// Track Alias を生成する
fn sample_alias(ctx: &mut noprop::TestCaseContext) -> u64 {
    noprop::sample_with_boundaries(ctx, &ALIAS_BOUNDARIES, boundary_ratio(), |ctx| {
        noprop::sample_u64_in(ctx, 0..=7)
    })
}

/// 状態に依存してオペレーションを選ぶ
///
/// entry が 1 つも無いときは `Push` / `TakeFor` / `Remove` が成立しないため、`Add` と
/// `Take` だけを選ぶ。成立しないオペレーションを生成して捨てるより、状態に合うものだけを
/// 選ぶ。
fn sample_command(ctx: &mut noprop::TestCaseContext, entry_count: usize) -> Command {
    if entry_count == 0 {
        return match noprop::sample_weighted_index(ctx, &[3, 1]) {
            0 => Command::Add {
                track_alias: sample_alias(ctx),
            },
            _ => Command::Take,
        };
    }
    // 重み: 追加 3 / 積む 6 / 購読の確立 2 / 終端 2 / session close 1 / 引き取り 5 /
    // entry 指定の引き取り 4 / 削除 2。上限超過は「積む」の連投で、タイムアウトは時刻の
    // 前進と「引き取り」の組み合わせで到達するため、どちらも 1 ケースあたり数回は通る。
    // entry 指定の引き取りは、指定した entry が引き取れずに他の entry は引き取れる場面を
    // 通すために、全体の引き取りと同じくらいの重みを与える
    match noprop::sample_weighted_index(ctx, &[3, 6, 2, 2, 1, 5, 4, 2]) {
        0 => Command::Add {
            track_alias: sample_alias(ctx),
        },
        1 => Command::Push {
            index: noprop::sample_usize_in(ctx, 0..entry_count),
            len: noprop::sample_with_boundaries(ctx, &CHUNK_BOUNDARIES, boundary_ratio(), |ctx| {
                noprop::sample_usize_in(ctx, 0..=16)
            }),
        },
        2 => Command::NoteSubscriber {
            track_alias: noprop::sample_usize_in(ctx, 0..=7) as u64,
        },
        3 => Command::NoteEndOfStream {
            track_alias: noprop::sample_usize_in(ctx, 0..=7) as u64,
        },
        4 => Command::NoteSessionClose,
        5 => Command::Take,
        6 => Command::TakeFor {
            index: noprop::sample_usize_in(ctx, 0..entry_count),
        },
        _ => Command::Remove {
            index: noprop::sample_usize_in(ctx, 0..entry_count),
        },
    }
}

/// チャンクを積んだ結果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PushOutcome {
    /// entry へ積んだ
    Stored,
    /// 破棄済み・引き取り済みの entry へ届いたため無視した
    Ignored,
    /// 上限超過で entry ごと破棄した
    Discarded(PendingNotifyReason),
}

/// テスト側のモデル entry
struct ModelEntry {
    /// SUT が `add` で返した識別子 (push / remove に渡す)
    id: PendingSubgroupEntryId,
    /// SUBGROUP_HEADER の Track Alias
    track_alias: u64,
    /// タイムアウトの期限
    deadline_us: i64,
    /// 保持しているチャンク (中身は長さだけを再現する)
    chunks: Vec<Vec<u8>>,
    /// 保持しているチャンクの合計バイト数
    bytes: usize,
    /// 確定した通知理由
    reason: Option<PendingNotifyReason>,
    /// 上限超過で破棄済みか
    discarded: bool,
    /// チャンクを引き渡し済みか
    taken: bool,
}

/// 引き取った entry (SUT の [`shiguredo_moqt::pending_subgroup_buffer::PendingSubgroupReady`]
/// と同じ内容をモデル側で組み立てたもの)
struct TakenEntry {
    /// モデル上の期限 (`Timeout` の判定に使う)
    deadline_us: i64,
    /// 引き取った entry の識別子
    id: PendingSubgroupEntryId,
    /// 引き取った entry の Track Alias
    track_alias: u64,
    /// 引き取った理由
    reason: PendingNotifyReason,
    /// 引き取ったチャンク
    chunks: Vec<Vec<u8>>,
}

/// テスト側のモデル (公開 API の契約をそのまま書き下したもの)
struct Model {
    /// 追加順の entry 一覧 (SUT の識別子は追加順に増えるため、引き取り順もこの順になる)
    entries: Vec<ModelEntry>,
    /// 保持しているチャンクの合計バイト数
    total_bytes: usize,
}

impl Model {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            total_bytes: 0,
        }
    }

    fn add(&mut self, id: PendingSubgroupEntryId, track_alias: u64, now_us: i64, timeout_us: i64) {
        self.entries.push(ModelEntry {
            id,
            track_alias,
            deadline_us: now_us.saturating_add(timeout_us),
            chunks: Vec::new(),
            bytes: 0,
            reason: None,
            discarded: false,
            taken: false,
        });
    }

    /// チャンクを積む。上限超過なら entry ごと破棄する
    fn push(
        &mut self,
        index: usize,
        chunk: &[u8],
        options: &PendingSubgroupBufferOptions,
    ) -> PushOutcome {
        let entry = &self.entries[index];
        if entry.taken || entry.discarded {
            return PushOutcome::Ignored;
        }
        let len = chunk.len();
        if entry.bytes.saturating_add(len) > options.per_stream_max_bytes {
            self.discard(index, PendingNotifyReason::OverflowPerStream);
            return PushOutcome::Discarded(PendingNotifyReason::OverflowPerStream);
        }
        if self.total_bytes.saturating_add(len) > options.per_session_max_bytes {
            self.discard(index, PendingNotifyReason::OverflowPerSession);
            return PushOutcome::Discarded(PendingNotifyReason::OverflowPerSession);
        }
        let entry = &mut self.entries[index];
        entry.chunks.push(chunk.to_vec());
        entry.bytes += len;
        self.total_bytes += len;
        PushOutcome::Stored
    }

    /// チャンクを捨てて集計から外す (通知理由は最初の 1 回だけ確定させる)
    fn discard(&mut self, index: usize, reason: PendingNotifyReason) {
        let released = {
            let entry = &mut self.entries[index];
            let released = entry.bytes;
            entry.chunks.clear();
            entry.bytes = 0;
            entry.discarded = true;
            released
        };
        self.total_bytes -= released;
        let entry = &mut self.entries[index];
        if entry.reason.is_none() {
            entry.reason = Some(reason);
        }
    }

    /// 該当 Track Alias の全 entry に通知する
    fn note_alias(&mut self, track_alias: u64, reason: PendingNotifyReason) {
        for entry in &mut self.entries {
            if entry.track_alias == track_alias && entry.reason.is_none() {
                entry.reason = Some(reason);
            }
        }
    }

    /// 全 entry に通知する
    fn note_all(&mut self, reason: PendingNotifyReason) {
        for entry in &mut self.entries {
            if entry.reason.is_none() {
                entry.reason = Some(reason);
            }
        }
    }

    /// 期限を過ぎた entry を `Timeout` として確定させ、確定させた件数を返す
    fn expire(&mut self, now_us: i64) -> usize {
        let mut expired = 0;
        for entry in &mut self.entries {
            if entry.reason.is_none() && now_us >= entry.deadline_us {
                entry.reason = Some(PendingNotifyReason::Timeout);
                expired += 1;
            }
        }
        expired
    }

    /// 引き取り可能な entry を 1 つ返し、引き渡したチャンクを集計から外す
    ///
    /// 戻り値の 1 つ目はこの呼び出しで期限切れにした entry 数である。
    fn take_ready(&mut self, now_us: i64) -> (usize, Option<TakenEntry>) {
        let expired = self.expire(now_us);
        let index = self
            .entries
            .iter()
            .position(|entry| entry.reason.is_some() && !entry.taken);
        self.take_entry(index, expired)
    }

    /// 指定した entry だけを期限切れの対象にして引き取る
    ///
    /// 期限切れの確定も指定した entry だけを対象にする (他の entry の状態を変えない)。
    /// 戻り値の 1 つ目はこの呼び出しで期限切れにした entry 数 (0 か 1) である。
    fn take_ready_for(&mut self, index: usize, now_us: i64) -> (usize, Option<TakenEntry>) {
        let mut expired = 0;
        {
            let entry = &mut self.entries[index];
            if entry.reason.is_none() && now_us >= entry.deadline_us {
                entry.reason = Some(PendingNotifyReason::Timeout);
                expired = 1;
            }
        }
        let index = if self.entries[index].reason.is_some() && !self.entries[index].taken {
            Some(index)
        } else {
            None
        };
        self.take_entry(index, expired)
    }

    /// 引き取り可能な entry をモデルから取り出し、引き渡したチャンクを集計から外す
    ///
    /// 引き取れない場合は `None` を返す。`expired` はその呼び出しで期限切れにした数である。
    fn take_entry(&mut self, index: Option<usize>, expired: usize) -> (usize, Option<TakenEntry>) {
        let Some(index) = index else {
            return (expired, None);
        };
        let entry = &mut self.entries[index];
        entry.taken = true;
        let chunks = std::mem::take(&mut entry.chunks);
        let moved = entry.bytes;
        entry.bytes = 0;
        self.total_bytes -= moved;
        let taken = TakenEntry {
            deadline_us: entry.deadline_us,
            id: entry.id,
            track_alias: entry.track_alias,
            reason: entry.reason.expect("reason is Some で絞り込んでいる"),
            chunks,
        };
        (expired, Some(taken))
    }

    /// entry を削除し、保持していたバイトを集計から外す
    fn remove(&mut self, index: usize) {
        let entry = self.entries.remove(index);
        self.total_bytes -= entry.bytes;
    }

    /// モデル自身の整合 (保持チャンクの合計と集計の一致)
    fn assert_internal(&self) {
        let held: usize = self.entries.iter().map(|entry| entry.bytes).sum();
        assert_eq!(
            held, self.total_bytes,
            "モデルの集計が保持チャンクの合計と一致しない (テスト側の不具合)"
        );
    }
}

/// 引き取りの結果がモデルと一致することを確かめ、引き取った場合は理由ごとの到達を数える
///
/// `take_ready` (バッファ全体) と `take_ready_for` (entry 指定) で共通に使う。モデルが
/// 引き取れると判定した entry の内容 (識別子・Track Alias・理由・チャンク) が一致すること、
/// `Timeout` で引き取れるのは期限を過ぎた entry だけであること、同じ entry を 2 度
/// 引き取らないことを確かめる。戻り値は実際に引き取ったかどうか。
fn assert_taken(
    ready: Option<PendingSubgroupReady>,
    expected: Option<TakenEntry>,
    now_us: i64,
    step: usize,
    taken_ids: &mut HashSet<PendingSubgroupEntryId>,
    reason_takes: &[Cell<usize>; REASON_COUNT],
    deadline_boundary_takes: &Cell<usize>,
) -> bool {
    let Some(taken) = expected else {
        assert!(
            ready.is_none(),
            "モデルが引き取れないのに entry を返した: step={step}, now_us={now_us}, \
             ready={ready:?}"
        );
        return false;
    };
    let ready = ready.unwrap_or_else(|| {
        panic!(
            "モデルが引き取れると判定した entry が返らなかった: step={step}, now_us={now_us}, \
             taken={:?}",
            taken.id
        )
    });
    assert_eq!(
        ready.id, taken.id,
        "引き取った識別子がモデルと一致しない: step={step}, now_us={now_us}"
    );
    assert_eq!(
        ready.track_alias, taken.track_alias,
        "引き取った Track Alias がモデルと一致しない: step={step}"
    );
    assert_eq!(
        ready.reason, taken.reason,
        "引き取りの理由がモデルと一致しない: step={step}, id={:?}",
        taken.id
    );
    assert_eq!(
        ready.chunks, taken.chunks,
        "引き取ったチャンクがモデルと一致しない: step={step}, id={:?}",
        taken.id
    );
    // `Timeout` で引き取れるのは期限を過ぎた entry だけである
    if taken.reason == PendingNotifyReason::Timeout {
        assert!(
            now_us >= taken.deadline_us,
            "期限前の entry が Timeout で引き取られた: now_us={now_us}, deadline_us={}",
            taken.deadline_us
        );
        if now_us == taken.deadline_us {
            bump(deadline_boundary_takes);
        }
    }
    assert!(
        taken_ids.insert(ready.id),
        "同じ entry を 2 度引き取った: id={:?}, step={step}",
        ready.id
    );
    bump(&reason_takes[reason_index(taken.reason)]);
    true
}

/// 任意の操作列に対して、バッファの集計と状態がモデルと一致することを検証する
#[test]
fn pending_subgroup_buffer_matches_model() -> noprop::TestResult {
    // 分岐ごとの到達を数えるゲートはケースをまたいで集計するため `Cell` を使う
    //
    // 引き取りの理由 (6 分岐)
    let reason_takes: [Cell<usize>; REASON_COUNT] = std::array::from_fn(|_| Cell::new(0));
    // チャンクを積んだ / 破棄済み・引き取り済みで無視した / 上限超過で破棄した
    let stored_pushes = Cell::new(0usize);
    let ignored_pushes = Cell::new(0usize);
    let discarded_pushes = Cell::new(0usize);
    // 期限切れで `Timeout` を確定させた entry 数 (バッファ全体 / entry 指定)
    let expired_entries = Cell::new(0usize);
    let expired_entries_for = Cell::new(0usize);
    // 引き取りを試して entry が無かった回数 (バッファ全体 / entry 指定) と、entry を削除した回数
    let empty_takes = Cell::new(0usize);
    let empty_takes_for = Cell::new(0usize);
    let entry_removals = Cell::new(0usize);
    // entry 指定の引き取りで entry を引き取った回数
    let taken_for_entries = Cell::new(0usize);
    // 指定した entry は引き取れず、他の entry は引き取れる状態だった回数
    // (バッファ全体を返す API では取り違える場面であり、entry 指定が要る理由である)
    let targeted_unavailable_takes_for = Cell::new(0usize);
    // 引き取り済みの識別子を観測した回数 (2 度引き取っていないことの検証)
    let taken_ids_checked = Cell::new(0usize);
    // 時刻を entry の期限へ合わせた回数と、期限ちょうどで `Timeout` になった回数
    // (期限ちょうどはバッファ全体と entry 指定の両方を通す)
    let deadline_snaps = Cell::new(0usize);
    let deadline_boundary_takes = Cell::new(0usize);
    let deadline_boundary_takes_for = Cell::new(0usize);

    let mut runner = test_runner()?;
    // 1 ケースあたり最大 24 オペレーション、256 ケース。重みは `sample_command` に
    // まとめてあり、どの分岐も 1 ケースで平均数回は通る
    runner.run(256, |ctx| {
        let options = PendingSubgroupBufferOptions {
            per_stream_max_bytes: noprop::sample_with_boundaries(
                ctx,
                &PER_STREAM_BOUNDARIES,
                boundary_ratio(),
                |ctx| noprop::sample_usize_in(ctx, 1..=64),
            ),
            per_session_max_bytes: noprop::sample_with_boundaries(
                ctx,
                &PER_SESSION_BOUNDARIES,
                boundary_ratio(),
                |ctx| noprop::sample_usize_in(ctx, 1..=128),
            ),
            timeout_us: noprop::sample_with_boundaries(
                ctx,
                &TIMEOUT_BOUNDARIES,
                boundary_ratio(),
                |ctx| noprop::sample_usize_in(ctx, 0..=1000) as i64,
            ),
        };
        let mut sut = PendingSubgroupBuffer::new(options);
        let mut model = Model::new();
        // 引き取り済みの識別子 (2 度引き取れないことの検証に使う)
        let mut taken_ids: HashSet<PendingSubgroupEntryId> = HashSet::new();

        let mut now_us: i64 = 0;
        let steps = noprop::sample_with_boundaries(
            ctx,
            &STEP_BOUNDARIES,
            noprop::Ratio::one_nth(3),
            |ctx| noprop::sample_usize_in(ctx, 0..=24),
        );
        for step in 0..steps {
            // 時刻は 0 を含む小さい歩幅で進める。タイムアウトを境界で跨がせるため、
            // 進めない場合と大きく進める場合の両方を作る
            now_us = now_us.saturating_add(noprop::sample_with_boundaries(
                ctx,
                &TIME_ADVANCE_BOUNDARIES,
                noprop::Ratio::one_nth(3),
                |ctx| noprop::sample_usize_in(ctx, 0..=50),
            ) as i64);

            let command = sample_command(ctx, model.entries.len());
            match command {
                Command::Add { track_alias } => {
                    let id = sut.add(track_alias, now_us);
                    model.add(id, track_alias, now_us, options.timeout_us);
                }
                Command::Push { index, len } => {
                    let chunk = vec![0u8; len];
                    let id = model.entries[index].id;
                    sut.push(id, &chunk);
                    match model.push(index, &chunk, &options) {
                        PushOutcome::Stored => bump(&stored_pushes),
                        PushOutcome::Ignored => bump(&ignored_pushes),
                        PushOutcome::Discarded(_) => bump(&discarded_pushes),
                    }
                }
                Command::NoteSubscriber { track_alias } => {
                    sut.note_subscriber(track_alias);
                    model.note_alias(track_alias, PendingNotifyReason::Subscriber);
                }
                Command::NoteEndOfStream { track_alias } => {
                    sut.note_end_of_stream(track_alias);
                    model.note_alias(track_alias, PendingNotifyReason::EndOfStream);
                }
                Command::NoteSessionClose => {
                    sut.note_session_close();
                    model.note_all(PendingNotifyReason::SessionClose);
                }
                Command::Take => {
                    // 期限ちょうどの境界 (`now_us == deadline_us`) を通すため、たまに時刻を
                    // entry の期限へ合わせる。単調増加の前提を壊さないよう前にだけ進める
                    if !model.entries.is_empty() && noprop::sample_weighted_index(ctx, &[3, 2]) == 1
                    {
                        let index = noprop::sample_usize_in(ctx, 0..model.entries.len());
                        now_us = now_us.max(model.entries[index].deadline_us);
                        bump(&deadline_snaps);
                    }
                    let ready = sut.take_ready(now_us);
                    let (expired, expected) = model.take_ready(now_us);
                    expired_entries.set(expired_entries.get() + expired);
                    if assert_taken(
                        ready,
                        expected,
                        now_us,
                        step,
                        &mut taken_ids,
                        &reason_takes,
                        &deadline_boundary_takes,
                    ) {
                        bump(&taken_ids_checked);
                    } else {
                        bump(&empty_takes);
                    }
                }
                Command::TakeFor { index } => {
                    // 指定した entry の期限へ時刻を合わせ、期限ちょうどの境界も通す
                    if noprop::sample_weighted_index(ctx, &[3, 2]) == 1 {
                        now_us = now_us.max(model.entries[index].deadline_us);
                        bump(&deadline_snaps);
                    }
                    // 指定した entry が引き取れず、他の entry は引き取れる状態か。この場面が
                    // バッファ全体を返す API では取り違えになる (entry 指定が要る理由である)。
                    // 期限の確定で状態が変わり得るため、引き取る前の状態で判定する
                    let available = |entry: &ModelEntry| {
                        !entry.taken && (entry.reason.is_some() || now_us >= entry.deadline_us)
                    };
                    let targeted_unavailable = !available(&model.entries[index])
                        && model
                            .entries
                            .iter()
                            .enumerate()
                            .any(|(other, entry)| other != index && available(entry));
                    let id = model.entries[index].id;
                    let ready = sut.take_ready_for(id, now_us);
                    let (expired, expected) = model.take_ready_for(index, now_us);
                    expired_entries_for.set(expired_entries_for.get() + expired);
                    if expected.as_ref().is_some_and(|taken| {
                        taken.reason == PendingNotifyReason::Timeout && now_us == taken.deadline_us
                    }) {
                        bump(&deadline_boundary_takes_for);
                    }
                    if assert_taken(
                        ready,
                        expected,
                        now_us,
                        step,
                        &mut taken_ids,
                        &reason_takes,
                        &deadline_boundary_takes,
                    ) {
                        bump(&taken_ids_checked);
                        bump(&taken_for_entries);
                    } else {
                        bump(&empty_takes_for);
                    }
                    if targeted_unavailable {
                        bump(&targeted_unavailable_takes_for);
                    }
                }
                Command::Remove { index } => {
                    let id = model.entries[index].id;
                    sut.remove(id);
                    model.remove(index);
                    bump(&entry_removals);
                }
            }

            // 集計と保持チャンクの一致、上限の遵守、entry 数の一致を毎オペレーションで確かめる
            model.assert_internal();
            assert_eq!(
                sut.total_bytes(),
                model.total_bytes,
                "集計バイト数が実際に保持しているチャンクの合計と一致しない: step={step}, \
                 now_us={now_us}, options={options:?}, sut={sut:?}"
            );
            assert!(
                sut.total_bytes() <= options.per_session_max_bytes,
                "集計バイト数が per-session の上限を超えた: total={}, limit={}, step={step}",
                sut.total_bytes(),
                options.per_session_max_bytes
            );
            for (index, entry) in model.entries.iter().enumerate() {
                assert!(
                    entry.bytes <= options.per_stream_max_bytes,
                    "entry のバイト数が per-stream の上限を超えた: index={index}, bytes={}, \
                     limit={}, step={step}",
                    entry.bytes,
                    options.per_stream_max_bytes
                );
            }
            assert_eq!(
                sut.stream_count(),
                model.entries.len(),
                "保持している entry 数がモデルと一致しない: step={step}, now_us={now_us}"
            );
        }
        Ok(())
    })?;

    // 分岐ごとの到達を確かめる。すべての理由と操作が 1 回以上現れていないと、
    // 対応する性質を検証していない (ゲートが空振りしている)
    for (index, name) in REASON_NAMES.iter().enumerate() {
        assert!(
            reason_takes[index].get() > 0,
            "理由 {name} で entry を引き取る経路が観測されなかった\n{runner}"
        );
    }
    assert!(
        stored_pushes.get() > 0,
        "チャンクを積む経路が観測されなかった\n{runner}"
    );
    assert!(
        ignored_pushes.get() > 0,
        "破棄済み・引き取り済みの entry へチャンクが届いて無視する経路が観測されなかった\n{runner}"
    );
    assert!(
        discarded_pushes.get() > 0,
        "上限超過で破棄する経路が観測されなかった\n{runner}"
    );
    assert!(
        expired_entries.get() > 0,
        "期限切れで Timeout を確定させる経路が観測されなかった\n{runner}"
    );
    assert!(
        expired_entries_for.get() > 0,
        "entry 指定の引き取りで期限切れを確定させる経路が観測されなかった\n{runner}"
    );
    assert!(
        empty_takes.get() > 0,
        "引き取れる entry が無い状態が観測されなかった\n{runner}"
    );
    assert!(
        empty_takes_for.get() > 0,
        "entry 指定の引き取りで引き取れない状態が観測されなかった\n{runner}"
    );
    assert!(
        entry_removals.get() > 0,
        "entry を削除する経路が観測されなかった\n{runner}"
    );
    assert!(
        taken_ids_checked.get() > 0,
        "entry を引き取る経路が観測されなかった\n{runner}"
    );
    assert!(
        taken_for_entries.get() > 0,
        "entry 指定で entry を引き取る経路が観測されなかった\n{runner}"
    );
    assert!(
        targeted_unavailable_takes_for.get() > 0,
        "指定した entry は引き取れず他の entry は引き取れる状態が観測されなかった\n{runner}"
    );
    assert!(
        deadline_snaps.get() > 0,
        "時刻を entry の期限へ合わせる経路が観測されなかった\n{runner}"
    );
    assert!(
        deadline_boundary_takes.get() > 0,
        "期限ちょうどで Timeout になる境界 (バッファ全体) が観測されなかった\n{runner}"
    );
    assert!(
        deadline_boundary_takes_for.get() > 0,
        "期限ちょうどで Timeout になる境界 (entry 指定) が観測されなかった\n{runner}"
    );
    assert_eq!(
        runner.stats().rejected_cases,
        0,
        "生成した入力がすべて有効であること (case rejection を使っていない)\n{runner}"
    );
    Ok(())
}
