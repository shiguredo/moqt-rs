//! `VideoDecodeOrder` の復号順判定の PBT
//!
//! publisher が送る映像 Object 列をモデルとして生成し、損失・到着順の入れ替え・重複を
//! 加えた受信列を判定に通す。復号すると判定した Object 列が、参照するフレームを欠かさずに
//! 復号できる列になっていることを確かめる。
//!
//! - 各 Group の先頭 (Object ID 0) はキーフレームであり、以降は直前の Object を参照する
//!   delta である
//! - publisher は Object ID を飛ばすことがあり、飛ばした数は次の Object の Prior Object ID
//!   Gap (draft-ietf-moq-transport-22 §10.9 (Prior Object ID Gap)) で示す
//! - 経路では任意の Object が失われ、到着順は任意に入れ替わり、同じ Object がもう一度届く
//!   ことがある (draft-ietf-moq-transport-22 §2.1.2 (Object States): "Since Objects can be
//!   delivered out of order")
//!
//! 検証する性質は次の 5 つである。
//!
//! - 復号した Object の Group は単調に増える
//! - 同じ Group で復号した delta の Object ID は単調に増える (キーフレームは参照を持たない
//!   ため、遅れて届いた Group の先頭から復号を始め直す場合だけ前後関係を縛らない)
//! - 復号する delta は、直前に復号した Object を参照している (参照先を欠いた delta を
//!   復号しない)
//! - 欠落を検出したらキーフレームを復号するまで delta を復号しない
//! - `Stale` / `MissingReference` の判定が状態と矛盾しない
//!
//! あわせて、判定に渡す Prior Object ID Gap が Object Properties からそのまま読めることも
//! 確かめる。
//!
//! 規則の分岐ごとに到達を数え、どの性質も空振りしていないことを確かめる。

use std::cell::{Cell, RefCell};

use pbt::common::{sample_varint, test_runner};
use shiguredo_moqt::loc::PROP_VIDEO_CONFIG;
use shiguredo_moqt::object_properties::{
    ObjectProperties, ObjectProperty, ObjectPropertyValue, PROP_PRIOR_OBJECT_ID_GAP,
};
use shiguredo_moqt::track_properties::PROP_OBJECT_DELIVERY_TIMEOUT;
use shiguredo_moqt::video_decode_order::{
    VideoDecodeOrder, VideoObjectAdmission, VideoObjectPosition, VideoObjectSkipReason,
    prior_object_id_gap_of,
};

/// 復号してよい
const DECODE: VideoObjectAdmission = VideoObjectAdmission::Decode;

/// 1 Group の Object 数の境界 (空 / 1 個 / 上限)
///
/// 空の Group は「Group の先頭が届かない」場合を作り、上限は長い Group で
/// Group 内の連続を長く保つ。
const OBJECT_COUNT_BOUNDARIES: [usize; 3] = [0, 1, 8];

/// 生成する Group 数の境界
const GROUP_COUNT_BOUNDARIES: [usize; 3] = [1, 2, 5];

/// 境界値を選ぶ確率 (1/4)。残りは一様に生成する
fn boundary_ratio() -> noprop::Ratio {
    noprop::Ratio::one_nth(4)
}

/// Object を 1 つ受信する確率 (損失は 1/5)
const RECEIVE_RATIO: noprop::Ratio = noprop::Ratio::new(4, 5);

/// 重複 (同じ Object の再送) を 1 つ足す確率
const DUPLICATE_RATIO: noprop::Ratio = noprop::Ratio::new(1, 4);

/// publisher が 1 つの Object で飛ばす Object ID の最大数
const MAX_SKIP: u64 = 2;

/// publisher が送った 1 Object
#[derive(Debug, Clone, Copy)]
struct PublishedObject {
    /// 判定に渡す位置と種別
    position: VideoObjectPosition,
    /// 同じ Group で、この Object が参照する直前の Object の Object ID
    ///
    /// 先頭の Object (キーフレーム) は他のフレームを参照しないため `None`。
    previous_existing_object_id: Option<u64>,
}

/// `VideoDecodeOrder` の状態 (公開アクセサで読める範囲)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OrderState {
    /// 復号中の Group
    decoding_group_id: Option<u64>,
    /// 直前に復号した Object ID
    last_decoded_object_id: Option<u64>,
    /// キーフレームを待っているか
    awaiting_key_frame: bool,
}

/// 状態を読む
fn order_state(order: &VideoDecodeOrder) -> OrderState {
    OrderState {
        decoding_group_id: order.decoding_group_id(),
        last_decoded_object_id: order.last_decoded_object_id(),
        awaiting_key_frame: order.awaiting_key_frame(),
    }
}

/// 1 ステップ分の観測 (受信した Object、判定、判定前後の状態)
#[derive(Debug, Clone, Copy)]
struct Step {
    /// 判定に渡した Object
    object: PublishedObject,
    /// 判定の結果
    admission: VideoObjectAdmission,
    /// 判定前の状態
    before: OrderState,
    /// 判定後の状態
    after: OrderState,
}

/// 受信列を順に判定し、判定前後の状態つきで返す
fn run_received(received: &[PublishedObject]) -> Vec<Step> {
    let mut order = VideoDecodeOrder::new();
    let mut steps = Vec::new();
    for object in received {
        let before = order_state(&order);
        let admission = order.admit(&object.position);
        let after = order_state(&order);
        steps.push(Step {
            object: *object,
            admission,
            before,
            after,
        });
    }
    steps
}

/// 1 Group 分の Object 列を生成する
///
/// 先頭は Object ID 0 のキーフレームであり、飛ばしは無い。以降の Object は直前の Object を
/// 参照する delta で、飛ばした Object ID の数 (0 から `MAX_SKIP`) を Prior Object ID Gap と
/// して示す (飛ばした Object は存在しない)。
fn sample_group(ctx: &mut noprop::TestCaseContext, group_id: u64) -> Vec<PublishedObject> {
    let mut objects = vec![PublishedObject {
        position: VideoObjectPosition {
            group_id,
            object_id: 0,
            is_key_frame: true,
            prior_object_id_gap: 0,
        },
        previous_existing_object_id: None,
    }];
    let mut object_id = 0u64;
    let count =
        noprop::sample_with_boundaries(ctx, &OBJECT_COUNT_BOUNDARIES, boundary_ratio(), |ctx| {
            noprop::sample_usize_in(ctx, 0..=OBJECT_COUNT_BOUNDARIES[2])
        });
    for _ in 0..count {
        let previous_existing_object_id = object_id;
        let skip = noprop::sample_u64_in(ctx, 0..=MAX_SKIP);
        object_id = object_id.saturating_add(skip).saturating_add(1);
        objects.push(PublishedObject {
            position: VideoObjectPosition {
                group_id,
                object_id,
                is_key_frame: false,
                prior_object_id_gap: skip,
            },
            previous_existing_object_id: Some(previous_existing_object_id),
        });
    }
    objects
}

/// 連続する Group の Object 列を生成する (Group ID は 0 から順に振る)
fn sample_published(ctx: &mut noprop::TestCaseContext) -> Vec<PublishedObject> {
    let group_count =
        noprop::sample_with_boundaries(ctx, &GROUP_COUNT_BOUNDARIES, boundary_ratio(), |ctx| {
            noprop::sample_usize_in(ctx, 1..=GROUP_COUNT_BOUNDARIES[2])
        });
    let mut published = Vec::new();
    for group_id in 0..group_count as u64 {
        published.extend(sample_group(ctx, group_id));
    }
    published
}

/// 損失・到着順の入れ替え・重複を加えた受信列を生成する
///
/// 経路で起きることをそのまま作る。入れ替えは Fisher-Yates で行うため、前の Group の
/// Object が次の Group の Object より後に届く列も作れる。
fn sample_received(
    ctx: &mut noprop::TestCaseContext,
    published: &[PublishedObject],
) -> Vec<PublishedObject> {
    // 損失
    let mut received: Vec<PublishedObject> = published
        .iter()
        .filter(|_| noprop::sample_ratio(ctx, RECEIVE_RATIO))
        .copied()
        .collect();

    // 到着順の入れ替え (Fisher-Yates)
    for i in (1..received.len()).rev() {
        let j = noprop::sample_usize_in(ctx, 0..=i);
        received.swap(i, j);
    }

    // 同じ Object の再送
    if noprop::sample_ratio(ctx, DUPLICATE_RATIO) && !received.is_empty() {
        let index = noprop::sample_usize_in(ctx, 0..received.len());
        let at = noprop::sample_usize_in(ctx, 0..=received.len());
        received.insert(at, received[index]);
    }

    received
}

/// 判定がどの規則で決まったか (カバレッジゲート用)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Branch {
    /// 復号中の Group より古い Group のため古いと判定した
    StaleOlderGroup,
    /// 同じ Group で直前に復号した Object 以前の Object ID のため古いと判定した
    StaleDuplicate,
    /// キーフレームを通して復号を始めた
    DecodeKeyFrame,
    /// 直前の Object の次の Object ID の delta を通した
    DecodeContinuousDelta,
    /// Prior Object ID Gap が覆う欠けを飛ばして delta を通した
    DecodeDeltaCoveredByPriorObjectIdGap,
    /// 復号中の Group が無い、または復号中の Group より新しい Group の delta を捨てた
    MissingReferenceNewGroupDelta,
    /// キーフレームを待っている間の delta を捨てた
    MissingReferenceAwaitingKeyFrame,
    /// Prior Object ID Gap を超える欠けを検出して delta を捨てた
    MissingReferenceGapExceedsPriorObjectIdGap,
}

/// 分岐の数 (ゲートの配列の長さ)
const BRANCH_COUNT: usize = 8;

/// 観測した判定を説明できる規則を選ぶ。どの規則でも説明できない場合は `None`
///
/// 判定そのものは写さず、判定と判定前後の状態から「その判定を説明できる規則」を選ぶ。
/// `None` は判定が状態と矛盾していることを意味する。
fn branch_of(step: &Step) -> Option<Branch> {
    let position = step.object.position;
    let last_decoded_object_id = step.before.last_decoded_object_id;
    // 直前に復号した Object との間にある、復号していない Object の数
    let skipped = last_decoded_object_id
        .map(|last| position.object_id.saturating_sub(last).saturating_sub(1));

    match step.admission {
        VideoObjectAdmission::Decode => {
            // 復号した後は「この Object を復号した」状態になっている
            if step.after.last_decoded_object_id != Some(position.object_id)
                || step.after.awaiting_key_frame
            {
                return None;
            }
            if position.is_key_frame {
                return Some(Branch::DecodeKeyFrame);
            }
            // delta を通すのは同じ Group で、欠けが Prior Object ID Gap に収まるときだけ
            if step.before.decoding_group_id != Some(position.group_id)
                || step.before.awaiting_key_frame
            {
                return None;
            }
            match skipped? {
                0 => Some(Branch::DecodeContinuousDelta),
                skipped if skipped <= position.prior_object_id_gap => {
                    Some(Branch::DecodeDeltaCoveredByPriorObjectIdGap)
                }
                _ => None,
            }
        }
        VideoObjectAdmission::Skip {
            reason: VideoObjectSkipReason::Stale,
        } => {
            // 古いと判定しても状態は変えない
            if step.after != step.before {
                return None;
            }
            if step
                .before
                .decoding_group_id
                .is_some_and(|decoding_group_id| position.group_id < decoding_group_id)
            {
                Some(Branch::StaleOlderGroup)
            } else if step.before.decoding_group_id == Some(position.group_id)
                && last_decoded_object_id.is_some_and(|last| position.object_id <= last)
            {
                Some(Branch::StaleDuplicate)
            } else {
                None
            }
        }
        VideoObjectAdmission::Skip {
            reason: VideoObjectSkipReason::MissingReference,
        } => {
            // 欠落を検出した後はキーフレームを待ち、欠落を検出した Group を保持する
            if !step.after.awaiting_key_frame
                || step.after.decoding_group_id != Some(position.group_id)
            {
                return None;
            }
            if step
                .before
                .decoding_group_id
                .is_none_or(|decoding_group_id| position.group_id > decoding_group_id)
            {
                Some(Branch::MissingReferenceNewGroupDelta)
            } else if step.before.awaiting_key_frame || last_decoded_object_id.is_none() {
                Some(Branch::MissingReferenceAwaitingKeyFrame)
            } else if skipped.is_some_and(|skipped| skipped > position.prior_object_id_gap) {
                Some(Branch::MissingReferenceGapExceedsPriorObjectIdGap)
            } else {
                None
            }
        }
    }
}

/// 復号した Object の Group は単調に増える
///
/// 次の Group のキーフレームを復号した後に前の Group の Object を復号しない
/// (draft-ietf-moq-transport-22 §2.1.2)。
#[test]
fn decoded_groups_are_non_decreasing() -> noprop::TestResult {
    // ゲート: Group が進む復号 (新しい Group のキーフレーム) と、同じ Group の復号の
    // 両方が観測されること。どちらかが 0 なら性質を空振りしたまま通ってしまう。
    // 古い Group の判定は、入れ替えで前の Group の Object が後ろに回ることで起きる
    // (Group 数は 1..=5、受信は 4/5 なので、2 Group 以上の列でほぼ確実に起きる)。
    let group_advanced = Cell::new(0usize);
    let same_group_continued = Cell::new(0usize);
    let stale_older_group = Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let published = sample_published(ctx);
        let received = sample_received(ctx, &published);
        let mut last_decoded_group_id: Option<u64> = None;
        for step in &run_received(&received) {
            match step.admission {
                VideoObjectAdmission::Decode => {
                    let group_id = step.object.position.group_id;
                    if let Some(previous_group_id) = last_decoded_group_id {
                        assert!(
                            group_id >= previous_group_id,
                            "復号した Object の Group は単調に増えること: \
                             previous={previous_group_id} group={group_id} step={step:?}"
                        );
                        if group_id > previous_group_id {
                            group_advanced.set(group_advanced.get() + 1);
                        } else {
                            same_group_continued.set(same_group_continued.get() + 1);
                        }
                    }
                    last_decoded_group_id = Some(group_id);
                }
                VideoObjectAdmission::Skip {
                    reason: VideoObjectSkipReason::Stale,
                } => {
                    // 古いと判定した根拠が復号中の Group であることを数える
                    let position = step.object.position;
                    if step
                        .before
                        .decoding_group_id
                        .is_some_and(|decoding_group_id| position.group_id < decoding_group_id)
                    {
                        stale_older_group.set(stale_older_group.get() + 1);
                    }
                }
                VideoObjectAdmission::Skip {
                    reason: VideoObjectSkipReason::MissingReference,
                } => {}
            }
        }
        Ok(())
    })?;
    assert!(
        group_advanced.get() > 0,
        "新しい Group のキーフレームで Group が進む復号が観測されなかった\n{runner}"
    );
    assert!(
        same_group_continued.get() > 0,
        "同じ Group の復号が観測されなかった\n{runner}"
    );
    assert!(
        stale_older_group.get() > 0,
        "復号中の Group より古い Group の判定が観測されなかった\n{runner}"
    );
    Ok(())
}

/// 同じ Group で復号した delta の Object ID は単調に増える
///
/// delta は直前の Object を参照するため、同じ Group の delta を復号すると Object ID は必ず
/// 直前の復号より後ろになる。キーフレームは参照を持たないため、Group の先頭が遅れて届いた
/// 場合は同じ Group でも前より前の Object ID から復号を始め直す (キーフレーム自体は安全に
/// 復号でき、表示順はタイムスタンプで決まる)。そのためキーフレームの Object ID は
/// 前後関係を縛らない。この例外は
/// `tests/test_video_decode_order.rs::keyframe_after_a_gap_restarts_from_an_earlier_object_id`
/// が固定する (任意の列では重複したキーフレームが復号される場合だけ起き、まれである)。
#[test]
fn decoded_delta_object_ids_increase_within_a_group() -> noprop::TestResult {
    // ゲート: 同じ Group で delta を続けて復号した列が観測されること。これが 0 なら
    // 増加の検証を一度も行っていない
    let delta_advanced = Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let published = sample_published(ctx);
        let received = sample_received(ctx, &published);
        let mut last_decoded: Option<(u64, u64)> = None;
        for step in &run_received(&received) {
            if step.admission != DECODE {
                continue;
            }
            let group_id = step.object.position.group_id;
            let object_id = step.object.position.object_id;
            if let Some((previous_group_id, previous_object_id)) = last_decoded
                && previous_group_id == group_id
                && !step.object.position.is_key_frame
            {
                assert!(
                    object_id > previous_object_id,
                    "同じ Group で復号した delta の Object ID は単調に増えること: \
                     previous={previous_object_id} object={object_id} step={step:?}"
                );
                delta_advanced.set(delta_advanced.get() + 1);
            }
            last_decoded = Some((group_id, object_id));
        }
        Ok(())
    })?;
    assert!(
        delta_advanced.get() > 0,
        "同じ Group で delta を続けて復号した列が観測されなかった\n{runner}"
    );
    Ok(())
}

/// 復号する delta は、直前に復号した Object を参照している
///
/// decoder は delta の参照先を復号済みでなければならない。損失・入れ替え・重複があっても、
/// 参照先を欠いた delta を復号しないことを確かめる。
#[test]
fn decoded_deltas_reference_the_previously_decoded_object() -> noprop::TestResult {
    // ゲート: キーフレームの直後の delta と、delta の直後の delta の両方が観測されること。
    // 前者は Group の先頭からの復号、後者は参照が連鎖する復号である
    let delta_after_key_frame = Cell::new(0usize);
    let delta_after_delta = Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let published = sample_published(ctx);
        let received = sample_received(ctx, &published);
        let mut previous_decoded: Option<Step> = None;
        for step in run_received(&received) {
            if step.admission != DECODE {
                continue;
            }
            if !step.object.position.is_key_frame {
                let Some(previous) = previous_decoded else {
                    panic!("キーフレームより前に delta を復号した: {step:?}");
                };
                assert_eq!(
                    step.object.position.group_id, previous.object.position.group_id,
                    "別の Group の delta を復号した: previous={previous:?} step={step:?}"
                );
                assert_eq!(
                    step.object.previous_existing_object_id,
                    Some(previous.object.position.object_id),
                    "参照先を復号していない delta を復号した: previous={previous:?} step={step:?}"
                );
                if previous.object.position.is_key_frame {
                    delta_after_key_frame.set(delta_after_key_frame.get() + 1);
                } else {
                    delta_after_delta.set(delta_after_delta.get() + 1);
                }
            }
            previous_decoded = Some(step);
        }
        Ok(())
    })?;
    assert!(
        delta_after_key_frame.get() > 0,
        "キーフレームの直後の delta の復号が観測されなかった\n{runner}"
    );
    assert!(
        delta_after_delta.get() > 0,
        "delta の直後の delta の復号が観測されなかった\n{runner}"
    );
    Ok(())
}

/// 欠落を検出したら、キーフレームを復号するまで delta を復号しない
///
/// 参照するフレームが欠けた delta は、キーフレームで参照の連鎖を切り直すまで復号しない
/// (draft-ietf-moq-transport-22 §2.1.2)。
#[test]
fn missing_reference_blocks_delta_until_a_key_frame() -> noprop::TestResult {
    // ゲート: 欠落の検出と、キーフレームでの復帰の両方が観測されること。復帰が 0 なら
    // 「キーフレームまで」の後半を検証していない
    let blocked_count = Cell::new(0usize);
    let resumed_by_key_frame = Cell::new(0usize);
    let delta_after_resume = Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let published = sample_published(ctx);
        let received = sample_received(ctx, &published);
        let mut blocked = false;
        let mut resumed = false;
        for step in run_received(&received) {
            match step.admission {
                VideoObjectAdmission::Skip {
                    reason: VideoObjectSkipReason::MissingReference,
                } => {
                    blocked = true;
                    blocked_count.set(blocked_count.get() + 1);
                }
                VideoObjectAdmission::Decode => {
                    if step.object.position.is_key_frame {
                        if blocked {
                            resumed = true;
                            resumed_by_key_frame.set(resumed_by_key_frame.get() + 1);
                        }
                        blocked = false;
                    } else {
                        assert!(
                            !blocked,
                            "欠落を検出した後はキーフレームを復号するまで delta を復号しないこと: {step:?}"
                        );
                        if resumed {
                            delta_after_resume.set(delta_after_resume.get() + 1);
                            resumed = false;
                        }
                    }
                }
                VideoObjectAdmission::Skip {
                    reason: VideoObjectSkipReason::Stale,
                } => {}
            }
        }
        Ok(())
    })?;
    assert!(
        blocked_count.get() > 0,
        "欠落の検出が観測されなかった\n{runner}"
    );
    assert!(
        resumed_by_key_frame.get() > 0,
        "キーフレームで復帰した復号が観測されなかった\n{runner}"
    );
    assert!(
        delta_after_resume.get() > 0,
        "復帰した後の delta の復号が観測されなかった\n{runner}"
    );
    Ok(())
}

/// `Stale` / `MissingReference` の判定が状態と矛盾しない
///
/// 観測した判定が、判定前後の状態から説明できる規則を持つこと (`Stale` は状態を変えず、
/// `MissingReference` はキーフレーム待ちになって欠落を検出した Group を保持し、`Decode` は
/// 「この Object を復号した」状態になる) を確かめる。規則の分岐ごとに到達を数え、
/// 1 つでも到達しなければその規則を検証しないまま通ってしまう。
#[test]
fn admission_reasons_match_the_observed_state() -> noprop::TestResult {
    // 分岐の重み (到達の見込み):
    // - 古い Group / 同じ Group の重複: 入れ替えと重複 (1/4) で起きる
    // - キーフレームでの開始: 各 Group の先頭 (Group 数 1..=5)
    // - 連続した delta: 損失 (1/5) が無い列
    // - Prior Object ID Gap が覆う欠け: 飛ばし (0..=2) が gap として届く列
    // - 新しい Group の delta / キーフレーム待ちの delta / gap を超える欠け:
    //   損失と入れ替えで参照が欠けた列
    // 分岐の到達を数えるゲートはケースをまたいで集計するため `RefCell` を使う
    // (プロパティは `Fn` であり、外側の配列を直接書き換えられない)
    let counts = RefCell::new([0usize; BRANCH_COUNT]);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let published = sample_published(ctx);
        let received = sample_received(ctx, &published);
        let mut order = VideoDecodeOrder::new();
        for object in &received {
            let before = order_state(&order);
            let admission = order.admit(&object.position);
            let after = order_state(&order);
            let step = Step {
                object: *object,
                admission,
                before,
                after,
            };
            let Some(branch) = branch_of(&step) else {
                panic!("判定が規則と状態から説明できない: {step:?}");
            };
            counts.borrow_mut()[branch as usize] += 1;
        }
        Ok(())
    })?;
    let counts = counts.into_inner();
    for (branch, count) in [
        (
            Branch::StaleOlderGroup,
            counts[Branch::StaleOlderGroup as usize],
        ),
        (
            Branch::StaleDuplicate,
            counts[Branch::StaleDuplicate as usize],
        ),
        (
            Branch::DecodeKeyFrame,
            counts[Branch::DecodeKeyFrame as usize],
        ),
        (
            Branch::DecodeContinuousDelta,
            counts[Branch::DecodeContinuousDelta as usize],
        ),
        (
            Branch::DecodeDeltaCoveredByPriorObjectIdGap,
            counts[Branch::DecodeDeltaCoveredByPriorObjectIdGap as usize],
        ),
        (
            Branch::MissingReferenceNewGroupDelta,
            counts[Branch::MissingReferenceNewGroupDelta as usize],
        ),
        (
            Branch::MissingReferenceAwaitingKeyFrame,
            counts[Branch::MissingReferenceAwaitingKeyFrame as usize],
        ),
        (
            Branch::MissingReferenceGapExceedsPriorObjectIdGap,
            counts[Branch::MissingReferenceGapExceedsPriorObjectIdGap as usize],
        ),
    ] {
        assert!(count > 0, "分岐 {branch:?} に到達しなかった\n{runner}");
    }
    Ok(())
}

/// 損失も入れ替えも重複も無ければ、すべての Object を復号する
///
/// Prior Object ID Gap が示す Object ID の飛びは欠落ではないため、どこでも止まらない。
/// 経路の乱れがある列だけでは「判定が厳しすぎる」退行を検出できない。
#[test]
fn without_loss_or_reorder_every_object_is_decoded() -> noprop::TestResult {
    // ゲート: 複数の Group と、Prior Object ID Gap が覆う飛びを含む列が観測されること。
    // どちらも無ければ自明な列だけを検証している
    let multi_group = Cell::new(0usize);
    let skipped_by_publisher = Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let published = sample_published(ctx);
        let mut groups = Vec::new();
        for object in &published {
            if !groups.contains(&object.position.group_id) {
                groups.push(object.position.group_id);
            }
            if object.position.prior_object_id_gap > 0 {
                skipped_by_publisher.set(skipped_by_publisher.get() + 1);
            }
        }
        if groups.len() >= 2 {
            multi_group.set(multi_group.get() + 1);
        }
        let steps = run_received(&published);
        let decoded = steps.iter().filter(|step| step.admission == DECODE).count();
        assert_eq!(
            decoded,
            published.len(),
            "損失も入れ替えも無ければすべての Object を復号すること: published={published:?}"
        );
        Ok(())
    })?;
    assert!(
        multi_group.get() > 0,
        "複数の Group を持つ列が観測されなかった\n{runner}"
    );
    assert!(
        skipped_by_publisher.get() > 0,
        "Prior Object ID Gap が覆う飛びを持つ列が観測されなかった\n{runner}"
    );
    Ok(())
}

/// Object Properties に書いた Prior Object ID Gap をそのまま読み戻す
///
/// Property が無い Object は 0 として扱う (draft-ietf-moq-transport-22 §10.9 (Prior Object ID
/// Gap))。判定に使う値が Property から取れなくなると、欠落の判定がすべて欠落側に倒れる。
#[test]
fn prior_object_id_gap_roundtrips_through_object_properties() -> noprop::TestResult {
    // ゲート: Property を持つ Object と持たない Object の両方が観測されること。
    // 片方だけでは「0 を返す」か「値を返す」のどちらかしか検証していない
    let with_property = Cell::new(0usize);
    let without_property = Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let mut properties = ObjectProperties::new();
        let gap = if noprop::sample_ratio(ctx, noprop::Ratio::new(3, 4)) {
            with_property.set(with_property.get() + 1);
            let gap = sample_varint(ctx);
            properties.push(ObjectProperty {
                prop_type: PROP_PRIOR_OBJECT_ID_GAP,
                value: ObjectPropertyValue::VarInt(gap),
            });
            Some(gap)
        } else {
            without_property.set(without_property.get() + 1);
            None
        };
        // LOC の Public Properties は同じブロックに載るため、他の Property を混ぜても
        // Prior Object ID Gap を読み戻せること
        for (prop_type, is_varint) in [
            (PROP_OBJECT_DELIVERY_TIMEOUT, true),
            (PROP_VIDEO_CONFIG, false),
        ] {
            if !noprop::sample_ratio(ctx, noprop::Ratio::one_nth(3)) {
                continue;
            }
            properties.push(ObjectProperty {
                prop_type,
                value: if is_varint {
                    ObjectPropertyValue::VarInt(sample_varint(ctx))
                } else {
                    let len = noprop::sample_usize_in(ctx, 1..=8);
                    ObjectPropertyValue::Bytes(noprop::sample_bytes_vec(ctx, len))
                },
            });
        }

        let mut bytes = Vec::new();
        properties
            .encode(&mut bytes)
            .expect("正当なテスト入力の encode は成功する");
        assert_eq!(
            prior_object_id_gap_of(Some(&bytes)),
            Ok(gap.unwrap_or(0)),
            "Prior Object ID Gap をそのまま読み戻すこと: gap={gap:?} properties={properties:?}"
        );
        Ok(())
    })?;
    assert!(
        with_property.get() > 0,
        "PRIOR_OBJECT_ID_GAP を持つ Object が観測されなかった\n{runner}"
    );
    assert!(
        without_property.get() > 0,
        "PRIOR_OBJECT_ID_GAP を持たない Object が観測されなかった\n{runner}"
    );
    Ok(())
}
