//! 映像 Group の長さを時間ベースで決める判定
//!
//! 映像は 1 Group = 1 unidirectional stream で送り、Group はキーフレームで始まる
//! (draft-ietf-moq-loc-04 §4.2)。Group 長をフレーム数で指定すると、実時間長が fps に
//! よって変わり (fps 5 で 12 秒 / fps 120 で 0.5 秒)、MSF カタログへ宣言する
//! `maxGopDuration` / `maxGroupDuration` (draft-ietf-moq-msf-01 §5.2.24 / §5.2.25) も
//! fps ごとの値になってしまう。そこで Group 長は時間 (`--gop-duration`) で指定し、
//! 直前のキーフレームからその時間が経過した入力フレームでキーフレームを強制する。
//!
//! 判定は入力フレームの Timestamp の差分だけで行う。live capture の Timestamp は
//! 起源が経路によって異なるため、絶対値ではなく差分を使う。
//!
//! 購読者からの `NEW_GROUP_REQUEST` (draft-ietf-moq-transport-22 §9.20.19) による Group 開始の
//! 前倒しもここで扱う。直前の Group が最小長に達するまでは前倒しせず、達した最初の入力
//! フレームでキーフレームを要求する (同節が認める遅延)。`--gop-duration` による通常の境界が
//! 先に来た場合は、そこで Group が始まって要求は満たされる。

/// `--gop-duration` のミリ秒をマイクロ秒へ換算する
///
/// 入力フレームの Timestamp はマイクロ秒であるため、判定の前に単位を揃える。
/// `u32::MAX × 1000` は `u64` に収まるため桁あふれしない。
fn gop_duration_us(gop_duration_ms: u32) -> u64 {
    u64::from(gop_duration_ms) * 1_000
}

/// `NEW_GROUP_REQUEST` に応じてキーフレームを要求するときの最小 Group 長 (マイクロ秒)
///
/// draft-ietf-moq-transport-22 §9.20.19 は、Group 長の最小値を実現するなどの実装固有の
/// 理由で `NEW_GROUP_REQUEST` を遅延させてよい (MAY) と定める。連続する要求で
/// キーフレームが乱発しないよう、直前の Group がこの長さに達するまでは要求を保留する。
const MIN_GROUP_DURATION_US: u64 = 300_000;

/// `NEW_GROUP_REQUEST` の値を受け付けるかどうかを返す
///
/// draft-ietf-moq-transport-22 §9.20.19 は、値が 0 (購読者が Group の情報を持たない) か、
/// 現在の Group より大きい場合に、現在の Group を終えて新しい Group を開始することを
/// SHOULD とする。`current_group_id` は直前に開始した Group の ID であり、まだ Group を
/// 開始していない場合は `None`。
pub(crate) fn accepts_new_group_request(next_group_id: u64, current_group_id: Option<u64>) -> bool {
    match current_group_id {
        None => true,
        Some(current_group_id) => next_group_id == 0 || next_group_id > current_group_id,
    }
}

/// 映像 Group の開始時刻を追跡し、キーフレームを強制する入力フレームを決める
///
/// 基準は「Group を開始した出力フレーム」を観測するたびに更新する。時間ベースの要求で
/// 出たキーフレームだけでなく、エンコーダがフレーム数の上限で自主的に出したキーフレーム
/// でも更新しないと、その直後に時間ベースの要求が重なって 1 フレームの Group ができる
/// (実カデンスが公称 fps より速いと上限の方が先に発火する)。
pub(crate) struct GopTracker {
    /// 現在の Group を開始した入力フレームの Timestamp (マイクロ秒)
    ///
    /// まだ 1 つも Group を開始していない場合 (エンコーダの最初の出力を待っている間) は
    /// `None`。
    group_start_us: Option<u64>,
    /// キーフレームを要求済みで、その出力をまだ観測していないかどうか
    ///
    /// 観測するまでの再要求を抑える。
    pending_keyframe: bool,
    /// `NEW_GROUP_REQUEST` により、新しい Group の開始を予約しているかどうか
    ///
    /// 実際の要求は次の入力フレームで行う。直前の Group が最小長に達するまでは保留する。
    new_group_request: bool,
    /// Group 長のしきい値 (マイクロ秒)
    gop_duration_us: u64,
}

impl GopTracker {
    /// Group 長 (ms) から追跡を開始する
    ///
    /// `gop_duration_ms` は 1 以上であること (CLI が 0 を拒否する)。0 を渡すと
    /// 2 フレーム目以降のすべてのフレームでキーフレームを要求する状態になる。
    pub(crate) fn new(gop_duration_ms: u32) -> Self {
        Self {
            group_start_us: None,
            pending_keyframe: false,
            new_group_request: false,
            gop_duration_us: gop_duration_us(gop_duration_ms),
        }
    }

    /// 次の入力フレームでキーフレームを強制すべきかを判定し、要求したことを記録する
    ///
    /// まだ Group を開始していない場合は要求しない。エンコーダは最初のフレームで必ず
    /// キーフレームを返すため先頭の Group は始まっており、基準が無い状態で要求すると
    /// Group が 1 フレームになる。
    /// `timestamp_us` が基準より小さい場合 (入力の時刻が巻き戻った場合) は経過時間を
    /// 0 として扱い、要求しない。
    ///
    /// [`Self::request_new_group`] で予約された要求は、直前の Group が
    /// [`MIN_GROUP_DURATION_US`] に達した最初の呼び出しで要求になり、達するまでは保留する
    /// (保留した要求は破棄しない)。
    ///
    /// 要求したキーフレームを観測できないままエンコーダがキーフレームを出さなくなった場合、
    /// Group を閉じるのはエンコーダのフレーム数上限だけになり、その実時間長は実際の
    /// フレーム間隔に依存する。キーフレームを返さないエンコーダは異常であり、再要求すると
    /// キーフレームが連続して 1 フレームの Group ができるため、再要求はしない。
    pub(crate) fn request_keyframe_if_due(&mut self, timestamp_us: u64) -> bool {
        // エンコーダが要求したフレームの出力を同じ呼び出しで返さない場合 (出力コールバックが
        // 非同期な Video Toolbox など)、出力を観測するまで基準が古いままで、次の入力
        // フレームでも要求を繰り返してしまう。要求が重なるとキーフレームが連続し、
        // 1 フレームの Group ができる
        if self.pending_keyframe {
            return false;
        }
        // NEW_GROUP_REQUEST による要求は、直前の Group が最小長に達するまで保留する。
        // 保留している要求は破棄しない
        let new_group = self.new_group_request && self.min_group_duration_elapsed(timestamp_us);
        if !new_group && !self.is_keyframe_due(timestamp_us) {
            return false;
        }
        self.new_group_request = false;
        self.pending_keyframe = true;
        true
    }

    /// `NEW_GROUP_REQUEST` による新しい Group の開始を予約する
    ///
    /// 実際のキーフレームの要求は、直前の Group が最小長に達した後の最初の入力フレームで
    /// 行う。保留の条件は [`MIN_GROUP_DURATION_US`] を参照すること。
    pub(crate) fn request_new_group(&mut self) {
        self.new_group_request = true;
    }

    /// 直前の Group が最小長に達しているかどうか
    ///
    /// まだ Group を開始していない場合は `false` を返す。先頭の Group は最初のキーフレームで
    /// 始まっており、その出力を観測する前に要求すると 1 フレームの Group ができる。
    fn min_group_duration_elapsed(&self, timestamp_us: u64) -> bool {
        self.elapsed_us(timestamp_us)
            .is_some_and(|elapsed_us| elapsed_us >= MIN_GROUP_DURATION_US)
    }

    /// キーフレームの要求が必要かどうかだけを返す (状態は変えない)
    fn is_keyframe_due(&self, timestamp_us: u64) -> bool {
        self.elapsed_us(timestamp_us)
            .is_some_and(|elapsed_us| elapsed_us >= self.gop_duration_us)
    }

    /// 直前の Group の開始からの経過時間 (マイクロ秒)
    ///
    /// まだ Group を開始していない場合は `None`。`timestamp_us` が基準より小さい場合
    /// (入力の時刻が巻き戻った場合) は 0 として扱う。
    fn elapsed_us(&self, timestamp_us: u64) -> Option<u64> {
        self.group_start_us
            .map(|group_start_us| timestamp_us.saturating_sub(group_start_us))
    }

    /// Group を開始した出力フレームを観測し、基準をその Timestamp へ進める
    ///
    /// 戻り値は直前の Group の長さ (マイクロ秒)。最初に観測した Group では `None` になる。
    /// 呼び出し側はカタログへ宣言した値と実測を突き合わせるために使える。
    ///
    /// キーフレームの要求が未観測であれば、ここで観測済みにする。新しい Group が始まった
    /// ため、保留していた `NEW_GROUP_REQUEST` も満たされたものとして消す (残すと、最小長が
    /// 経過した時点で誰も要求していないキーフレームを強制し、短い Group が 1 つ増える)。
    pub(crate) fn observe_keyframe(&mut self, timestamp_us: u64) -> Option<u64> {
        let previous_duration_us = self.elapsed_us(timestamp_us);
        self.group_start_us = Some(timestamp_us);
        self.pending_keyframe = false;
        self.new_group_request = false;
        previous_duration_us
    }
}

/// エンコーダへ渡すキーフレーム間隔のフレーム数上限
///
/// 時間ベースの要求が効かない場合の安全弁であり、`ceil(fps × gop_duration / 1000)` で
/// 求める。公称 fps のカデンスでは、このフレーム数が `gop_duration` 以上の時間幅になる
/// (`keyframe_interval_frames(fps, gop) × 1000 ≥ fps × gop`)。切り下げると、公称 fps でも
/// 安全弁が時間ベースの要求より先に発火し得る。
///
/// 1 フレーム未満にはしない。0 を渡すとエンコーダによって `NonZeroU32` / `NonZeroUsize` が
/// `None` になり、上限を設定せず既定値に委ねるか、上限無しとして扱われる。
pub(crate) fn keyframe_interval_frames(fps: u32, gop_duration_ms: u32) -> u32 {
    // `u32::MAX × u32::MAX` でも `u64` に収まるため、この乗算は桁あふれしない
    let frames = (u64::from(fps) * u64::from(gop_duration_ms)).div_ceil(1_000);
    u32::try_from(frames).unwrap_or(u32::MAX).max(1)
}

/// MSF カタログへ宣言する `maxGopDuration` / `maxGroupDuration` (ms)
///
/// 時間ベースの判定は入力フレームの Timestamp の差分で行うため、境界の直後にフレームが
/// 無いと次の入力まで Group が伸びる。宣言は公称 fps のカデンスで生じる最大値に合わせて
/// 1 フレーム分を足す。Group は 1 キーフレーム間隔 = 1 GOP であるため、両方に同じ値を
/// 載せる (draft-ietf-moq-msf-01 §5.2.24 (Maximum GOP Duration) /
/// §5.2.25 (Maximum Group Duration))。
///
/// 入力のゆらぎが 1 フレームの切り上げ分を超える場合や、capture が止まってフレームが
/// 落ちた場合は、実測がこの値を超え得る (宣言は「公称 fps で境界の 1 フレームを見込んだ
/// 値」であり、Group 長の厳密な上限ではない)。
///
/// `fps` はエンコーダが 0 を拒否するため 1 以上になるが、除算を守るために 0 は 1 として
/// 扱う。
pub(crate) fn catalog_max_duration_ms(fps: u32, gop_duration_ms: u32) -> u64 {
    u64::from(gop_duration_ms) + 1_000u64.div_ceil(u64::from(fps.max(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// fps ごとのフレーム間隔 (マイクロ秒)
    fn frame_interval_us(fps: u32) -> u64 {
        1_000_000 / u64::from(fps)
    }

    /// fps とフレーム数から入力フレームの Timestamp の並びを作る
    fn frame_timestamps(fps: u32, frames: u64) -> Vec<u64> {
        let interval = frame_interval_us(fps);
        (0..frames).map(|index| index * interval).collect()
    }

    /// 入力フレームの並びに対してキーフレームを強制した添字を返す
    ///
    /// パイプラインと同じ規則で追跡する。先頭フレームのキーフレームで最初の Group が
    /// 始まり、強制したフレームの出力がキーフレームとして観測される。
    fn forced_indices(timestamps: &[u64], gop_duration_ms: u32) -> Vec<usize> {
        let mut tracker = GopTracker::new(gop_duration_ms);
        let mut forced = Vec::new();
        if let Some(&first_us) = timestamps.first() {
            tracker.observe_keyframe(first_us);
        }
        for (index, &timestamp_us) in timestamps.iter().enumerate() {
            if tracker.request_keyframe_if_due(timestamp_us) {
                // 強制したフレームの出力がキーフレームとして観測される
                tracker.observe_keyframe(timestamp_us);
                forced.push(index);
            }
        }
        forced
    }

    /// グループ開始時刻の並び (先頭フレームを含む) から Group の長さ (マイクロ秒) を求める
    fn group_durations_us(timestamps: &[u64], forced: &[usize]) -> Vec<u64> {
        let mut starts = vec![timestamps[0]];
        starts.extend(forced.iter().map(|&index| timestamps[index]));
        starts
            .windows(2)
            .map(|pair| pair[1] - pair[0])
            .collect::<Vec<_>>()
    }

    /// Group を開始していない間は強制しないこと
    ///
    /// エンコーダの最初の出力を待っている間に強制すると、Group が 1 フレームになる。
    #[test]
    fn no_request_before_the_first_group_starts() {
        let mut tracker = GopTracker::new(2_000);
        assert!(
            !tracker.request_keyframe_if_due(10_000_000),
            "基準となる Group 開始時刻が無ければ要求しないこと"
        );
    }

    /// 経過時間が指定未満なら要求せず、到達したら要求すること
    #[test]
    fn request_when_the_gop_duration_elapses() {
        let mut tracker = GopTracker::new(2_000);
        tracker.observe_keyframe(1_000_000);
        assert!(
            !tracker.request_keyframe_if_due(2_999_999),
            "指定した時間に達していなければ要求しないこと"
        );
        assert!(
            tracker.request_keyframe_if_due(3_000_000),
            "指定した時間ちょうどで要求すること"
        );
        // 要求した時点で基準が進むため、直後のフレームでは要求しない
        assert!(
            !tracker.request_keyframe_if_due(3_000_001),
            "要求した直後のフレームでは要求しないこと"
        );

        let mut tracker = GopTracker::new(2_000);
        tracker.observe_keyframe(1_000_000);
        assert!(
            tracker.request_keyframe_if_due(3_000_001),
            "指定した時間を過ぎたら要求すること"
        );
    }

    /// 要求したキーフレームを観測するまで同じ要求を繰り返さないこと
    ///
    /// エンコーダが要求したフレームの出力を同じ呼び出しで返さない場合でも、要求が重なって
    /// キーフレームが連続し 1 フレームの Group ができないようにする。
    #[test]
    fn the_request_is_not_repeated_until_the_keyframe_is_observed() {
        let mut tracker = GopTracker::new(2_000);
        tracker.observe_keyframe(0);
        assert!(
            tracker.request_keyframe_if_due(2_000_000),
            "指定した時間で要求すること"
        );
        assert!(
            !tracker.request_keyframe_if_due(2_033_333),
            "出力を観測するまで再要求しないこと"
        );
        assert_eq!(
            tracker.observe_keyframe(2_033_333),
            Some(2_033_333),
            "要求から出力までを含む直前の Group の長さが返ること"
        );
        assert!(
            !tracker.request_keyframe_if_due(3_000_000),
            "出力を観測した後も gop_duration が経過するまで要求しないこと"
        );
        assert!(
            tracker.request_keyframe_if_due(4_033_333),
            "出力を観測した時刻から gop_duration が経過したら要求すること"
        );
    }

    /// NEW_GROUP_REQUEST の値が 0 か現在の Group より大きいときだけ受け付けること
    ///
    /// draft-ietf-moq-transport-22 §9.20.19 は、値が 0 (購読者が Group の情報を持たない)
    /// か、現在の Group より大きい場合に新しい Group を開始することを SHOULD とする。
    #[test]
    fn accepts_new_group_request_only_for_zero_or_a_future_group() {
        assert!(
            accepts_new_group_request(0, Some(100)),
            "値 0 は受け付けること"
        );
        assert!(
            accepts_new_group_request(101, Some(100)),
            "現在の Group より大きければ受け付けること"
        );
        assert!(
            !accepts_new_group_request(100, Some(100)),
            "現在の Group と同じ値は受け入れないこと"
        );
        assert!(
            !accepts_new_group_request(99, Some(100)),
            "過去の Group への要求は受け入れないこと"
        );
        assert!(
            accepts_new_group_request(1, None),
            "まだ Group を開始していなければ受け付けること"
        );
    }

    /// NEW_GROUP_REQUEST が最小長の経過後、次の入力フレームでキーフレームを要求すること
    #[test]
    fn a_new_group_request_forces_a_keyframe_after_the_minimum_duration() {
        let mut tracker = GopTracker::new(2_000);
        tracker.observe_keyframe(0);
        tracker.request_new_group();
        assert!(
            tracker.request_keyframe_if_due(MIN_GROUP_DURATION_US),
            "NEW_GROUP_REQUEST で次の入力フレームから要求すること"
        );
        assert!(
            !tracker.request_keyframe_if_due(MIN_GROUP_DURATION_US + 33_333),
            "要求したキーフレームを観測するまで再要求しないこと"
        );
    }

    /// NEW_GROUP_REQUEST は直前の Group が最小長に達するまで保留すること
    ///
    /// 連続する要求でキーフレームが乱発しないようにする (draft-ietf-moq-transport-22
    /// §9.20.19 が認める遅延)。保留した要求は破棄しない。
    #[test]
    fn a_new_group_request_waits_for_the_minimum_group_duration() {
        let mut tracker = GopTracker::new(2_000);
        let group_start_us = 1_000_000;
        tracker.observe_keyframe(group_start_us);
        tracker.request_new_group();
        assert!(
            !tracker.request_keyframe_if_due(group_start_us + 100_000),
            "最小長に達するまでは要求しないこと"
        );
        assert!(
            !tracker.request_keyframe_if_due(group_start_us + MIN_GROUP_DURATION_US - 1),
            "最小長の 1 マイクロ秒手前では要求しないこと"
        );
        assert!(
            tracker.request_keyframe_if_due(group_start_us + MIN_GROUP_DURATION_US),
            "最小長に達した最初の入力フレームで要求すること"
        );
    }

    /// キーフレームの観測で NEW_GROUP_REQUEST が満たされること
    ///
    /// キーフレームの要求が飛行中に届いた NEW_GROUP_REQUEST は、その出力の観測で新しい
    /// Group が始まるため満たされる。保留したままにすると、新しい Group の最小長が経過した
    /// 時点で誰も要求していないキーフレームを強制し、短い Group が 1 つ増える。
    #[test]
    fn observing_a_keyframe_satisfies_a_new_group_request() {
        let mut tracker = GopTracker::new(2_000);
        tracker.observe_keyframe(0);
        assert!(
            tracker.request_keyframe_if_due(2_000_000),
            "時間ベースの要求が出ること"
        );
        tracker.request_new_group();
        assert!(
            !tracker.request_keyframe_if_due(2_000_000 + MIN_GROUP_DURATION_US),
            "要求したキーフレームを観測するまでは要求しないこと"
        );
        tracker.observe_keyframe(2_000_000);
        assert!(
            !tracker.request_keyframe_if_due(2_000_000 + MIN_GROUP_DURATION_US),
            "観測で満たされた要求を再実行しないこと"
        );
    }

    /// Group を開始する前に来た NEW_GROUP_REQUEST では要求しないこと
    ///
    /// 先頭の Group は最初のキーフレームで始まっており、その出力を観測する前に要求すると
    /// 1 フレームの Group ができる。要求は先頭のキーフレームで満たされる。
    #[test]
    fn no_request_before_the_first_group_starts_with_a_new_group_request() {
        let mut tracker = GopTracker::new(2_000);
        tracker.request_new_group();
        assert!(
            !tracker.request_keyframe_if_due(10_000_000),
            "Group を開始するまでは要求しないこと"
        );
        tracker.observe_keyframe(10_000_000);
        assert!(
            !tracker.request_keyframe_if_due(10_000_000 + MIN_GROUP_DURATION_US),
            "先頭のキーフレームの観測で要求が満たされること"
        );
    }

    /// 連続する NEW_GROUP_REQUEST でキーフレームが乱発しないこと
    #[test]
    fn consecutive_new_group_requests_do_not_repeat_keyframes() {
        let mut tracker = GopTracker::new(2_000);
        let group_start_us = 1_000_000;
        tracker.observe_keyframe(group_start_us);
        tracker.request_new_group();
        // 最小長に達した最初の入力フレームで 1 回だけ要求する
        assert!(
            tracker.request_keyframe_if_due(group_start_us + MIN_GROUP_DURATION_US),
            "最初の要求でキーフレームを要求すること"
        );
        // 要求を観測する前に連続で要求が来ても、キーフレームは増えない
        for offset_us in [
            MIN_GROUP_DURATION_US + 33_333,
            MIN_GROUP_DURATION_US + 66_666,
        ] {
            tracker.request_new_group();
            assert!(
                !tracker.request_keyframe_if_due(group_start_us + offset_us),
                "要求を観測するまで追加のキーフレームを要求しないこと: {offset_us}"
            );
        }
        tracker.observe_keyframe(group_start_us + MIN_GROUP_DURATION_US);
        // 観測後も、最小長に達するまでは追加の要求を出さない
        tracker.request_new_group();
        assert!(
            !tracker.request_keyframe_if_due(group_start_us + MIN_GROUP_DURATION_US + 100_000),
            "新しい Group が最小長に達するまでは要求しないこと"
        );
        assert!(
            tracker.request_keyframe_if_due(group_start_us + 2 * MIN_GROUP_DURATION_US),
            "最小長に達したら要求すること"
        );
    }

    /// 最初に観測した Group では前の Group の長さを返さないこと
    #[test]
    fn the_first_group_has_no_previous_duration() {
        let mut tracker = GopTracker::new(2_000);
        assert_eq!(
            tracker.observe_keyframe(5_000_000),
            None,
            "最初の Group に長さは無いこと"
        );
        assert_eq!(
            tracker.observe_keyframe(7_000_000),
            Some(2_000_000),
            "2 つ目以降は直前の Group の長さが返ること"
        );
    }

    /// 入力の時刻が巻き戻った場合は経過時間を 0 として扱うこと
    #[test]
    fn a_rewinding_timestamp_does_not_request() {
        let mut tracker = GopTracker::new(2_000);
        tracker.observe_keyframe(5_000_000);
        assert!(
            !tracker.request_keyframe_if_due(1_000_000),
            "時刻が基準より前なら要求しないこと"
        );
    }

    /// エンコーダのフレーム数上限で出たキーフレームの直後に強制しないこと
    ///
    /// 実カデンスが公称 fps より速いと、フレーム数上限の方が時間ベースの要求より先に
    /// 発火する。そのとき基準を更新していないと次の入力フレームで時間ベースの要求が
    /// 重なり、1 フレームの Group ができる。
    #[test]
    fn the_safety_net_keyframe_moves_the_reference() {
        let gop_ms = 2_000;
        let gop_us = gop_duration_us(gop_ms);
        // 公称 30 fps よりわずかに速いカデンス
        let interval_us = 32_787;
        let mut tracker = GopTracker::new(gop_ms);
        tracker.observe_keyframe(0);

        // フレーム数上限でエンコーダがキーフレームを出したところを観測する
        let cap_frames = u64::from(keyframe_interval_frames(30, gop_ms));
        let cap_timestamp_us = cap_frames * interval_us;
        assert!(
            cap_timestamp_us < gop_us,
            "カデンスが公称より速いときは上限の方が先に発火すること: {cap_timestamp_us}"
        );
        assert_eq!(
            tracker.observe_keyframe(cap_timestamp_us),
            Some(cap_timestamp_us),
            "上限のキーフレームまでの長さが返ること"
        );
        assert!(
            !tracker.request_keyframe_if_due((cap_frames + 1) * interval_us),
            "上限のキーフレームの直後で強制しないこと"
        );
        assert!(
            tracker.request_keyframe_if_due(cap_timestamp_us + gop_us),
            "上限のキーフレームから gop_duration が経過したら強制すること"
        );
    }

    /// 5 / 30 / 120 fps のいずれでも Group が指定した時間で進むこと
    ///
    /// 判定は入力フレームの Timestamp の差分で行うため、境界を含むフレームの分だけ
    /// 1 フレーム長くなり得る。10 秒ぶんの入力で Group が 4 回切り替わり、各 Group が
    /// 指定した時間以上・指定した時間 + 1 フレーム以下・カタログへ宣言する値以下に
    /// なることを固定する。
    #[test]
    fn the_group_advances_at_the_same_duration_for_every_fps() {
        let gop_duration_ms = 2_000;
        let gop_us = gop_duration_us(gop_duration_ms);
        for fps in [5, 30, 120] {
            let timestamps = frame_timestamps(fps, u64::from(fps) * 10);
            let forced = forced_indices(&timestamps, gop_duration_ms);
            assert_eq!(
                forced.len(),
                4,
                "10 秒で Group が 4 回切り替わること (fps {fps})"
            );
            let declared_us = catalog_max_duration_ms(fps, gop_duration_ms) * 1_000;
            for duration_us in group_durations_us(&timestamps, &forced) {
                assert!(
                    duration_us >= gop_us,
                    "Group が指定した時間より短くならないこと (fps {fps}): {duration_us}"
                );
                assert!(
                    duration_us <= gop_us + frame_interval_us(fps),
                    "Group が指定した時間 + 1 フレーム以内で進むこと (fps {fps}): {duration_us}"
                );
                assert!(
                    duration_us <= declared_us,
                    "Group がカタログへ宣言する値以下であること (fps {fps}): {duration_us}"
                );
            }
        }
    }

    /// フレーム間隔がゆらいでも Group が 1 フレーム + ゆらぎ以内で進むこと
    ///
    /// 実際の capture の Timestamp は一定間隔ではなく、エンコードの遅延でも揺らぐ。
    /// 10 秒ぶんの入力で Group が 4 回切り替わり、各 Group が指定した時間以上、
    /// 指定した時間 + 1 フレーム + ゆらぎ幅以下になることを固定する。カタログへ宣言する
    /// [`catalog_max_duration_ms`] は境界の 1 フレーム分だけを見込むため、入力のゆらぎが
    /// その分を超えると実測が宣言をわずかに超え得る。
    #[test]
    fn jitter_keeps_the_group_within_one_frame_plus_jitter() {
        let gop_duration_ms = 2_000;
        let gop_us = gop_duration_us(gop_duration_ms);
        let interval_us = frame_interval_us(30);
        // 1 フレーム間隔を 1 ms ぶん短く / 長くした並びを作る
        let jitter_us = 1_000;
        let mut timestamps = Vec::new();
        let mut timestamp_us = 0u64;
        for index in 0..300u64 {
            timestamps.push(timestamp_us);
            timestamp_us += if index % 2 == 0 {
                interval_us - jitter_us
            } else {
                interval_us + jitter_us
            };
        }
        let forced = forced_indices(&timestamps, gop_duration_ms);
        assert_eq!(forced.len(), 4, "10 秒ぶんで Group が 4 回切り替わること");
        for duration_us in group_durations_us(&timestamps, &forced) {
            assert!(
                duration_us >= gop_us,
                "ゆらぎで Group が短くならないこと: {duration_us}"
            );
            assert!(
                duration_us <= gop_us + interval_us + jitter_us,
                "ゆらぎによる遅れが 1 フレーム + ゆらぎ以内に収まること: {duration_us}"
            );
        }
    }

    /// 連続してフレームが落ちた場合は落ちた時間ぶんだけ Group が伸びること
    ///
    /// 判定は入力フレームの Timestamp の差分で行うため、指定した時間の経過が
    /// フレームの空白に重なると、次の入力フレームまで Group が伸びる。カタログへ宣言する
    /// 値は境界の 1 フレーム分だけを見込むため、この場合の Group 長は宣言を超える。
    #[test]
    fn dropped_frames_extend_the_group() {
        let gop_duration_ms = 2_000;
        let interval_us = frame_interval_us(30);
        // 41 フレーム (約 1.33 秒) 進んだところで、指定した時間 (2 秒) を跨ぐ空白を空ける
        let mut timestamps: Vec<u64> = (0..41).map(|index| index * interval_us).collect();
        let restart_us = 2_500_000;
        timestamps.extend((0..40).map(|index| restart_us + index * interval_us));

        let forced = forced_indices(&timestamps, gop_duration_ms);
        assert_eq!(
            forced,
            vec![41],
            "空白の後の最初のフレームで強制すること: {forced:?}"
        );
        let duration_us = timestamps[41] - timestamps[0];
        assert_eq!(
            duration_us, restart_us,
            "空白が無ければ境界の直後に強制したところを、空白の終わりまで伸びること"
        );
    }

    /// キーフレーム間隔のフレーム数上限が切り上げられること
    ///
    /// 切り下げると、時間ベースの要求より安全弁が先に発火して Group を短縮し得る。
    #[test]
    fn keyframe_interval_frames_rounds_up() {
        assert_eq!(
            keyframe_interval_frames(30, 2_000),
            60,
            "30 fps の 2 秒は 60 フレームになること"
        );
        assert_eq!(
            keyframe_interval_frames(5, 2_000),
            10,
            "5 fps の 2 秒は 10 フレームになること"
        );
        assert_eq!(
            keyframe_interval_frames(120, 2_000),
            240,
            "120 fps の 2 秒は 240 フレームになること"
        );
        assert_eq!(
            keyframe_interval_frames(30, 1),
            1,
            "1 フレームに満たない場合も 1 フレームに切り上げること"
        );
        assert_eq!(
            keyframe_interval_frames(1, 1_500),
            2,
            "小数点以下を切り上げること (1.5 フレーム → 2 フレーム)"
        );
    }

    /// 安全弁のフレーム数が公称 fps のカデンスで gop_duration 以上の時間幅になること
    ///
    /// 切り下げると、公称 fps でも安全弁が時間ベースの要求より先に発火し得る。example が
    /// 想定する fps と、切り上げが効く値 (1000 で割り切れない 24 / 30 / 60) で確かめる。
    #[test]
    fn the_safety_net_covers_the_gop_duration_at_the_nominal_fps() {
        for fps in [1u32, 5, 24, 30, 60, 120] {
            for gop_ms in [1u32, 33, 1_000, 2_000] {
                let frames = u64::from(keyframe_interval_frames(fps, gop_ms));
                assert!(
                    frames * 1_000 >= u64::from(fps) * u64::from(gop_ms),
                    "安全弁が gop_duration 以上になること (fps {fps} / {gop_ms} ms): {frames} フレーム"
                );
            }
        }
    }

    /// fps が 0 でも上限が 1 フレーム以上になること
    ///
    /// 0 をエンコーダの `NonZero*` へ渡すと上限無しとして扱われ、安全弁が消える。
    /// なお fps 0 はエンコーダの生成時点で拒否される。
    #[test]
    fn keyframe_interval_frames_is_never_zero() {
        assert_eq!(
            keyframe_interval_frames(0, 2_000),
            1,
            "0 フレームにはせず 1 フレームにすること"
        );
    }

    /// fps と gop_duration が極端に大きくても u32 の範囲へ飽和すること
    ///
    /// 乗算は u64 で行うため桁あふれしないが、結果は u32 に収まらない。`u32::try_from` の
    /// 飽和分岐はここでしか通らない。
    #[test]
    fn keyframe_interval_frames_saturates_to_u32_max() {
        assert_eq!(
            keyframe_interval_frames(u32::MAX, u32::MAX),
            u32::MAX,
            "u32 の範囲に収まるよう飽和させること"
        );
    }

    /// カタログへ宣言する値が Group 長 + 1 フレーム分になること
    #[test]
    fn catalog_max_duration_adds_one_frame() {
        assert_eq!(
            catalog_max_duration_ms(30, 2_000),
            2_034,
            "30 fps では 1 フレーム (34 ms の切り上げ) を足すこと"
        );
        assert_eq!(
            catalog_max_duration_ms(5, 2_000),
            2_200,
            "5 fps では 1 フレーム (200 ms) を足すこと"
        );
        assert_eq!(
            catalog_max_duration_ms(120, 2_000),
            2_009,
            "120 fps では 1 フレーム (9 ms の切り上げ) を足すこと"
        );
        assert_eq!(
            catalog_max_duration_ms(1, 2_000),
            3_000,
            "1 fps では 1 フレーム (1000 ms) を足すこと"
        );
        assert_eq!(
            catalog_max_duration_ms(0, 2_000),
            3_000,
            "fps 0 はエンコーダが拒否するため到達しないが、除算を守って 1 として扱うこと"
        );
    }
}
