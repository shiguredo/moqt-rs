//! Group の切り替えで次の Group の Object を保留する規則のテスト
//!
//! [`GroupSwitchGate`] の公開 API の契約を確認する。映像は Group ごとに別の Subgroup
//! ストリームで届くため (draft-ietf-moq-transport-23 §2.2 (Subgroups))、前の Group の stream
//! が終わる前に次の Group のキーフレームが届くことがある。次の Group のキーフレームを先に
//! 復号すると、遅れて届いた前の Group の末尾が古い Group として捨てられ、表示が途切れる。
//!
//! ここでは実際に起きる到着の形と、保留の解除条件を固定する。任意の到着列に対する性質は
//! `pbt/tests/prop_group_switch_gate.rs` が固定する。

use shiguredo_moqt::group_switch_gate::{
    GROUP_SWITCH_HOLD_US, GroupSwitchDecision, GroupSwitchGate,
};

/// 配送してよい
const DELIVER: GroupSwitchDecision = GroupSwitchDecision::Deliver;

/// 保留し、`release_at_us` 以降に呼び直す
fn hold(release_at_us: i64) -> GroupSwitchDecision {
    GroupSwitchDecision::Hold { release_at_us }
}

/// 判定に使う現在時刻 (マイクロ秒)
const NOW_US: i64 = 1_000_000;

/// 前の Group の stream が開いている間、次の Group の Object は保留すること
///
/// 前の Group の stream が終端したあとの判定は配送になる。stream の終端は FIN に限らず
/// RESET_STREAM / STOP_SENDING / 読み取りエラーでも起きるため、gate は理由を知らない。
#[test]
fn holds_a_newer_group_while_the_previous_stream_is_open() {
    let mut gate = GroupSwitchGate::new();
    // Group 10 の stream を開き、Group 10 の Object を配送して復号を始める
    gate.begin_stream(0, 10, 100);
    assert_eq!(
        gate.admit(0, 10, NOW_US),
        DELIVER,
        "最初の Group の Object は配送すること"
    );
    assert_eq!(
        gate.admit(0, 11, NOW_US),
        hold(NOW_US + GROUP_SWITCH_HOLD_US),
        "前の Group の stream が開いている間は次の Group の Object を保留すること"
    );

    // 前の Group の stream が終端したら、同じ時刻でも配送になる
    gate.end_stream(0, 10, 100);
    assert_eq!(
        gate.admit(0, 11, NOW_US),
        DELIVER,
        "前の Group の stream が終端したら保留を解除すること"
    );
}

/// 前の Group の stream が先に終わっていれば保留しないこと
#[test]
fn does_not_hold_after_the_previous_stream_has_ended() {
    let mut gate = GroupSwitchGate::new();
    gate.begin_stream(0, 10, 100);
    assert_eq!(gate.admit(0, 10, NOW_US), DELIVER);
    gate.end_stream(0, 10, 100);
    assert_eq!(
        gate.admit(0, 11, NOW_US),
        DELIVER,
        "前の Group の stream が終わっていれば保留しないこと"
    );
}

/// 期限を過ぎた判定は配送になり、同じ Group の保留中は期限が延びないこと
///
/// 前の Group の stream が終端しないまま時間が経っても、保留は 50 ms で打ち切る。これを
/// 伸ばすと、前の Group の終端が遅れ続ける限り次の Group の復号も始まらない。
#[test]
fn releases_a_held_group_at_the_deadline_and_keeps_the_same_deadline() {
    let mut gate = GroupSwitchGate::new();
    gate.begin_stream(0, 10, 100);
    assert_eq!(gate.admit(0, 10, NOW_US), DELIVER);

    let deadline = NOW_US + GROUP_SWITCH_HOLD_US;
    assert_eq!(gate.admit(0, 11, NOW_US), hold(deadline));
    assert_eq!(
        gate.admit(0, 11, NOW_US + 1_000),
        hold(deadline),
        "同じ Group の保留中は期限を延ばさないこと"
    );
    assert_eq!(
        gate.admit(0, 11, deadline - 1),
        hold(deadline),
        "期限の直前までは保留すること"
    );
    assert_eq!(
        gate.admit(0, 11, deadline),
        DELIVER,
        "期限ちょうどで配送すること"
    );
}

/// 同じ Group と配信済みより前の Group は保留しないこと
#[test]
fn does_not_hold_the_same_or_an_older_group() {
    let mut gate = GroupSwitchGate::new();
    gate.begin_stream(0, 10, 100);
    assert_eq!(gate.admit(0, 10, NOW_US), DELIVER);

    // 同じ Group の Object は、自分の stream が開いていても保留しない。ここで保留すると
    // 自分の stream の終端を自分で待つことになる
    assert_eq!(
        gate.admit(0, 10, NOW_US),
        DELIVER,
        "同じ Group の Object は保留しないこと"
    );
    // 配信済みより前の Group は、遅れて届いた末尾であり配送してよい。古いとして捨てるのは
    // 復号順の判定の役目である
    assert_eq!(
        gate.admit(0, 9, NOW_US),
        DELIVER,
        "配信済みより前の Group の Object は保留しないこと"
    );
}

/// 配信済みより前の Group の stream は、より新しい Group の保留に使わないこと
///
/// 配信した Group より前の stream は、以後の判定に使わない。使うと、遅れて残った古い
/// stream が次の Group の復号を不必要に遅らせる。
#[test]
fn ignores_streams_older_than_the_delivered_group() {
    let mut gate = GroupSwitchGate::new();
    // 古い Group 9 の stream が開いたまま、Group 10 の Object を配送する
    gate.begin_stream(0, 9, 90);
    gate.begin_stream(0, 10, 100);
    assert_eq!(gate.admit(0, 10, NOW_US), DELIVER);

    // Group 9 の stream はまだ開いているが、Group 11 の Object は Group 10 の stream だけで
    // 判定する
    assert_eq!(
        gate.admit(0, 11, NOW_US + 1_000),
        hold(NOW_US + 1_000 + GROUP_SWITCH_HOLD_US),
        "配信済みより前の Group の stream ではなく、配信済みの Group の stream で保留すること"
    );

    // Group 10 の stream が終端すれば、Group 9 が開いていても配送になる
    gate.end_stream(0, 10, 100);
    assert_eq!(
        gate.admit(0, 11, NOW_US + 1_000),
        DELIVER,
        "配信済みの Group の stream が終端したら、古い Group の stream が開いていても配送すること"
    );
    // Group 11 を配送したので、Group 9 と Group 10 の stream は以後の判定に使わない
    assert_eq!(
        gate.admit(0, 12, NOW_US + 2_000),
        DELIVER,
        "配送済みの Group より前の stream は以後の判定に使わないこと"
    );
}

/// 1 Group が複数の stream で届く場合、すべて終わるまで保留すること
#[test]
fn holds_until_every_stream_of_the_group_ends() {
    let mut gate = GroupSwitchGate::new();
    gate.begin_stream(0, 10, 100);
    gate.begin_stream(0, 10, 101);
    assert_eq!(gate.admit(0, 10, NOW_US), DELIVER);
    assert_eq!(
        gate.admit(0, 11, NOW_US),
        hold(NOW_US + GROUP_SWITCH_HOLD_US),
        "1 Group に 2 本の stream があるとき、まだ両方開いていれば保留すること"
    );

    gate.end_stream(0, 10, 100);
    assert_eq!(
        gate.admit(0, 11, NOW_US),
        hold(NOW_US + GROUP_SWITCH_HOLD_US),
        "1 本が終端しても、もう 1 本が開いていれば保留すること"
    );

    gate.end_stream(0, 10, 101);
    assert_eq!(
        gate.admit(0, 11, NOW_US),
        DELIVER,
        "すべての stream が終端したら保留を解除すること"
    );
}

/// Group ID が飛んでいる場合、飛ぶ前の Group の stream の終端まで待つこと
///
/// 飛んだ Group の stream は存在しないため待つ対象にならない。飛ぶ前の Group の stream が
/// 開いていれば、その終端までは待つ。
#[test]
fn waits_for_the_group_before_a_skipped_group() {
    let mut gate = GroupSwitchGate::new();
    gate.begin_stream(0, 10, 100);
    assert_eq!(gate.admit(0, 10, NOW_US), DELIVER);

    // Group 11 から 14 が無く、Group 15 の Object が届く。Group 10 の stream の終端までは待つ
    assert_eq!(
        gate.admit(0, 15, NOW_US),
        hold(NOW_US + GROUP_SWITCH_HOLD_US),
        "飛ぶ前の Group の stream が開いている間は、飛んだ先の Group も保留すること"
    );
    gate.end_stream(0, 10, 100);
    assert_eq!(
        gate.admit(0, 15, NOW_US),
        DELIVER,
        "飛ぶ前の Group の stream が終端したら配送すること"
    );
}

/// `reset` で状態が消え、旧世代の呼び出しが新しい世代の判定に影響しないこと
///
/// session 移行では旧 session の stream task を止めきれないことがある。旧い task が
/// `begin_stream` を呼んでも、新しい購読の保留判定に使ってはならない。
#[test]
fn reset_clears_the_state_and_ignores_old_generations() {
    let mut gate = GroupSwitchGate::new();
    gate.begin_stream(0, 10, 100);
    assert_eq!(gate.admit(0, 10, NOW_US), DELIVER);
    assert_eq!(
        gate.admit(0, 11, NOW_US),
        hold(NOW_US + GROUP_SWITCH_HOLD_US)
    );

    // 移行の直前に世代を進めて初期化する
    gate.reset(1);

    // 旧世代の admit は状態を変えずに配送を返し、配送済みの最大を進めない
    assert_eq!(
        gate.admit(0, 21, NOW_US),
        DELIVER,
        "旧世代の admit は状態を変えずに配送を返すこと"
    );

    // 新世代では、旧世代で開いていた stream も保留の期限も残っていない
    assert_eq!(
        gate.admit(1, 11, NOW_US),
        DELIVER,
        "reset で保留の期限を捨てること (旧世代の保留を引きずらない)"
    );

    // 旧世代の begin_stream が新しい世代の stream として登録されないこと。登録されれば
    // Group 13 の stream が開いているものとして Group 14 の Object が保留になる
    gate.begin_stream(0, 13, 999);
    assert_eq!(
        gate.admit(1, 14, NOW_US),
        DELIVER,
        "旧世代の begin_stream を新しい世代の判定に使わないこと"
    );

    // 新世代で Group 14 の stream を開くと保留になり、旧世代の end_stream では解除されない
    gate.begin_stream(1, 14, 140);
    assert_eq!(
        gate.admit(1, 15, NOW_US),
        hold(NOW_US + GROUP_SWITCH_HOLD_US),
        "reset 後の新しい世代では通常どおり保留すること"
    );
    gate.end_stream(0, 14, 140);
    assert_eq!(
        gate.admit(1, 15, NOW_US),
        hold(NOW_US + GROUP_SWITCH_HOLD_US),
        "旧世代の end_stream で新しい世代の stream を消さないこと"
    );

    // 新しい世代の end_stream で解除される
    gate.end_stream(1, 14, 140);
    assert_eq!(
        gate.admit(1, 15, NOW_US),
        DELIVER,
        "新しい世代の end_stream で保留を解除すること"
    );
}
