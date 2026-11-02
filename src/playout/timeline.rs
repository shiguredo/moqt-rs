//! 受信した音声と映像を鳴らす・表示する時刻を決める共通の時間軸
//!
//! 送信側の TIMESTAMP (LOC の TIMESTAMP) と受信側の時計を結び、トラックごとの遅れを
//! 足した「鳴らす時刻・表示する時刻」を返す。表示時刻は
//! 「TIMESTAMP + 基準の遅れ + 表示の遅れ」であり、音声と映像で同じ式を使うため、
//! A/V の同期はトラックごとの遅れの差だけで決まる。時計もデバイスも触らず、時刻は
//! すべて呼び出し側が引数で渡す。
//!
//! - 基準の遅れ: トラックごとの (復号の出力の時刻 − TIMESTAMP) の直近 10 秒の最小値。
//!   受信側と送信側の時計のずれと、経路と復号の最小遅延を含む。遅れは到着ではなく
//!   復号の出力の時刻で測る (表示できる時刻には復号の時間も含まれるため)
//! - 表示の遅れ: トラックごとの jitter buffer の遅延
//!   - 音声は直近 2 秒の到着の揺らぎの百分位と、直近 1 秒の実出力の先行量の p95 を
//!     足し、50〜100 ms の自動予算にする ([`TIMELINE_AUDIO_DELAY_FLOOR_US`] と
//!     [`TIMELINE_ARRIVAL_DELAY_US`])
//!   - 映像は到着の揺らぎの百分位を使う。百分位は配信 fps から決め、表示時刻の後に
//!     届くフレームが 1 秒に [`TIMELINE_LATE_FRAMES_PER_SECOND`] 枚までになる値
//!     ([`playout_delay_percentile`]) と下限の大きい方である
//!   - 追いつき中と停滞後のまとめた復号を分布へ混ぜない。古い観測が窓から消えたら
//!     必要値へ戻す
//! - A/V 同期: 2 つのトラックの表示時刻の差が [`TIMELINE_SYNC_MIN_DELTA_US`] を超えた
//!   ときだけ、先行する側の表示の遅れを「後行側 − 不感帯」まで上げる (観測のたび)
//! - 基準を共有しない判定: 2 つのトラックの基準の差が、表示の遅れの上限から求まる
//!   閾値を超えたときと、差が [`TIMELINE_BASE_DRIFT_US`] を超えて動き続けている
//!   (TIMESTAMP が壁時計からずれている) ときである。共有しない間は、その時点までに
//!   足した分を戻す
//! - 基準を共有できない側が音声のときは、同期をやめずに映像だけを音声の到着基準の遅れ
//!   ([`PlayoutTimeline::audio_arrival_delay_us`]) へ合わせる。合わせないと相対関係を
//!   見る相手がいなくなり、音声と映像が別々の基準で並ぶ
//! - いったん共有をやめたら [`TIMELINE_BASE_UNSHARED_HOLD_US`] の間は戻さない。ただし
//!   解除の原因が去って差が戻った状態が [`TIMELINE_BASE_UNSHARED_RELEASE_US`] 続いたら、
//!   往復の恐れが無いため保持を待たずに戻す
//! - 遅延の内訳 (基準の遅れ・jitter buffer の遅延・足した分・共有できているかとその理由・
//!   差の動き・音声の自動予算) は [`PlayoutTimeline::delay_breakdown`] が返す
//!
//! 時刻の単位はすべてマイクロ秒 (i64) である。moqt-js はミリ秒の浮動小数で計算するが、
//! 本実装はマイクロ秒の整数で計算する。比較はすべてマイクロ秒で厳密に行い、整数化の差は
//! 1 マイクロ秒未満に収める (切り捨て・四捨五入のどちらを使うかは各所のコメントに書く)。
//! 例外は出力の残差の平滑化 (再帰フィルタ) であり、切り捨てを続けると定常状態で下振れが
//! 積もるため四捨五入する (差は 1 マイクロ秒未満)。

use alloc::collections::VecDeque;
use alloc::vec::Vec;

/// 基準の遅れを求める直近の窓 (マイクロ秒)
pub const TIMELINE_WINDOW_US: i64 = 10_000_000;

/// 到着の揺らぎを使う窓 (マイクロ秒)
///
/// 時計の基準の 10 秒と分け、一度の停滞を保持し続けない。
pub const TIMELINE_JITTER_WINDOW_US: i64 = 2_000_000;

/// 実出力の先行量の百分位を求める窓 (マイクロ秒)
pub const TIMELINE_AUDIO_OUTPUT_LEAD_WINDOW_US: i64 = 1_000_000;

/// 到着基準で並べた実出力の深さを使う窓 (マイクロ秒)
pub const TIMELINE_AUDIO_ARRIVAL_DELAY_WINDOW_US: i64 = 1_000_000;

/// TIMESTAMP と壁時計の関係が基準からこれ以上離れたら基準を取り直す (マイクロ秒)
///
/// 配信元の切り替えや、配信が長く止まったあとの再開で基準が古くなるのを避ける。
pub const TIMELINE_DISCONTINUITY_US: i64 = 2_000_000;

/// 表示時刻の後に届くことを許すフレームの数 (1 秒あたり)
///
/// 表示時刻の後に届いたフレームはその表示周期に描けず、止まりになる。見る側が感じるのは
/// 1 秒あたりの止まりの数であり、同じ割合で遅れを許すと配信 fps が高いほど止まりが
/// 増える。許す数を 1 秒あたりで決め、再生遅延の目標にする揺らぎの百分位を配信 fps から
/// 求める ([`playout_delay_percentile`])。
pub const TIMELINE_LATE_FRAMES_PER_SECOND: i64 = 1;

/// 再生遅延の目標にする揺らぎの百分位の下限 (100 万分率)
///
/// 配信 fps が低い (20 fps 以下) と 1 秒に 1 枚は 5% を超えるため、95% のフレームは
/// 表示時刻までに届く長さを保つ。経路のまれな大きな遅延の跳ね (数分に数回、300 ms
/// 前後) まで吸収しようとすると、常に大きく遅れて表示することになるため、百分位は
/// 100% にしない。
pub const TIMELINE_MIN_PLAYOUT_DELAY_PERCENTILE_PPM: i64 = 950_000;

/// 100 万分率で表した 95% の百分位
///
/// 実出力の先行量の p95 に使う。
pub const TIMELINE_P95_PERCENTILE_PPM: i64 = 950_000;

/// 100 万分率で表した 50% の百分位
pub const TIMELINE_P50_PERCENTILE_PPM: i64 = 500_000;

/// 表示の遅れの上限の既定値 (ミリ秒)
pub const TIMELINE_MAX_PRESENTATION_DELAY_MS: i64 = 500;

/// 映像の表示待ちのキューの上限のうち、揺らぎで一時的に増える分として空けておく枚数
///
/// 表示の遅れは (キューの上限 − この枚数) 枚分のフレーム間隔までに抑える。フレーム
/// 間隔が短いほど長く待てない。
pub const TIMELINE_QUEUE_HEADROOM_FRAMES: usize = 4;

/// 2 つのトラックの基準の遅れの差の閾値の下限 (ミリ秒)
///
/// 閾値が 0 に近いと、同期の制御と解除を往復して基準の学習と並べ直しが繰り返し
/// 起きるため、下限を置く。
pub const TIMELINE_MIN_BASE_DIFFERENCE_MS: i64 = 100;

/// フレーム間隔の中央値を求めるために保持する TIMESTAMP の差の数
pub const TIMELINE_FRAME_INTERVAL_SAMPLES: usize = 32;

/// 実績を A/V のずれの推定に使う期間 (マイクロ秒)
pub const TIMELINE_SKEW_WINDOW_US: i64 = 1_000_000;

/// 出力の残差を平滑化する時定数 (マイクロ秒)
///
/// 単発の予約の揺れを抑え、数フレームで追従する。
/// [`PlayoutTimeline::record_presentation_with_target`] の重み
/// `elapsed / (SMOOTHING + elapsed)` に使う。
const TIMELINE_AUDIO_PLAYOUT_CORRECTION_SMOOTHING_US: i64 = 250_000;

/// 追いつき中かを確かめる間隔 (マイクロ秒)
pub const TIMELINE_CATCH_UP_CHECK_INTERVAL_US: i64 = 250_000;

/// 追いつき中とみなす、間隔の間の基準の遅れの下がり幅 (マイクロ秒)
///
/// 実時間の 1.1 倍の速さで追いつく場合も 25 ms 下がる。live に追いついた後の経路の
/// 最小の遅延の変化 (数ミリ秒) より十分大きくする。
pub const TIMELINE_CATCH_UP_MIN_BASE_DROP_US: i64 = 20_000;

/// 基準を共有できない間に、同期の補償を戻す速さ (マイクロ秒 / 秒)
pub const TIMELINE_DELAY_DECAY_US_PER_SECOND: i64 = 20_000;

/// A/V 同期の制御で許す、表示時刻の差の下限 (マイクロ秒)
///
/// この不感帯の中では遅延を変えないため、2 つのトラックは最大この値だけ離れたまま
/// になる。libwebrtc の `video/stream_synchronization.cc` の `kMinDeltaMs` は 30 ms で
/// あるが、実リレーで測り直して 5 ms にした (2026-10-15)。
///
/// 実測 (実リレーの同じ条件で、不感帯だけを 30 ms と 5 ms にして交互に 2 回、250 ms
/// ごとに 60 秒観測。ずれは負が「音声が映像より遅れる」側):
///
/// | 不感帯 | ずれの p50 | p95 | 絶対値の最大 | 映像の表示待ち p50 | 映像の取りこぼし |
/// | --- | --- | --- | --- | --- | --- |
/// | 30 ms | −34 / −35 ms | −15 / −16 ms | 49 / 48 ms | 95 / 96 ms | 18 / 19 |
/// | 5 ms | −16 / −17 ms | +2 / −13 ms | 24 / 19 ms | 112 / 112 ms | 7 / 19 |
///
/// 30 ms のままだと、実測のずれ (−34 ms) は ITU-R BT.1359-1 の許容 (音声の遅れ
/// 30 ms、進み 22.5 ms) を外れる。5 ms にすると許容の内側に入る (−16 ms)。代わりに
/// 映像は 16 ms 余計に待つ (95 → 112 ms) が、表示の遅れの上限
/// ([`TIMELINE_MAX_PRESENTATION_DELAY_MS`] の 500 ms) には遠い。映像の取りこぼしは
/// 増えていない。
pub const TIMELINE_SYNC_MIN_DELTA_US: i64 = 5_000;

/// 実際に先行する側へ足してよい補償量 (マイクロ秒)
///
/// 基準の差には [`TIMELINE_MAX_COMPENSATED_DIFFERENCE_US`] の保護を掛け、同じ方向の
/// 既知のバッファ差は別に補償する。バッファが基準の差を相殺している場合、その分を
/// さらに予算へ足すことはしない。先行側は必ず両方を合成した実際の表示時刻から決める。
///
/// `buffer_difference_us` は「音声の自分の遅れ − 映像の自分の遅れ」である。
pub fn sync_compensation_limit_us(base_difference_us: i64, buffer_difference_us: i64) -> i64 {
    // JS の Math.sign(base + buffer) * buffer と同じ結果を、乗算を使わずに求める
    let total_difference_us = base_difference_us.saturating_add(buffer_difference_us);
    let allowance_us = if total_difference_us > 0 {
        buffer_difference_us.max(0)
    } else if total_difference_us < 0 {
        buffer_difference_us.saturating_neg().max(0)
    } else {
        0
    };
    TIMELINE_MAX_COMPENSATED_DIFFERENCE_US.saturating_add(allowance_us)
}

/// 音声の再生の遅れの下限 (マイクロ秒)
///
/// 到着基準で並べるときも、共有の時間軸で並べるときも、これより短くは並べない。揺らぎと
/// 実出力の先行量の合計がこの値を下回る間は、この値が自動予算になる。基準を共有できない
/// ときは、映像をこの「到着 + 下限」へ合わせる ([`audio_arrival_delay_us`]) ため、値が
/// ずれると A/V の合わせ先がずれる。
///
/// 2026-10-15 の実リレーの実測 (60 秒の配信と購読、下限だけを変えて交互に 3 回) で決めた。
/// 適用中の目標は、当時の揺らぎの学習が下限を超えていなければ下限そのものになる:
///
/// | 下限 | 適用中の目標 | 鳴り始め p50 | 時間圧縮 / 60 秒 | 補間 | 基準の取り直し |
/// | --- | --- | --- | --- | --- | --- |
/// | 80 ms | 80 ms | 78.6〜95.3 ms | 5〜9 ms | 0〜17 ms | 0 |
/// | 50 ms | 50 ms | 45.6〜67.7 ms | 32〜65 ms | 0〜8 ms | 0 |
/// | 30 ms | 30〜40 ms | 34.6〜56.8 ms | 496〜1143 ms | 0〜48 ms | 3 回中 1 回で 2 |
///
/// 揺らぎの学習が下限を超えたときはそちらが使われる (実測では jitter 100〜140 ms のとき
/// 適用中の目標は上限の 100 ms になり、下限の値は効かない)。下限は経路が静かなときに
/// 遅延を積まないための値である。50 ms は時間圧縮を 60 秒で 65 ms 以下 (鳴らす音の
/// 0.1 % 程度) に保ったまま 80 ms より 30 ms 低く、30 ms は圧縮が 1 桁増えるため採らない。
pub const TIMELINE_AUDIO_DELAY_FLOOR_US: i64 = 50_000;

/// 時間圧縮を始める遅れの下限 (マイクロ秒)
///
/// [`PlayoutTimeline::compression_deadband_us`] が返す値の下限である。moqt-js の
/// `compressionDeadbandSeconds` の 0.005 秒に対応する。
pub const TIMELINE_MIN_COMPRESSION_DEADBAND_US: i64 = 5_000;

/// 音声を到着基準で並べるときの再生の遅れの上限 (マイクロ秒)
///
/// 学習した値は経路の揺らぎを吸収するために必要である。ただし上限を超える分は使わない。
/// TIMESTAMP が壁時計からずれているトラックでは、ずれそのものを揺らぎとして学習して
/// しまうためである。呼び出し側 (音声を鳴らす時刻を決めるとき) と時間軸 (映像を
/// 合わせるとき) の両方が [`audio_arrival_delay_us`] を使う。値がずれると A/V の
/// 合わせ先がずれる。
pub const TIMELINE_ARRIVAL_DELAY_US: i64 = 100_000;

/// 2 つのトラックの基準の差が、この幅を超えて動いたら時計がずれているとみなす (マイクロ秒)
///
/// 差が大きいだけなら「経路と復号の遅い側」であり、同期の制御で合わせられる。しかし差が
/// 動き続ける場合は経路の遅れではなく、片方の TIMESTAMP が壁時計からずれていくことを
/// 意味する。ずれ続ける差を合わせると、もう片方の表示の遅れが上限まで伸びて戻せない。
///
/// 実時間に対する時計の進み方の違いは 500 ppm (毎秒 0.5 ms) 未満であり、経路と復号の
/// 最小遅延の差も毎秒ミリ秒の桁でしか動かない。したがってこの幅 (5 秒で 50 ms = 毎秒
/// 10 ms) を超える動きは時計のずれとみなしてよい。
pub const TIMELINE_BASE_DRIFT_US: i64 = 50_000;

/// 基準の差の動きを見る窓 (マイクロ秒)
pub const TIMELINE_BASE_DRIFT_WINDOW_US: i64 = 5_000_000;

/// A/V 同期で補償する基準の差の上限 (マイクロ秒)
///
/// 基準には経路の最小遅延だけでなく、送信側の時計のずれも含まれる。後者に際限なく
/// 合わせないため、基準の差にだけ保護を掛ける。自分で決めたバッファの差と、出力の時計で
/// 測った残差は別に補償する ([`sync_compensation_limit_us`])。最終的な待ち時間はキューと
/// 表示の上限で切る。
pub const TIMELINE_MAX_COMPENSATED_DIFFERENCE_US: i64 = 100_000;

/// いったん基準を共有しないと決めた後、判定を戻さない時間 (マイクロ秒)
///
/// 閾値は「表示の遅れの上限 − そのトラックの遅延」で決まるため、jitter buffer の目標
/// 遅延が段差で動くたびに閾値も動く。往復のたびに、足した分を戻して (間に合わない
/// フレームを捨てる) すぐ足し直す (表示が待って止まる) ことになるため、しばらくは
/// 戻さない。
pub const TIMELINE_BASE_UNSHARED_HOLD_US: i64 = 30_000_000;

/// 解除の原因が去って差が戻った状態が、この時間続いたら保持を解除する (マイクロ秒)
///
/// 保持は「差が同じ水準のままで、表示の遅れの上限から決まる閾値だけが動く」場合に必要で
/// ある ([`TIMELINE_BASE_UNSHARED_HOLD_US`])。逆に、解除の原因が去って差が戻ったなら、
/// 同じ水準へ留まる理由は無い。戻ったかどうかは、動きの判定が解けていること・直近に
/// 往復していないこと・差が閾値の内側にあること・差が水準へ戻っていることで見る
/// (`can_release_unshared_hold`)。
///
/// 実測 (CI の 4 vCPU の runner) では、購読の直後に relay の cache から届いた分を
/// まとめて復号している間だけ差が 1 秒近く開き、復号が実時間に追いつくと 5 ms 前後へ
/// 戻った。この一過性の動きで解除した後、30 秒の保持がそのまま効き、25 秒の観測が
/// すべて解除のままになった。続ける時間は、実リレーの観測が解除の判断を待たない長さ
/// (差の記録の間隔の 8 回分) にする。
pub const TIMELINE_BASE_UNSHARED_RELEASE_US: i64 = 2_000_000;

/// 基準の差がこれ以下の変化なら「動いていない」とみなす幅 (マイクロ秒)
pub const TIMELINE_BASE_DIFFERENCE_QUIET_US: i64 = 5_000;

/// 動きが止まってから、落ち着いた水準を観測したとみなすまでの時間 (マイクロ秒)
///
/// 購読の直後は、relay の cache から届いた分と購読を始めるまでにたまった分をまとめて
/// 復号しており、基準の遅れが数百 ms から数秒動く。この動きを時計のずれとみなすと、
/// 共有を解除したままにしてしまう。
pub const TIMELINE_BASE_DIFFERENCE_SETTLE_US: i64 = 3_000_000;

/// 最初の記録から、動きが続いていても水準を決めるまでの時間 (マイクロ秒)
///
/// 動き続ける差 (ドリフト) のために、水準を観測するまでの待ちに上限を置く。
pub const TIMELINE_BASE_DIFFERENCE_START_US: i64 = 10_000_000;

/// 1 回の記録で水準から離れたとみなす幅 (マイクロ秒)
///
/// 段差はここに現れ、経路と復号の乱れは数十 ms ずつしか動かない (乱れは窓の最小値が
/// 入れ替わることで現れるため)。
pub const TIMELINE_BASE_DIFFERENCE_JUMP_US: i64 = 150_000;

/// 水準から離れたまま、時計のずれとみなすまでに要求する時間 (マイクロ秒)
///
/// 離れた幅が元へ戻れば、その動きは一過性 (読み出しが一瞬遅れた、経路が一瞬つまずいた)
/// であったと分かる。持続を確かめてから解除する。
pub const TIMELINE_BASE_DRIFT_CONFIRM_US: i64 = 6_000_000;

/// 待たずに時計のずれとみなす、水準からの離れ幅 (マイクロ秒)
///
/// この大きさでは、時計のずれと読み出しの遅れを見分けられない。ただし参照 (表示時刻が
/// 使う基準の差) が同じ向きに [`TIMELINE_BASE_DRIFT_US`] を超えて動いていることを
/// 確かめる。確かめないと、経路と復号の乱れを時計のずれとみなして解除し、しかも水準が
/// 乱れの側で取られると解除が解けなくなる。
pub const TIMELINE_BASE_DRIFT_STEP_US: i64 = 200_000;

/// 基準の差を記録する間隔 (マイクロ秒)
pub const TIMELINE_BASE_DIFFERENCE_SAMPLE_INTERVAL_US: i64 = 250_000;

/// 基準の差の動きを見るときに使う、直近の基準を求める窓 (マイクロ秒)
///
/// 窓全体 ([`TIMELINE_WINDOW_US`]) の最小値は、TIMESTAMP が壁時計から遅れていくときも
/// 窓が埋まるまで動かない。短い窓で取り直すことで、合わせる側の遅れが上限へ伸びる前に
/// 動きを見つける。短くするほど経路と復号の乱れの影響を受けやすいため、待たずに判定
/// する段差では窓全体の差を参照する ([`TIMELINE_BASE_DRIFT_STEP_US`])。
pub const TIMELINE_BASE_DIFFERENCE_RECENT_WINDOW_US: i64 = 2_000_000;

/// 到着基準で並べるときの再生の遅れを求める (マイクロ秒)
///
/// 学習した遅れを下限 [`TIMELINE_AUDIO_DELAY_FLOOR_US`] と上限
/// [`TIMELINE_ARRIVAL_DELAY_US`] の間に切る。呼び出し側 (音声を鳴らす時刻を決めるとき)
/// と時間軸 (基準を共有できないときに映像を合わせるとき) の両方がこれを使う。値が
/// ずれると A/V の合わせ先がずれる。
pub fn audio_arrival_delay_us(learned_delay_us: i64) -> i64 {
    learned_delay_us.clamp(TIMELINE_AUDIO_DELAY_FLOOR_US, TIMELINE_ARRIVAL_DELAY_US)
}

/// 表示時刻を決める相手
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Track {
    /// 音声
    Audio,
    /// 映像
    Video,
}

impl Track {
    /// 配列の添字
    fn index(self) -> usize {
        match self {
            Self::Audio => 0,
            Self::Video => 1,
        }
    }

    /// もう片方のトラック
    fn other(self) -> Self {
        match self {
            Self::Audio => Self::Video,
            Self::Video => Self::Audio,
        }
    }
}

/// 2 つのトラックで基準を共有できているか、できていない理由
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsharedReason {
    /// 共有している (どちらも TIMESTAMP を使えている)
    None,
    /// 基準がまだ足りない (どちらかを観測していない、表示の遅れがまだ決まっていない)
    Unobserved,
    /// 基準の差が表示の遅れの上限を超えている (上限では合わせられない)
    Difference,
    /// 基準の差が動き続けている (TIMESTAMP が壁時計からずれている)
    Drift,
    /// 直前にやめた判定を保持している (閾値が動いても往復させない)
    Hold,
}

/// 到着と出力時計から求めた、音声の再生予算の内訳
///
/// 未観測の値は `None` とする。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPlayoutDelayStats {
    /// 到着の揺らぎの百分位 (マイクロ秒)。未観測なら None
    pub jitter_us: Option<i64>,
    /// 今予約できる最早位置の先行量の p95 (マイクロ秒)。有効な出力時計が未観測なら None
    pub output_lead_us: Option<i64>,
    /// 自動予算 (マイクロ秒)。catalog の明示下限と同期の補償は別に扱う
    pub target_us: i64,
}

/// トラックごとの表示時刻の内訳 (遅延の解析に使う)
///
/// 表示時刻 = TIMESTAMP + 基準の遅れ + 表示の遅れ であり、表示の遅れは jitter buffer の
/// 遅延と `target_latency` の大きい方に、同期の制御が足した分を加えて上限で切った値である。
/// どこで遅れが生じているかを分けるために、この 3 つを別々に出す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackBreakdown {
    /// 基準の遅れ (マイクロ秒)。送受信の時計のずれと、経路と復号の最小遅延。未観測なら None
    pub base_delay_us: Option<i64>,
    /// jitter buffer の遅延 (マイクロ秒)。音声は到着の揺らぎと実出力の先行量から求めた
    /// 予算、映像は到着の揺らぎの百分位。未観測なら None
    ///
    /// `target_latency` と同期の制御が足した分は含まない。
    pub jitter_delay_us: Option<i64>,
    /// 同期の制御が足した分 (マイクロ秒)。0 以上
    pub sync_extra_delay_us: i64,
    /// 表示の遅れ (マイクロ秒)。TIMESTAMP から表示時刻までの差そのものである。上限で
    /// 切った後の値であり、そのトラックの TIMESTAMP を使わないとき (未観測、または基準が
    /// ずれている) は None
    pub presentation_delay_us: Option<i64>,
    /// 表示の遅れの上限 (マイクロ秒)。切り下げが起きているかはこの値との比較で分かる
    pub presentation_delay_cap_us: i64,
}

/// 音声と映像の遅延の内訳 (遅延の解析に使う)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelayBreakdown {
    /// 音声の内訳
    pub audio: TrackBreakdown,
    /// 映像の内訳
    pub video: TrackBreakdown,
    /// 音声の予定から実際の出力までの残差 (マイクロ秒)。映像の合わせ先にだけ足す
    pub audio_playout_correction_us: i64,
    /// 基準の差「音声 − 映像」(マイクロ秒)。どちらかを観測していなければ None
    pub base_difference_us: Option<i64>,
    /// 直近の窓 ([`TIMELINE_BASE_DIFFERENCE_RECENT_WINDOW_US`]) の最小値で見た基準の差
    /// (マイクロ秒)
    ///
    /// 動きの判定 (`base_difference_deviation_us`) に使う値である。
    pub base_difference_recent_us: Option<i64>,
    /// 動きの判定に使っている水準 (マイクロ秒)。まだ決まっていなければ None
    pub base_difference_settled_us: Option<i64>,
    /// 動きの判定に使う水準からの隔たり (マイクロ秒)。水準がまだ無ければ None
    pub base_difference_deviation_us: Option<i64>,
    /// 2 つのトラックで基準を共有しているか
    pub sharing_bases: bool,
    /// 共有できていない理由
    pub unshared_reason: UnsharedReason,
    /// 直近の基準の差の動き (マイクロ秒 / 秒)。まだ履歴が無ければ None
    ///
    /// 整数除算の切り捨てで求めるため、負の値は 0 の方向へ丸める。ログと解析のための値で
    /// あり、時計のずれの判定 ([`DelayBreakdown::base_difference_deviation_us`] と
    /// [`DelayBreakdown::base_drift_limit_us`]) には使わない。
    pub base_drift_us_per_second: Option<i64>,
    /// 時計のずれとみなす、基準の差の水準からの隔たりの幅 (マイクロ秒)
    ///
    /// [`DelayBreakdown::base_drift_us_per_second`] と並ぶが**次元が違う** (あちらは動きの
    /// 速さ、こちらは隔たりの幅)。判定が使うのは隔たりの幅であり、動きの速さは計器と
    /// して出すだけで判定には使わない。
    pub base_drift_limit_us: i64,
    /// 保持を早く解除する条件 (解除のきっかけが去って差が戻った状態) が続いている時間
    /// (マイクロ秒)。数えていなければ None
    pub base_unshared_return_us: Option<i64>,
    /// jitter buffer の遅延を切り下げる上限 (マイクロ秒)
    pub presentation_delay_cap_us: i64,
    /// 音声の到着と実出力の時計から求めた自動予算の内訳
    pub audio_delay: AudioPlayoutDelayStats,
}

/// 時間軸の設定
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineConfig {
    /// 映像の表示待ちのキューの上限 (枚)
    ///
    /// 時間軸はこの値から表示の遅れの上限 (キューが吸収できる長さ) を求める。実際に
    /// フレームを積む [`crate::playout::buffer::PlayoutBuffer`] の上限とは独立しており、
    /// 呼び出し側が同じ値を渡す ([`crate::playout::buffer::PlayoutBuffer::new`])。片方だけを
    /// 変えると、表示の遅れの上限と実際に保持できる枚数が食い違う。
    pub video_queue_limit: usize,
    /// 表示の遅れの上限 (ミリ秒)
    pub max_presentation_delay_ms: i64,
    /// MSF の `targetLatency` (ミリ秒、draft-ietf-moq-msf-01 §5.2.8 (Target latency))。
    /// 0 のときは下限にしない (moqt-js の null に相当する)
    pub target_latency_ms: i64,
}

impl Default for TimelineConfig {
    fn default() -> Self {
        Self {
            video_queue_limit: 24,
            max_presentation_delay_ms: TIMELINE_MAX_PRESENTATION_DELAY_MS,
            target_latency_ms: 0,
        }
    }
}

/// 昇順に並べた値の nearest-rank 法の百分位の添字を求める
///
/// `numerator_milli` は百分位を 1000 分率で表した値 (950 なら 0.95)。添字は
/// `ceil(numerator_milli * 個数 / 1000) - 1` を 0 以上 `count` 未満に切った値である。
/// 音声の再生の観測値 (`crate::playout::timing`) が同じ求め方を使う。
pub(crate) fn percentile_index(count: usize, numerator_milli: i64) -> usize {
    if count == 0 {
        return 0;
    }
    let scaled = numerator_milli.saturating_mul(count as i64);
    let ceil = (scaled + 999) / 1_000;
    (ceil.max(1) as usize - 1).min(count - 1)
}

/// 昇順に並べた値の nearest-rank 法の百分位 (100 万分率) を求める
///
/// `ceil(numerator_ppm * 個数 / 1_000_000) - 1` 番目を返す。空なら 0。
fn percentile_ppm(sorted: &[i64], numerator_ppm: i64) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let scaled = numerator_ppm.saturating_mul(sorted.len() as i64);
    let ceil = (scaled + 999_999) / 1_000_000;
    let index = (ceil.max(1) as usize - 1).min(sorted.len() - 1);
    sorted[index]
}

/// 再生遅延の目標にする揺らぎの百分位 (100 万分率)
///
/// 表示時刻の後に届くフレームが 1 秒に [`TIMELINE_LATE_FRAMES_PER_SECOND`] 枚までに
/// なる百分位 `1 - フレーム間隔 × 枚数 / 1 秒` と下限の大きい方。30 fps で約 96.7%、
/// 60 fps で約 98.3%、120 fps で約 99.2% になる。フレーム間隔が不明、または 0 以下なら
/// 下限 ([`TIMELINE_MIN_PLAYOUT_DELAY_PERCENTILE_PPM`]) を返す。
pub fn playout_delay_percentile(frame_interval_us: Option<i64>) -> i64 {
    match frame_interval_us {
        Some(frame_interval_us) if frame_interval_us > 0 => {
            let numerator_ppm =
                1_000_000 - frame_interval_us.saturating_mul(TIMELINE_LATE_FRAMES_PER_SECOND);
            numerator_ppm.max(TIMELINE_MIN_PLAYOUT_DELAY_PERCENTILE_PPM)
        }
        _ => TIMELINE_MIN_PLAYOUT_DELAY_PERCENTILE_PPM,
    }
}

/// 表示待ちのキューが吸収できる表示の遅れ (マイクロ秒)
///
/// (キューの上限 − 余裕) 枚分のフレーム間隔。フレーム間隔が分からないときは上限
/// ([`TIMELINE_MAX_PRESENTATION_DELAY_MS`]) を返す。
pub fn playout_queue_cap_us(max_queued_frames: usize, frame_interval_us: Option<i64>) -> i64 {
    let Some(frame_interval_us) = frame_interval_us else {
        return TIMELINE_MAX_PRESENTATION_DELAY_MS.saturating_mul(1_000);
    };
    (max_queued_frames.saturating_sub(TIMELINE_QUEUE_HEADROOM_FRAMES) as i64)
        .max(0)
        .saturating_mul(frame_interval_us)
}

/// 表示の遅れの上限 (マイクロ秒)
///
/// 「表示の遅れの上限 (設定値)」と「表示待ちのキューが吸収できる長さ」の小さい方である。
fn presentation_cap_us(config: &TimelineConfig, frame_interval_us: Option<i64>) -> i64 {
    let max_us = config.max_presentation_delay_ms.saturating_mul(1_000);
    max_us.min(playout_queue_cap_us(
        config.video_queue_limit,
        frame_interval_us,
    ))
}

/// 観測の時刻付きの値の窓 (古い順)
///
/// 記録の時刻は単調に増えるため、窓より古い値は先頭にある。moqt-js の `TimedValues` と
/// 同じく、件数の上限は置かない (呼び出し側の観測の頻度と窓の長さで決まる)。
#[derive(Debug, Default)]
struct TimedWindow {
    entries: VecDeque<(i64, i64)>,
}

impl TimedWindow {
    /// 観測を 1 つ足す
    fn push(&mut self, at_us: i64, value_us: i64) {
        self.entries.push_back((at_us, value_us));
    }

    /// `oldest_us` より前の観測を捨てる
    fn prune(&mut self, oldest_us: i64) {
        while self
            .entries
            .front()
            .is_some_and(|(at_us, _)| *at_us < oldest_us)
        {
            self.entries.pop_front();
        }
    }

    /// 窓の中の最小値
    fn min_value(&self) -> Option<i64> {
        self.entries.iter().map(|(_, value_us)| *value_us).min()
    }

    /// 窓の中の最も古い記録 (時刻, 値)。無ければ None
    fn oldest(&self) -> Option<(i64, i64)> {
        self.entries.front().copied()
    }

    /// `since_us` 以降の記録の最小値。無ければ None
    ///
    /// 窓より短い区間の最小値を取り直すために使う。窓全体の最小値は、値が単調に動いて
    /// いるときに最も古い観測を指したままになる。
    fn min_after(&self, since_us: i64) -> Option<i64> {
        let mut min: Option<i64> = None;
        for (at_us, value_us) in self.entries.iter().rev() {
            if *at_us < since_us {
                break;
            }
            min = Some(min.map_or(*value_us, |min| min.min(*value_us)));
        }
        min
    }

    /// 昇順に並べた、窓の中の値
    fn sorted_values(&self) -> Vec<i64> {
        let mut values: Vec<i64> = self.entries.iter().map(|(_, value_us)| *value_us).collect();
        values.sort_unstable();
        values
    }

    /// 記録をすべて捨てる
    fn clear(&mut self) {
        self.entries.clear();
    }
}

/// 追いつき中の確認点
#[derive(Debug, Clone, Copy)]
struct CatchUpCheckpoint {
    /// 確認した時刻 (マイクロ秒)
    at_us: i64,
    /// 確認したときの基準の遅れ (マイクロ秒)
    base_us: i64,
    /// 不規則な遅着でもう 1 期間だけ確認し直したか
    rechecked: bool,
}

/// live に追いつくまでの間かを決める
///
/// [`TIMELINE_CATCH_UP_CHECK_INTERVAL_US`] ごとに基準の遅れの下がり幅を見て、
/// [`TIMELINE_CATCH_UP_MIN_BASE_DROP_US`] より小さければ追いついたとみなす。不規則な
/// 遅着の最中ならもう 1 期間だけ確認し、周期的な揺らぎで学習を止め続けることを防ぐ。
/// 起動直後に処理が止まると、最小値が動かないまま古い出力だけが返ることがある。その
/// 最初の遅着で完了すると、解消した停滞を通常の jitter として保持してしまう。一度
/// 追いついたら、基準を取り直すまで追いつき中に戻らない (live の揺らぎは学習する)。
fn is_catching_up(
    state: &mut TrackState,
    now_us: i64,
    base_us: i64,
    offset_us: i64,
    regular_arrival: bool,
) -> bool {
    if !state.catching_up {
        return false;
    }
    let Some(checkpoint) = state.catch_up_checkpoint else {
        state.catch_up_checkpoint = Some(CatchUpCheckpoint {
            at_us: now_us,
            base_us,
            rechecked: false,
        });
        return true;
    };
    if now_us.saturating_sub(checkpoint.at_us) < TIMELINE_CATCH_UP_CHECK_INTERVAL_US {
        return true;
    }
    // 最小の経路より 1 フレーム分を超えて古く、到着間隔も長い出力はまだ追いつき中である。
    // 遅い経路でも媒体の周期で届く場合は、通常の学習へ進める。
    let delayed_arrival = !regular_arrival
        && offset_us.saturating_sub(base_us)
            > TIMELINE_CATCH_UP_MIN_BASE_DROP_US.max(state.frame_interval_us().unwrap_or(0));
    if checkpoint.base_us.saturating_sub(base_us) >= TIMELINE_CATCH_UP_MIN_BASE_DROP_US {
        state.catch_up_checkpoint = Some(CatchUpCheckpoint {
            at_us: now_us,
            base_us,
            rechecked: false,
        });
        return true;
    }
    // 最小値が動かない不規則な遅着では、1 期間だけ確認し直す。周期的な揺らぎしか
    // 観測できない経路でも、その都度延期して初期学習を止め続けない。
    if delayed_arrival && !checkpoint.rechecked {
        state.catch_up_checkpoint = Some(CatchUpCheckpoint {
            at_us: now_us,
            base_us,
            rechecked: true,
        });
        return true;
    }
    state.catching_up = false;
    state.catch_up_checkpoint = None;
    false
}

/// トラック 1 つぶんの状態
#[derive(Debug)]
struct TrackState {
    /// (復号の出力の時刻 − TIMESTAMP) の窓 (直近 10 秒)。基準を求める
    offsets: TimedWindow,
    /// 揺らぎの学習に使う (復号の出力の時刻 − TIMESTAMP) の窓 (直近 2 秒)
    learning_offsets: TimedWindow,
    /// 基準の遅れ (マイクロ秒)
    base_us: Option<i64>,
    /// 自分の揺らぎから求めた jitter buffer の遅延 (マイクロ秒)
    ///
    /// 音声は到着の揺らぎの百分位と実出力の先行量の合計を 50〜100 ms に切った値、
    /// 映像は揺らぎの百分位を上限で切った値である。`target_latency` と同期の制御の分を
    /// 含まない。
    delay_us: Option<i64>,
    /// 直近の観測の復号の出力の時刻 (マイクロ秒)
    last_arrival_us: Option<i64>,
    /// 直近の観測の TIMESTAMP (マイクロ秒)
    last_timestamp_us: Option<i64>,
    /// 直近のフレーム間隔 (TIMESTAMP の差、マイクロ秒) の窓
    frame_intervals_us: VecDeque<i64>,
    /// live に追いつくまでの間か
    catching_up: bool,
    /// 追いつき中の確認点
    catch_up_checkpoint: Option<CatchUpCheckpoint>,
}

impl TrackState {
    /// 観測の無い状態で作る
    fn new() -> Self {
        Self {
            offsets: TimedWindow::default(),
            learning_offsets: TimedWindow::default(),
            base_us: None,
            delay_us: None,
            last_arrival_us: None,
            last_timestamp_us: None,
            frame_intervals_us: VecDeque::new(),
            catching_up: true,
            catch_up_checkpoint: None,
        }
    }

    /// 直近のフレーム間隔の中央値 (マイクロ秒)。まだ分からなければ None
    fn frame_interval_us(&self) -> Option<i64> {
        if self.frame_intervals_us.is_empty() {
            return None;
        }
        let mut sorted: Vec<i64> = self.frame_intervals_us.iter().copied().collect();
        // 整数なので通常の比較で並べ替える
        sorted.sort_unstable();
        Some(sorted[percentile_index(sorted.len(), 500)])
    }
}

/// 直近に表示すると決めた実績
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PresentationRecord {
    /// 表示した TIMESTAMP (マイクロ秒)
    timestamp_us: i64,
    /// 実際に表示する (鳴らす) 時刻 (受信側の壁時計、マイクロ秒)
    presented_wall_clock_us: i64,
}

/// 到着基準で予約した実出力の深さの観測
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AudioArrivalDelayObservation {
    /// 深さ (マイクロ秒)
    delay_us: i64,
    /// 観測した時刻 (復号の出力の時刻、マイクロ秒)
    at_us: i64,
}

/// 音声と映像に共通の再生時刻を決める
///
/// 観測のたびに [`PlayoutTimeline::observe`] を呼び、鳴らす・表示する時刻を
/// [`PlayoutTimeline::present_us`] で求める。A/V 同期の制御も観測のたびに動く。
/// 音声の再生の実出力は [`PlayoutTimeline::observe_audio_output_lead`] と
/// [`PlayoutTimeline::observe_audio_arrival_delay`] で記録する。
#[derive(Debug)]
pub struct PlayoutTimeline {
    /// 設定
    config: TimelineConfig,
    /// 音声と映像の状態
    tracks: [TrackState; 2],
    /// 到着の揺らぎの百分位 (音声)。未観測なら None
    audio_jitter_us: Option<i64>,
    /// 有効な学習が続き始めた時刻 (マイクロ秒)。10 秒の基準窓の古さとは区別する
    audio_learning_since_us: Option<i64>,
    /// 到着基準で予約した実出力の深さの直近の観測
    audio_arrival_delay_observation: Option<AudioArrivalDelayObservation>,
    /// 実出力の先行量の p95 (マイクロ秒)。有効な出力時計が未観測なら None
    audio_output_lead_us: Option<i64>,
    /// 実出力の先行量の窓 (直近 1 秒)
    audio_output_leads: TimedWindow,
    /// 同期の制御が各トラックへ足した遅延 (マイクロ秒)。0 以上
    sync_extra_us: [i64; 2],
    /// 直前に同期の制御を行った時刻 (マイクロ秒)。まだ行っていなければ None
    last_sync_us: Option<i64>,
    /// 基準を共有しないと決めた直近の時刻と、そのときの側 (マイクロ秒、トラック)。
    /// 往復を防ぐ
    last_unshared_at_us: Option<i64>,
    /// 基準を共有しないと決めた直近の側
    last_unshared_track: Option<Track>,
    /// その前の解除の時刻 (マイクロ秒)。直近に共有と解除を往復しているかを見るために
    /// 持つ。保持が満了して戻した場合も含める (往復の検出に使う)
    previous_unshared_at_us: Option<i64>,
    /// 差が戻った状態 (閾値の内側で動かない) になってからの時刻 (マイクロ秒)
    unshared_returned_since_us: Option<i64>,
    /// 直前に同期の制御に使った「自分の遅延の下限」(マイクロ秒)
    last_own_floor_us: Option<[i64; 2]>,
    /// 2 つのトラックの基準の差の直近の履歴 (マイクロ秒)。動きの速さを出すために使う
    base_differences: TimedWindow,
    /// 直前に基準の差を記録した時刻 (マイクロ秒)。まだ記録していなければ None
    last_base_difference_at_us: Option<i64>,
    /// 直前に記録した基準の差 (マイクロ秒)。短い窓の最小値の差であり、動きの判定が
    /// 使う値である
    base_difference_value_us: Option<i64>,
    /// 差が離れ始めたときの、表示時刻が使う基準の差 (マイクロ秒)
    base_difference_deviation_reference_us: Option<i64>,
    /// 直前に記録したときの、表示時刻が使う基準の差 (マイクロ秒)。離れ始めの参照に使う
    last_reference_us: Option<i64>,
    /// 差が動いていないときの水準 (マイクロ秒)。一過性の動きを見分ける基準になる
    settled_base_difference_us: Option<i64>,
    /// 差がその水準から [`TIMELINE_BASE_DRIFT_US`] を超えて離れたままになっている始まりの
    /// 時刻 (マイクロ秒)
    base_difference_deviation_since_us: Option<i64>,
    /// 差が動かなくなってからの時刻 (マイクロ秒)。動いていれば None
    base_difference_quiet_since_us: Option<i64>,
    /// 差を最初に記録した時刻 (マイクロ秒)
    base_difference_first_at_us: Option<i64>,
    /// 直近の記録で求めた、差が動き続けているかどうか。判定は記録のたびに 1 回だけ行う
    base_difference_drifted_value: bool,
    /// 使う `targetLatency` (ミリ秒)。0 は「下限にしない」
    target_latency_ms: i64,
    /// 上限に収まらず切り下げた分 (マイクロ秒)
    limited_us: i64,
    /// 基準を取り直した回数
    generation: u64,
    /// 直近の表示の実績
    presented: [Option<PresentationRecord>; 2],
    /// 予約時の予定と実際の出力の差 (マイクロ秒)
    ///
    /// 予定そのものを測り直すと、補償した分が再び残差へ入るため、平滑化した値だけを
    /// 持つ。映像の合わせ先にだけ足し、音声の表示時刻には含めない。
    audio_playout_correction_us: i64,
    /// 有効な出力時計で残差を測れた最後の時刻 (受信側の壁時計、マイクロ秒)
    audio_playout_correction_at_us: Option<i64>,
}

impl PlayoutTimeline {
    /// 既定の設定で作る
    pub fn new() -> Self {
        Self::with_config(TimelineConfig::default())
    }

    /// 設定を決めて作る
    ///
    /// 負のミリ秒の値は 0 として扱う。
    pub fn with_config(mut config: TimelineConfig) -> Self {
        config.max_presentation_delay_ms = config.max_presentation_delay_ms.max(0);
        config.target_latency_ms = config.target_latency_ms.max(0);
        Self {
            config,
            tracks: [TrackState::new(), TrackState::new()],
            audio_jitter_us: None,
            audio_learning_since_us: None,
            audio_arrival_delay_observation: None,
            audio_output_lead_us: None,
            audio_output_leads: TimedWindow::default(),
            sync_extra_us: [0, 0],
            last_sync_us: None,
            last_unshared_at_us: None,
            last_unshared_track: None,
            previous_unshared_at_us: None,
            unshared_returned_since_us: None,
            last_own_floor_us: None,
            base_differences: TimedWindow::default(),
            last_base_difference_at_us: None,
            base_difference_value_us: None,
            base_difference_deviation_reference_us: None,
            last_reference_us: None,
            settled_base_difference_us: None,
            base_difference_deviation_since_us: None,
            base_difference_quiet_since_us: None,
            base_difference_first_at_us: None,
            base_difference_drifted_value: false,
            target_latency_ms: config.target_latency_ms,
            limited_us: 0,
            generation: 0,
            presented: [None, None],
            audio_playout_correction_us: 0,
            audio_playout_correction_at_us: None,
        }
    }

    /// 使っている設定
    pub fn config(&self) -> TimelineConfig {
        self.config
    }

    /// 復号の出力を 1 つ記録する
    ///
    /// `wall_clock_us` は復号の出力の時刻 (受信側の壁時計)、`timestamp_us` はその
    /// データの TIMESTAMP (どちらもマイクロ秒)。TIMESTAMP と壁時計の関係が基準から
    /// [`TIMELINE_DISCONTINUITY_US`] 以上離れたときは、配信元の切り替えとみなして
    /// 基準を取り直す (世代を進める)。
    pub fn observe(&mut self, track: Track, wall_clock_us: i64, timestamp_us: i64) {
        let offset_us = wall_clock_us.saturating_sub(timestamp_us);
        if let Some(base_us) = self.tracks[track.index()].base_us
            && offset_us.saturating_sub(base_us).saturating_abs() >= TIMELINE_DISCONTINUITY_US
        {
            // 共有の時間軸ごと取り直す。両方のトラックが同じだけ動くため同期は保たれる
            self.reset();
        }
        let index = track.index();

        // フレーム間隔を更新し、揺らぎの学習に使うかを決める
        let mut learns = false;
        let mut regular_arrival = false;
        {
            let state = &mut self.tracks[index];
            if let (Some(last_timestamp_us), Some(last_arrival_us)) =
                (state.last_timestamp_us, state.last_arrival_us)
            {
                let interval_us = timestamp_us.saturating_sub(last_timestamp_us);
                if interval_us > 0 {
                    state.frame_intervals_us.push_back(interval_us);
                    while state.frame_intervals_us.len() > TIMELINE_FRAME_INTERVAL_SAMPLES {
                        state.frame_intervals_us.pop_front();
                    }
                }
                let arrival_interval_us = wall_clock_us.saturating_sub(last_arrival_us);
                // まとまって届いた観測 (前の観測から、現フレームの TIMESTAMP 間隔の半分
                // 未満) は学習に使わない。間隔の中央値ではなく現フレームの間隔で判定する
                learns = arrival_interval_us.saturating_mul(2) >= interval_us;
                // まとめた出力とは逆に、長い停止の最初の出力も通常の周期とは区別する。
                // 媒体の欠落で両方の間隔が同じだけ開く場合は、通常の到着として扱う
                regular_arrival = interval_us > 0
                    && learns
                    && arrival_interval_us <= interval_us.saturating_mul(2);
            }
        }
        {
            let state = &mut self.tracks[index];
            state.last_timestamp_us = Some(timestamp_us);
            state.last_arrival_us = Some(wall_clock_us);
            // 基準の遅れ (直近 10 秒の最小値) を更新する
            state.offsets.push(wall_clock_us, offset_us);
            state
                .offsets
                .prune(wall_clock_us.saturating_sub(TIMELINE_WINDOW_US));
        }
        let Some(base_us) = self.tracks[index].offsets.min_value() else {
            // 窓の外の観測だけになった (呼び出し側が時刻を戻したなど)。基準は更新しない
            return;
        };
        self.tracks[index].base_us = Some(base_us);
        self.record_base_difference(wall_clock_us);

        // 学習の窓を更新する。音声は窓が空になったら学習の開始を忘れる
        let state = &mut self.tracks[index];
        state
            .learning_offsets
            .prune(wall_clock_us.saturating_sub(TIMELINE_JITTER_WINDOW_US));
        if track == Track::Audio && state.learning_offsets.oldest().is_none() {
            self.audio_learning_since_us = None;
        }
        // 追いつき中の出力と、停滞後にまとめて復号した分を経路の分布へ混ぜない
        let catching_up = is_catching_up(state, wall_clock_us, base_us, offset_us, regular_arrival);
        if learns && !catching_up {
            state.learning_offsets.push(wall_clock_us, offset_us);
        }
        state
            .learning_offsets
            .prune(wall_clock_us.saturating_sub(TIMELINE_JITTER_WINDOW_US));

        // 揺らぎ (基準からの遅れ) の百分位を求める。表示の遅れの上限を超える揺らぎは
        // 吸収できないため、分位点へ入れる前に除外する
        let jitters: Vec<i64> = state
            .learning_offsets
            .sorted_values()
            .into_iter()
            .map(|offset_us| offset_us.saturating_sub(base_us).max(0))
            .filter(|jitter_us| {
                *jitter_us <= TIMELINE_MAX_PRESENTATION_DELAY_MS.saturating_mul(1_000)
            })
            .collect();
        let frame_interval_us = state.frame_interval_us();
        let percentile = playout_delay_percentile(frame_interval_us);
        let jitter_us = percentile_ppm(&jitters, percentile);
        if track == Track::Audio {
            self.audio_jitter_us = if jitters.is_empty() {
                None
            } else {
                Some(jitter_us)
            };
            if self.audio_jitter_us.is_none() {
                self.audio_learning_since_us = None;
            } else if self.audio_learning_since_us.is_none() {
                self.audio_learning_since_us = Some(wall_clock_us);
            }
            self.update_audio_delay();
        } else {
            // 古い分布が窓から消えたら必要値へ戻す。上限超過しか残らない空の分布は、
            // 正常な 0 ms の観測ではないため、直前の値を保つ
            let target_us = if jitters.is_empty() {
                self.tracks[index].delay_us.unwrap_or(0)
            } else {
                jitter_us
            };
            self.tracks[index].delay_us = Some(target_us.min(self.delay_cap_us(frame_interval_us)));
        }

        self.update_sync_delays(wall_clock_us);
        self.update_limited_us();
    }

    /// 実出力の時計から、今予約できる最早時刻までの先行量を記録する
    ///
    /// moqt-js の `observeAudioOutputLead` に対応する。負の値は 0 として扱う。
    pub fn observe_audio_output_lead(&mut self, lead_us: i64, wall_clock_us: i64) {
        self.audio_output_leads.push(wall_clock_us, lead_us.max(0));
        self.audio_output_leads
            .prune(wall_clock_us.saturating_sub(TIMELINE_AUDIO_OUTPUT_LEAD_WINDOW_US));
        self.audio_output_lead_us = Some(percentile_ppm(
            &self.audio_output_leads.sorted_values(),
            TIMELINE_P95_PERCENTILE_PPM,
        ));
        self.update_audio_delay();
        self.update_sync_delays(wall_clock_us);
    }

    /// 到着基準で予約した実出力の深さを記録する
    ///
    /// 目標の予算と混同せず、映像の同期先へ使う。`None` を渡すと観測を捨てる
    /// (出力を止めたときなど)。moqt-js の `observeAudioArrivalDelay` に対応する。
    pub fn observe_audio_arrival_delay(&mut self, delay_us: Option<i64>, wall_clock_us: i64) {
        self.audio_arrival_delay_observation =
            delay_us.map(|delay_us| AudioArrivalDelayObservation {
                delay_us: delay_us
                    .clamp(0, TIMELINE_MAX_PRESENTATION_DELAY_MS.saturating_mul(1_000)),
                at_us: wall_clock_us,
            });
        self.update_sync_delays(wall_clock_us);
    }

    /// 到着の分布が揃うまでは既存の不感帯を保ち、静かな経路だけ細かく回復する
    ///
    /// 時間圧縮を始める遅れ (マイクロ秒) を返す。音声の揺らぎの学習が
    /// [`TIMELINE_JITTER_WINDOW_US`] ぶん続いていれば、揺らぎの大きさまで細かく詰める。
    /// 返り値は `min(default_us, max(`[`TIMELINE_MIN_COMPRESSION_DEADBAND_US`]`,
    /// audio_jitter_us))` である。
    /// moqt-js の `compressionDeadbandSeconds` に対応する (あちらは秒の浮動小数で返す)。
    ///
    /// 次のどれかに当たるときは `default_us` をそのまま返す。
    ///
    /// - 音声の揺らぎをまだ学習していない (`audio_learning_since_us` が無い)
    /// - 音声の到着をまだ観測していない
    /// - 学習が [`TIMELINE_JITTER_WINDOW_US`] ぶん続いていない (窓が埋まっていない)
    /// - 音声の揺らぎが無い (`audio_jitter_us` が無い)
    pub fn compression_deadband_us(&self, default_us: i64) -> i64 {
        let audio = &self.tracks[Track::Audio.index()];
        let Some(learning_since_us) = self.audio_learning_since_us else {
            return default_us;
        };
        let Some(last_arrival_us) = audio.last_arrival_us else {
            return default_us;
        };
        if last_arrival_us.saturating_sub(learning_since_us) < TIMELINE_JITTER_WINDOW_US {
            return default_us;
        }
        let Some(audio_jitter_us) = self.audio_jitter_us else {
            return default_us;
        };
        default_us.min(audio_jitter_us.max(TIMELINE_MIN_COMPRESSION_DEADBAND_US))
    }

    /// 既に届く音の揺らぎに、出力バッファの先行量を一度だけ足す
    fn update_audio_delay(&mut self) {
        if self.tracks[Track::Audio.index()].base_us.is_none() {
            return;
        }
        let learned_us = self.audio_jitter_us.unwrap_or(0) + self.audio_output_lead_us.unwrap_or(0);
        self.tracks[Track::Audio.index()].delay_us = Some(audio_arrival_delay_us(learned_us));
    }

    /// 音声の待ちを増やす根拠と、共有の時間軸へ適用する予算
    pub fn audio_delay_stats(&self) -> AudioPlayoutDelayStats {
        AudioPlayoutDelayStats {
            jitter_us: self.audio_jitter_us,
            output_lead_us: self.audio_output_lead_us,
            target_us: self
                .playout_delay_us()
                .unwrap_or(TIMELINE_AUDIO_DELAY_FLOOR_US),
        }
    }

    /// 使う `targetLatency` を決める (ミリ秒。使わないときは 0)
    ///
    /// 同じ render group の track は同じ値でなければならない (draft-ietf-moq-msf-01
    /// §5.2.8) ため、音声と映像で 1 つの値を使う。解決の規則は呼び出し側が持ち、ここへは
    /// 確定した値だけを渡す。値は同期の基準の遅延になり、2 つのトラックの表示の遅れの
    /// 下限になる。0 は「下限にしない」の意味であり、moqt-js の null に相当する。
    pub fn set_target_latency_ms(&mut self, target_latency_ms: i64) {
        self.target_latency_ms = target_latency_ms.max(0);
        // moqt-js は同期の自然な表示の遅れを求める中で解除の判定も更新する。同じ順序で
        // 保持の状態を先に更新する
        self.refresh_unshared_state();
        // 基準の遅延は 2 つのトラックの表示の遅れの下限になる。片方だけがこの下限に当たる
        // ことがあるため、差が開いていればその場で合わせ直す (次の観測を待つと、その間だけ
        // 表示時刻の差が開いたままになる)。過去の補償を保持せず、現在の必要量を計算する
        if let Some(natural_us) = self.sync_natural_presentation_us() {
            self.align_sync_extras(natural_us);
        }
        self.update_limited_us();
    }

    /// 使っている `targetLatency` (ミリ秒)。0 は使っていない
    pub fn target_latency_ms(&self) -> i64 {
        self.target_latency_ms
    }

    /// 上限に収まらず切り下げた分 (マイクロ秒)
    pub fn target_latency_limited_us(&self) -> i64 {
        self.limited_us
    }

    /// 表示の遅れ (マイクロ秒)。TIMESTAMP から表示時刻までの差そのものである
    ///
    /// 基準の遅れを含む。映像を観測していなければ音声の値、どちらも無ければ None。
    /// 時計のずれの分だけ負にもなる。moqt-js の `presentationDelayMs` に対応し、ログや
    /// 解析のように「TIMESTAMP から表示までどれだけ待つか」を読むために使う。
    ///
    /// 基準の遅れを含まない jitter buffer の遅延は、トラックを指定する
    /// [`PlayoutTimeline::presentation_extra_delay_us`] (音声) と
    /// [`PlayoutTimeline::video_delay_us`] (映像) で読む。スケジューラへ渡す値は
    /// そちらである (時計がずれている間も表示の並べ方は決まっているため)。
    pub fn presentation_delay_us(&self) -> Option<i64> {
        self.presentation_delay_us_of(Track::Video)
            .or_else(|| self.presentation_delay_us_of(Track::Audio))
    }

    /// 音声の jitter buffer の遅延 (マイクロ秒)。まだ観測していなければ None
    ///
    /// 音声の自動予算 (上限適用後) である。直近の到着の揺らぎと、出力時計で測った先行量を
    /// 足し、50〜100 ms に収める。過去の穴や時間圧縮を理由に、別の上限を広げない。
    /// moqt-js の `playoutDelayMs` に対応する。
    ///
    /// 壁時計の TIMESTAMP を持たない音を到着基準で並べるときの再生の遅れに使う。
    /// `target_latency` と同期の制御による下限は含まない
    /// ([`PlayoutTimeline::presentation_extra_delay_us`] を使う)。
    pub fn playout_delay_us(&self) -> Option<i64> {
        let own_delay_us = self.tracks[Track::Audio.index()].delay_us?;
        Some(own_delay_us.min(self.presentation_cap_us()))
    }

    /// 音声の表示の遅れのうち、基準の遅れを除いた分 (マイクロ秒)
    ///
    /// 自分の揺らぎと `target_latency` の大きい方に、同期の制御が足した分
    /// ([`TrackBreakdown::sync_extra_delay_us`]) を加えた値であり、上限で切る。まだ観測
    /// していなければ None。
    ///
    /// [`PlayoutTimeline::presentation_delay_us`] が基準の遅れを含むのに対し、こちらは
    /// 含まない。名前の "extra" は「基準の遅れを除いた分」の意味であり、同期の制御が
    /// 足した分だけを指すのではない。moqt-js の `presentationExtraDelayMs` に対応し、
    /// 音声を鳴らす時刻を決めるスケジューラへ渡す値である。時計がずれている
    /// (TIMESTAMP を使えない) 間も、音声の並べ方はこの値で決まる。
    pub fn presentation_extra_delay_us(&self) -> Option<i64> {
        self.delay_us_of(Track::Audio)
    }

    /// 映像の jitter buffer の遅延 (マイクロ秒)
    ///
    /// 映像の表示待ちのキューが保持する長さである。`target_latency` と同期の制御が
    /// 足した分を含む。まだ観測していなければ None。
    pub fn video_delay_us(&self) -> Option<i64> {
        self.delay_us_of(Track::Video)
    }

    /// 目標の表示時刻 (受信側の壁時計、Unix epoch マイクロ秒)
    ///
    /// `timestamp_us` はそのデータの TIMESTAMP (Unix epoch マイクロ秒)。
    /// moqt-js の `presentationWallClockMicros` に対応する。このトラックの TIMESTAMP を
    /// 使わない (基準がまだ無い、または基準がずれている) ときは None。
    pub fn present_us(&self, track: Track, timestamp_us: i64) -> Option<i64> {
        let delay_us = self.presentation_delay_us_of(track)?;
        Some(timestamp_us.saturating_add(delay_us))
    }

    /// 表示した (鳴らすと決めた) 実績を記録する
    ///
    /// `timestamp_us` は表示した TIMESTAMP (Unix epoch マイクロ秒)、
    /// `presented_wall_clock_us` は実際に表示する (鳴らす) 時刻 (Unix epoch マイクロ秒)。
    pub fn record_presentation(
        &mut self,
        track: Track,
        timestamp_us: i64,
        presented_wall_clock_us: i64,
    ) {
        self.record_presentation_with_target(track, timestamp_us, presented_wall_clock_us, None);
    }

    /// 表示した実績と、音声を予約した時点の予定を記録する
    ///
    /// `target_wall_clock_us` は音声を予約した時点の予定 (受信側の壁時計、マイクロ秒)。
    /// 出力の時計が有効なときだけ渡す。映像では無視する。音声では、予定より遅れた分
    /// (`presented_wall_clock_us - target_wall_clock_us`、0 以上
    /// [`TIMELINE_MAX_PRESENTATION_DELAY_MS`] 以下) を 250 ms の時定数で平滑化し、
    /// [`PlayoutTimeline::delay_breakdown`] の `audio_playout_correction_us` と、映像の
    /// 合わせ先へ反映する。
    pub fn record_presentation_with_target(
        &mut self,
        track: Track,
        timestamp_us: i64,
        presented_wall_clock_us: i64,
        target_wall_clock_us: Option<i64>,
    ) {
        if track == Track::Audio
            && let Some(target_wall_clock_us) = target_wall_clock_us
        {
            // 出力の都合で予定より遅れた分を、映像の合わせ先へ反映する。残差そのものを
            // 平滑化し、既に補償した映像の遅延を入力へ戻さない。250 ms の時定数で瞬間的な
            // 予約の揺れを抑える。最初の出力と長い停止の後は、その場で対応を作り直す
            let elapsed_us = self
                .audio_playout_correction_at_us
                .map(|at_us| presented_wall_clock_us.saturating_sub(at_us));
            let residual_us = presented_wall_clock_us
                .saturating_sub(target_wall_clock_us)
                .max(0)
                .min(TIMELINE_MAX_PRESENTATION_DELAY_MS.saturating_mul(1_000));
            self.audio_playout_correction_us = match elapsed_us {
                Some(elapsed_us) if elapsed_us <= TIMELINE_SKEW_WINDOW_US => {
                    // JS の `correction += weight * (residual - correction)`、
                    // `weight = elapsed / (SMOOTHING + elapsed)` を、重み付き平均の式
                    // `(correction * SMOOTHING + residual * elapsed) / (SMOOTHING + elapsed)`
                    // で整数に写す。差分へ掛ける形にすると、差分が小さいときに丸めで
                    // 0 に収束せず残るため、この形にする。再帰フィルタなので切り捨てを
                    // 続けると定常状態で下振れが積もる (20 ms 間隔で約 7 µs)。四捨五入して
                    // 偏りを無くす (JS の浮動小数との差は 1 マイクロ秒未満)
                    let elapsed_us = elapsed_us.max(0);
                    let denominator_us =
                        TIMELINE_AUDIO_PLAYOUT_CORRECTION_SMOOTHING_US.saturating_add(elapsed_us);
                    let numerator_us = self
                        .audio_playout_correction_us
                        .saturating_mul(TIMELINE_AUDIO_PLAYOUT_CORRECTION_SMOOTHING_US)
                        .saturating_add(residual_us.saturating_mul(elapsed_us));
                    // 0 以上であるため、分母の半分を足して切り捨てれば四捨五入になる
                    numerator_us
                        .saturating_add(denominator_us / 2)
                        .saturating_div(denominator_us)
                }
                // 最初の出力と、前回から 1 秒以上空いた (停止の後の) 出力は、その場で
                // 作り直す
                _ => residual_us,
            };
            self.audio_playout_correction_at_us = Some(presented_wall_clock_us);
        }
        let record = PresentationRecord {
            timestamp_us,
            presented_wall_clock_us,
        };
        self.presented[track.index()] = Some(record);
        // 音声の実績は未来の予約なので、制御の時刻には今回の到着を使う
        let observed_wall_clock_us = if track == Track::Audio {
            self.tracks[Track::Audio.index()].last_arrival_us
        } else {
            Some(presented_wall_clock_us)
        };
        if let Some(observed_wall_clock_us) = observed_wall_clock_us {
            self.update_sync_delays(observed_wall_clock_us);
        }
    }

    /// 同期ずれの推定値 (マイクロ秒)。映像の表示が音声より遅れていれば正
    ///
    /// 直近に表示した音声と映像それぞれの「表示時刻 − TIMESTAMP」の差を取る。両方が
    /// [`TIMELINE_SKEW_WINDOW_US`] 以内の実績を持つときだけ値を返す。捨てた音と write
    /// しなかったフレームは実績に含めないため、捨てが続くと実績が古くなり None になる。
    pub fn skew_us(&self) -> Option<i64> {
        let audio = self.presented[Track::Audio.index()]?;
        let video = self.presented[Track::Video.index()]?;
        let now_us = audio
            .presented_wall_clock_us
            .max(video.presented_wall_clock_us);
        if now_us.saturating_sub(audio.presented_wall_clock_us) > TIMELINE_SKEW_WINDOW_US
            || now_us.saturating_sub(video.presented_wall_clock_us) > TIMELINE_SKEW_WINDOW_US
        {
            return None;
        }
        let audio_offset_us = audio
            .presented_wall_clock_us
            .saturating_sub(audio.timestamp_us);
        let video_offset_us = video
            .presented_wall_clock_us
            .saturating_sub(video.timestamp_us);
        Some(video_offset_us.saturating_sub(audio_offset_us))
    }

    /// 2 つのトラックで基準を共有しているか
    ///
    /// 共有できないのは、どちらかをまだ観測していないときと、基準の差が
    /// 「遅い側を待つ」ことで合わせられないときである (差が表示の遅れの上限を超えている、
    /// または差が動き続けている = TIMESTAMP が壁時計からずれている)。理由は
    /// [`PlayoutTimeline::delay_breakdown`] の `unshared_reason` に出る。
    pub fn sharing_bases(&self) -> bool {
        self.unshared_reason() == UnsharedReason::None
    }

    /// 基準を共有できていない理由
    ///
    /// 未観測 (どちらかの基準か表示の遅れがまだ無い) を先に見る。差と動きの判定は基準が
    /// そろってから意味を持つ (差が 0 であるとも、動きが無いとも言えない)。動きは差より
    /// 先に見る。動き続けている差は経路の遅れではなく時計のずれであり、合わせることを
    /// やめる原因そのものであるため、差の大きさより先に知りたい。
    pub fn unshared_reason(&self) -> UnsharedReason {
        let audio = &self.tracks[Track::Audio.index()];
        let video = &self.tracks[Track::Video.index()];
        if audio.base_us.is_none()
            || video.base_us.is_none()
            || audio.delay_us.is_none()
            || video.delay_us.is_none()
        {
            return UnsharedReason::Unobserved;
        }
        if self.base_difference_drifted() {
            return UnsharedReason::Drift;
        }
        if self
            .current_base_difference_us()
            .unwrap_or(0)
            .saturating_abs()
            > self.base_difference_limit_us()
        {
            return UnsharedReason::Difference;
        }
        // 直前にやめた判定を保持している (閾値が動いても往復させない)
        if self.held_unshared_stream().is_some() {
            return UnsharedReason::Hold;
        }
        UnsharedReason::None
    }

    /// 音声と映像の遅延の内訳 (遅延の解析に使う)
    ///
    /// 「表示の遅れがどこで生じているか」と「2 つのトラックを同じ時計として扱えている
    /// か」を 1 つの値にまとめる。表示時刻そのものは [`PlayoutTimeline::present_us`] が
    /// 返す。
    pub fn delay_breakdown(&self) -> DelayBreakdown {
        // 動きの判定に使っている水準と、そこからの隔たり。水準がまだ無ければ隔たりも無い
        let settled_us = self.settled_base_difference_us;
        let value_us = self.base_difference_value_us;
        DelayBreakdown {
            audio: self.track_breakdown(Track::Audio),
            video: self.track_breakdown(Track::Video),
            audio_playout_correction_us: self.audio_playout_correction_us,
            base_difference_us: self.current_base_difference_us(),
            base_difference_recent_us: value_us,
            base_difference_settled_us: settled_us,
            base_difference_deviation_us: match (settled_us, value_us) {
                (Some(settled_us), Some(value_us)) => {
                    Some(value_us.saturating_sub(settled_us).saturating_abs())
                }
                _ => None,
            },
            sharing_bases: self.sharing_bases(),
            unshared_reason: self.unshared_reason(),
            base_drift_us_per_second: self.base_drift_us_per_second(),
            base_drift_limit_us: TIMELINE_BASE_DRIFT_US,
            base_unshared_return_us: self.base_unshared_return_us(),
            presentation_delay_cap_us: self.presentation_cap_us(),
            audio_delay: self.audio_delay_stats(),
        }
    }

    /// 直近の基準の差の動き (マイクロ秒 / 秒)。まだ履歴が無ければ None
    fn base_drift_us_per_second(&self) -> Option<i64> {
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
    fn base_unshared_return_us(&self) -> Option<i64> {
        let returned_since_us = self.unshared_returned_since_us?;
        let last_base_difference_at_us = self.last_base_difference_at_us?;
        Some(
            last_base_difference_at_us
                .saturating_sub(returned_since_us)
                .max(0),
        )
    }

    /// トラックごとの表示時刻の内訳
    fn track_breakdown(&self, track: Track) -> TrackBreakdown {
        let state = &self.tracks[track.index()];
        TrackBreakdown {
            base_delay_us: state.base_us,
            jitter_delay_us: state.delay_us,
            sync_extra_delay_us: self.sync_extra_us[track.index()],
            presentation_delay_us: self.presentation_delay_us_of(track),
            presentation_delay_cap_us: self.presentation_cap_us(),
        }
    }

    /// 2 つのトラックの基準の差を記録する ([`TIMELINE_BASE_DIFFERENCE_SAMPLE_INTERVAL_US`]
    /// ごと)
    ///
    /// 記録するのは短い区間の最小値の差である。窓全体の最小値の差は、TIMESTAMP が壁時計
    /// から遅れていくときも窓が埋まるまで動かないため、動きを早く見つけられない。短い
    /// 窓は経路と復号の乱れでも動くため、待たずに判定するときは窓全体の差 (参照) を
    /// 確かめる。
    fn record_base_difference(&mut self, now_us: i64) {
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
    fn base_difference_drifted(&self) -> bool {
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
    fn current_base_difference_us(&self) -> Option<i64> {
        let audio_base_us = self.tracks[Track::Audio.index()].base_us?;
        let video_base_us = self.tracks[Track::Video.index()].base_us?;
        Some(audio_base_us.saturating_sub(video_base_us))
    }

    /// 1 つのトラックの基準と学習と実績だけを消す。世代は進めない
    ///
    /// 音声の再生を止めたときなど、そのトラックを観測していない状態に戻す。表示時刻の式は
    /// 残ったトラックの値だけで決まるようになる。世代を進めると、既に積んでいる映像
    /// フレームの表示時刻が決められなくなり、到着順に落ちてしまう。
    ///
    /// 同期が足していた分は即座に 0 に戻す。残ったトラックの表示時刻が前に動くため、その
    /// トラックのキューに積んだフレームは表示時刻を過ぎ、目標表示時刻に最も近い 1 枚以外が
    /// 捨てられ得る。
    /// 消えたトラックの遅延を引きずり続けるより、この副作用を受け入れる。
    pub fn reset_track(&mut self, track: Track) {
        self.tracks[track.index()] = TrackState::new();
        self.presented[track.index()] = None;
        if track == Track::Audio {
            self.audio_playout_correction_us = 0;
            self.audio_playout_correction_at_us = None;
            self.audio_jitter_us = None;
            self.audio_learning_since_us = None;
            self.audio_arrival_delay_observation = None;
            self.audio_output_lead_us = None;
            self.audio_output_leads.clear();
        }
        // 同期の制御の状態も、そのトラックの観測が無い状態に戻す。基準の遅延
        // (target_latency) は呼び出し側が決めた値であり、残す。もう片方に足した分は、
        // 消したトラックとの差を合わせるためのものなので残さない (残すと、消えたトラック
        // の遅延を引きずり続ける)
        self.sync_extra_us = [0, 0];
        self.last_sync_us = None;
        self.last_own_floor_us = None;
        // 基準の差の履歴も消す (片方の基準が無い状態の差に意味は無い)
        self.clear_base_differences();
        // 学習を消すとキューの上限 (フレーム間隔) も変わるため、切り下げた分を取り直す
        self.update_limited_us();
    }

    /// 基準と学習をすべて消す。次の観測で作り直す (TIMESTAMP の飛び、購読のやり直し)
    ///
    /// 世代を進める。
    pub fn reset(&mut self) {
        self.generation = self.generation.saturating_add(1);
        for track in [Track::Audio, Track::Video] {
            self.reset_track(track);
        }
    }

    /// 基準を取り直した回数
    ///
    /// 取り直すと、それより前に積んだフレームの表示時刻は新しい基準では決められない
    /// (飛びの分だけ未来になる)。積む側が世代を見て、古いフレームを到着順に扱えるように
    /// する。
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// 表示の遅れの上限 (マイクロ秒)
    fn presentation_cap_us(&self) -> i64 {
        presentation_cap_us(
            &self.config,
            self.tracks[Track::Video.index()].frame_interval_us(),
        )
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
    fn update_sync_delays(&mut self, now_us: i64) {
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

    /// 音声を到着基準で並べるときの再生の遅れ (マイクロ秒)。まだ観測していなければ None
    ///
    /// 音声の TIMESTAMP が信用できないとき (基準を共有できないとき) に、映像を合わせる
    /// 先の時刻である。有効な実出力時計による新鮮な予約の深さを使う。まだ実値が無い
    /// 場合だけ予定の予算 ([`audio_arrival_delay_us`]) へフォールバックする。
    pub fn audio_arrival_delay_us(&self) -> Option<i64> {
        let now_us = match (
            self.tracks[Track::Audio.index()].last_arrival_us,
            self.tracks[Track::Video.index()].last_arrival_us,
        ) {
            (Some(audio_us), Some(video_us)) => audio_us.max(video_us),
            (Some(audio_us), None) => audio_us,
            (None, Some(video_us)) => video_us,
            (None, None) => i64::MIN,
        };
        if let Some(observation) = self.audio_arrival_delay_observation
            && now_us.saturating_sub(observation.at_us) <= TIMELINE_AUDIO_ARRIVAL_DELAY_WINDOW_US
        {
            return Some(observation.delay_us);
        }
        let playout_delay_us = self.playout_delay_us()?;
        Some(audio_arrival_delay_us(playout_delay_us))
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
    /// 先行側は [`TIMELINE_SYNC_MIN_DELTA_US`] だけ手前へ寄せ、現在の自然な表示の遅れ
    /// から毎回求め直す。基準を共有できないときだけ [`PlayoutTimeline::update_sync_delays`]
    /// が残った補償を徐々に戻す。
    ///
    /// `natural_us` は同期が足した分を除いた、トラックごとの表示の遅れ (マイクロ秒)。
    fn align_sync_extras(&mut self, natural_us: [i64; 2]) {
        let floor_us = natural_us[Track::Audio.index()].max(natural_us[Track::Video.index()]);
        for track in [Track::Audio, Track::Video] {
            let index = track.index();
            let other = track.other();
            // 先行する側だけ不感帯を残す
            let margin_us = if natural_us[index] < natural_us[other.index()] {
                TIMELINE_SYNC_MIN_DELTA_US
            } else {
                0
            };
            // 自分の jitter が既存の補償を埋めたら、その分を必ず差し引く。表示時刻その
            // ものを前へ動かさずに余分な補償を解消でき、共通の待ち時間も積み上がらない
            let room_us = self
                .presentation_cap_us()
                .saturating_sub(self.natural_delay_us_of(track).unwrap_or(0))
                .max(0);
            let wanted_us = floor_us
                .saturating_sub(margin_us)
                .saturating_sub(natural_us[index])
                .max(0);
            self.sync_extra_us[index] = room_us.min(wanted_us);
        }
    }

    /// トラックの表示の遅れ (マイクロ秒)。TIMESTAMP から表示時刻までの差であり、基準の
    /// 遅れと jitter buffer の遅延の和。まだ基準が無ければ None
    fn presentation_delay_us_of(&self, track: Track) -> Option<i64> {
        let state = &self.tracks[track.index()];
        let base_us = state.base_us?;
        // 基準が大きく離れているときは、大きい方のトラックの TIMESTAMP は壁時計から
        // ずれているとみなし、表示時刻を返さない (使う側が到着基準の再生へフォールバック
        // する)。表示時刻を返すと、ずれた分だけ未来の時刻になり、そのトラックがすべて
        // 捨てられる
        if self.drifted_stream() == Some(track) {
            return None;
        }
        let delay_us = self.delay_us_of(track)?;
        Some(base_us.saturating_add(delay_us))
    }

    /// トラックの jitter buffer の遅延 (マイクロ秒)。自分の揺らぎから求めた値と
    /// `target_latency` の大きい方に、同期の制御が足した分を加えた値であり、上限
    /// ([`PlayoutTimeline::presentation_cap_us`]) で切る。まだ観測していなければ None
    fn delay_us_of(&self, track: Track) -> Option<i64> {
        let uncapped_us = self.uncapped_delay_us_of(track)?;
        Some(uncapped_us.min(self.presentation_cap_us()))
    }

    /// 上限を掛ける前の jitter buffer の遅延 (マイクロ秒)
    ///
    /// 同期が足した分を含む。観測が無ければ None。
    fn uncapped_delay_us_of(&self, track: Track) -> Option<i64> {
        let natural_us = self.natural_delay_us_of(track)?;
        // 同期が足した分は、自分の下限の上に乗る
        Some(natural_us.saturating_add(self.sync_extra_us[track.index()]))
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
    fn drifted_stream(&self) -> Option<Track> {
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
    fn refresh_unshared_state(&mut self) {
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
    fn held_unshared_stream(&self) -> Option<Track> {
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
    fn base_difference_limit_us(&self) -> i64 {
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
    fn sync_natural_presentation_us(&self) -> Option<[i64; 2]> {
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
            .saturating_sub(TIMELINE_SYNC_MIN_DELTA_US)
            .max(0);
        if difference_us >= 0 {
            Some([audio_us, video_us.saturating_add(uncorrected_us)])
        } else {
            Some([audio_us.saturating_add(uncorrected_us), video_us])
        }
    }

    /// 同期が足した分を含まない、トラックの表示の遅れの下限 (マイクロ秒)
    ///
    /// 自分の jitter buffer の遅延と `target_latency` の大きい方である。観測が無ければ
    /// None。
    fn natural_delay_us_of(&self, track: Track) -> Option<i64> {
        let own_delay_us = self.tracks[track.index()].delay_us?;
        Some(own_delay_us.max(self.target_latency_ms.saturating_mul(1_000).max(0)))
    }

    /// 上限に収まらず切り下げた分を更新する
    fn update_limited_us(&mut self) {
        if self.target_latency_ms <= 0 {
            // target_latency を使っていないときは切り下げも起きない
            self.limited_us = 0;
            return;
        }
        self.limited_us = self
            .target_latency_ms
            .saturating_mul(1_000)
            .saturating_sub(self.presentation_cap_us())
            .max(0);
    }

    /// 2 つのトラックの基準の差の履歴を消す
    fn clear_base_differences(&mut self) {
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

    /// キューが吸収できる長さで切った表示の遅れの上限 (マイクロ秒)
    fn delay_cap_us(&self, frame_interval_us: Option<i64>) -> i64 {
        presentation_cap_us(&self.config, frame_interval_us)
    }
}

impl Default for PlayoutTimeline {
    fn default() -> Self {
        Self::new()
    }
}
