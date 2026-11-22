//! A/V 同期と基準の差の判定・解除
//!
//! 2 トラックの表示時刻の差を不感帯へ収める制御、基準の差の履歴と共有解除の判定を担う。
//! 公開 API は `timeline.rs` に置き、ここでは `PlayoutTimeline` の内部状態だけを扱う。

use super::*;

impl PlayoutTimeline {
    /// 直近の基準の差の動き (マイクロ秒 / 秒)。まだ履歴が無ければ None
    pub(super) fn base_drift_us_per_second(&self) -> Option<i64> {
        let (movement_us, span_us) = self.base_difference_history()?;
        if span_us <= 0 {
            return None;
        }
        Some(movement_us.saturating_mul(1_000_000) / span_us)
    }

    /// 解除を早める条件が続いている時間 (マイクロ秒)。数えていなければ None
    ///
    /// 最後に基準の差を記録した時刻から測る。記録のたびに
    /// [`PlayoutTimeline::update_unshared_release`] が数え直す。
    pub(super) fn base_unshared_return_us(&self) -> Option<i64> {
        let returned_since_us = self.unshared_returned_since_us?;
        let last_base_difference_at_us = self.last_base_difference_at_us?;
        Some(
            last_base_difference_at_us
                .saturating_sub(returned_since_us)
                .max(0),
        )
    }

    /// 2 つのトラックの基準の差を記録する ([`TIMELINE_BASE_DIFFERENCE_SAMPLE_INTERVAL_US`]
    /// ごと)
    ///
    /// 記録するのは短い区間の最小値の差である。窓全体の最小値の差は、TIMESTAMP が壁時計
    /// から遅れていくときも窓が埋まるまで動かないため、動きを早く見つけられない。短い
    /// 窓は経路と復号の乱れでも動くため、待たずに判定するときは窓全体の差 (参照) を
    /// 確かめる。
    pub(super) fn record_base_difference(&mut self, now_us: i64) {
        if let Some(last_base_difference_at_us) = self.last_base_difference_at_us
            && now_us.saturating_sub(last_base_difference_at_us)
                < TIMELINE_BASE_DIFFERENCE_SAMPLE_INTERVAL_US
        {
            return;
        }
        let Some(audio_us) = self.recent_base_us(Track::Audio, now_us) else {
            return;
        };
        let Some(video_us) = self.recent_base_us(Track::Video, now_us) else {
            return;
        };
        // 表示時刻が使う基準 (窓全体の最小値) の差。動きが時計のずれかを見分ける参照になる
        let Some(reference_us) = self.current_base_difference_us() else {
            return;
        };
        let previous_value_us = self.base_difference_value_us;
        let previous_reference_us = self.last_reference_us;
        self.last_base_difference_at_us = Some(now_us);
        self.base_difference_value_us = Some(audio_us.saturating_sub(video_us));
        self.last_reference_us = Some(reference_us);
        let value_us = audio_us.saturating_sub(video_us);
        self.base_differences.push(now_us, value_us);
        self.base_differences
            .prune(now_us.saturating_sub(TIMELINE_BASE_DRIFT_WINDOW_US));
        if self.base_difference_first_at_us.is_none() {
            self.base_difference_first_at_us = Some(now_us);
        }
        self.update_base_difference_drift(
            now_us,
            value_us,
            previous_value_us,
            reference_us,
            previous_reference_us,
        );
        self.update_unshared_release(now_us);
    }

    /// 記録した差から、動きの持続を判定する
    ///
    /// 差が「離れた幅のまま」になっている時間を測り、[`TIMELINE_BASE_DRIFT_CONFIRM_US`]
    /// 続いたときだけ時計のずれとみなす。離れた水準が元へ戻れば、その動きは一過性
    /// (読み出しが一瞬遅れた、経路が一瞬つまずいた) だったと分かるため、判定を消して
    /// 水準を取り直す。
    ///
    /// 水準 (基準) は動きが戻ったときにだけ取り直す。記録のたびに取り直すと、ゆっくり
    /// したドリフトでも水準が一緒に動いてしまい、離れた幅が [`TIMELINE_BASE_DRIFT_US`]
    /// を超えない (毎秒 20〜50 ms で動くドリフトを見つけられない)。
    ///
    /// 待たずに判定する段差 ([`TIMELINE_BASE_DRIFT_STEP_US`]) だけは、参照
    /// (`reference_us`) が同じ向きに [`TIMELINE_BASE_DRIFT_US`] を超えて動いていることも
    /// 確かめる。確かめないと、経路と復号の乱れを時計のずれとみなして解除し、しかも
    /// 水準が乱れの側で取られると解除が解けなくなる。
    ///
    /// 判定は落ち着いた水準を観測してから始める。購読の直後は、relay の cache から
    /// 届いた分と購読を始めるまでにたまった分をまとめて復号しており、基準の遅れが数百 ms
    /// から数秒動く。この動きを時計のずれとみなすと、共有を解除したままになってしまう。
    fn update_base_difference_drift(
        &mut self,
        now_us: i64,
        value_us: i64,
        previous_value_us: Option<i64>,
        reference_us: i64,
        previous_reference_us: Option<i64>,
    ) {
        // 差が動いているかどうか。記録の間隔ごとの変化であり、窓の最小値のような遅れが
        // 無い
        let change_us = previous_value_us.map_or(0, |previous_value_us| {
            value_us.saturating_sub(previous_value_us).saturating_abs()
        });
        if change_us <= TIMELINE_BASE_DIFFERENCE_QUIET_US {
            if self.base_difference_quiet_since_us.is_none() {
                self.base_difference_quiet_since_us = Some(now_us);
            }
        } else {
            self.base_difference_quiet_since_us = None;
        }

        let Some(settled_us) = self.settled_base_difference_us else {
            // まだ「動いていない水準」を観測していない。動き続ける差 (ドリフト) の
            // ために、待つ時間には上限を置く
            let settled_by_quiet_us = self.base_difference_quiet_since_us.map(|quiet_since_us| {
                quiet_since_us.saturating_add(TIMELINE_BASE_DIFFERENCE_SETTLE_US)
            });
            let settled_by_latest_us = self
                .base_difference_first_at_us
                .map(|first_at_us| first_at_us.saturating_add(TIMELINE_BASE_DIFFERENCE_START_US));
            let settled_at_us = match (settled_by_quiet_us, settled_by_latest_us) {
                (Some(quiet_us), Some(latest_us)) => Some(quiet_us.min(latest_us)),
                (Some(quiet_us), None) => Some(quiet_us),
                (None, Some(latest_us)) => Some(latest_us),
                (None, None) => None,
            };
            // 水準は参照 (表示時刻が使う基準の差) から取る。動きを見る短い窓の差は経路と
            // 復号の乱れの位相で動くため、そこから取ると乱れの側の水準を基準にしてしまい、
            // 乱れが去っても離れた幅が戻らず、解除が解けなくなる
            if settled_at_us.is_some_and(|settled_at_us| now_us >= settled_at_us) {
                self.settled_base_difference_us = Some(reference_us);
            }
            self.base_difference_drifted_value = false;
            return;
        };

        let deviation_us = value_us.saturating_sub(settled_us).saturating_abs();
        if deviation_us <= TIMELINE_BASE_DRIFT_US {
            self.base_difference_drifted_value = false;
            if self.base_difference_deviation_since_us.is_some() {
                // 離れた水準から戻った。動いていた幅は一過性だったので、今の水準を基準に
                // する
                self.settled_base_difference_us = Some(value_us);
                self.base_difference_deviation_since_us = None;
                self.base_difference_deviation_reference_us = None;
            }
            return;
        }
        if self.base_difference_deviation_since_us.is_none() {
            // 離れ始めの参照を残す。動きと同じ向きに参照が動いていなければ、この動きは
            // 経路と復号の乱れであり、待たずに判定してはいけない
            self.base_difference_deviation_reference_us = previous_reference_us;
        }
        let since_us = self.base_difference_deviation_since_us.unwrap_or(now_us);
        self.base_difference_deviation_since_us = Some(since_us);
        // 1 回の記録で水準から離れた幅。段差はここに現れ、経路と復号の乱れは数十 ms ずつ
        // しか動かない (乱れは窓の最小値が入れ替わることで現れるため)
        let previous_deviation_us = previous_value_us.map_or(0, |previous_value_us| {
            previous_value_us
                .saturating_sub(settled_us)
                .saturating_abs()
        });
        let jumped_away_us =
            deviation_us.saturating_sub(previous_deviation_us) > TIMELINE_BASE_DIFFERENCE_JUMP_US;
        // 段差とみなせる大きさで、参照も同じ向きに離れていれば待たない。読み出しの遅れが
        // 戻るのを待つのは、その大きさでは時計のずれと読み出しの遅れを見分けられないため
        // である
        let reference_from_us = self
            .base_difference_deviation_reference_us
            .unwrap_or(reference_us);
        let reference_movement_us = reference_us.saturating_sub(reference_from_us);
        let deviation_signed_us = value_us.saturating_sub(settled_us);
        let reference_moved_same_way = deviation_signed_us.saturating_mul(reference_movement_us)
            > 0
            && reference_movement_us.saturating_abs() > TIMELINE_BASE_DRIFT_US;
        self.base_difference_drifted_value = (deviation_us > TIMELINE_BASE_DRIFT_STEP_US
            && (jumped_away_us || reference_moved_same_way))
            || now_us.saturating_sub(since_us) >= TIMELINE_BASE_DRIFT_CONFIRM_US;
    }

    /// 直近の基準 (マイクロ秒)。まだ観測していなければ None
    ///
    /// 窓全体の最小値 (`base_us`) ではなく
    /// [`TIMELINE_BASE_DIFFERENCE_RECENT_WINDOW_US`] の最小値である。窓全体の最小値は、
    /// TIMESTAMP が壁時計から遅れていくときも窓が埋まるまで動かないため、動きを早く
    /// 見つけるには短い窓が要る。
    fn recent_base_us(&self, track: Track, now_us: i64) -> Option<i64> {
        self.tracks[track.index()]
            .offsets
            .min_after(now_us.saturating_sub(TIMELINE_BASE_DIFFERENCE_RECENT_WINDOW_US))
    }

    /// 基準の差が動き続けているか (時計がずれているとみなすか)
    ///
    /// 差が動いていないときの水準から [`TIMELINE_BASE_DRIFT_US`] を超えて離れたまま
    /// [`TIMELINE_BASE_DRIFT_CONFIRM_US`] 続いたかで決める
    /// ([`PlayoutTimeline::update_base_difference_drift`] が記録のたびに求める)。
    pub(super) fn base_difference_drifted(&self) -> bool {
        self.base_difference_drifted_value
    }

    /// 基準の差の履歴の、最も古い記録から今までの動き (遅延の解析に使う)
    ///
    /// 履歴の値と今の差は同じ求め方でなければ比べられない。どちらも動きの判定が使う値
    /// (短い窓の最小値の差) である。
    ///
    /// 戻り値は (動いた幅, その幅を測った時間)。まだ記録が無ければ None。
    fn base_difference_history(&self) -> Option<(i64, i64)> {
        let (oldest_at_us, oldest_value_us) = self.base_differences.oldest()?;
        let last_value_us = self.base_difference_value_us?;
        let last_at_us = self.last_base_difference_at_us?;
        Some((
            last_value_us.saturating_sub(oldest_value_us),
            last_at_us.saturating_sub(oldest_at_us),
        ))
    }

    /// 今の「音声の基準 − 映像の基準」(マイクロ秒)。どちらかを観測していなければ None
    pub(super) fn current_base_difference_us(&self) -> Option<i64> {
        let audio_base_us = self.tracks[Track::Audio.index()].base_us?;
        let video_base_us = self.tracks[Track::Video.index()].base_us?;
        Some(audio_base_us.saturating_sub(video_base_us))
    }

    /// 2 つのトラックの表示時刻の差を不感帯に収める (観測のたびに行う)
    ///
    /// 表示時刻は「基準の遅れ + 表示の遅れ」であり、2 つのトラックの差はこの和の差で
    /// ある。したがって合わせる量は「下限を外した表示の遅れ」の差そのものであり、経路の
    /// 相対遅延 (直近の観測の offset) ではない。
    ///
    /// - 先行側だけ不感帯を残し、現在の必要量をキューの上限まで補償する
    /// - 過去の共有の下限を保持しない。音声キューの回復は実際の予約の残差から追従する
    /// - 音声と映像の両方を観測していて、基準の差が閾値の中にあるときだけ行う
    pub(super) fn update_sync_delays(&mut self, now_us: i64) {
        // 音声が止まった後に、最後の予約の残差だけを映像へ掛け続けない
        if let Some(correction_at_us) = self.audio_playout_correction_at_us
            && now_us.saturating_sub(correction_at_us) > TIMELINE_SKEW_WINDOW_US
        {
            self.audio_playout_correction_us = 0;
            self.audio_playout_correction_at_us = None;
        }
        // 差と動きから共有をやめた側を記録する (観測のたびに判定し直す)
        self.refresh_unshared_state();

        if self.drifted_stream() == Some(Track::Audio) {
            // 音声の TIMESTAMP が信用できず、到着基準で鳴っている。基準は共有できないが、
            // 音声の並べ方は分かっているため、映像だけをその時刻へ合わせる
            self.update_sync_delays_to_arrival_audio(now_us);
            return;
        }
        let Some(natural_us) = self.sync_natural_presentation_us() else {
            // 2 つのトラックの基準を共有できない (基準がずれている、またはまだ観測して
            // いない)。合わせる相手がいないため、足した分を自分の基準だけの表示時刻へ
            // 戻す。戻さないと、ずれたトラックへ合わせて足した分がそのまま残り (観測の
            // たびに増え続けて上限で頭打ちになる)、その分だけ 2 つの表示時刻が離れた
            // ままになる
            self.decay_sync_extras(now_us, |_| 0);
            return;
        };

        // 必要な表示時刻は現在の到着の分布と音声の予約から決める。過去に同期のために
        // 足した待ちを、次の音声の下限として保持しない
        self.align_sync_extras(natural_us);
        self.last_sync_us = Some(now_us);
        self.last_own_floor_us = Some([
            self.natural_delay_us_of(Track::Audio).unwrap_or(0),
            self.natural_delay_us_of(Track::Video).unwrap_or(0),
        ]);
    }

    /// 音声が到着基準で鳴っているときの同期の制御 (映像だけを音声へ合わせる)
    ///
    /// 音声の TIMESTAMP が信用できないと 2 つのトラックの基準は共有できず、これまでは
    /// 同期の制御そのものを止めていた。止めると相対関係を見る相手がいなくなり、音声は
    /// 「到着 + 到着基準の再生の遅れ」、映像は自分の jitter buffer の遅延で別々に並ぶ。
    ///
    /// 音声の並べ方は分かっている ([`audio_arrival_delay_us`]) ため、映像の到着からの
    /// 遅れ ([`PlayoutTimeline::natural_delay_us_of`]) をその値へ合わせることはできる。
    /// これで基準を共有できない状態でもリップシンクが取れる。音声と映像の到着が同じ
    /// 時刻であることは前提にする (同じ publisher・同じ経路の 2 つのトラック)。
    ///
    /// - 足すのは映像だけである。音声を遅らせると到着から鳴るまでの時間がその分だけ
    ///   増える (音声の目標は [`TIMELINE_ARRIVAL_DELAY_US`] であり、それ以上は遅らせない)
    /// - 足す量は [`TIMELINE_MAX_COMPENSATED_DIFFERENCE_US`] までにする。映像が音声より
    ///   遅いときは戻さない (音声を遅らせないと合わせられないため、その分は A/V のずれと
    ///   して残す)
    /// - 足すのは即座、戻すのは毎秒 [`TIMELINE_DELAY_DECAY_US_PER_SECOND`] までにする
    fn update_sync_delays_to_arrival_audio(&mut self, now_us: i64) {
        let (Some(audio_us), Some(video_us)) = (
            self.audio_arrival_delay_us(),
            self.natural_delay_us_of(Track::Video),
        ) else {
            // 音声の jitter buffer の遅延か、映像の遅延がまだ決まっていない
            self.decay_sync_extras(now_us, |_| 0);
            return;
        };
        // 目指す差は不感帯にする。ただし合わせる量は上限までにする (足りない分は A/V の
        // ずれとして残す)。差の大きさで不感帯に収まる場合も、既存の規則と同じく足さない
        let desired_us = TIMELINE_SYNC_MIN_DELTA_US.max(
            audio_us
                .saturating_sub(video_us)
                .saturating_abs()
                .saturating_sub(TIMELINE_MAX_COMPENSATED_DIFFERENCE_US),
        );
        let target_extra_us = if video_us < audio_us {
            audio_us
                .saturating_sub(desired_us)
                .saturating_sub(video_us)
                .max(0)
        } else {
            0
        };
        // 戻す向きは毎秒の速さまでにする (音声は足さない。足すと到着から鳴るまでが増える)
        self.decay_sync_extras(now_us, |track| {
            if track == Track::Video {
                target_extra_us
            } else {
                0
            }
        });
        // 足す向きは即座に行う (減らすのは decay_sync_extras だけにする)
        let video_index = Track::Video.index();
        let video_with_extra_us = video_us.saturating_add(self.sync_extra_us[video_index]);
        if video_with_extra_us < audio_us.saturating_sub(desired_us) {
            self.sync_extra_us[video_index] =
                audio_us.saturating_sub(desired_us).saturating_sub(video_us);
        }
    }

    /// 足した分を目標へ戻す (毎秒 [`TIMELINE_DELAY_DECAY_US_PER_SECOND`] まで)
    ///
    /// 観測の間隔で按分するため、観測が疎でも速さは変わらない。自分の遅延の下限が同時に
    /// 下がっているときは、その分だけ戻す量を減らす (下限が下がるだけでも表示時刻は前に
    /// 動くため、戻しすぎると不感帯を通り越す)。
    ///
    /// 戻す量の計算はマイクロ秒の整数で行うため、按分の切り捨てで 1 マイクロ秒未満の
    /// 差が生じる。戻す速さは毎秒 20 ms であり、この差は判定に影響しない。
    fn decay_sync_extras(&mut self, now_us: i64, target_extra_us: impl Fn(Track) -> i64) {
        let previous_sync_us = self.last_sync_us;
        self.last_sync_us = Some(now_us);
        let elapsed_us = previous_sync_us.map_or(0, |previous_sync_us| {
            now_us.saturating_sub(previous_sync_us).max(0)
        });
        let budget_us = TIMELINE_DELAY_DECAY_US_PER_SECOND.saturating_mul(elapsed_us) / 1_000_000;
        for track in [Track::Audio, Track::Video] {
            let index = track.index();
            let own_decrease_us = self.last_own_floor_us.map_or(0, |floor_us| {
                floor_us[index]
                    .saturating_sub(self.natural_delay_us_of(track).unwrap_or(0))
                    .max(0)
            });
            let allowed_us = budget_us.saturating_sub(own_decrease_us).max(0);
            let excess_us = self.sync_extra_us[index]
                .saturating_sub(target_extra_us(track))
                .max(0);
            self.sync_extra_us[index] =
                self.sync_extra_us[index].saturating_sub(allowed_us.min(excess_us));
        }
        // 次の制御で「自分の下限が下がった分」を求めるために、今の下限を残す
        self.last_own_floor_us = Some([
            self.natural_delay_us_of(Track::Audio).unwrap_or(0),
            self.natural_delay_us_of(Track::Video).unwrap_or(0),
        ]);
    }

    /// 共有する表示時刻の下限から、各トラックへ足す遅延を求め直す
    ///
    /// 基準を共有できるときは、先行する側も後行する側と同じ表示時刻へ揃える。固定の先行量は
    /// 残さない。合わせる量は現在の自然な表示の遅れから毎回求め直し、過去に足した補償を
    /// 下限として保持しない。基準を共有できないときだけ
    /// [`PlayoutTimeline::update_sync_delays`] が残った補償を徐々に戻す。
    ///
    /// `natural_us` は同期が足した分を除いた、トラックごとの表示の遅れ (マイクロ秒)。
    pub(super) fn align_sync_extras(&mut self, natural_us: [i64; 2]) {
        let floor_us = natural_us[Track::Audio.index()].max(natural_us[Track::Video.index()]);
        for track in [Track::Audio, Track::Video] {
            let index = track.index();
            // 自分の jitter が既存の補償を埋めたら、その分を必ず差し引く。表示時刻その
            // ものを前へ動かさずに余分な補償を解消でき、共通の待ち時間も積み上がらない
            let room_us = self
                .presentation_cap_us()
                .saturating_sub(self.natural_delay_us_of(track).unwrap_or(0))
                .max(0);
            let wanted_us = floor_us.saturating_sub(natural_us[index]).max(0);
            self.sync_extra_us[index] = room_us.min(wanted_us);
        }
    }

    /// 基準を共有できないトラック。無ければ None (共有している)
    ///
    /// 次のどちらかで共有できないと判定する。どちらの場合も、遅い側へ合わせて足した分は
    /// [`PlayoutTimeline::update_sync_delays`] が戻す。
    ///
    /// - 2 つのトラックの基準の差が
    ///   [`PlayoutTimeline::base_difference_limit_us`] を超えている。上限で切られる分は
    ///   合わせられないため、同期の制御では足りない
    /// - 基準の差が動き続けている ([`PlayoutTimeline::base_difference_drifted`])。これは
    ///   経路の遅れではなく TIMESTAMP の時計のずれであり、合わせると片側の表示の遅れが
    ///   上限まで伸びる
    ///
    /// 大きい側 (遅れて届いている側) が、TIMESTAMP が壁時計からずれている側である。
    /// 一度やめた側は [`TIMELINE_BASE_UNSHARED_HOLD_US`] の間は保持する。
    pub(super) fn drifted_stream(&self) -> Option<Track> {
        let difference_us = self.current_base_difference_us()?;
        let track = if difference_us > 0 {
            Track::Audio
        } else {
            Track::Video
        };
        if difference_us.saturating_abs() > self.base_difference_limit_us()
            || self.base_difference_drifted()
        {
            return Some(track);
        }
        self.held_unshared_stream()
    }

    /// 共有をやめた側を保持する (解除を早めてよいかの判断に使う、解除の時刻も記録する)
    ///
    /// 保持の間は閾値が動いても共有を戻さない ([`TIMELINE_BASE_UNSHARED_HOLD_US`])。
    /// ただし解除の原因が去って差が戻ったなら、解除を早めてよい
    /// ([`PlayoutTimeline::update_unshared_release`])。閾値
    /// (`base_difference_limit_us`) だけが動いて解除した場合、差は同じ水準に留まっており、
    /// 閾値が戻ればまた解除される (往復する) ため、直近の解除の時刻を控えて往復を見る
    /// ([`PlayoutTimeline::is_unshared_repeatedly`])。
    fn hold_unshared_stream(&mut self, track: Track) {
        if self.held_unshared_stream().is_none() {
            // 新しく解除する。前の解除の時刻を控える (直近に往復していないかを見る)。
            // 前の解除の判断 (戻った状態を数えた分) は持ち越さない
            self.previous_unshared_at_us = self.last_unshared_at_us;
            self.unshared_returned_since_us = None;
        }
        self.last_unshared_at_us = self.last_base_difference_at_us;
        self.last_unshared_track = Some(track);
    }

    /// 共有をやめた側の記録を更新する
    ///
    /// moqt-js は `driftedStream()` の中でこの更新を行う。Rust では `&self` の getter から
    /// 状態を変えられないため、制御の入口 (`update_sync_delays` と
    /// `set_target_latency_ms`) でまとめて更新する。解除のタイミングは
    /// `last_base_difference_at_us` を基準にするため、更新を 1 回の観測の中へ遅らせても
    /// 結果は変わらない。
    pub(super) fn refresh_unshared_state(&mut self) {
        let Some(difference_us) = self.current_base_difference_us() else {
            return;
        };
        if difference_us.saturating_abs() <= self.base_difference_limit_us()
            && !self.base_difference_drifted()
        {
            return;
        }
        let track = if difference_us > 0 {
            Track::Audio
        } else {
            Track::Video
        };
        self.hold_unshared_stream(track);
    }

    /// 解除を早めてよいかを見る (基準の差を記録するたびに 1 回)
    ///
    /// 解除のきっかけが去り ([`PlayoutTimeline::can_release_unshared_hold`])、差が戻った
    /// 状態が [`TIMELINE_BASE_UNSHARED_RELEASE_US`] 続いたら、保持を待たずに解除する。
    /// 戻った状態が崩れたら数え直す。
    fn update_unshared_release(&mut self, now_us: i64) {
        if !self.can_release_unshared_hold() {
            self.unshared_returned_since_us = None;
            return;
        }
        if self.unshared_returned_since_us.is_none() {
            self.unshared_returned_since_us = Some(now_us);
        }
        let Some(returned_since_us) = self.unshared_returned_since_us else {
            return;
        };
        if now_us.saturating_sub(returned_since_us) < TIMELINE_BASE_UNSHARED_RELEASE_US {
            return;
        }
        // 原因が去って差が戻った状態が続いた。同じ水準へ留まる理由が無いため解除する。
        // 解除の時刻は残す (直近に往復していないかを次の解除で見る)
        self.last_unshared_track = None;
        self.unshared_returned_since_us = None;
    }

    /// 保持を早く解除してよい状態か (解除の原因が去り、差が戻っているか)
    ///
    /// 次のすべてを満たすこと。どれかが崩れれば、差はまだ解除の水準の側にあり、共有と
    /// 解除を往復し得る (保持の対象である)。
    ///
    /// - 保持していること
    /// - 動きの判定 ([`PlayoutTimeline::base_difference_drifted`]) が解けていること
    /// - 直近に往復していないこと ([`PlayoutTimeline::is_unshared_repeatedly`])
    /// - 差が閾値 ([`PlayoutTimeline::base_difference_limit_us`]) を超えていないこと
    /// - 差が水準へ戻っていること (`base_difference_deviation_since_us` が消えていること)
    ///
    /// 解除のきっかけが動きだったかは見ない。購読の直後の cache の再生では差が静かに
    /// 動き (記録 1 回あたり 5 ms 以下)、時計のずれと判定されて解除されることがある。
    /// この場合も差が水準へ戻れば原因は去っている。
    fn can_release_unshared_hold(&self) -> bool {
        if self.last_unshared_track.is_none() || self.held_unshared_stream().is_none() {
            return false;
        }
        if self.base_difference_drifted() || self.is_unshared_repeatedly() {
            return false;
        }
        let Some(difference_us) = self.current_base_difference_us() else {
            return false;
        };
        if difference_us.saturating_abs() > self.base_difference_limit_us() {
            return false;
        }
        self.base_difference_deviation_since_us.is_none()
    }

    /// 直近に共有と解除を往復しているか
    ///
    /// 2 回の解除が [`TIMELINE_BASE_UNSHARED_HOLD_US`] の内側で起きていれば、閾値
    /// (表示の遅れの上限 − 再生の遅れ) が動き続けており、解除してもまた解除される。
    /// 往復のたびに、同期が足した分を戻して (間に合わないフレームを捨てる) すぐ足し直す
    /// (表示が待って止まる) ことになるため、落ち着くまで解かない。
    fn is_unshared_repeatedly(&self) -> bool {
        let (Some(previous_unshared_at_us), Some(last_unshared_at_us)) =
            (self.previous_unshared_at_us, self.last_unshared_at_us)
        else {
            return false;
        };
        last_unshared_at_us.saturating_sub(previous_unshared_at_us) < TIMELINE_BASE_UNSHARED_HOLD_US
    }

    /// 直前に共有をやめた側。保持していなければ None
    ///
    /// 閾値は jitter buffer の目標遅延で動くため、差が変わらなくても共有と解除を往復
    /// し得る。往復のたびに、足した分を戻して (フレームを捨てる) すぐ足し直す (表示が
    /// 止まる) ため、一度やめたら [`TIMELINE_BASE_UNSHARED_HOLD_US`] の間は戻さない。
    /// ただし解除のきっかけになった動きが去って差が戻った状態が続いたときは、往復の
    /// 恐れが無いため [`PlayoutTimeline::update_unshared_release`] が保持を解除する
    /// (そのときは None になる)。
    ///
    /// 満了は基準の差を記録した時刻 (`last_base_difference_at_us`) から測る。片方の
    /// トラックが届かなくなると記録が止まり、満了の判断もその時点で止まる (moqt-js も
    /// 同じ)。記録が再開すると、その間に経過した時間で判定する。
    pub(super) fn held_unshared_stream(&self) -> Option<Track> {
        let last_unshared_track = self.last_unshared_track?;
        let last_unshared_at_us = self.last_unshared_at_us?;
        let last_base_difference_at_us = self.last_base_difference_at_us?;
        if last_base_difference_at_us.saturating_sub(last_unshared_at_us)
            >= TIMELINE_BASE_UNSHARED_HOLD_US
        {
            return None;
        }
        Some(last_unshared_track)
    }

    /// 2 つのトラックの基準の差の閾値 (マイクロ秒)
    ///
    /// 表示の遅れの上限 ([`PlayoutTimeline::presentation_cap_us`]) から、同期が足した分を
    /// 含まない表示の遅れ (2 つのトラックの大きい方) を引いた値である。上限で切られる分
    /// は合わせられないため、キューが吸収できる長さではなく上限から引く。下限を置くのは、
    /// 閾値が 0 に近いと同期の制御と解除を往復するため。
    pub(super) fn base_difference_limit_us(&self) -> i64 {
        // 同期が足した分は含めない。含めると、合わせるために遅らせた結果で閾値が下がり、
        // 合わせた直後に「基準がずれている」と判定されてしまう
        let delay_us = self
            .natural_delay_us_of(Track::Audio)
            .unwrap_or(0)
            .max(self.natural_delay_us_of(Track::Video).unwrap_or(0));
        self.presentation_cap_us()
            .saturating_sub(delay_us)
            .max(TIMELINE_MIN_BASE_DIFFERENCE_MS.saturating_mul(1_000))
    }

    /// 同期の制御に使う、トラックごとの「同期が足した分を除いた表示の遅れ」(マイクロ秒)
    ///
    /// 基準の遅れと、同期が足した分を除いた表示の遅れ (自分の jitter buffer の遅延と
    /// `target_latency` の大きい方) の和である。2 つのトラックのこの値の差が、同期で
    /// 合わせる対象になる。どちらかが未観測、または基準がずれているときは None を返す。
    pub(super) fn sync_natural_presentation_us(&self) -> Option<[i64; 2]> {
        let audio_base_us = self.tracks[Track::Audio.index()].base_us?;
        let video_base_us = self.tracks[Track::Video.index()].base_us?;
        let audio_delay_us = self.natural_delay_us_of(Track::Audio)?;
        let video_delay_us = self.natural_delay_us_of(Track::Video)?;
        if self.drifted_stream().is_some() {
            // TIMESTAMP が壁時計からずれているトラックがある。同期の制御は行わない
            return None;
        }
        let cap_us = self.presentation_cap_us();
        let audio_own_us = audio_delay_us
            .min(cap_us)
            .saturating_add(self.audio_playout_correction_us);
        let video_own_us = video_delay_us.min(cap_us);
        let audio_us = audio_base_us.saturating_add(audio_own_us);
        let video_us = video_base_us.saturating_add(video_own_us);
        let difference_us = audio_us.saturating_sub(video_us);
        let limit_us = sync_compensation_limit_us(
            audio_base_us.saturating_sub(video_base_us),
            audio_own_us.saturating_sub(video_own_us),
        );
        // 先行側は実際の表示時刻から決める。基準を先に切ると、バッファが逆方向へ働くとき
        // に前後が逆転する。補償しない差だけを先行側の計算上の下限へ移し、足す量を
        // 制限する
        let uncorrected_us = difference_us
            .saturating_abs()
            .saturating_sub(limit_us)
            .max(0);
        if difference_us >= 0 {
            Some([audio_us, video_us.saturating_add(uncorrected_us)])
        } else {
            Some([audio_us.saturating_add(uncorrected_us), video_us])
        }
    }

    /// 2 つのトラックの基準の差の履歴を消す
    pub(super) fn clear_base_differences(&mut self) {
        self.base_differences.clear();
        self.last_base_difference_at_us = None;
        self.base_difference_value_us = None;
        self.base_difference_deviation_reference_us = None;
        self.last_reference_us = None;
        self.settled_base_difference_us = None;
        self.base_difference_deviation_since_us = None;
        self.base_difference_quiet_since_us = None;
        self.base_difference_first_at_us = None;
        self.base_difference_drifted_value = false;
        // 解除を早める判断のうち、差の記録を数える分はやり直す。保持そのもの
        // (解除の時刻と側) は残す (基準を取り直しても、原因が去ったかどうかの判断は
        // 続ける)
        self.unshared_returned_since_us = None;
    }
}
