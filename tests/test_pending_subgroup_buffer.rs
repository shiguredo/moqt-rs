//! Track Alias 未確立の Subgroup ストリームを保持するバッファのテスト
//!
//! [`PendingSubgroupBuffer`] の公開 API の契約を確認する。moqt-js の
//! `src/pendingSubgroupBuffer.test.ts` が固定している性質を、Sans I/O な Rust の API
//! (時刻を引数で受け、所有者が poll する) に対応させて移植したものである。
//!
//! 固定する性質は次のとおり。
//!
//! - 購読の確立 (`note_subscriber`) で該当 alias の entry を引き取れる
//! - 追加から `timeout_us` を過ぎた entry だけが `Timeout` で引き取れる
//! - per-stream / per-session の上限超過で entry のチャンクを破棄する
//! - 破棄済みの entry には以後のチャンクを足さない (無関係な stream を巻き込まない)
//! - 引き取りと削除でバイト数の集計が戻る
//! - 同じ Track Alias に複数の entry を保持できる
//! - `take_ready_for` は指定した entry だけを返し、他の entry の状態を変えない
//! - `Discarded` / `FilteredOut` 相当の Object は保持しない
//! - `reset` で全 entry と集計を捨てる
//!
//! 任意の操作列に対する性質は `pbt/tests/prop_pending_subgroup_buffer.rs` が固定する。

use shiguredo_moqt::pending_subgroup_buffer::{
    DEFAULT_PENDING_SUBGROUP_BUFFER_OPTIONS, PendingNotifyReason, PendingSubgroupBuffer,
    PendingSubgroupBufferOptions,
};

/// テスト用の設定を作る (`timeout_us` は既定で十分に長くし、明示したときだけ期限切れさせる)
fn options(
    per_stream_max_bytes: usize,
    per_session_max_bytes: usize,
) -> PendingSubgroupBufferOptions {
    PendingSubgroupBufferOptions {
        per_stream_max_bytes,
        per_session_max_bytes,
        timeout_us: 1_000_000,
    }
}

/// per-stream の上限だけを小さくした設定を作る
fn options_per_stream(per_stream_max_bytes: usize) -> PendingSubgroupBufferOptions {
    PendingSubgroupBufferOptions {
        per_stream_max_bytes,
        per_session_max_bytes: 1024,
        timeout_us: 1_000_000,
    }
}

/// テスト用の設定を作る (タイムアウトを指定する)
fn options_with_timeout(timeout_us: i64) -> PendingSubgroupBufferOptions {
    PendingSubgroupBufferOptions {
        per_stream_max_bytes: 1024,
        per_session_max_bytes: 4096,
        timeout_us,
    }
}

/// 空のバッファの集計は 0
#[test]
fn empty_buffer_has_zero_counters() {
    let buffer = PendingSubgroupBuffer::new(options(1024, 4096));
    assert_eq!(
        buffer.stream_count(),
        0,
        "entry が 1 つも無ければ 0 件である"
    );
    assert_eq!(
        buffer.total_bytes(),
        0,
        "entry が 1 つも無ければ 0 バイトである"
    );
    let mut buffer = buffer;
    assert!(
        buffer.take_ready(0).is_none(),
        "引き取り可能な entry が無ければ None を返す"
    );
}

/// 既定値は 1 MiB / 16 MiB / 5 秒
///
/// draft-ietf-moq-transport-22 §3.1.3.1 "buffer it briefly" の上限として決めた値であり、
/// 変更するとメモリ使用量の見積もりが変わる。
#[test]
fn default_options_match_documented_limits() {
    assert_eq!(
        DEFAULT_PENDING_SUBGROUP_BUFFER_OPTIONS.per_stream_max_bytes,
        1 << 20,
        "per-stream の既定値は 1 MiB"
    );
    assert_eq!(
        DEFAULT_PENDING_SUBGROUP_BUFFER_OPTIONS.per_session_max_bytes,
        16 << 20,
        "per-session の既定値は 16 MiB"
    );
    assert_eq!(
        DEFAULT_PENDING_SUBGROUP_BUFFER_OPTIONS.timeout_us, 5_000_000,
        "timeout の既定値は 5 秒"
    );
    // 既定値でも add / push / take / remove が動く
    let mut buffer = PendingSubgroupBuffer::new(DEFAULT_PENDING_SUBGROUP_BUFFER_OPTIONS);
    let id = buffer.add(1, 0);
    buffer.push(id, &[1, 2, 3]);
    assert_eq!(buffer.total_bytes(), 3, "既定値でもチャンクを保持できる");
    buffer.note_subscriber(1);
    let ready = buffer
        .take_ready(1)
        .expect("購読の確立で引き取れるはずである");
    assert_eq!(ready.reason, PendingNotifyReason::Subscriber);
    buffer.remove(ready.id);
    assert_eq!(buffer.stream_count(), 0, "削除で entry が消える");
    assert_eq!(buffer.total_bytes(), 0, "削除で集計が 0 に戻る");
}

/// add は entry を登録し、同じ Track Alias に複数の entry を並べられる
#[test]
fn add_registers_entry_with_no_bytes() {
    let mut buffer = PendingSubgroupBuffer::new(options(1024, 4096));
    let first = buffer.add(7, 0);
    assert_eq!(buffer.stream_count(), 1, "add した分だけ entry が増える");
    assert_eq!(buffer.total_bytes(), 0, "add だけではバイト数は増えない");
    let second = buffer.add(7, 0);
    assert_ne!(first, second, "同じ alias でも識別子は異なる");
    assert_eq!(
        buffer.stream_count(),
        2,
        "同じ Track Alias に複数の entry を保持できる"
    );
    buffer.remove(second);
    assert_eq!(buffer.stream_count(), 1, "片方だけ削除できる");
    buffer.remove(first);
    assert_eq!(buffer.stream_count(), 0, "両方削除すると 0 件になる");
}

/// push はチャンクを受信順に積み、合計バイト数を集計する
#[test]
fn push_accumulates_chunks_in_order() {
    let mut buffer = PendingSubgroupBuffer::new(options(1024, 4096));
    let id = buffer.add(1, 0);
    buffer.push(id, &[1, 2, 3]);
    buffer.push(id, &[4, 5]);
    assert_eq!(buffer.total_bytes(), 5, "積んだチャンクの合計が集計される");
    buffer.note_subscriber(1);
    let ready = buffer
        .take_ready(0)
        .expect("購読の確立で引き取れるはずである");
    assert_eq!(
        ready.chunks,
        vec![vec![1, 2, 3], vec![4, 5]],
        "チャンクは受信順に引き取れる"
    );
    assert_eq!(
        buffer.total_bytes(),
        0,
        "引き取ったチャンクは集計から外れる (entry は remove まで残る)"
    );
    assert_eq!(buffer.stream_count(), 1, "remove するまで entry は残る");
    buffer.remove(ready.id);
    assert_eq!(buffer.stream_count(), 0, "remove で entry が消える");
}

/// per-stream の上限を超えたら entry を破棄し、保持していたバイトも集計から外す
///
/// draft-ietf-moq-transport-22 §3.1.3.1 の "buffer it briefly" の上限であり、
/// 超過したチャンクを保持し続けると session の集計に残って他ストリームを巻き込む。
#[test]
fn per_stream_overflow_abandons_and_releases_bytes() {
    let mut buffer = PendingSubgroupBuffer::new(options_per_stream(8));
    let id = buffer.add(1, 0);
    buffer.push(id, &[0; 5]);
    buffer.push(id, &[0; 5]);
    assert_eq!(
        buffer.total_bytes(),
        0,
        "破棄した entry のバイトは集計から外れる"
    );
    let ready = buffer
        .take_ready(0)
        .expect("上限超過で通知済みになるはずである");
    assert_eq!(ready.reason, PendingNotifyReason::OverflowPerStream);
    assert!(ready.chunks.is_empty(), "破棄したチャンクは引き取れない");
    // 破棄後も entry 自体は remove まで残る
    assert_eq!(buffer.stream_count(), 1, "remove するまで entry は残る");
    buffer.remove(ready.id);
    assert_eq!(buffer.stream_count(), 0, "remove で entry が消える");
}

/// per-session の上限を超えたら、上限を超えさせた entry だけを破棄する
#[test]
fn per_session_overflow_discards_last_pushed_entry_only() {
    // per-stream は十分に大きくし、per-session だけで判定させる
    let mut buffer = PendingSubgroupBuffer::new(options(1024, 8));
    let first = buffer.add(1, 0);
    let second = buffer.add(2, 0);
    buffer.push(first, &[0; 5]);
    assert_eq!(buffer.total_bytes(), 5, "上限内のチャンクは保持される");
    buffer.push(second, &[0; 5]);
    assert_eq!(
        buffer.total_bytes(),
        5,
        "上限を超えさせた entry だけを破棄し、他の entry は巻き込まない"
    );
    assert_eq!(
        buffer.stream_count(),
        2,
        "破棄しても entry は remove まで残る"
    );
    let ready = buffer
        .take_ready(0)
        .expect("上限超過で通知済みになるはずである");
    assert_eq!(ready.id, second, "破棄されたのは最後に積んだ entry である");
    assert_eq!(ready.reason, PendingNotifyReason::OverflowPerSession);
    assert!(ready.chunks.is_empty(), "破棄したチャンクは引き取れない");
    buffer.remove(ready.id);
    // 巻き込まれなかった entry は通知されていない
    assert!(
        buffer.take_ready(0).is_none(),
        "関係のない entry は引き取り待ちにならない"
    );
    assert_eq!(buffer.total_bytes(), 5, "関係のない entry のバイトは残る");
    buffer.remove(first);
    assert_eq!(buffer.total_bytes(), 0, "削除で集計が 0 に戻る");
}

/// 破棄済みの entry には以後のチャンクを足さない
///
/// moqt-js の `pendingSubgroupBuffer.test.ts` の
/// "per-stream 上限超過で破棄した entry は以後のチャンクを加算しない" と同じ性質である。
/// 足し続けると、破棄したはずのバイトが集計に積み上がり、無関係な stream を巻き添えで
/// 上限超過させる。
#[test]
fn discarded_entry_accepts_no_more_chunks() {
    let mut buffer = PendingSubgroupBuffer::new(options_per_stream(8));
    let id = buffer.add(1, 0);
    buffer.push(id, &[0; 5]);
    buffer.push(id, &[0; 5]);
    let ready = buffer
        .take_ready(0)
        .expect("上限超過で通知済みになるはずである");
    assert_eq!(ready.reason, PendingNotifyReason::OverflowPerStream);
    // 上限に触れない小さなチャンクは、破棄済みでなければ加算される大きさである。
    // 加算されないことを確かめる (加算されると破棄したはずのバイトが集計に残る)
    for _ in 0..10 {
        buffer.push(id, &[0; 1]);
    }
    assert_eq!(
        buffer.total_bytes(),
        0,
        "破棄後のチャンクは保持も集計もしない"
    );
    // 上限を超える大きなチャンクは再び上限判定に当たるだけで、集計は変わらない
    for _ in 0..10 {
        buffer.push(id, &[0; 100]);
    }
    assert_eq!(
        buffer.total_bytes(),
        0,
        "破棄後の大きなチャンクも集計を変えない"
    );
    assert!(
        buffer.take_ready(0).is_none(),
        "同じ entry が 2 度引き取られることはない"
    );
    buffer.remove(ready.id);
    assert_eq!(buffer.stream_count(), 0, "remove で entry が消える");
}

/// 破棄した entry の後続チャンクが、他のストリームを巻き添えで上限超過させない
#[test]
fn discarded_entry_does_not_overflow_other_stream() {
    // per-stream 64 / per-session 100
    let mut buffer = PendingSubgroupBuffer::new(options(64, 100));
    let abandoned = buffer.add(1, 0);
    let healthy = buffer.add(2, 0);

    // 1 本目が per-stream の上限 (64) を超えて破棄される
    buffer.push(abandoned, &[0; 40]);
    buffer.push(abandoned, &[0; 40]);
    let ready = buffer
        .take_ready(0)
        .expect("上限超過で通知済みになるはずである");
    assert_eq!(ready.reason, PendingNotifyReason::OverflowPerStream);
    assert_eq!(buffer.total_bytes(), 0, "破棄で集計は 0 に戻る");

    // 破棄された entry へ大量のチャンクが届いても集計は増えない
    for _ in 0..10 {
        buffer.push(abandoned, &[0; 100]);
    }
    assert_eq!(buffer.total_bytes(), 0, "破棄後のチャンクは集計に残らない");

    // 健在なストリームは per-stream の上限 (64) と per-session の上限 (100) の内側で
    // 受け取り続けられる
    buffer.push(healthy, &[0; 15]);
    assert_eq!(buffer.total_bytes(), 15, "健在なストリームは保持できる");
    assert!(
        buffer.take_ready(0).is_none(),
        "健在なストリームは上限超過していない"
    );
    buffer.remove(ready.id);
    buffer.remove(healthy);
    assert_eq!(buffer.total_bytes(), 0, "削除で集計が 0 に戻る");
}

/// remove は保持していたバイトを集計から外す
#[test]
fn remove_subtracts_bytes_from_totals() {
    let mut buffer = PendingSubgroupBuffer::new(options(1024, 4096));
    let id = buffer.add(1, 0);
    buffer.push(id, &[0; 10]);
    assert_eq!(buffer.total_bytes(), 10, "積んだ分が集計される");
    buffer.remove(id);
    assert_eq!(buffer.total_bytes(), 0, "削除で集計が 0 に戻る");
    assert_eq!(buffer.stream_count(), 0, "削除で entry が消える");
}

/// 引き取り済みと削除済みの entry への remove は no-op
#[test]
fn remove_after_take_is_noop() {
    let mut buffer = PendingSubgroupBuffer::new(options(1024, 4096));
    let id = buffer.add(1, 0);
    buffer.push(id, &[0; 10]);
    buffer.note_subscriber(1);
    let ready = buffer
        .take_ready(0)
        .expect("購読の確立で引き取れるはずである");
    buffer.remove(ready.id);
    buffer.remove(ready.id);
    assert_eq!(buffer.stream_count(), 0, "二重の削除でも件数は 0 のまま");
    assert_eq!(buffer.total_bytes(), 0, "二重の削除でも集計は 0 のまま");
}

/// 削除済み・未登録の識別子への push は集計を変えない
#[test]
fn push_to_removed_entry_is_noop() {
    let mut buffer = PendingSubgroupBuffer::new(options(1024, 4096));
    let id = buffer.add(1, 0);
    buffer.remove(id);
    buffer.push(id, &[0; 100]);
    assert_eq!(buffer.total_bytes(), 0, "削除済みの entry には積まない");
    assert_eq!(buffer.stream_count(), 0, "削除済みの entry は復活しない");
}

/// 引き取った entry には以後のチャンクを足さない
///
/// チャンクの所有権は呼び出し側へ移っており、集計にも残っていない。積むと誰も
/// 引き取らないバイトが集計だけに残るため、足さない。
#[test]
fn push_after_take_is_ignored() {
    let mut buffer = PendingSubgroupBuffer::new(options(1024, 4096));
    let id = buffer.add(1, 0);
    buffer.push(id, &[0; 3]);
    buffer.note_subscriber(1);
    let ready = buffer
        .take_ready(0)
        .expect("購読の確立で引き取れるはずである");
    assert_eq!(ready.chunks, vec![vec![0, 0, 0]], "引き取ったチャンク");
    buffer.push(id, &[0; 100]);
    assert_eq!(buffer.total_bytes(), 0, "引き取り後のチャンクは集計しない");
    assert_eq!(buffer.stream_count(), 1, "remove するまで entry は残る");
    buffer.remove(ready.id);
}

/// note_subscriber は該当 Track Alias の entry だけを引き取り待ちにする
#[test]
fn note_subscriber_releases_only_that_alias() {
    let mut buffer = PendingSubgroupBuffer::new(options(1024, 4096));
    let first = buffer.add(1, 0);
    let second = buffer.add(2, 0);
    buffer.push(first, &[0; 3]);
    buffer.push(second, &[0; 3]);
    buffer.note_subscriber(1);
    let ready = buffer
        .take_ready(0)
        .expect("alias=1 の entry が引き取れるはずである");
    assert_eq!(ready.id, first, "引き取ったのは alias=1 の entry である");
    assert_eq!(ready.reason, PendingNotifyReason::Subscriber);
    assert!(
        buffer.take_ready(0).is_none(),
        "alias=2 の entry は通知されていない"
    );
    assert_eq!(
        buffer.total_bytes(),
        3,
        "引き取っていない entry のバイトは残る"
    );
    buffer.note_subscriber(2);
    let ready = buffer
        .take_ready(0)
        .expect("alias=2 の entry が引き取れるはずである");
    assert_eq!(ready.id, second, "引き取ったのは alias=2 の entry である");
}

/// 同じ Track Alias の複数 entry はまとめて引き取り待ちになる
#[test]
fn multiple_entries_on_same_alias_are_released_together() {
    let mut buffer = PendingSubgroupBuffer::new(options(1024, 4096));
    let first = buffer.add(5, 0);
    let second = buffer.add(5, 0);
    buffer.push(first, &[0; 2]);
    buffer.push(second, &[0; 3]);
    buffer.note_subscriber(5);
    let ready = buffer
        .take_ready(0)
        .expect("1 つ目の entry が引き取れるはずである");
    assert_eq!(ready.id, first, "追加順に引き取る");
    assert_eq!(ready.reason, PendingNotifyReason::Subscriber);
    let ready = buffer
        .take_ready(0)
        .expect("2 つ目の entry が引き取れるはずである");
    assert_eq!(ready.id, second, "同じ alias の残りも引き取れる");
    assert_eq!(ready.chunks, vec![vec![0, 0, 0]], "チャンクも引き取れる");
    assert!(
        buffer.take_ready(0).is_none(),
        "引き取り済みの entry は 2 度返らない"
    );
}

/// note_session_close は全 entry を引き取り待ちにする
#[test]
fn note_session_close_releases_all_entries() {
    let mut buffer = PendingSubgroupBuffer::new(options(1024, 4096));
    let first = buffer.add(1, 0);
    let second = buffer.add(2, 0);
    buffer.push(first, &[0; 4]);
    buffer.push(second, &[0; 5]);
    buffer.note_session_close();
    let ready = buffer
        .take_ready(0)
        .expect("1 つ目の entry が引き取れるはずである");
    assert_eq!(ready.reason, PendingNotifyReason::SessionClose);
    assert_eq!(ready.chunks, vec![vec![0, 0, 0, 0]], "チャンクは残っている");
    let ready = buffer
        .take_ready(0)
        .expect("2 つ目の entry が引き取れるはずである");
    assert_eq!(ready.reason, PendingNotifyReason::SessionClose);
    buffer.remove(ready.id);
    assert_eq!(buffer.stream_count(), 1, "引き取っただけでは削除されない");
}

/// note_end_of_stream は該当 Track Alias の entry を引き取り待ちにする
#[test]
fn note_end_of_stream_releases_that_alias() {
    let mut buffer = PendingSubgroupBuffer::new(options(1024, 4096));
    let id = buffer.add(9, 0);
    let other = buffer.add(10, 0);
    buffer.push(id, &[0; 6]);
    buffer.note_end_of_stream(9);
    let ready = buffer
        .take_ready(0)
        .expect("終端した alias の entry が引き取れるはずである");
    assert_eq!(ready.reason, PendingNotifyReason::EndOfStream);
    assert_eq!(
        ready.chunks,
        vec![vec![0, 0, 0, 0, 0, 0]],
        "終端でもチャンクは残る"
    );
    assert!(
        buffer.take_ready(0).is_none(),
        "終端していない alias の entry は引き取れない"
    );
    buffer.remove(ready.id);
    buffer.remove(other);
    assert_eq!(buffer.total_bytes(), 0, "削除で集計が 0 に戻る");
}

/// 期限を過ぎた entry だけが Timeout で引き取れる
///
/// moqt-js の `setTimeout` による期限を、Sans I/O では `take_ready(now_us)` の
/// 期限判定に置き換えている。
#[test]
fn timeout_releases_only_expired_entries() {
    let mut buffer = PendingSubgroupBuffer::new(options_with_timeout(100));
    let first = buffer.add(1, 0);
    let second = buffer.add(2, 50);
    buffer.push(first, &[0; 3]);
    buffer.push(second, &[0; 3]);
    assert!(
        buffer.take_ready(99).is_none(),
        "期限前の entry は引き取れない"
    );
    let ready = buffer
        .take_ready(100)
        .expect("期限を過ぎた entry が引き取れるはずである");
    assert_eq!(
        ready.id, first,
        "期限を過ぎたのは先に追加した entry だけである"
    );
    assert_eq!(ready.reason, PendingNotifyReason::Timeout);
    assert_eq!(
        ready.chunks,
        vec![vec![0, 0, 0]],
        "期限切れでもチャンクは残る"
    );
    assert!(
        buffer.take_ready(149).is_none(),
        "2 つ目の entry はまだ期限前である"
    );
    let ready = buffer
        .take_ready(150)
        .expect("2 つ目の entry も期限を過ぎれば引き取れる");
    assert_eq!(ready.id, second);
    assert_eq!(ready.reason, PendingNotifyReason::Timeout);
    buffer.remove(ready.id);
}

/// 確定した通知理由は後から別の理由で上書きしない
///
/// moqt-js の "notify は 1 回しか resolve しない" と同じ性質である。
#[test]
fn overflow_reason_is_not_overwritten_by_later_notification() {
    let mut buffer = PendingSubgroupBuffer::new(options_per_stream(8));
    let id = buffer.add(1, 0);
    buffer.push(id, &[0; 5]);
    buffer.push(id, &[0; 5]);
    buffer.note_subscriber(1);
    buffer.note_session_close();
    buffer.note_end_of_stream(1);
    let ready = buffer
        .take_ready(0)
        .expect("上限超過で通知済みになるはずである");
    assert_eq!(
        ready.reason,
        PendingNotifyReason::OverflowPerStream,
        "最初に確定した理由 (上限超過) が残る"
    );
    assert!(
        buffer.take_ready(0).is_none(),
        "同じ entry が 2 度引き取られることはない"
    );
}

/// 購読の確立で通知しても、引き取るまではチャンクを足せる
///
/// `Subscriber` と `Timeout` は「破棄済み」ではない。所有者がまだ保持しているチャンクを
/// 引き取る可能性があるため、削除は所有者の `remove` に委ねる。
#[test]
fn subscriber_notified_entry_keeps_chunks_until_take() {
    let mut buffer = PendingSubgroupBuffer::new(options(1024, 4096));
    let id = buffer.add(1, 0);
    buffer.push(id, &[0; 3]);
    buffer.note_subscriber(1);
    // 引き取る前に届いたチャンクも保持する (破棄済みではないため)
    buffer.push(id, &[0; 2]);
    assert_eq!(buffer.total_bytes(), 5, "通知後もチャンクは保持される");
    let ready = buffer
        .take_ready(0)
        .expect("購読の確立で引き取れるはずである");
    assert_eq!(
        ready.chunks,
        vec![vec![0, 0, 0], vec![0, 0]],
        "通知後も積んだチャンクは失われない"
    );
    assert_eq!(buffer.total_bytes(), 0, "引き取りで集計から外れる");
    buffer.remove(ready.id);
}

/// 保持しないと決めた Object (`Discarded` / `FilteredOut` 相当) はバッファに残らない
///
/// draft-ietf-moq-transport-22 §3.1.3 (Track Alias) / §3.1 (Subscriptions) により、
/// キャンセル済み subscription への遅延 Object とフィルタ不通過の Object は確実に不要で
/// あるため、呼び出し側は entry を作らずに捨てる。作ってしまった場合も `remove` で
/// 何も残らないことを固定する。
#[test]
fn caller_dropped_objects_are_not_retained() {
    let mut buffer = PendingSubgroupBuffer::new(options(1024, 4096));
    assert_eq!(buffer.stream_count(), 0, "捨てる Object は add しない");
    assert_eq!(buffer.total_bytes(), 0, "捨てる Object は push しない");
    // Discarded / FilteredOut として entry を作ってしまった場合の後始末
    let id = buffer.add(1, 0);
    buffer.remove(id);
    assert_eq!(buffer.stream_count(), 0, "remove で entry は残らない");
    assert_eq!(buffer.total_bytes(), 0, "remove で集計は 0 に戻る");
    assert!(
        buffer.take_ready(i64::MAX).is_none(),
        "削除済みの entry は期限が来ても引き取れない"
    );
}

/// reset は全 entry と集計を捨て、識別子は再利用しない
#[test]
fn reset_clears_entries_and_totals() {
    let mut buffer = PendingSubgroupBuffer::new(options(1024, 4096));
    let first = buffer.add(1, 0);
    let second = buffer.add(2, 0);
    buffer.push(first, &[0; 10]);
    buffer.push(second, &[0; 20]);
    assert_eq!(buffer.total_bytes(), 30, "削除前に集計へ載る");
    buffer.reset();
    assert_eq!(buffer.stream_count(), 0, "reset で entry が消える");
    assert_eq!(buffer.total_bytes(), 0, "reset で集計が 0 に戻る");
    assert!(
        buffer.take_ready(i64::MAX).is_none(),
        "reset 後の entry は引き取れない"
    );
    let third = buffer.add(1, 0);
    assert_ne!(third, first, "reset 前の識別子を再利用しない");
    assert_eq!(buffer.stream_count(), 1, "reset 後も add できる");
    buffer.remove(third);
    assert_eq!(buffer.stream_count(), 0, "reset 後の entry も削除できる");
}

/// take_ready_for は指定した entry だけを返す
///
/// 1 つのバッファを複数の主体で共有する場合、各主体は自分の entry の識別子を保持して
/// これを呼ぶ。指定していない entry は、通知されていても引き取らない。
#[test]
fn take_ready_for_returns_only_the_specified_entry() {
    let mut buffer = PendingSubgroupBuffer::new(options(1024, 4096));
    let first = buffer.add(1, 0);
    let second = buffer.add(2, 0);
    buffer.push(first, &[0; 3]);
    buffer.push(second, &[0; 5]);
    buffer.note_subscriber(1);
    assert!(
        buffer.take_ready_for(second, 0).is_none(),
        "まだ通知されていない entry は指定しても引き取れない"
    );
    let ready = buffer
        .take_ready_for(first, 0)
        .expect("通知済みの指定 entry は引き取れるはずである");
    assert_eq!(ready.id, first, "指定した entry が返る");
    assert_eq!(
        ready.track_alias, 1,
        "Track Alias も指定した entry のものである"
    );
    assert_eq!(ready.reason, PendingNotifyReason::Subscriber);
    assert_eq!(
        ready.chunks,
        vec![vec![0, 0, 0]],
        "指定した entry のチャンクが返る"
    );
    assert_eq!(
        buffer.total_bytes(),
        5,
        "指定していない entry のチャンクは集計に残る"
    );
    assert!(
        buffer.take_ready_for(second, 0).is_none(),
        "指定していない entry は通知されないままである"
    );
    buffer.note_subscriber(2);
    let ready = buffer
        .take_ready_for(second, 0)
        .expect("後から通知された entry も引き取れるはずである");
    assert_eq!(ready.id, second, "指定した entry が返る");
    assert_eq!(
        ready.chunks,
        vec![vec![0, 0, 0, 0, 0]],
        "通知が遅れてもチャンクは失われない"
    );
    assert_eq!(buffer.total_bytes(), 0, "両方の引き取りで集計が 0 に戻る");
    buffer.remove(ready.id);
    buffer.remove(first);
    assert_eq!(buffer.stream_count(), 0, "remove で entry が消える");
}

/// take_ready_for は他の entry を引き取らず、take_ready と混在しても二重に返らない
#[test]
fn take_ready_for_does_not_consume_other_entries() {
    let mut buffer = PendingSubgroupBuffer::new(options(1024, 4096));
    let first = buffer.add(1, 0);
    let second = buffer.add(2, 0);
    buffer.push(first, &[0; 3]);
    buffer.push(second, &[0; 4]);
    buffer.note_session_close();
    // 追加が遅い方 (second) を entry 指定で引き取る
    let ready = buffer
        .take_ready_for(second, 0)
        .expect("通知済みの指定 entry は引き取れるはずである");
    assert_eq!(ready.id, second, "指定した entry が返る");
    assert_eq!(ready.reason, PendingNotifyReason::SessionClose);
    // バッファ全体の take_ready は、まだ引き取っていない entry だけを返す
    let ready = buffer
        .take_ready(0)
        .expect("残った entry が引き取れるはずである");
    assert_eq!(
        ready.id, first,
        "take_ready_for で引き取った entry は take_ready に返らない"
    );
    assert!(
        buffer.take_ready(0).is_none(),
        "同じ entry が 2 度引き取られることはない"
    );
    assert_eq!(buffer.total_bytes(), 0, "引き取りで集計が 0 に戻る");
    // 引き取り済みの entry を指定しても返らない
    assert!(
        buffer.take_ready_for(second, 0).is_none(),
        "引き取り済みの entry は 2 度引き取れない"
    );
    // 削除済みの entry を指定しても返らない
    buffer.remove(second);
    assert!(
        buffer.take_ready_for(second, 0).is_none(),
        "バッファから削除済みの entry は引き取れない"
    );
    buffer.remove(first);
    assert_eq!(buffer.stream_count(), 0, "remove で entry が消える");
}

/// take_ready_for は未通知・引き取り済み・削除済みの entry に None を返す
///
/// 本モジュールの「破棄済み」は上限超過でチャンクを捨てた状態を指すが、その状態は
/// [`PendingSubgroupBuffer::take_ready`] と同じく `Some` で理由を返す (理由を返さないと
/// 所有者が上限超過を観測できない)。ここではバッファから削除済みの entry を確かめる。
#[test]
fn take_ready_for_returns_none_for_unnotified_taken_and_removed_entries() {
    let mut buffer = PendingSubgroupBuffer::new(options(1024, 4096));
    let id = buffer.add(1, 0);
    buffer.push(id, &[0; 3]);
    assert!(
        buffer.take_ready_for(id, 0).is_none(),
        "まだ通知されていない entry は引き取れない"
    );
    buffer.note_subscriber(1);
    let ready = buffer
        .take_ready_for(id, 0)
        .expect("通知済みの entry は引き取れるはずである");
    assert!(
        buffer.take_ready_for(id, 0).is_none(),
        "引き取り済みの entry は 2 度引き取れない"
    );
    buffer.remove(ready.id);
    assert!(
        buffer.take_ready_for(id, 0).is_none(),
        "バッファから削除済みの entry は引き取れない"
    );
    // reset で削除された entry (未登録の識別子) も返らない
    buffer.reset();
    assert!(
        buffer.take_ready_for(id, i64::MAX).is_none(),
        "reset 済みの識別子はどの entry も指さない"
    );
    assert_eq!(buffer.stream_count(), 0, "reset で entry が消える");
    assert_eq!(buffer.total_bytes(), 0, "reset で集計が 0 に戻る");
}

/// take_ready_for の期限切れの確定は指定した entry だけを対象にする
///
/// 他の entry は所有者が自分の周期で take_ready_for を呼ぶまで期限切れにならない。
/// 共有相手の状態を勝手に変えないことを固定する。指定していない entry が期限切れに
/// なっていれば、後から購読の確立を通知しても理由は `Timeout` のままになる。
#[test]
fn take_ready_for_expires_only_the_specified_entry() {
    let mut buffer = PendingSubgroupBuffer::new(options_with_timeout(100));
    let first = buffer.add(1, 0);
    let second = buffer.add(2, 0);
    buffer.push(first, &[0; 3]);
    buffer.push(second, &[0; 4]);
    let ready = buffer
        .take_ready_for(second, 100)
        .expect("指定した entry は期限切れで引き取れるはずである");
    assert_eq!(ready.id, second, "指定した entry が返る");
    assert_eq!(ready.reason, PendingNotifyReason::Timeout);
    assert_eq!(
        ready.chunks,
        vec![vec![0, 0, 0, 0]],
        "期限切れでもチャンクは残る"
    );
    assert_eq!(
        buffer.total_bytes(),
        3,
        "指定していない entry のチャンクは集計に残る"
    );
    // 期限を過ぎていても、指定していない entry はこの呼び出しでは期限切れになっていない
    buffer.note_subscriber(1);
    let ready = buffer
        .take_ready_for(first, 100)
        .expect("通知済みの entry は引き取れるはずである");
    assert_eq!(
        ready.reason,
        PendingNotifyReason::Subscriber,
        "指定していない entry の状態を take_ready_for が変えない"
    );
    // 期限前は指定しても引き取れず、期限ちょうどで期限切れになる
    let third = buffer.add(3, 50);
    buffer.push(third, &[0; 2]);
    assert!(
        buffer.take_ready_for(third, 149).is_none(),
        "期限前の entry は指定しても引き取れない"
    );
    let ready = buffer
        .take_ready_for(third, 150)
        .expect("期限ちょうどの entry は引き取れるはずである");
    assert_eq!(ready.id, third);
    assert_eq!(ready.reason, PendingNotifyReason::Timeout);
    assert_eq!(ready.chunks, vec![vec![0, 0]]);
}

/// 上限超過で破棄した entry を指定すると、理由と空のチャンクが返る
///
/// [`PendingSubgroupBuffer::take_ready`] と同じ挙動である。`None` を返すと、所有者が
/// 上限超過を観測して entry を削除する手段を失う。
#[test]
fn take_ready_for_returns_overflow_reason_for_discarded_entry() {
    let mut buffer = PendingSubgroupBuffer::new(options_per_stream(8));
    let id = buffer.add(1, 0);
    buffer.push(id, &[0; 5]);
    buffer.push(id, &[0; 5]);
    assert_eq!(buffer.total_bytes(), 0, "破棄で集計は 0 に戻る");
    let ready = buffer
        .take_ready_for(id, 0)
        .expect("上限超過で通知済みの entry は引き取れるはずである");
    assert_eq!(ready.id, id, "指定した entry が返る");
    assert_eq!(ready.reason, PendingNotifyReason::OverflowPerStream);
    assert!(ready.chunks.is_empty(), "破棄したチャンクは引き取れない");
    assert!(
        buffer.take_ready_for(id, 0).is_none(),
        "引き取り済みの entry は 2 度引き取れない"
    );
    assert!(
        buffer.take_ready(0).is_none(),
        "take_ready にも同じ entry は返らない"
    );
    buffer.remove(ready.id);
    assert_eq!(buffer.stream_count(), 0, "remove で entry が消える");
}
