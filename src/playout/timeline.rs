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
//! - 表示の遅れ (音声): [`crate::playout::delay`] の目標遅延 (揺らぎの学習) と、実際に
//!   鳴った結果から決める閉ループの目標 ([`crate::playout::feedback`]) の大きい方。
//!   明示された `targetLatency` は閉ループの目標の上限にだけ掛け、学習値には掛けない。
//! - 表示の遅れ (映像): (遅れ − 基準の遅れ) の揺らぎの百分位。表示時刻の後に届く
//!   フレームが 1 秒に [`TIMELINE_LATE_FRAMES_PER_SECOND`] 枚までになる値である
//! - A/V 同期: 2 つのトラックの表示時刻の差が [`TIMELINE_SYNC_MIN_DELTA_US`] を超えた
//!   ときだけ、先行する側へ足して合わせる (観測のたび)。合わせる量は「基準の遅れ +
//!   表示の遅れ」の差そのものであり、経路の相対遅延 (直近の観測) ではない。ただし基準の
//!   差を補償する量は [`TIMELINE_MAX_COMPENSATED_DIFFERENCE_US`] までにする。既知の
//!   バッファの差と出力の残差は別に補償する。時計のずれの証拠を見たかどうかに関わらず
//!   常に掛ける。共有する表示時刻の下限は即座に上げ、下げるのは毎秒
//!   [`TIMELINE_DELAY_DECAY_US_PER_SECOND`] までにする
//! - 基準を共有しない判定: 2 つのトラックの基準の差が、表示の遅れの上限から求まる
//!   閾値を超えたときと、差が [`TIMELINE_BASE_DRIFT_US`] を超えて動き続けている
//!   (TIMESTAMP が壁時計からずれている) ときである。共有しない間は、その時点までに
//!   足した分を戻す
//! - 基準を共有できない側が音声のときは、同期をやめずに映像だけを音声の到着基準の遅れ
//!   ([`audio_arrival_delay_us`]) へ合わせる。合わせないと相対関係を見る相手がいなくなり、
//!   音声と映像が別々の基準で並ぶ
//! - いったん共有をやめたら [`TIMELINE_BASE_UNSHARED_HOLD_US`] の間は戻さない。閾値は
//!   表示の遅れで動くため、差が変わらなくても共有と解除を往復し得る。解除の原因が去って
//!   差が戻った状態が続き、かつ直近に解除と解除を往復していないときだけ、保持を待たずに
//!   戻す
//! - 遅延の内訳 (基準の遅れ・揺らぎ・足した分・共有できているかとその理由・差の動き) は
//!   [`PlayoutTimeline::delay_breakdown`] が返す

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::playout::delay::AudioDelayManager;
use crate::playout::feedback::{AudioDelayFeedback, AudioDelayFeedbackSnapshot};
use crate::playout::timing::AudioDelayFeedbackObservation;

/// 基準の遅れと揺らぎを求める直近の窓 (マイクロ秒)
pub const TIMELINE_WINDOW_US: i64 = 10_000_000;

/// TIMESTAMP と壁時計の関係が基準からこれ以上離れたら基準を取り直す (マイクロ秒)
///
/// 配信元の切り替えや、配信が長く止まったあとの再開で基準が古くなるのを避ける。
pub const TIMELINE_DISCONTINUITY_US: i64 = 2_000_000;

/// 表示時刻の後に届くことを許すフレームの数 (1 秒あたり)
///
/// 表示時刻の後に届いたフレームはその表示周期に描けず、止まりになる。許す数を
/// 1 秒あたりで決めることで、配信 fps が高いほど多くの遅れを許さないようにする。
pub const TIMELINE_LATE_FRAMES_PER_SECOND: i64 = 1;

/// 映像の表示の遅れの目標にする揺らぎの百分位の下限 (1000 分率)
///
/// 配信 fps が低いと 1 秒に 1 枚は 5% を超えるため、95% のフレームは表示時刻までに
/// 届く長さを保つ。経路のまれな大きな遅れまで吸収しようとすると、常に大きく遅れて
/// 表示することになるため、百分位は 100% にしない。
pub const TIMELINE_MIN_PERCENTILE_MILLI: i64 = 950;

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

/// 目標が下がったときに表示の遅れを下げる速さ (マイクロ秒/秒)
pub const TIMELINE_DELAY_DECAY_US_PER_SECOND: i64 = 20_000;

/// A/V 同期の制御で許す、表示時刻の差の下限 (マイクロ秒)
///
/// この不感帯の中では遅延を変えないため、先行する側は最大この値だけ先行できる。libwebrtc の
/// `video/stream_synchronization.cc` の `kMinDeltaMs` は 30 ms であるが、実リレーで測り直して
/// 5 ms にした (2026-10-15)。
///
/// 実測 (実リレーの同じ条件で、不感帯だけを 30 ms と 5 ms にして交互に 2 回、250 ms ごとに
/// 60 秒観測。ずれは負が「音声が映像より遅れる」側):
///
/// | 不感帯 | ずれの p50 | p95 | 絶対値の最大 | 映像の表示待ち p50 | 映像の取りこぼし |
/// | --- | --- | --- | --- | --- | --- |
/// | 30 ms | −34 / −35 ms | −15 / −16 ms | 49 / 48 ms | 95 / 96 ms | 18 / 19 |
/// | 5 ms | −16 / −17 ms | +2 / −13 ms | 24 / 19 ms | 112 / 112 ms | 7 / 19 |
///
/// 30 ms のままだと、実測のずれ (−34 ms) は ITU-R BT.1359-1 の許容 (音声の遅れ 30 ms、
/// 進み 22.5 ms) を外れる。5 ms にすると許容の内側に入る (−16 ms)。代わりに映像は 16 ms
/// 余計に待つ (95 → 112 ms) が、表示の遅れの上限 ([`TIMELINE_MAX_PRESENTATION_DELAY_MS`] の
/// 500 ms) には遠い。映像の取りこぼしは増えていない。
///
/// 30 ms は libwebrtc が「表示の遅れの上限 (500 ms) に対する不感帯」として決めた値であり、
/// こちらの上限も同じであるため、そのまま採っていた。実測では、ずれの推定の揺れは 250 ms
/// ごとの観測で 10 ms 程度であり、5 ms でも制御は暴れない (不感帯 15 ms も測ったが、30 ms と
/// 同じか悪かった)。
pub const TIMELINE_SYNC_MIN_DELTA_US: i64 = 5_000;

/// 実際に先行する側へ足してよい補償量 (マイクロ秒)
///
/// 基準の差には [`TIMELINE_MAX_COMPENSATED_DIFFERENCE_US`] の保護を掛け、同じ方向の既知の
/// バッファ差は別に補償する。バッファが基準の差を相殺している場合、その分をさらに予算へ
/// 足すことはしない。先行側は必ず両方を合成した実際の表示時刻から決める。
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
/// 到着基準で並べるときも、共有の時間軸で並べるときも、これより短くは並べない。閉ループの
/// 下限 ([`crate::playout::feedback::AUDIO_DELAY_FEEDBACK_MIN_US`]) と同じ値にする。基準を
/// 共有できないときは、映像をこの「到着 + 下限」へ合わせる ([`audio_arrival_delay_us`]) ため、
/// 値がずれると A/V の合わせ先がずれる。
///
/// 2026-10-15 の実リレーの実測 (60 秒の配信と購読、下限だけを変えて交互に 3 回) で決めた。
/// 適用中の目標は、揺らぎの学習が下限を超えていなければ下限そのものになる。
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
/// 0.1 % 程度) に保ったまま 80 ms より 30 ms 低い。30 ms は圧縮が 1 桁増え、基準の取り直しも
/// 起きるため採らない。
pub const TIMELINE_AUDIO_DELAY_FLOOR_US: i64 = 50_000;

/// 音声を到着基準で並べるときの再生の遅れの上限 (マイクロ秒)
///
/// 学習した遅れは経路の揺らぎを吸収するために必要である。ただし上限を超える分は使わない。
/// TIMESTAMP が壁時計からずれているトラックでは、ずれそのものを揺らぎとして学習して
/// しまうためである。呼び出し側 (音声を鳴らす時刻を決めるとき) と時間軸 (映像を
/// 合わせるとき) の両方が [`audio_arrival_delay_us`] を使う。値がずれると A/V の
/// 合わせ先がずれる。
pub const TIMELINE_ARRIVAL_DELAY_US: i64 = 100_000;

/// 音声だけを購読するときの、到着基準で並べる再生の遅れの下限 (マイクロ秒)
///
/// 音声だけの購読は映像に合わせる相手が無く、到着基準で並べる。このときの再生の遅れは
/// 閉ループの目標に追随するが、目標の適用上限は 100 ms であり、圧縮か無音の穴が 2 秒
/// 続いたときだけ 160 ms へ広がる
/// ([`crate::playout::feedback::AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US`])。Wi-Fi の
/// 周期的な停滞 (実測: 約 0.5 秒ごとに 100 ms 前後) は孤立した穴として現れるため上限が
/// 広がらず、遅れが 100 ms に張り付いて停滞を吸収しきれない (実測: 60 秒に 1 回 109 ms の
/// 無音の穴、うち復旧できない分 32 ms)。
///
/// 揃える相手がいない音声だけの購読では、経路の停滞を先に吸収するためにこの下限まで
/// 遅らせる。値は moqt-js の実リレー Wi-Fi 計測の A/B (0 / 120 / 140 ms を各 6 回、60 秒)
/// で決めた。0 ms は 6 回すべてで無音の穴 (最大 116 ms、復旧できない分 28〜43 ms) と境界の
/// 段差が出た。120 ms は穴が 1〜12 ms まで縮んだが 6 回すべてで境界の段差が 1 回出た。
/// 140 ms は 4 回で穴も段差も 0、残る 2 回は経路が 140 ms を超えて跳ねた回で 33 ms の穴
/// (復旧できない分 16〜17 ms) が 1 回だけであった。再生開始は 186〜192 ms から 226 ms へ
/// +40 ms 遅くなるが、音声だけの購読は映像に合わせる相手がいないため許容する。160 ms まで
/// 上げても 140 ms を超える跳ねは残るため、遅延の代償が増えるだけで止まらない。
///
/// 映像を購読しているときは、この下限を渡さない (A/V のずれになる)。
pub const TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_FLOOR_US: i64 = 140_000;

/// 音声だけを購読するときの、学習した再生の遅れを使える上限 (マイクロ秒)
///
/// 映像に合わせる相手がいないため、閉ループが穴を観測して広げた目標 (最大
/// [`crate::playout::feedback::AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US`] = 160 ms) まで
/// 到着基準の遅れに使う。共有の基準があるトラックの上限 ([`TIMELINE_ARRIVAL_DELAY_US`] =
/// 100 ms) はそのままにする。TIMESTAMP の時計のずれを揺らぎとして学習するトラックでは、
/// 100 ms を超える分を使うと遅延だけが増えるためである。
///
/// 値は [`crate::playout::feedback::AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US`] と一致させる
/// (テストで一致を固定する)。循環参照を避けるため、定数そのものは参照せず数値で持つ。
pub const TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_MAX_US: i64 = 160_000;

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

/// 基準の差がこれ以下の変化なら「動いていない」とみなす幅 (マイクロ秒)
pub const TIMELINE_BASE_DIFFERENCE_QUIET_US: i64 = 5_000;

/// 動きが止まってから、落ち着いた水準を観測したとみなすまでの時間 (マイクロ秒)
///
/// 水準は動きが戻ったときにだけ取り直す。記録のたびに取り直すと、ゆっくりしたドリフトでも
/// 水準が一緒に動いてしまい、離れた幅が [`TIMELINE_BASE_DRIFT_US`] を超えない。
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
/// 離れた幅が元へ戻れば、その動きは一過性 (読み出しが一瞬遅れた、経路が一瞬つまずいた) で
/// あったと分かる。持続を確かめてから解除する。
pub const TIMELINE_BASE_DRIFT_CONFIRM_US: i64 = 6_000_000;

/// 待たずに時計のずれとみなす、水準からの離れ幅 (マイクロ秒)
///
/// この大きさでは、時計のずれと読み出しの遅れを見分けられない。ただし参照 (表示時刻が
/// 使う基準の差) が同じ向きに [`TIMELINE_BASE_DRIFT_US`] を超えて動いていることを確かめる。
/// 確かめないと、経路と復号の乱れを時計のずれとみなして解除し、しかも水準が乱れの側で
/// 取られると解除が解けなくなる。
pub const TIMELINE_BASE_DRIFT_STEP_US: i64 = 200_000;

/// 解除のきっかけが去ってから、保持を待たずに戻すまでの時間 (マイクロ秒)
pub const TIMELINE_BASE_UNSHARED_RELEASE_US: i64 = 2_000_000;

/// 基準の差を記録する間隔 (マイクロ秒)
const TIMELINE_BASE_DIFFERENCE_SAMPLE_INTERVAL_US: i64 = 250_000;

/// 基準の差の動きを見るときに使う、直近の基準を求める窓 (マイクロ秒)
///
/// 窓全体 ([`TIMELINE_WINDOW_US`]) の最小値は、TIMESTAMP が壁時計から遅れていくときも
/// 窓が埋まるまで動かない。短い窓で取り直すことで、合わせる側の遅れが上限へ伸びる前に
/// 動きを見つける。短くするほど経路の揺らぎの影響を受けやすいため、映像の到着が
/// まとまっていても最小値が動かない長さにする。
const TIMELINE_BASE_DIFFERENCE_RECENT_WINDOW_US: i64 = 2_000_000;

/// 到着基準で並べるときの再生の遅れを求める (マイクロ秒)
///
/// 学習した遅れを下限 [`TIMELINE_AUDIO_DELAY_FLOOR_US`] と上限
/// [`TIMELINE_ARRIVAL_DELAY_US`] の間に切る。呼び出し側 (音声を鳴らす時刻を決めるとき) と
/// 時間軸 (基準を共有できないときに映像を合わせるとき) の両方がこれを使う。値がずれると
/// A/V の合わせ先がずれる。
pub fn audio_arrival_delay_us(learned_delay_us: i64) -> i64 {
    learned_delay_us.clamp(TIMELINE_AUDIO_DELAY_FLOOR_US, TIMELINE_ARRIVAL_DELAY_US)
}

/// 音声だけを購読するときの到着基準の再生の遅れを求める (マイクロ秒)
///
/// 下限 (`floor_us`) を必ず確保し、その上で学習した値 (`learned_delay_us`) を上限
/// [`TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_MAX_US`] まで使う。穴を観測して閉ループが目標を
/// 広げたとき、その分だけ深く構える。`floor_us` が上限より大きい場合は下限を優先する。
///
/// この関数は音声だけの購読でだけ使う。映像を購読しているときは
/// [`audio_arrival_delay_us`] を使う (A/V のずれになるため)。
pub fn audio_only_arrival_delay_us(learned_delay_us: i64, floor_us: i64) -> i64 {
    learned_delay_us
        .min(TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_MAX_US)
        .max(floor_us)
}

/// 窓に保持する観測の上限
///
/// 観測の頻度は呼び出し側の供給量で決まる。同じ時刻の観測が繰り返し届いても
/// メモリが増え続けないよう、件数でも上限を置く。
const TIMELINE_WINDOW_MAX_ENTRIES: usize = 4096;

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
}

/// 2 つのトラックで基準を共有できているか、できていない理由
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsharedReason {
    /// 共有している (どちらも TIMESTAMP を使えている)
    None,
    /// 基準がまだ足りない (どちらかを観測していない)
    Unobserved,
    /// 基準の差が表示の遅れの上限を超えている (上限では合わせられない)
    Difference,
    /// 基準の差が動き続けている (TIMESTAMP が壁時計からずれている)
    Drift,
    /// 直前にやめた判定を保持している (閾値が動いても往復させない)
    Hold,
}

/// トラックごとの表示時刻の内訳 (遅延の解析に使う)
///
/// 表示時刻 = TIMESTAMP + 基準の遅れ + 表示の遅れ であり、表示の遅れは jitter buffer の
/// 遅延と `targetLatency` の大きい方に、同期の制御が足した分を加えて上限で切った値である。
/// どこで遅れが生じているかを分けるために、この 3 つを別々に出す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackBreakdown {
    /// 基準の遅れ (マイクロ秒)。送受信の時計のずれと、経路と復号の最小遅延。未観測なら None
    pub base_delay_us: Option<i64>,
    /// jitter buffer の遅延 (マイクロ秒)。揺らぎから求めた値と、実際に鳴った結果から閉ループが
    /// 決めた目標の大きい方を、適用の上限
    /// ([`applied_ceiling_us`](crate::playout::feedback::AudioDelayFeedback::applied_ceiling_us))
    /// で切った値。観測が届く前は揺らぎから求めた値だけを同じ上限で切る。未観測なら None
    pub jitter_delay_us: Option<i64>,
    /// 同期の制御が足した分 (マイクロ秒)。0 以上
    pub sync_extra_delay_us: i64,
    /// TIMESTAMP から表示時刻までの差 (マイクロ秒)。上限で切った後であり、そのトラックの
    /// TIMESTAMP を使わないとき (未観測、または基準がずれている) は None
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
    ///
    /// 予約した時刻 (予定) と実際に鳴り始めた時刻の差を、250 ms の時定数で平滑化した値。
    /// 音声の予測 (予定どうし) だけを揃えると、実際に鳴る音から映像が先行したままになるため、
    /// 映像の合わせ先にはこの値を足す。表示時刻そのもの (音声の再生予定) には含めない。
    pub audio_playout_correction_us: i64,
    /// 基準の差「音声 − 映像」(マイクロ秒)。どちらかを観測していなければ None
    pub base_difference_us: Option<i64>,
    /// 2 つのトラックで基準を共有しているか
    pub sharing_bases: bool,
    /// 共有できていない理由
    pub unshared_reason: UnsharedReason,
    /// 直近の基準の差の動き (マイクロ秒/秒)。まだ履歴が無ければ None
    pub base_drift_us_per_second: Option<i64>,
    /// 時計のずれとみなす、基準の差の水準からの隔たりの幅 (マイクロ秒)
    ///
    /// [`DelayBreakdown::base_drift_us_per_second`] と並ぶが**次元が違う** (あちらは動きの
    /// 速さ、こちらは隔たりの幅)。判定 (`base_difference_drifted`) が使うのは隔たりの幅で
    /// あり、動きの速さは計器として出すだけで判定には使わない。隔たりがこの幅を超えたまま
    /// [`TIMELINE_BASE_DRIFT_CONFIRM_US`] 続いたとき (または 1 回の記録で段差
    /// ([`TIMELINE_BASE_DRIFT_STEP_US`]) として現れたとき) に時計のずれとみなす
    pub base_drift_limit_us: i64,
    /// jitter buffer の遅延を切り下げる上限 (マイクロ秒)
    pub presentation_delay_cap_us: i64,
    /// 音声の目標遅延を決める閉ループの状態
    ///
    /// 閉ループが決めた目標は音声の表示の遅れの一部であり、その目標がどう動いたかを
    /// 読むために返す。実際に使う値 (揺らぎの学習値との大きい方) は
    /// [`PlayoutTimeline::presentation_delay_us`] と [`PlayoutTimeline::learned_delay_us`] が
    /// 返す。
    pub audio_delay_feedback: AudioDelayFeedbackSnapshot,
}

/// 時間軸の設定
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineConfig {
    /// 映像の表示待ちのキューの上限 (枚)
    pub video_queue_limit: usize,
    /// 表示の遅れの上限 (ミリ秒)
    pub max_presentation_delay_ms: i64,
    /// MSF の `targetLatency` (ミリ秒、draft-ietf-moq-msf-01 §5.2.8 (Target latency))。
    /// 0 のときは下限にしない
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
/// 映像の表示の遅れと音声の再生の観測 (`crate::playout::timing`) が同じ求め方を使う。
pub(crate) fn percentile_index(count: usize, numerator_milli: i64) -> usize {
    if count == 0 {
        return 0;
    }
    let scaled = numerator_milli.saturating_mul(count as i64);
    let ceil = (scaled + 999) / 1_000;
    (ceil.max(1) as usize - 1).min(count - 1)
}

/// 映像の表示の遅れの目標にする百分位 (1000 分率)
///
/// `max(0.95, 1 - フレーム間隔 (ミリ秒) / 1000)` を整数で求める。フレーム間隔が
/// まだ分からないときは下限 (0.95) を使う。
fn video_percentile_milli(frame_interval_us: Option<i64>) -> i64 {
    let numerator_milli = match frame_interval_us {
        Some(frame_interval_us) => {
            1_000 - (frame_interval_us / 1_000) * TIMELINE_LATE_FRAMES_PER_SECOND
        }
        None => 0,
    };
    numerator_milli.clamp(TIMELINE_MIN_PERCENTILE_MILLI, 1_000)
}

/// 表示の遅れの上限 (マイクロ秒)
///
/// 「表示の遅れの上限 (設定値)」と「表示待ちのキューが吸収できる長さ
/// ((キューの上限 − 余裕) 枚分のフレーム間隔)」の小さい方である。フレーム間隔が
/// まだ分からないときは設定値だけを使い、計算結果が負のときは 0 とする。
fn presentation_cap_us(config: &TimelineConfig, frame_interval_us: Option<i64>) -> i64 {
    let max_us = config.max_presentation_delay_ms.saturating_mul(1_000);
    let queue_us = match frame_interval_us {
        Some(frame_interval_us) => (config
            .video_queue_limit
            .saturating_sub(TIMELINE_QUEUE_HEADROOM_FRAMES)
            as i64)
            .max(0)
            .saturating_mul(frame_interval_us),
        None => max_us,
    };
    max_us.min(queue_us)
}

/// live に追いつくまでの間かを決める
///
/// [`TIMELINE_CATCH_UP_CHECK_INTERVAL_US`] ごとに基準の遅れの下がり幅を見て、
/// [`TIMELINE_CATCH_UP_MIN_BASE_DROP_US`] より小さければ追いついたとみなす。
/// 一度追いついたら、基準を取り直すまで追いつき中に戻らない。
fn is_catching_up(state: &mut TrackState, now_us: i64, base_us: i64) -> bool {
    if !state.catching_up {
        return false;
    }
    let Some((checkpoint_at_us, checkpoint_base_us)) = state.catch_up_checkpoint else {
        state.catch_up_checkpoint = Some((now_us, base_us));
        return true;
    };
    if now_us.saturating_sub(checkpoint_at_us) < TIMELINE_CATCH_UP_CHECK_INTERVAL_US {
        return true;
    }
    if checkpoint_base_us.saturating_sub(base_us) >= TIMELINE_CATCH_UP_MIN_BASE_DROP_US {
        state.catch_up_checkpoint = Some((now_us, base_us));
        return true;
    }
    state.catching_up = false;
    state.catch_up_checkpoint = None;
    false
}

/// 観測の時刻付きの値の窓 (古い順)
#[derive(Debug, Default)]
struct TimedWindow {
    entries: VecDeque<(i64, i64)>,
}

impl TimedWindow {
    /// 観測を 1 つ足す
    fn push(&mut self, at_us: i64, value_us: i64) {
        self.entries.push_back((at_us, value_us));
        while self.entries.len() > TIMELINE_WINDOW_MAX_ENTRIES {
            self.entries.pop_front();
        }
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

    /// 一番小さい値
    fn min_value(&self) -> Option<i64> {
        self.entries.iter().map(|(_, value_us)| *value_us).min()
    }

    /// 窓の中の最も古い記録 (時刻, 値)。無ければ None
    ///
    /// 値が窓の中でどれだけ動いたかを見るために使う。
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

    /// 値のイテレータ
    fn values(&self) -> impl Iterator<Item = i64> + '_ {
        self.entries.iter().map(|(_, value_us)| *value_us)
    }
}

/// トラック 1 つぶんの状態
#[derive(Debug)]
struct TrackState {
    /// 到着の揺らぎから目標遅延を求める (音声だけが使う)
    delay: AudioDelayManager,
    /// (復号の出力の時刻 − TIMESTAMP) の窓 (直近 10 秒)。基準を求める
    offsets: TimedWindow,
    /// 揺らぎの学習に使う (復号の出力の時刻 − TIMESTAMP) の窓 (映像だけが使う)
    learning_offsets: TimedWindow,
    /// 基準の遅れ (マイクロ秒)
    base_us: Option<i64>,
    /// 自分の揺らぎから求めた表示の遅れ (マイクロ秒)
    ///
    /// 音声は揺らぎの学習と閉ループの目標 ([`crate::playout::feedback`]) の大きい方、
    /// 映像は揺らぎの百分位である。
    own_delay_us: i64,
    /// A/V 同期の制御が足した遅れ (マイクロ秒)。自分の遅れの上に乗る
    sync_extra_us: i64,
    /// 直近の観測の復号の出力の時刻 (マイクロ秒)
    last_arrival_us: Option<i64>,
    /// 直近の観測の TIMESTAMP (マイクロ秒)
    last_timestamp_us: Option<i64>,
    /// 表示の遅れを最後に更新した時刻 (減衰の経過時間を求める)
    last_delay_update_us: i64,
    /// 直近のフレーム間隔 (TIMESTAMP の差、マイクロ秒) の窓
    frame_intervals_us: VecDeque<i64>,
    /// live に追いつくまでの間か
    catching_up: bool,
    /// 追いつき中の確認点 (時刻, 基準)
    catch_up_checkpoint: Option<(i64, i64)>,
}

impl TrackState {
    /// 観測の無い状態で作る
    fn new() -> Self {
        Self {
            delay: AudioDelayManager::new(),
            offsets: TimedWindow::default(),
            learning_offsets: TimedWindow::default(),
            base_us: None,
            own_delay_us: 0,
            sync_extra_us: 0,
            last_arrival_us: None,
            last_timestamp_us: None,
            last_delay_update_us: 0,
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

/// 音声と映像に共通の再生時刻を決める
///
/// 観測のたびに [`PlayoutTimeline::observe`] を呼び、鳴らす・表示する時刻を
/// [`PlayoutTimeline::present_us`] で求める。A/V 同期の制御も観測のたびに動く。
#[derive(Debug)]
pub struct PlayoutTimeline {
    /// 設定
    config: TimelineConfig,
    /// 音声と映像の状態
    tracks: [TrackState; 2],
    /// 実際に鳴った結果から決める音声の目標遅延 (閉ループ)
    ///
    /// 音声トラックごとではなく時間軸に 1 つ持つ。目標は到着から鳴るまでの経路の学習で
    /// あり、購読のやり直しでは変わらないためである。
    audio_delay_feedback: AudioDelayFeedback,
    /// 直近に同期の制御を行った時刻 (足した遅延を戻す速さの経過時間を求める)
    last_sync_us: Option<i64>,
    /// 直近に観測した、同期の制御が足していない方の遅れ
    ///
    /// 自分の遅れが下がった分だけ、足した遅延を戻す量を減らすために使う。
    last_own_floor_us: Option<[i64; 2]>,
    /// 基準を共有しないと決めた直近の時刻と、そのときの側。往復を防ぐ
    last_unshared_at_us: Option<i64>,
    /// 基準を共有しないと決めた直近の側
    last_unshared_track: Option<Track>,
    /// 2 つのトラックの基準の差の直近の履歴 (マイクロ秒)。差が動き続けていれば時計のずれ
    base_differences: TimedWindow,
    /// 直前に基準の差を記録した時刻 (マイクロ秒)。まだ記録していなければ None
    last_base_difference_at_us: Option<i64>,
    /// 直前に記録した基準の差 (マイクロ秒)。履歴と同じ求め方であり、動きの今側の値になる
    last_base_difference_value_us: Option<i64>,
    /// 直前に記録した、表示時刻が使う基準の差 (マイクロ秒)
    ///
    /// 「参照」であり、離れ始めの参照と比べて同じ向きに動いたかを見る。
    last_base_difference_reference_us: Option<i64>,
    /// 動きが止まっている状態が続いている開始時刻 (マイクロ秒)
    base_difference_quiet_since_us: Option<i64>,
    /// 基準の差を最初に記録した時刻 (マイクロ秒)
    base_difference_first_at_us: Option<i64>,
    /// 動きが止まっているとみなした水準 (マイクロ秒)。まだ観測していなければ None
    settled_base_difference_us: Option<i64>,
    /// 水準から離れている状態が続いている開始時刻 (マイクロ秒)
    base_difference_deviation_since_us: Option<i64>,
    /// 離れ始めたときの参照 (マイクロ秒)
    base_difference_deviation_reference_us: Option<i64>,
    /// いま基準の差を時計のずれとみなしているか
    base_difference_drifted_value: bool,
    /// その前の解除の時刻 (マイクロ秒)
    ///
    /// 直近に共有と解除を往復しているかを見るために持つ。保持が満了して戻した場合も含める
    /// (往復の検出に使う)。解除の原因が動きだったかは見ない (購読の直後の cache の再生では
    /// 差が静かに動いて解除されることがあり、原因が去って差が戻れば保持を待たずに戻せる)。
    previous_unshared_at_us: Option<i64>,
    /// きっかけが去って差が戻っている状態の開始時刻 (マイクロ秒)
    unshared_returned_since_us: Option<i64>,
    /// 同期が決めた、共有する表示時刻の下限 (受信側の壁時計、マイクロ秒)
    ///
    /// 同期が足す前の表示時刻 (基準の遅れ + 自分の遅れ + 出力の残差) の最大から決め、
    /// 必要値が増えたら即座に上げ、減ったら毎秒
    /// [`TIMELINE_DELAY_DECAY_US_PER_SECOND`] までで戻す。過去に各トラックへ足した補償を
    /// 必要値の算定へ戻さないため、揺らぎが上下しても待ちを積み上げない。
    sync_presentation_floor_us: Option<i64>,
    /// 前回の同期で使った基準の遅れ (音声, 映像、マイクロ秒)
    ///
    /// 共通の基準移動とバッファの減衰を分けるために残す。現在のバッファ量を旧基準へ当てた
    /// 値と比べることで、cache から追いついて基準が下がった分を下限へ移さない。
    sync_bases_us: Option<[i64; 2]>,
    /// 壁時計を持たない音声でも、再生結果だけから到着基準の遅延を制御できたか
    ///
    /// [`PlayoutTimeline::observe_audio_playout`] が 1 度でも呼ばれたら true。音声の
    /// 観測 ([`PlayoutTimeline::observe`]) がまだ無い間は、揺らぎの学習の開始値を閉ループの
    /// 入力へ掛け続けない ([`PlayoutTimeline::audio_jitter_target_us`])。
    audio_playout_observed: bool,
    /// 予約時の予定と実際の出力の差 (マイクロ秒)。0 以上
    ///
    /// 予定そのものを測り直すと、補償した分が再び残差へ入るため、平滑化した値だけを持つ。
    /// 映像の合わせ先にだけ足し、音声の表示時刻には含めない。
    audio_playout_correction_us: i64,
    /// 有効な出力時計で残差を測れた最後の時刻 (受信側の壁時計、マイクロ秒)
    ///
    /// 代用の時計での予約では延長しない。ここから
    /// [`TIMELINE_SKEW_WINDOW_US`] を超えたら残差を失効させる。
    audio_playout_correction_at_us: Option<i64>,
    /// 基準を取り直した回数
    generation: u64,
    /// 直近の表示の実績
    presented: [Option<PresentationRecord>; 2],
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
            audio_delay_feedback: AudioDelayFeedback::new(),
            last_sync_us: None,
            last_own_floor_us: None,
            last_unshared_at_us: None,
            last_unshared_track: None,
            base_differences: TimedWindow::default(),
            last_base_difference_at_us: None,
            last_base_difference_value_us: None,
            last_base_difference_reference_us: None,
            base_difference_quiet_since_us: None,
            base_difference_first_at_us: None,
            settled_base_difference_us: None,
            base_difference_deviation_since_us: None,
            base_difference_deviation_reference_us: None,
            base_difference_drifted_value: false,
            previous_unshared_at_us: None,
            unshared_returned_since_us: None,
            sync_presentation_floor_us: None,
            sync_bases_us: None,
            audio_playout_observed: false,
            audio_playout_correction_us: 0,
            audio_playout_correction_at_us: None,
            generation: 0,
            presented: [None, None],
        }
    }

    /// 使っている設定
    pub fn config(&self) -> TimelineConfig {
        self.config
    }

    /// 使う `targetLatency` (ミリ秒) を決める
    ///
    /// 同じ render group のトラックは同じ値でなければならないため、音声と映像で
    /// 1 つの値を使う。解決の規則は呼び出し側が持ち、ここへは確定した値だけを渡す。
    /// 値は A/V 同期の基準の遅延になり、2 つのトラックの表示の遅れの下限になる。
    /// 同時に、音声の目標遅延を閉ループで決めるときの上限にもなる (明示された値より
    /// 自動で大きくしない)。0 は「下限にしない」の意味であり、上限の解除として扱う。
    pub fn set_target_latency_ms(&mut self, target_latency_ms: i64) {
        self.config.target_latency_ms = target_latency_ms.max(0);
        // 明示された目標遅延は、閉ループが自動で超えない上限にする。0 は「下限にしない」
        // の意味であるため、上限を掛けない (None)
        self.audio_delay_feedback
            .set_ceiling_us(if target_latency_ms > 0 {
                Some(target_latency_ms.saturating_mul(1_000))
            } else {
                None
            });
        // 公開する適用値も同じ呼び出しで更新する。次の音声観測を待つと、停止中などでは
        // 閉ループの計器だけが新しい上限になり、実際に使う保持値には古い目標が残る
        if self.tracks[Track::Audio.index()].base_us.is_some() {
            let jitter_target_us = self.audio_jitter_target_us();
            self.tracks[Track::Audio.index()].own_delay_us =
                self.audio_delay_feedback.target_delay_us(jitter_target_us);
        }
        // 明示された値は基準を共有するかの閾値にも効くため、保持の起点を取り直す
        self.refresh_unshared_state();
        // 下限は 2 つのトラックの表示の遅れの下限になる。片方だけがこの下限に当たることが
        // あるため、差が開いていれば次の観測を待たずにその場で合わせ直す (戻す向きは毎秒の
        // 速さに限るので、ここでは足す向きだけを直す)
        if let Some(natural_us) = self.sync_natural_presentation_us() {
            self.align_sync_extras(natural_us);
        }
    }

    /// 使っている `targetLatency` (ミリ秒)
    pub fn target_latency_ms(&self) -> i64 {
        self.config.target_latency_ms
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
            self.reset();
        }
        let config = self.config;
        let state = &mut self.tracks[track.index()];

        // フレーム間隔を更新し、揺らぎの学習に使うかを決める
        let mut learns = false;
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
            // まとまって届いた観測 (前の観測から、現フレームの TIMESTAMP 間隔の半分未満) は
            // 学習に使わない。間隔の中央値ではなく現フレームの間隔で判定する。中央値を使うと、
            // 間隔の長いフレームの後に短い間隔で届いた観測を学習してしまい、1 フレーム遅れて
            // 届いただけの揺らぎを経路の揺らぎとして取り込む
            learns = wall_clock_us.saturating_sub(last_arrival_us) >= interval_us / 2;
        }
        state.last_arrival_us = Some(wall_clock_us);
        state.last_timestamp_us = Some(timestamp_us);

        // 基準の遅れ (直近 10 秒の最小値) を更新する
        state.offsets.push(wall_clock_us, offset_us);
        state
            .offsets
            .prune(wall_clock_us.saturating_sub(TIMELINE_WINDOW_US));
        if let Some(min_us) = state.offsets.min_value() {
            state.base_us = Some(min_us);
        }

        match track {
            Track::Audio => {
                // 音声の表示の遅れは目標遅延の学習 (`playout::delay`) から求める
                state.delay.observe(wall_clock_us, timestamp_us);
                // 実際に鳴った結果から決めた目標 (`playout::feedback`) との大きい方を使う。
                // 学習だけではストリーム全体が一様に遅れている分が見えない
                state.own_delay_us = self
                    .audio_delay_feedback
                    .target_delay_us(state.delay.target_delay_ms().saturating_mul(1_000));
            }
            Track::Video => {
                if let Some(base_us) = state.base_us {
                    // live に追いつくまでに届いたフレームの遅れは経路の揺らぎではない
                    if learns && !is_catching_up(state, wall_clock_us, base_us) {
                        state.learning_offsets.push(wall_clock_us, offset_us);
                    }
                    state
                        .learning_offsets
                        .prune(wall_clock_us.saturating_sub(TIMELINE_WINDOW_US));

                    let cap_us = presentation_cap_us(&config, state.frame_interval_us());
                    let target_us =
                        video_target_us(state, base_us, state.frame_interval_us()).min(cap_us);
                    if target_us >= state.own_delay_us {
                        // まだ学習していない、または目標が上がったときはすぐ反映する
                        state.own_delay_us = target_us;
                    } else {
                        // 目標が下がるときは少しずつ下げる。上限が下がったときは直ちに従う
                        let elapsed_us = wall_clock_us
                            .saturating_sub(state.last_delay_update_us)
                            .max(0);
                        let decay_us = TIMELINE_DELAY_DECAY_US_PER_SECOND
                            .saturating_mul(elapsed_us)
                            / 1_000_000;
                        let decayed_us = state.own_delay_us.saturating_sub(decay_us);
                        state.own_delay_us = target_us.max(decayed_us).min(cap_us);
                    }
                    state.last_delay_update_us = wall_clock_us;
                }
            }
        }

        // 基準の差の動きを見るために記録する (基準がそろってから意味を持つ)
        self.record_base_difference(wall_clock_us);

        // 同期の制御の前後で共有の可否が変わったかを見るために、制御の前の状態を残す
        let shared_before_sync = self.drifted_track().is_none();

        // ここまでの更新で表示の遅れが変わったため、A/V 同期の制御をそろえる
        self.update_sync_delays(wall_clock_us);

        // 解除のきっかけが去って差が戻ったら、保持を待たずに共有を戻す
        self.update_unshared_release(wall_clock_us);

        // 保持していた間は相手がいないものとして足した分を戻すだけであり、共有へ戻った
        // 最初の観測では足し直しが行われない。そのままにすると、表示時刻の差が目標より
        // 1 観測分だけ大きい状態が残るため、共有へ戻ったらもう一度そろえる
        if !shared_before_sync && self.drifted_track().is_none() {
            self.update_sync_delays(wall_clock_us);
        }
    }

    /// 音声を実際に鳴らした結果の観測を渡す (購読側が鳴らすたびに呼ぶ)
    ///
    /// 「予定をどれだけ過ぎて鳴ったか」「到着から鳴り始めるまで」「並べすぎで捨てた量」から、
    /// 音声の目標遅延を閉ループで決める ([`crate::playout::feedback`])。目標を動かすのは
    /// 毎秒 1 回までであり、観測のたびに呼んでよい。動かした目標はその場で音声の表示の
    /// 遅れへ反映する。
    ///
    /// 揺らぎの学習 ([`crate::playout::delay`]) は「直近で最も早く届いた音との差」しか
    /// 見ないため、ストリーム全体が一様に遅れている分を見つけられない。この観測がその分を
    /// 補う。遅れが許容の中に収まっていれば目標を減らすため、正常時は遅延が増えない。
    ///
    /// 揺らぎだけから求めた目標遅延は時間軸が持っているため、計器の観測へここで足す。
    /// 閉ループはこれを使って、圧縮が止まったときに適用する上限を 100 ms へ戻してよいかを
    /// 判断する (経路が 100 ms を超えているまま戻すと往復する)。
    pub fn observe_audio_playout(&mut self, mut observation: AudioDelayFeedbackObservation) {
        // 音声の観測 (observe) が無くても、再生の結果だけで閉ループを回す
        self.audio_playout_observed = true;
        // 揺らぎだけから求めた目標遅延を閉ループへ渡す。音声の観測が無い間は 0 を渡し、
        // 未学習の開始値を掛け続けない
        observation.jitter_target_us = self.audio_jitter_target_us();
        let at_us = observation.at_us;
        self.audio_delay_feedback.update(observation);
        if self.tracks[Track::Audio.index()].base_us.is_some() {
            // 次の音から新しい目標で並ぶように、その場で表示の遅れを取り直す
            let jitter_target_us = self.audio_jitter_target_us();
            self.tracks[Track::Audio.index()].own_delay_us =
                self.audio_delay_feedback.target_delay_us(jitter_target_us);
        }
        // 再生結果で目標を変えた時点で同期も更新する。次の復号出力まで待つと、穴からの
        // 復帰時にその間だけ音声と映像の時刻が離れる
        self.update_sync_delays(at_us);
    }

    /// この TIMESTAMP を鳴らす・表示する時刻 (受信側の壁時計のマイクロ秒)
    ///
    /// 基準がまだ無いときと、そのトラックの TIMESTAMP を使わないとき (基準がずれて
    /// いるとき) は None。None のときは、呼び出し側が届いた順に鳴らす経路へ委ねる。
    pub fn present_us(&self, track: Track, timestamp_us: i64) -> Option<i64> {
        if self.drifted_track() == Some(track) {
            return None;
        }
        let state = &self.tracks[track.index()];
        let base_us = state.base_us?;
        Some(
            timestamp_us
                .saturating_add(base_us)
                .saturating_add(self.delay_us(track)),
        )
    }

    /// トラックの表示に使う遅れ (マイクロ秒)
    ///
    /// 表示時刻から基準の遅れを除いた分であり、自分の揺らぎから求めた遅れと
    /// `targetLatency` の大きい方に、A/V 同期の制御の結果を反映し、表示の遅れの上限で
    /// 切った値である。基準がまだ無いときと、基準がずれているときは None。音声の値は
    /// スケジューラ ([`crate::playout::scheduler::AudioPlayoutInput::presentation_delay_us`])
    /// へそのまま渡せる (観測がまだ無いときは呼び出し側の既定値を使う)。
    pub fn presentation_delay_us(&self, track: Track) -> Option<i64> {
        if self.drifted_track() == Some(track) {
            return None;
        }
        self.tracks[track.index()].base_us?;
        Some(self.delay_us(track))
    }

    /// 自分の揺らぎから求めた表示の遅れ (マイクロ秒)
    ///
    /// 音声は揺らぎの学習と閉ループの目標 ([`crate::playout::feedback`]) の大きい方、
    /// 映像は揺らぎの百分位である。`targetLatency` と同期の制御の分を含まない。観測が
    /// 無いときは 0 になる。
    pub fn learned_delay_us(&self, track: Track) -> i64 {
        self.tracks[track.index()].own_delay_us
    }

    /// 閉ループへ渡す、揺らぎだけから求めた目標遅延 (マイクロ秒)
    ///
    /// 到着時刻と媒体時刻の差を測れない音声へ、未学習の推定開始値 (80 ms) を掛け続けない。
    /// 壁時計の観測が始まれば、到着の推定と閉ループの大きい方を再び使う。
    fn audio_jitter_target_us(&self) -> i64 {
        if self.audio_playout_observed && self.tracks[Track::Audio.index()].base_us.is_none() {
            0
        } else {
            self.tracks[Track::Audio.index()]
                .delay
                .target_delay_ms()
                .saturating_mul(1_000)
        }
    }

    /// 上限に収まらず切り下げた `targetLatency` の分 (マイクロ秒)
    pub fn limited_us(&self) -> i64 {
        self.config
            .target_latency_ms
            .saturating_mul(1_000)
            .saturating_sub(self.presentation_cap_us())
            .max(0)
    }

    /// 表示した実績を記録する
    ///
    /// 呼び出し側が表示時刻を決めて実行したあとに呼ぶ。実績は
    /// [`PlayoutTimeline::skew_us`] の推定に使う。音声の出力の残差を測るには
    /// [`PlayoutTimeline::record_presentation_with_target`] を使う。
    pub fn record_presentation(
        &mut self,
        track: Track,
        timestamp_us: i64,
        presented_wall_clock_us: i64,
    ) {
        self.record_presentation_with_target(track, timestamp_us, presented_wall_clock_us, None);
    }

    /// 表示した実績と、予約した時点の予定を記録する
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
            let correction_us = match elapsed_us {
                Some(elapsed_us) if elapsed_us <= TIMELINE_SKEW_WINDOW_US => {
                    // 重み elapsed / (時定数 + elapsed) の平滑化を整数で求める。差分へ掛けて
                    // 足す形にすると、差分が小さいときに切り捨てで 0 に収束せず残るため、
                    // 重み付き平均の式 (correction * 時定数 + residual * elapsed) / (時定数 + elapsed)
                    // で求める
                    let elapsed_us = elapsed_us.max(0);
                    self.audio_playout_correction_us
                        .saturating_mul(TIMELINE_AUDIO_PLAYOUT_CORRECTION_SMOOTHING_US)
                        .saturating_add(residual_us.saturating_mul(elapsed_us))
                        / TIMELINE_AUDIO_PLAYOUT_CORRECTION_SMOOTHING_US.saturating_add(elapsed_us)
                }
                // 最初の出力と、前回から 1 秒以上空いた (停止の後の) 出力は、その場で作り直す
                _ => residual_us,
            };
            self.audio_playout_correction_us = correction_us;
            self.audio_playout_correction_at_us = Some(presented_wall_clock_us);
        }
        self.presented[track.index()] = Some(PresentationRecord {
            timestamp_us,
            presented_wall_clock_us,
        });
    }

    /// 実績から求めた A/V のずれ (マイクロ秒)
    ///
    /// 直近に表示した音声と映像それぞれの「表示時刻 − TIMESTAMP」の差を取る。映像の
    /// 表示が音声より遅れていれば正。両方の実績が
    /// [`TIMELINE_SKEW_WINDOW_US`] 以内にあるときだけ値を返す。
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
    /// [`PlayoutTimeline::unshared_reason`] と [`PlayoutTimeline::delay_breakdown`] に出る。
    pub fn sharing_bases(&self) -> bool {
        self.unshared_reason() == UnsharedReason::None
    }

    /// 基準を共有できていない理由
    ///
    /// 未観測 (どちらかの基準がまだ無い) を先に見る。差と動きの判定は基準がそろってから
    /// 意味を持つ (差が 0 であるとも、動きが無いとも言えない)。動きは差より先に見る。
    /// 動き続けている差は経路の遅れではなく時計のずれであり、合わせることをやめる原因
    /// そのものであるため、差の大きさより先に知りたい。
    pub fn unshared_reason(&self) -> UnsharedReason {
        if self.tracks[Track::Audio.index()].base_us.is_none()
            || self.tracks[Track::Video.index()].base_us.is_none()
        {
            return UnsharedReason::Unobserved;
        }
        if self.base_difference_drifted() {
            return UnsharedReason::Drift;
        }
        let difference_us = self.current_base_difference_us().unwrap_or(0);
        if difference_us.saturating_abs() > self.base_difference_limit_us() {
            return UnsharedReason::Difference;
        }
        // 直前にやめた判定を保持している (閾値が動いても往復させない)
        if self.held_unshared_track().is_some() {
            return UnsharedReason::Hold;
        }
        UnsharedReason::None
    }

    /// 音声と映像の遅延の内訳 (遅延の解析に使う)
    ///
    /// 「表示の遅れがどこで生じているか」と「2 つのトラックを同じ時計として扱えているか」を
    /// 1 つの値にまとめる。表示時刻そのものは [`PlayoutTimeline::present_us`] が返す。
    pub fn delay_breakdown(&self) -> DelayBreakdown {
        DelayBreakdown {
            audio: self.track_breakdown(Track::Audio),
            video: self.track_breakdown(Track::Video),
            audio_playout_correction_us: self.audio_playout_correction_us,
            base_difference_us: self.current_base_difference_us(),
            sharing_bases: self.sharing_bases(),
            unshared_reason: self.unshared_reason(),
            base_drift_us_per_second: self.base_drift_us_per_second(),
            base_drift_limit_us: TIMELINE_BASE_DRIFT_US,
            presentation_delay_cap_us: self.presentation_cap_us(),
            audio_delay_feedback: self
                .audio_delay_feedback
                .snapshot(self.audio_jitter_target_us()),
        }
    }

    /// 音声の jitter buffer の目標遅延の閉ループの内訳 (表示用)
    ///
    /// 音声だけを購読するときは A/V 同期の推定を出さないため、目標遅延がどう決まったかを
    /// ここから読めるようにする。まだ音声を観測していなければ初期値の内訳を返す
    /// ([`crate::playout::feedback::AudioDelayFeedbackSnapshot`])。
    pub fn audio_delay_feedback_stats(&self) -> AudioDelayFeedbackSnapshot {
        self.audio_delay_feedback
            .snapshot(self.audio_jitter_target_us())
    }

    /// 音声の jitter buffer の遅延 (マイクロ秒、上限適用後)。まだ観測していなければ None
    ///
    /// 壁時計の TIMESTAMP を持たない音を到着基準で並べるときの再生の遅れに使う。
    /// `targetLatency` と同期の制御による下限は含まない
    /// ([`PlayoutTimeline::presentation_delay_us`] を使う)。音声の観測が無くても、再生の
    /// 観測だけから決めた閉ループの目標があればそれを返す。
    pub fn playout_delay_us(&self) -> Option<i64> {
        let own_delay_us = if self.tracks[Track::Audio.index()].base_us.is_some() {
            // 音声の観測がある。自分の遅れ (揺らぎの学習と閉ループの大きい方) を使う
            self.tracks[Track::Audio.index()].own_delay_us
        } else if self.audio_playout_observed {
            // 壁時計を持たない音声でも、再生結果だけから決めた目標を使う。音声の観測が
            // 無い間は jitter の入力が 0 なので、閉ループの目標 (下限 50 ms) がそのまま出る
            self.audio_delay_feedback
                .target_delay_us(self.audio_jitter_target_us())
        } else {
            return None;
        };
        Some(own_delay_us.min(self.presentation_cap_us()))
    }

    /// 音声を到着基準で並べるときの再生の遅れ (マイクロ秒)。まだ観測していなければ None
    ///
    /// 音声の TIMESTAMP が信用できないとき (基準を共有できないとき) に、映像を合わせる先の
    /// 時刻である。呼び出し側が音声を鳴らすときに使う値と同じ規則で求める
    /// ([`audio_arrival_delay_us`])。値がずれると A/V の合わせ先がずれる。
    pub fn audio_arrival_delay_us(&self) -> Option<i64> {
        Some(audio_arrival_delay_us(self.playout_delay_us()?))
    }

    /// 1 つのトラックの基準と学習と実績を消す (音声の再生を止めたときなど)
    ///
    /// 世代は進めない。世代を進めると、もう片方に既に積んだフレームの表示時刻が
    /// 決められなくなるためである。`targetLatency` は設定の値であり残す。
    ///
    /// 音声を取り直したときは、出力の残差と閉ループの目標も初期化する。過去の穴で広げた
    /// 目標や、停止前に測った残差を再開後へ持ち込むと、再開直後に余分な遅れが残るためで
    /// ある ([`crate::playout::feedback::AudioDelayFeedback::reset`])。
    pub fn reset_track(&mut self, track: Track) {
        self.tracks[track.index()] = TrackState::new();
        self.presented[track.index()] = None;
        if track == Track::Audio {
            // 音声の再生を取り直したときは、過去の穴と遅れを持ち込まないように初期化する
            self.audio_playout_correction_us = 0;
            self.audio_playout_correction_at_us = None;
            self.audio_delay_feedback.reset();
            self.audio_playout_observed = false;
        }
        // 同期の制御の状態も、そのトラックの観測が無い状態に戻す。もう片方に足した分は
        // 消したトラックとの差を合わせるためのものなので、足した分をすべて消す
        for state in &mut self.tracks {
            state.sync_extra_us = 0;
        }
        self.sync_presentation_floor_us = None;
        self.sync_bases_us = None;
        self.last_sync_us = None;
        self.last_own_floor_us = None;
        // 基準の差の履歴も消す (片方の基準が無い状態の差に意味は無い)
        self.clear_base_differences();
    }

    /// 基準と学習をすべて消す (TIMESTAMP の飛び、購読のやり直し)。世代を進める
    ///
    /// `targetLatency` は設定の値であり残す。音声の閉ループの目標と出力の残差も
    /// [`PlayoutTimeline::reset_track`] と同じく初期化する。
    pub fn reset(&mut self) {
        self.generation = self.generation.saturating_add(1);
        // JS の reset は 2 つのトラックへ resetStream を呼ぶ。音声側の閉ループと残差も
        // そこで初期化されるため、同じ挙動になるよう reset_track を再利用する
        for track in [Track::Audio, Track::Video] {
            self.reset_track(track);
        }
    }

    /// 基準を取り直した回数
    ///
    /// 取り直すと、それより前に積んだフレームの表示時刻は新しい基準では決められない
    /// (飛びの分だけ未来になる)。積む側が世代を見て、古いフレームを到着順に扱える
    /// ようにする。
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// トラックの表示の遅れの下限 (マイクロ秒)
    ///
    /// 自分の揺らぎから求めた遅れと `targetLatency` の大きい方である。表示の遅れの
    /// 上限は掛けない。
    fn natural_delay_us(&self, track: Track) -> i64 {
        self.tracks[track.index()]
            .own_delay_us
            .max(self.config.target_latency_ms.saturating_mul(1_000))
    }

    /// 表示に使う遅れ (マイクロ秒)
    ///
    /// 表示時刻から基準の遅れを除いた分であり、自分の揺らぎから求めた遅れと
    /// `targetLatency` の大きい方に、A/V 同期の制御が足した分を加えた値を上限で切った
    /// ものである。
    fn delay_us(&self, track: Track) -> i64 {
        let state = &self.tracks[track.index()];
        self.natural_delay_us(track)
            .saturating_add(state.sync_extra_us)
            .min(self.presentation_cap_us())
    }

    /// 表示の遅れの上限 (マイクロ秒)
    fn presentation_cap_us(&self) -> i64 {
        presentation_cap_us(
            &self.config,
            self.tracks[Track::Video.index()].frame_interval_us(),
        )
    }

    /// A/V 同期の制御に使う、トラックごとの「同期が足した分を除いた表示の遅れ」(マイクロ秒)
    ///
    /// 基準の遅れと、同期が足した分を除いた表示の遅れ (自分の jitter buffer の遅延と
    /// `targetLatency` の大きい方) の和である。音声には出力の残差を足す (映像の合わせ先に
    /// するため)。2 つのトラックのこの値の差が、同期で合わせる対象になる。どちらかが
    /// 未観測、または基準がずれているときは `None` を返す。
    ///
    /// 補償しない差は、先行側の計算上の下限へ移してから返す。実際の表示時刻から先行側を
    /// 決めないと、バッファが基準の差と逆向きに働くときに前後が逆転する。
    fn sync_natural_presentation_us(&self) -> Option<[i64; 2]> {
        let audio_base_us = self.tracks[Track::Audio.index()].base_us?;
        let video_base_us = self.tracks[Track::Video.index()].base_us?;
        if self.drifted_track().is_some() {
            // TIMESTAMP が壁時計からずれているトラックがある。同期の制御は行わない
            return None;
        }
        let cap_us = self.presentation_cap_us();
        let audio_own_us = self
            .natural_delay_us(Track::Audio)
            .min(cap_us)
            .saturating_add(self.audio_playout_correction_us);
        let video_own_us = self.natural_delay_us(Track::Video).min(cap_us);
        let audio_us = audio_base_us.saturating_add(audio_own_us);
        let video_us = video_base_us.saturating_add(video_own_us);
        let difference_us = audio_us.saturating_sub(video_us);
        let limit_us = sync_compensation_limit_us(
            audio_base_us.saturating_sub(video_base_us),
            audio_own_us.saturating_sub(video_own_us),
        );
        // 先行側は実際の表示時刻から決める。基準を先に切ると、バッファが逆方向へ働くときに
        // 前後が逆転する。補償しない差だけを先行側の計算上の下限へ移し、足す量を制限する
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

    /// 共有する表示時刻の下限から、各トラックへ足す遅延を求め直す
    ///
    /// 先行側は [`TIMELINE_SYNC_MIN_DELTA_US`] だけ手前へ寄せる。共有の下限を戻す速さは
    /// [`PlayoutTimeline::update_sync_delays`] が制限し、自分の遅延が増えた分は補償から
    /// 差し引く。表示待ちのキューの上限 ([`PlayoutTimeline::presentation_cap_us`]) を
    /// 超えない範囲で足す。
    ///
    /// `natural_us` は同期が足した分を除いた、トラックごとの表示の遅れ (マイクロ秒)。
    /// [`PlayoutTimeline::sync_natural_presentation_us`] の戻り値 (補償しない差を先行側へ
    /// 移した後の値) を渡す。
    fn align_sync_extras(&mut self, natural_us: [i64; 2]) {
        let floor_us = self
            .sync_presentation_floor_us
            .unwrap_or(i64::MIN)
            .max(natural_us[Track::Audio.index()])
            .max(natural_us[Track::Video.index()]);
        self.sync_presentation_floor_us = Some(floor_us);
        for track in [Track::Audio, Track::Video] {
            let index = track.index();
            let other = match track {
                Track::Audio => Track::Video,
                Track::Video => Track::Audio,
            };
            // 先行する側だけ不感帯を残す
            let margin_us = if natural_us[index] < natural_us[other.index()] {
                TIMELINE_SYNC_MIN_DELTA_US
            } else {
                0
            };
            // 自分の jitter が既存の補償を埋めたら、その分を必ず差し引く。表示時刻そのものを
            // 前へ動かさずに余分な補償を解消でき、共通の待ち時間も積み上がらない
            let room_us = self
                .presentation_cap_us()
                .saturating_sub(self.natural_delay_us(track))
                .max(0);
            let wanted_us = floor_us
                .saturating_sub(margin_us)
                .saturating_sub(natural_us[index])
                .max(0);
            self.tracks[index].sync_extra_us = room_us.min(wanted_us);
        }
    }

    /// A/V 同期の制御を行う (観測のたびに呼ぶ)
    ///
    /// 表示時刻は「基準の遅れ + 表示の遅れ」であり、2 つのトラックの差はこの和の差である。
    /// したがって合わせる量は、同期が足した分を含まない表示の遅れの差そのものであり、
    /// 経路の相対遅延 (直近の観測の差) ではない。直近の観測を使うと、観測のたびに動く
    /// 揺らぎがそのまま制御量に入り、表示時刻の差を合わせられない。
    ///
    /// - 音声の TIMESTAMP が信用できないときは、映像だけを音声の到着基準の遅れへ合わせる
    ///   ([`PlayoutTimeline::update_sync_delays_to_arrival_audio`])
    /// - 基準を共有できないときは、足した分を自分の基準だけの表示時刻へ戻す (合わせる
    ///   相手がいないため)
    /// - 同期が足す前の表示時刻から、共有する表示時刻の下限を 1 つだけ決める。必要値が
    ///   増えたら即座に上げ、減ったら毎秒 [`TIMELINE_DELAY_DECAY_US_PER_SECOND`] までで
    ///   戻す。過去の各トラックの補償を必要値へ戻さないため、揺らぎが上下しても待ちを
    ///   積み上げない
    /// - 各トラックの補償は共有の下限から毎回計算し直す。自分の jitter buffer が増えた分は
    ///   既存の補償から差し引く
    fn update_sync_delays(&mut self, now_us: i64) {
        // 差と動きから共有をやめた側を記録する (観測のたびに判定し直す)
        self.refresh_unshared_state();

        // 音声が止まった後に、最後の予約の残差だけを映像へ掛け続けない。有効な出力時計で
        // 残差を測れた最後の時刻から 1 秒 ([`TIMELINE_SKEW_WINDOW_US`]) を超えたら失効させる
        if let Some(correction_at_us) = self.audio_playout_correction_at_us
            && now_us.saturating_sub(correction_at_us) > TIMELINE_SKEW_WINDOW_US
        {
            self.audio_playout_correction_us = 0;
            self.audio_playout_correction_at_us = None;
        }

        if self.drifted_track() == Some(Track::Audio) {
            self.sync_presentation_floor_us = None;
            self.sync_bases_us = None;
            // 音声の TIMESTAMP が信用できず、到着基準で鳴っている。基準は共有できないが、
            // 音声の並べ方は分かっているため、映像だけをその時刻へ合わせる
            self.update_sync_delays_to_arrival_audio(now_us);
            return;
        }
        let Some(natural_us) = self.sync_natural_presentation_us() else {
            self.sync_presentation_floor_us = None;
            self.sync_bases_us = None;
            // 2 つのトラックの基準を共有できない (基準がずれている、またはまだ観測して
            // いない)。合わせる相手がいないため、足した分を自分の基準だけの表示時刻へ
            // 戻す。戻さないと、ずれたトラックへ合わせて足した分がそのまま残り、その分
            // だけ 2 つの表示時刻が離れたままになる
            self.decay_sync_extras(now_us, |_| 0);
            return;
        };

        // 自然な遅延から共通の表示時刻を決める。過去の各 extra を max の入力へ戻すと、
        // 一方の jitter や残差が上下するたびに、同じ待ち時間を重ねて足してしまう。
        // 共有へ戻った最初だけ既存の表示待ちを引き継ぎ、その後は必要値だけから更新する
        let previous_sync_us = self.last_sync_us;
        let elapsed_us = previous_sync_us.map_or(0, |previous_sync_us| {
            now_us.saturating_sub(previous_sync_us).max(0)
        });
        let budget_us = TIMELINE_DELAY_DECAY_US_PER_SECOND.saturating_mul(elapsed_us) / 1_000_000;
        let required_us = natural_us[Track::Audio.index()].max(natural_us[Track::Video.index()]);
        let previous_floor_us = self.sync_presentation_floor_us.unwrap_or_else(|| {
            natural_us[Track::Audio.index()]
                .saturating_add(self.tracks[Track::Audio.index()].sync_extra_us)
                .max(
                    natural_us[Track::Video.index()]
                        .saturating_add(self.tracks[Track::Video.index()].sync_extra_us),
                )
        });
        // 同じバッファ量に対する基準の移動だけを下限へ反映する。cache から追いついて
        // 基準が下がった分を extra へ移すと、解消済みの遅れを数十秒保持してしまう。
        // 片側ずつ到着するため、両側の差分の平均ではなく、現在のバッファ量を旧基準へ
        // 当てた最大値と比較する。バッファや出力残差の変化はこの移動量に含めない
        let cap_us = self.presentation_cap_us();
        let previous_required_us = match self.sync_bases_us {
            None => required_us,
            Some([audio_base_us, video_base_us]) => {
                let audio_natural_us = self
                    .natural_delay_us(Track::Audio)
                    .min(cap_us)
                    .saturating_add(self.audio_playout_correction_us);
                let video_natural_us = self.natural_delay_us(Track::Video).min(cap_us);
                audio_base_us
                    .saturating_add(audio_natural_us)
                    .max(video_base_us.saturating_add(video_natural_us))
            }
        };
        self.sync_presentation_floor_us = Some(
            required_us.max(
                previous_floor_us
                    .saturating_add(required_us)
                    .saturating_sub(previous_required_us)
                    .saturating_sub(budget_us),
            ),
        );
        self.sync_bases_us = Some([
            self.tracks[Track::Audio.index()].base_us.unwrap_or(0),
            self.tracks[Track::Video.index()].base_us.unwrap_or(0),
        ]);
        self.align_sync_extras(natural_us);
        self.last_sync_us = Some(now_us);
        self.last_own_floor_us = Some([
            self.natural_delay_us(Track::Audio),
            self.natural_delay_us(Track::Video),
        ]);
    }

    /// 音声が到着基準で鳴っているときの同期の制御 (映像だけを音声へ合わせる)
    ///
    /// 音声の TIMESTAMP が信用できないと 2 つのトラックの基準は共有できず、これまでは
    /// 同期の制御そのものを止めていた。止めると相対関係を見る相手がいなくなり、音声は
    /// 「到着 + 到着基準の再生の遅れ」、映像は自分の jitter buffer の遅延で別々に並ぶ。
    ///
    /// 音声の並べ方は分かっている ([`audio_arrival_delay_us`]) ため、映像の到着からの
    /// 遅れ ([`PlayoutTimeline::natural_delay_known_us`]) をその値へ合わせることはできる。
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
        let Some(audio_us) = self.audio_arrival_delay_us() else {
            // 音声の jitter buffer の遅延がまだ決まっていない
            self.decay_sync_extras(now_us, |_| 0);
            return;
        };
        let Some(video_us) = self.natural_delay_known_us(Track::Video) else {
            // 映像の jitter buffer の遅延がまだ決まっていない
            self.decay_sync_extras(now_us, |_| 0);
            return;
        };
        // 目指す差は不感帯にする。ただし合わせる量は上限までにする (足りない分は A/V の
        // ずれとして残す)。差が不感帯に収まる場合も、既存の規則と同じく足さない
        let desired_us = TIMELINE_SYNC_MIN_DELTA_US.max(
            audio_us
                .saturating_sub(video_us)
                .saturating_abs()
                .saturating_sub(TIMELINE_MAX_COMPENSATED_DIFFERENCE_US),
        );
        let target_extra_us = if video_us < audio_us {
            audio_us.saturating_sub(desired_us).saturating_sub(video_us)
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
        let video_with_extra_us =
            video_us.saturating_add(self.tracks[Track::Video.index()].sync_extra_us);
        if video_with_extra_us < audio_us.saturating_sub(desired_us) {
            self.tracks[Track::Video.index()].sync_extra_us =
                audio_us.saturating_sub(desired_us).saturating_sub(video_us);
        }
    }

    /// 足した分を目標へ戻す (毎秒 [`TIMELINE_DELAY_DECAY_US_PER_SECOND`] まで)
    ///
    /// 観測の間隔で按分するため、観測が疎でも速さは変わらない。自分の遅延の下限が同時に
    /// 下がっているときは、その分だけ戻す量を減らす (下限が下がるだけでも表示時刻は前に
    /// 動くため、戻しすぎると不感帯を通り越す)。
    fn decay_sync_extras(&mut self, now_us: i64, target_extra_us: impl Fn(Track) -> i64) {
        let previous_sync_us = self.last_sync_us;
        self.last_sync_us = Some(now_us);
        let elapsed_us = previous_sync_us.map_or(0, |previous_sync_us| {
            now_us.saturating_sub(previous_sync_us).max(0)
        });
        let budget_us = TIMELINE_DELAY_DECAY_US_PER_SECOND.saturating_mul(elapsed_us) / 1_000_000;
        for track in [Track::Audio, Track::Video] {
            let index = track.index();
            // 自分の遅れが下がった分は、戻す量から差し引く (基準の遅れは含めない)
            let own_decrease_us = self.last_own_floor_us.map_or(0, |floor_us| {
                floor_us[index]
                    .saturating_sub(self.natural_delay_us(track))
                    .max(0)
            });
            let allowed_us = budget_us.saturating_sub(own_decrease_us).max(0);
            let excess_us = self.tracks[index]
                .sync_extra_us
                .saturating_sub(target_extra_us(track))
                .max(0);
            self.tracks[index].sync_extra_us = self.tracks[index]
                .sync_extra_us
                .saturating_sub(allowed_us.min(excess_us));
        }
        // 次の制御で「自分の下限が下がった分」を求めるために、今の下限を残す
        self.last_own_floor_us = Some([
            self.natural_delay_us(Track::Audio),
            self.natural_delay_us(Track::Video),
        ]);
    }

    /// 基準を共有できないトラック。無ければ None (共有している)
    ///
    /// 次のどちらかで共有できないと判定する。
    ///
    /// - 2 つのトラックの基準の差が [`PlayoutTimeline::base_difference_limit_us`] を
    ///   超えている。上限で切られる分は合わせられないため、同期の制御では足りない
    ///   (キューが保持する時間は「表示時刻 − 復号の出力時刻」= 2 つのトラックの基準の差 +
    ///   表示の遅れであり、基準の遅れそのものは含まない)
    /// - 基準の差が動き続けている ([`PlayoutTimeline::base_difference_drifted`])。これは
    ///   経路の遅れではなく TIMESTAMP の時計のずれであり、合わせると片側の表示の遅れが
    ///   上限まで伸びる
    ///
    /// 一度やめた側は [`TIMELINE_BASE_UNSHARED_HOLD_US`] の間は保持する
    /// ([`PlayoutTimeline::held_unshared_track`])。大きい側 (遅れて届いている側) が、
    /// TIMESTAMP が壁時計からずれている側である。
    fn drifted_track(&self) -> Option<Track> {
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
        self.held_unshared_track()
    }

    /// 直前に共有をやめた側。保持の時間を過ぎていれば None
    ///
    /// 閾値は jitter buffer の目標遅延で動くため、差が変わらなくても共有と解除を
    /// 往復し得る。往復のたびに、足した分を戻して (フレームを捨てる) すぐ足し直す
    /// (表示が止まる) ため、一度やめたらしばらくは戻さない。
    fn held_unshared_track(&self) -> Option<Track> {
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

    /// 共有をやめた側の記録を更新する
    ///
    /// 判定そのものは [`PlayoutTimeline::drifted_track`] が毎回計算する。保持の起点だけを
    /// ここで記録する。基準の差は観測のたびにしか動かないため、観測のたびに呼べば足りる。
    ///
    /// 新しく解除するときだけ、前回の解除の時刻を控える
    /// ([`PlayoutTimeline::is_unshared_repeatedly`])。既に保持している間は解除の時刻を
    /// 進めるだけであり、前回の解除の時刻は変えない。
    fn refresh_unshared_state(&mut self) {
        let Some(difference_us) = self.current_base_difference_us() else {
            return;
        };
        let drifted = self.base_difference_drifted();
        if difference_us.saturating_abs() <= self.base_difference_limit_us() && !drifted {
            return;
        }
        if self.held_unshared_track().is_none() {
            // 新しく解除する。前の解除の時刻を控え、前の解除の判断 (戻った状態を数えた分) は
            // 持ち越さない
            self.previous_unshared_at_us = self.last_unshared_at_us;
            self.unshared_returned_since_us = None;
        }
        self.last_unshared_at_us = self.last_base_difference_at_us;
        self.last_unshared_track = Some(if difference_us > 0 {
            Track::Audio
        } else {
            Track::Video
        });
    }

    /// 解除のきっかけが去って差が戻ったら、保持を待たずに共有を戻す
    ///
    /// 戻った状態が [`TIMELINE_BASE_UNSHARED_RELEASE_US`] 続いたら解除する。同じ水準へ
    /// 留まる理由が無いためである。
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
        // 基準を戻す。解除の時刻は残す (直近に往復していないかを次の解除で見る)
        self.last_unshared_track = None;
        self.unshared_returned_since_us = None;
    }

    /// 保持を早く解除してよい状態か (解除のきっかけが去り、差が戻っているか)
    ///
    /// 次のすべてを満たすこと。どれかが崩れれば、差はまだ解除の水準の側にあり、共有と
    /// 解除を往復し得る。
    ///
    /// - 保持していること
    /// - 動きの判定が解けていること。離れた幅が続いている間 (真のドリフト) は、その幅が
    ///   元の水準へ戻ったことにならない
    /// - 直近に解除と解除を往復していないこと ([`PlayoutTimeline::is_unshared_repeatedly`])。
    ///   閾値が動き続けている間は、差が閾値の内側にあっても解除と解除を繰り返す
    /// - 差が閾値を超えていないこと (超えていれば合わせられない)
    /// - 差が水準へ戻っていること。記録の間隔ごとの変化ではなく水準への復帰を見るのは、
    ///   負荷のある環境では差が数十 ms の幅で細かく動き続け、変化が 0 になる瞬間が来ない
    ///   ためである
    ///
    /// 解除のきっかけが動きだったかは見ない。購読の直後の cache の再生では差が静かに動き
    /// (記録 1 回あたり 5 ms 以下)、時計のずれと判定されて解除されることがある。この場合も
    /// 差が水準へ戻れば原因は去っている。
    fn can_release_unshared_hold(&self) -> bool {
        if self.last_unshared_track.is_none() || self.held_unshared_track().is_none() {
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
    /// 2 回の解除が [`TIMELINE_BASE_UNSHARED_HOLD_US`] の内側で起きていれば、閾値 (表示の
    /// 遅れの上限 − 再生の遅れ) が動き続けており、解除してもまた解除される。往復のたびに、
    /// 同期が足した分を戻して (間に合わないフレームを捨てる) すぐ足し直す (表示が待って
    /// 止まる) ことになるため、落ち着くまで解かない。
    fn is_unshared_repeatedly(&self) -> bool {
        let (Some(previous_unshared_at_us), Some(last_unshared_at_us)) =
            (self.previous_unshared_at_us, self.last_unshared_at_us)
        else {
            return false;
        };
        last_unshared_at_us.saturating_sub(previous_unshared_at_us) < TIMELINE_BASE_UNSHARED_HOLD_US
    }

    /// 直近の基準の差の動き (マイクロ秒/秒)。まだ履歴が無ければ None
    fn base_drift_us_per_second(&self) -> Option<i64> {
        let (movement_us, span_us) = self.base_difference_history()?;
        if span_us <= 0 {
            return None;
        }
        Some(movement_us.saturating_mul(1_000_000) / span_us)
    }

    /// トラックごとの表示時刻の内訳
    fn track_breakdown(&self, track: Track) -> TrackBreakdown {
        let state = &self.tracks[track.index()];
        TrackBreakdown {
            base_delay_us: state.base_us,
            jitter_delay_us: state.base_us.map(|_| state.own_delay_us),
            sync_extra_delay_us: state.sync_extra_us,
            presentation_delay_us: self.presentation_delay_total_us(track),
            presentation_delay_cap_us: self.presentation_cap_us(),
        }
    }

    /// TIMESTAMP から表示時刻までの差 (基準の遅れを含む)。まだ使えないときは None
    fn presentation_delay_total_us(&self, track: Track) -> Option<i64> {
        let base_us = self.tracks[track.index()].base_us?;
        if self.drifted_track() == Some(track) {
            return None;
        }
        Some(base_us.saturating_add(self.delay_us(track)))
    }

    /// 2 つのトラックの基準の差を記録する (一定の間隔で)
    ///
    /// 差が動き続けているかを見るために使う ([`PlayoutTimeline::base_difference_drifted`])。
    /// 記録するのは窓全体の最小値ではなく短い区間の最小値である。窓全体の最小値は、
    /// TIMESTAMP が壁時計から遅れていくときも窓が埋まるまで動かないため、動きを早く
    /// 見つけられない。
    fn record_base_difference(&mut self, now_us: i64) {
        if self
            .last_base_difference_at_us
            .is_some_and(|last_base_difference_at_us| {
                now_us.saturating_sub(last_base_difference_at_us)
                    < TIMELINE_BASE_DIFFERENCE_SAMPLE_INTERVAL_US
            })
        {
            return;
        }
        let Some(audio_us) = self.recent_base_us(Track::Audio, now_us) else {
            return;
        };
        let Some(video_us) = self.recent_base_us(Track::Video, now_us) else {
            return;
        };
        self.last_base_difference_at_us = Some(now_us);
        let difference_us = audio_us.saturating_sub(video_us);
        let previous_value_us = self.last_base_difference_value_us;
        let previous_reference_us = self.last_base_difference_reference_us;
        self.last_base_difference_value_us = Some(difference_us);
        // 参照 (表示時刻が使う基準の差) を残す。動きの向きを確かめるのに使う
        let reference_us = self.current_base_difference_us();
        self.last_base_difference_reference_us = reference_us;
        self.base_differences.push(now_us, difference_us);
        self.base_differences
            .prune(now_us.saturating_sub(TIMELINE_BASE_DRIFT_WINDOW_US));
        self.update_base_difference_drift(
            now_us,
            difference_us,
            previous_value_us,
            previous_reference_us,
            reference_us,
        );
    }

    /// 直近の基準 (マイクロ秒)。まだ観測していなければ None
    ///
    /// 窓全体の最小値 (`base_us`) ではなく短い区間の最小値である。基準が単調に動いて
    /// いるとき、窓全体の最小値は最も古い観測を指したままになる。
    fn recent_base_us(&self, track: Track, now_us: i64) -> Option<i64> {
        self.tracks[track.index()]
            .offsets
            .min_after(now_us.saturating_sub(TIMELINE_BASE_DIFFERENCE_RECENT_WINDOW_US))
    }

    /// 基準の差が動き続けているか (時計がずれているとみなすか)
    ///
    /// 判定は [`PlayoutTimeline::update_base_difference_drift`] が記録のたびに行い、結果を
    /// ここへ残す。
    fn base_difference_drifted(&self) -> bool {
        self.base_difference_drifted_value
    }

    /// 記録した差から、動きの持続と水準への復帰を判定する
    ///
    /// 差が「離れた幅のまま」になっている時間を測り、[`TIMELINE_BASE_DRIFT_CONFIRM_US`]
    /// 続いたときだけ時計のずれとみなす。離れた水準が元へ戻れば、その動きは一過性
    /// (読み出しが一瞬遅れた、経路が一瞬つまずいた) だったと分かるため、判定を消して水準を
    /// 取り直す。
    ///
    /// 水準は動きが戻ったときにだけ取り直す。記録のたびに取り直すと、ゆっくりしたドリフト
    /// でも水準が一緒に動いてしまい、離れた幅が [`TIMELINE_BASE_DRIFT_US`] を超えない。
    ///
    /// 待たずに判定する段差 ([`TIMELINE_BASE_DRIFT_STEP_US`]) だけは、参照 (`reference_us`) が
    /// 同じ向きに [`TIMELINE_BASE_DRIFT_US`] を超えて動いていることも確かめる。短い窓の
    /// 最小値の差は、経路と復号がつまずくと 200 ms を超えて動くが、表示時刻が使う基準は
    /// 動かない。参照を確かめないと、その乱れを時計のずれとみなして共有を解除し、しかも
    /// 水準が乱れの側で取られると解除が解けなくなる。
    ///
    /// 判定は落ち着いた水準を観測してから始める。購読の直後は、relay の cache から届いた分と
    /// 購読を始めるまでにたまった分をまとめて復号しており、基準の遅れが数百 ms から数秒
    /// 動く。この動きを時計のずれとみなすと、共有を 30 秒解除してしまい、落ち着いた後の
    /// 観測がすべて解除されたままになる。
    fn update_base_difference_drift(
        &mut self,
        now_us: i64,
        difference_us: i64,
        previous_value_us: Option<i64>,
        previous_reference_us: Option<i64>,
        reference_us: Option<i64>,
    ) {
        // 差が動いているかどうか。記録の間隔ごとの変化であり、窓の最小値のような遅れが無い
        let change_us = previous_value_us.map_or(0, |previous_value_us| {
            difference_us
                .saturating_sub(previous_value_us)
                .saturating_abs()
        });
        if change_us <= TIMELINE_BASE_DIFFERENCE_QUIET_US {
            if self.base_difference_quiet_since_us.is_none() {
                self.base_difference_quiet_since_us = Some(now_us);
            }
        } else {
            self.base_difference_quiet_since_us = None;
        }
        if self.base_difference_first_at_us.is_none() {
            self.base_difference_first_at_us = Some(now_us);
        }

        let Some(settled_us) = self.settled_base_difference_us else {
            // まだ「動いていない水準」を観測していない。動き続ける差 (ドリフト) のために、
            // 待つ時間には上限を置く
            let settled_by_quiet_us = self.base_difference_quiet_since_us.map(|quiet_since_us| {
                quiet_since_us.saturating_add(TIMELINE_BASE_DIFFERENCE_SETTLE_US)
            });
            let settled_by_latest_us = self
                .base_difference_first_at_us
                .map(|first_at_us| first_at_us.saturating_add(TIMELINE_BASE_DIFFERENCE_START_US));
            let settled_at_us = match (settled_by_quiet_us, settled_by_latest_us) {
                (Some(quiet), Some(latest)) => Some(quiet.min(latest)),
                (Some(quiet), None) => Some(quiet),
                (None, Some(latest)) => Some(latest),
                (None, None) => None,
            };
            // 水準は参照 (表示時刻が使う基準の差) から取る。動きを見る短い窓の差は経路と
            // 復号の乱れの位相で動くため、そこから取ると乱れの側の水準を基準にしてしまい、
            // 乱れが去っても離れた幅が戻らず、解除が解けなくなる
            if let Some(settled_at_us) = settled_at_us
                && now_us >= settled_at_us
                && let Some(reference_us) = reference_us
            {
                self.settled_base_difference_us = Some(reference_us);
            }
            self.base_difference_drifted_value = false;
            return;
        };

        let deviation_us = difference_us.saturating_sub(settled_us).saturating_abs();
        if deviation_us <= TIMELINE_BASE_DRIFT_US {
            self.base_difference_drifted_value = false;
            if self.base_difference_deviation_since_us.is_some() {
                // 離れた水準から戻った。動いていた幅は一過性だったので、今の水準を基準にする
                self.settled_base_difference_us = Some(difference_us);
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
        // 戻るのを待つのは、その大きさでは時計のずれと読み出しの遅れを見分けられないためで
        // ある
        let reference_from_us = self.base_difference_deviation_reference_us.or(reference_us);
        let reference_movement_us = match (reference_us, reference_from_us) {
            (Some(reference_us), Some(from_us)) => reference_us.saturating_sub(from_us),
            _ => 0,
        };
        let deviation_signed_us = difference_us.saturating_sub(settled_us);
        let reference_moved_same_way = deviation_signed_us.saturating_mul(reference_movement_us)
            > 0
            && reference_movement_us.saturating_abs() > TIMELINE_BASE_DRIFT_US;
        self.base_difference_drifted_value = (deviation_us > TIMELINE_BASE_DRIFT_STEP_US
            && (jumped_away_us || reference_moved_same_way))
            || now_us.saturating_sub(since_us) >= TIMELINE_BASE_DRIFT_CONFIRM_US;
    }

    /// 基準の差の履歴の、最も古い記録から今までの動き
    ///
    /// 履歴の値と今の差は同じ求め方 (直近の窓の最小値の差) でなければ比べられない。
    /// 窓全体の最小値と比べると、TIMESTAMP が遅れていくときに符号が逆になる。
    ///
    /// 戻り値は (動いた幅, その幅を測った時間)。まだ記録が無ければ None。
    fn base_difference_history(&self) -> Option<(i64, i64)> {
        let (oldest_at_us, oldest_value_us) = self.base_differences.oldest()?;
        let last_value_us = self.last_base_difference_value_us?;
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

    /// 基準の差の履歴を消す
    fn clear_base_differences(&mut self) {
        self.base_differences = TimedWindow::default();
        self.last_base_difference_at_us = None;
        self.last_base_difference_value_us = None;
        self.last_base_difference_reference_us = None;
        self.base_difference_quiet_since_us = None;
        self.base_difference_first_at_us = None;
        self.settled_base_difference_us = None;
        self.base_difference_deviation_since_us = None;
        self.base_difference_deviation_reference_us = None;
        self.base_difference_drifted_value = false;
        // 解除を早める判断のうち、差の記録を数える分はやり直す。保持そのもの
        // (解除の時刻と側) は残す (基準を取り直しても、原因が去ったかどうかの判断は続ける)
        self.unshared_returned_since_us = None;
    }

    /// 同期が足した分を含まない、トラックの表示の遅れ (マイクロ秒)。未観測なら None
    ///
    /// 自分の jitter buffer の遅延と `targetLatency` の大きい方である。
    fn natural_delay_known_us(&self, track: Track) -> Option<i64> {
        self.tracks[track.index()].base_us?;
        Some(self.natural_delay_us(track))
    }

    /// 2 つのトラックの基準の遅れの差の閾値 (マイクロ秒)
    ///
    /// 表示の遅れの上限から、上限を適用する前の 2 つのトラックの表示の遅れの大きい方
    /// (自分の揺らぎから求めた遅れと `targetLatency` の大きい方) を引いた値である。
    /// 上限で切られる分は合わせられないためである。同期の制御が足した分は含めない
    /// (含めると、合わせるために遅らせた結果で閾値が下がり、合わせた直後に「基準が
    /// ずれている」と判定されてしまう)。
    fn base_difference_limit_us(&self) -> i64 {
        let delay_us = self
            .natural_delay_us(Track::Audio)
            .max(self.natural_delay_us(Track::Video));
        self.presentation_cap_us()
            .saturating_sub(delay_us)
            .max(TIMELINE_MIN_BASE_DIFFERENCE_MS * 1_000)
    }
}

impl Default for PlayoutTimeline {
    fn default() -> Self {
        Self::new()
    }
}

/// 揺らぎの百分位から映像の表示の遅れの目標を求める (マイクロ秒)
///
/// 学習に使う観測の (遅れ − 基準の遅れ) を並べ、百分位の値を返す。観測が無いときは 0。
fn video_target_us(state: &TrackState, base_us: i64, frame_interval_us: Option<i64>) -> i64 {
    // 表示の遅れの上限を超える揺らぎは吸収できないため、分位点へ入れる前に除外する。
    // 上限で切るだけでは、バーストで 500 ms を超える遅れが混ざったときに目標が上限へ
    // 張り付き、A/V 同期が音声へ最大 100 ms を即座に足してしまう
    let mut jitters: Vec<i64> = state
        .learning_offsets
        .values()
        .map(|offset_us| offset_us.saturating_sub(base_us).max(0))
        .filter(|jitter_us| *jitter_us <= TIMELINE_MAX_PRESENTATION_DELAY_MS * 1_000)
        .collect();
    if jitters.is_empty() {
        return 0;
    }
    jitters.sort_unstable();
    jitters[percentile_index(jitters.len(), video_percentile_milli(frame_interval_us))]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playout::delay::AUDIO_DELAY_START_MS;

    /// 音声の観測を 1 つ足す
    fn observe_audio(timeline: &mut PlayoutTimeline, wall_clock_us: i64, timestamp_us: i64) {
        timeline.observe(Track::Audio, wall_clock_us, timestamp_us);
    }

    /// 観測が無い間は表示の遅れが無いこと
    #[test]
    fn starts_without_a_delay() {
        let timeline = PlayoutTimeline::new();
        assert!(
            timeline.presentation_delay_us(Track::Audio).is_none(),
            "観測が無ければ表示の遅れは無い"
        );
        assert_eq!(timeline.learned_delay_us(Track::Audio), 0);
    }

    /// 音声の表示の遅れが既定の目標遅延から始まること
    #[test]
    fn audio_starts_with_the_default_learned_delay() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        assert_eq!(
            timeline.learned_delay_us(Track::Audio),
            AUDIO_DELAY_START_MS * 1_000,
            "最初の観測では既定の目標遅延を使う"
        );
        assert_eq!(
            timeline.presentation_delay_us(Track::Audio),
            Some(AUDIO_DELAY_START_MS * 1_000)
        );
    }

    /// TIMESTAMP の差どおりに鳴らす時刻が進むこと
    #[test]
    fn present_time_follows_the_timestamp() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        assert_eq!(
            timeline.present_us(Track::Audio, 1_000_000),
            Some(10_080_000),
            "基準の時刻に表示の遅れを足した時刻に鳴らす"
        );
        assert_eq!(
            timeline.present_us(Track::Audio, 1_020_000),
            Some(10_100_000),
            "TIMESTAMP が 20 ms 進めば鳴らす時刻も 20 ms 進む"
        );
    }

    /// 遅れて届いた観測が基準を動かさないこと
    #[test]
    fn late_arrival_does_not_move_the_basis() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        let present_us = timeline.present_us(Track::Audio, 1_000_000);
        // 同じ TIMESTAMP のフレームが 50 ms 遅れて届いても、基準は早く届いた方のまま
        observe_audio(&mut timeline, 10_050_000, 1_000_000);
        assert_eq!(
            timeline.present_us(Track::Audio, 1_000_000),
            present_us,
            "遅れて届いた観測では基準が動かない"
        );
    }

    /// 早く届いた観測で基準が動くこと
    #[test]
    fn earlier_arrival_moves_the_basis() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_050_000, 1_000_000);
        observe_audio(&mut timeline, 10_050_000, 1_020_000);
        // 経路が空いて 20 ms 早く届くようになった
        observe_audio(&mut timeline, 10_040_000, 1_040_000);
        assert_eq!(
            timeline.present_us(Track::Audio, 1_040_000),
            Some(10_040_000 + timeline.presentation_delay_us(Track::Audio).unwrap_or(0)),
            "早く届いた観測が新しい基準になる"
        );
    }

    /// TIMESTAMP が基準から大きく離れたら取り直すこと
    #[test]
    fn timestamp_jump_starts_a_new_generation() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        let generation = timeline.generation();
        // TIMESTAMP だけが 3 秒進んだ (配信元の切り替えなど)
        observe_audio(&mut timeline, 10_000_000, 4_000_000);
        assert_eq!(
            timeline.generation(),
            generation + 1,
            "大きく離れたら取り直す"
        );
        assert_eq!(
            timeline.present_us(Track::Audio, 4_000_000),
            Some(10_000_000 + timeline.presentation_delay_us(Track::Audio).unwrap_or(0)),
            "新しい基準で鳴らす時刻を求める"
        );
        // TIMESTAMP が大きく戻ったときも取り直す
        observe_audio(&mut timeline, 20_000_000, 2_000_000);
        assert_eq!(
            timeline.generation(),
            generation + 2,
            "戻ったときも取り直す"
        );
    }

    /// 同期の制御は両方のトラックに基準ができてから動くこと
    #[test]
    fn sync_needs_bases_on_both_tracks() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        assert_eq!(
            timeline.presentation_delay_us(Track::Audio),
            Some(80_000),
            "映像の基準が無ければ自分の遅れのまま"
        );
        assert!(
            timeline.presentation_delay_us(Track::Video).is_none(),
            "映像はまだ観測していない"
        );
        // 映像が 300 ms 遅れて届くと、先行する音声を後行の映像に合わせて遅らせる。
        // ただし移す分は上限 (TIMELINE_MAX_COMPENSATED_DIFFERENCE_US) までであり、
        // 残りは A/V のずれとして受け入れる
        timeline.observe(Track::Video, 10_300_000, 1_000_000);
        assert_eq!(
            timeline.presentation_delay_us(Track::Audio),
            Some(180_000),
            "音声の遅れを上げて映像に合わせる"
        );
    }

    /// 映像が遅れて届くとき、先行する音声の遅れを上げて表示時刻を合わせること
    #[test]
    fn sync_raises_the_audio_toward_the_late_video() {
        let mut timeline = PlayoutTimeline::new();
        // 音声は 10 ms で届き、映像は同じ TIMESTAMP で 300 ms 遅れて届く
        observe_audio(&mut timeline, 10_010_000, 1_000_000);
        timeline.observe(Track::Video, 10_310_000, 1_000_000);
        let audio_us = timeline
            .presentation_delay_us(Track::Audio)
            .expect("基準があるので遅れが決まる");
        assert!(audio_us > 80_000, "音声の遅れが上がる: {audio_us}");
        assert!(audio_us < 300_000, "後行側を追い越さない: {audio_us}");
        // 2 つのトラックの表示時刻の差は、目標の差に収まる。自然な差は
        // 300 ms (基準の差) − 80 ms (音声の遅れ) = 220 ms であり、そこから
        // TIMELINE_MAX_COMPENSATED_DIFFERENCE_US を引いた 120 ms になる
        let audio_present_us = timeline
            .present_us(Track::Audio, 1_000_000)
            .expect("基準がある");
        let video_present_us = timeline
            .present_us(Track::Video, 1_000_000)
            .expect("基準がある");
        assert_eq!(
            video_present_us - audio_present_us,
            120_000,
            "相手側へ移す分は上限までにする"
        );
    }

    /// 同期で足す遅延は、表示待ちのキューの上限で切られること
    ///
    /// 共有する表示時刻の下限から計算し直すため、上限を超える分は足さない。旧実装は
    /// 「相手側が既に足した分を含めて上限で切る」だったが、新実装は自分の jitter が
    /// 補償を埋めた分を差し引く。
    #[test]
    fn align_sync_extras_is_bounded_by_the_presentation_cap() {
        let mut timeline = PlayoutTimeline::new();
        // 音声の素の表示の遅れが 1,000 ms、映像が 0 ms で、音声へは既に上限いっぱい足して
        // いる (映像が遅れていたため音声を遅らせた後、映像の遅れだけが消えた状態)
        timeline.tracks[Track::Audio.index()].sync_extra_us =
            TIMELINE_MAX_COMPENSATED_DIFFERENCE_US;
        timeline.align_sync_extras([1_000_000, 0]);
        // フレーム間隔がまだ分からないため、表示の遅れの上限は設定値の 500 ms になる
        assert_eq!(
            timeline.tracks[Track::Audio.index()].sync_extra_us,
            0,
            "後行側になった音声の補償は消えること"
        );
        assert_eq!(
            timeline.tracks[Track::Video.index()].sync_extra_us,
            TIMELINE_MAX_PRESENTATION_DELAY_MS * 1_000,
            "先行する映像へ足す量を表示の上限で切ること"
        );
    }

    /// 基準の差が解消したら、足した補償をその場で手放すこと
    #[test]
    fn a_resolved_base_difference_releases_the_compensation() {
        let mut timeline = PlayoutTimeline::new();
        // 1 回目: 映像が 300 ms 遅れて届き、先行する音声の遅れが上がる
        observe_audio(&mut timeline, 10_010_000, 1_000_000);
        timeline.observe(Track::Video, 10_310_000, 1_000_000);
        assert_eq!(
            timeline.tracks[Track::Audio.index()].sync_extra_us,
            TIMELINE_MAX_COMPENSATED_DIFFERENCE_US,
            "先行する音声へ補償を足すこと"
        );
        assert_eq!(
            timeline.presentation_delay_us(Track::Audio),
            Some(180_000),
            "音声の表示の遅れは自分の遅れ + 補償になること"
        );

        // 2 回目: 映像が同じ遅れで届くようになる (基準の差が無くなる)
        observe_audio(&mut timeline, 11_010_000, 2_000_000);
        timeline.observe(Track::Video, 11_010_000, 2_000_000);
        assert_eq!(
            timeline.tracks[Track::Audio.index()].sync_extra_us,
            0,
            "基準の差が解消したら補償を戻すこと"
        );
        // 補償が消え、自分の遅れ (揺らぎの学習と閉ループの大きい方) だけになる
        assert_eq!(
            timeline.presentation_delay_us(Track::Audio),
            Some(timeline.learned_delay_us(Track::Audio)),
            "自分の遅れだけになること"
        );
    }

    /// 共有する表示時刻の下限は、必要値が下がれば毎秒の速さで戻ること
    #[test]
    fn the_shared_floor_decays_by_the_budget() {
        let mut timeline = PlayoutTimeline::new();
        // targetLatency で表示の遅れの下限を上げておく
        timeline.set_target_latency_ms(300);
        observe_audio(&mut timeline, 10_010_000, 1_000_000);
        timeline.observe(Track::Video, 10_010_000, 1_000_000);
        let raised_us = timeline.presentation_delay_us(Track::Audio).unwrap_or(0);
        assert!(raised_us >= 300_000, "下限まで遅らせる: {raised_us}");

        // 下限を外して 1 秒後の観測を渡すと、毎秒の速さだけ戻る
        timeline.set_target_latency_ms(0);
        observe_audio(&mut timeline, 11_010_000, 2_000_000);
        timeline.observe(Track::Video, 11_010_000, 2_000_000);
        let decayed_us = timeline.presentation_delay_us(Track::Audio).unwrap_or(0);
        assert_eq!(
            decayed_us,
            raised_us - TIMELINE_DELAY_DECAY_US_PER_SECOND,
            "毎秒 {} マイクロ秒だけ戻す: raised={raised_us} decayed={decayed_us}",
            TIMELINE_DELAY_DECAY_US_PER_SECOND
        );
    }

    /// 映像の表示の遅れが揺らぎの百分位から求まり、上限を超えないこと
    #[test]
    fn video_delay_comes_from_the_jitter_percentile() {
        let mut timeline = PlayoutTimeline::new();
        let start_wall_us = 10_000_000;
        for index in 0..300i64 {
            // 10 枚に 1 枚だけ 40 ms 遅れて届く
            let jitter_us = if index % 10 == 0 { 40_000 } else { 0 };
            let wall_us = start_wall_us + index * 33_333 + jitter_us;
            let timestamp_us = 1_000_000 + index * 33_333;
            timeline.observe(Track::Video, wall_us, timestamp_us);
        }
        let learned_us = timeline.learned_delay_us(Track::Video);
        assert_eq!(
            learned_us, 40_000,
            "揺らぎの百分位 (40 ms) になる: {learned_us}"
        );
        assert!(
            learned_us <= TIMELINE_MAX_PRESENTATION_DELAY_MS * 1_000,
            "上限を超えない"
        );
    }

    /// 表示待ちのキューが浅いと表示の遅れが上限で切られること
    #[test]
    fn video_delay_is_capped_by_the_queue() {
        let config = TimelineConfig {
            video_queue_limit: 5,
            ..TimelineConfig::default()
        };
        let mut timeline = PlayoutTimeline::with_config(config);
        let start_wall_us = 10_000_000;
        for index in 0..300i64 {
            let jitter_us = if index % 10 == 0 { 40_000 } else { 0 };
            let wall_us = start_wall_us + index * 33_333 + jitter_us;
            let timestamp_us = 1_000_000 + index * 33_333;
            timeline.observe(Track::Video, wall_us, timestamp_us);
        }
        // (キューの上限 - 余裕 4 枚) x フレーム間隔 (33 ms) で切られる
        assert_eq!(timeline.learned_delay_us(Track::Video), 33_333);
    }

    /// 表示の遅れの上限 (500 ms) を超える揺らぎは分位点へ入れないこと
    ///
    /// 上限で切るだけでは、バーストで 500 ms を超える遅れが混ざったときに目標が上限へ
    /// 張り付く。超える揺らぎは吸収できないため、分位点へ入れる前に除外する。
    #[test]
    fn video_delay_is_capped_by_the_maximum() {
        let mut timeline = PlayoutTimeline::new();
        // 2 枚に 1 枚が 600 ms 遅れて届く。超えた分は分位点へ入れないため、残る 0 ms だけを見る
        for index in 0..300i64 {
            let jitter_us = if index % 2 == 0 { 600_000 } else { 0 };
            let wall_us = 10_000_000 + index * 33_333 + jitter_us;
            let timestamp_us = 1_000_000 + index * 33_333;
            timeline.observe(Track::Video, wall_us, timestamp_us);
        }
        assert_eq!(
            timeline.learned_delay_us(Track::Video),
            0,
            "上限を超える揺らぎを分位点へ入れないこと"
        );
    }

    /// 上限ちょうどの揺らぎは残ること
    #[test]
    fn video_delay_keeps_a_jitter_at_the_maximum() {
        let mut timeline = PlayoutTimeline::new();
        // 2 枚に 1 枚がちょうど 500 ms 遅れて届く。上限ちょうどは除外しない
        for index in 0..300i64 {
            let jitter_us = if index % 2 == 0 {
                TIMELINE_MAX_PRESENTATION_DELAY_MS * 1_000
            } else {
                0
            };
            let wall_us = 10_000_000 + index * 33_333 + jitter_us;
            let timestamp_us = 1_000_000 + index * 33_333;
            timeline.observe(Track::Video, wall_us, timestamp_us);
        }
        assert_eq!(
            timeline.learned_delay_us(Track::Video),
            TIMELINE_MAX_PRESENTATION_DELAY_MS * 1_000,
            "上限ちょうどの揺らぎは残ること"
        );
    }

    /// 表示待ちのキューが余裕分しか無いときは表示の遅れが 0 になること
    #[test]
    fn video_delay_is_zero_when_the_queue_has_no_room() {
        let config = TimelineConfig {
            video_queue_limit: TIMELINE_QUEUE_HEADROOM_FRAMES,
            ..TimelineConfig::default()
        };
        let mut timeline = PlayoutTimeline::with_config(config);
        let start_wall_us = 10_000_000;
        for index in 0..300i64 {
            let jitter_us = if index % 10 == 0 { 40_000 } else { 0 };
            let wall_us = start_wall_us + index * 33_333 + jitter_us;
            let timestamp_us = 1_000_000 + index * 33_333;
            timeline.observe(Track::Video, wall_us, timestamp_us);
        }
        assert_eq!(timeline.learned_delay_us(Track::Video), 0);
    }

    /// live に追いつくまでに届いた揺らぎは学習しないこと
    #[test]
    fn catch_up_arrivals_are_not_learned() {
        let mut timeline = PlayoutTimeline::new();
        // 基準が 250 ms ごとに 20 ms 以上下がる (live へ追いついている最中) 観測を続ける。
        // 遅れは大きいが、これは経路の揺らぎではない
        for index in 0..30i64 {
            let wall_us = 10_000_000 + index * 33_333 + 100_000;
            let timestamp_us = 1_000_000 + index * 53_333;
            timeline.observe(Track::Video, wall_us, timestamp_us);
        }
        assert_eq!(
            timeline.learned_delay_us(Track::Video),
            0,
            "追いつき中は学習しない"
        );
        // 追いついた後に届いた揺らぎは学習する
        let mut wall_us = 11_000_000;
        let mut timestamp_us = 2_599_990;
        for index in 0..100i64 {
            let jitter_us = if index % 10 == 0 { 40_000 } else { 0 };
            wall_us += 33_333;
            timestamp_us += 33_333;
            timeline.observe(Track::Video, wall_us + jitter_us, timestamp_us);
        }
        assert_eq!(
            timeline.learned_delay_us(Track::Video),
            40_000,
            "追いついた後の揺らぎだけを学習する"
        );
    }

    /// 負の設定値は 0 として扱うこと
    #[test]
    fn negative_config_values_are_clamped() {
        let config = TimelineConfig {
            max_presentation_delay_ms: -100,
            target_latency_ms: -50,
            ..TimelineConfig::default()
        };
        let mut timeline = PlayoutTimeline::with_config(config);
        observe_audio(&mut timeline, 10_000_000, 9_000_000);
        assert_eq!(timeline.target_latency_ms(), 0);
        assert_eq!(
            timeline.presentation_delay_us(Track::Audio),
            Some(0),
            "上限が 0 なら表示の遅れも 0"
        );
        assert_eq!(timeline.limited_us(), 0);
    }

    /// targetLatency が 2 つのトラックの表示の遅れの下限になり、切り下げ分が読めること
    #[test]
    fn target_latency_is_the_lower_bound() {
        let config = TimelineConfig {
            target_latency_ms: 120,
            ..TimelineConfig::default()
        };
        let mut timeline = PlayoutTimeline::with_config(config);
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        timeline.observe(Track::Video, 10_000_000, 1_000_000);
        assert_eq!(timeline.presentation_delay_us(Track::Audio), Some(120_000));
        assert_eq!(timeline.presentation_delay_us(Track::Video), Some(120_000));
        assert_eq!(timeline.limited_us(), 0, "上限 (500 ms) に収まっている");

        // 後から上げたときもその場で下限へ反映される
        timeline.set_target_latency_ms(200);
        assert_eq!(timeline.presentation_delay_us(Track::Audio), Some(200_000));

        // 上限を 100 ms にすると 20 ms 切り下げられ、表示の遅れも上限で切られる
        let config = TimelineConfig {
            target_latency_ms: 120,
            max_presentation_delay_ms: 100,
            ..TimelineConfig::default()
        };
        let mut timeline = PlayoutTimeline::with_config(config);
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        assert_eq!(timeline.limited_us(), 20_000, "切り下げた分が読める");
        assert_eq!(
            timeline.presentation_delay_us(Track::Audio),
            Some(100_000),
            "表示の遅れは上限で切られる"
        );
    }

    /// 基準が大きく離れたトラックの表示時刻を返さないこと
    #[test]
    fn a_drifting_track_has_no_presentation_time() {
        let mut timeline = PlayoutTimeline::new();
        // 音声は 1 s の基準、映像は 2 s の基準 (閾値 420 ms を超える)
        observe_audio(&mut timeline, 10_000_000, 9_000_000);
        timeline.observe(Track::Video, 10_000_000, 8_000_000);
        assert!(!timeline.sharing_bases(), "基準を共有していない");
        assert!(timeline.present_us(Track::Audio, 9_000_000).is_some());
        assert!(
            timeline.present_us(Track::Video, 8_000_000).is_none(),
            "遅れている側の表示時刻は返さない"
        );
        assert!(timeline.presentation_delay_us(Track::Video).is_none());
    }

    /// 実績から A/V のずれが読めること
    #[test]
    fn skew_comes_from_the_presented_records() {
        let mut timeline = PlayoutTimeline::new();
        timeline.record_presentation(Track::Audio, 1_000_000, 5_000_000);
        timeline.record_presentation(Track::Video, 1_000_000, 5_040_000);
        assert_eq!(timeline.skew_us(), Some(40_000), "映像が 40 ms 遅れている");
        // 1 秒より古い実績は使わない
        timeline.record_presentation(Track::Audio, 1_000_000, 4_000_000);
        assert_eq!(timeline.skew_us(), None);
    }

    /// 1 つのトラックだけリセットできること
    #[test]
    fn reset_track_keeps_the_other_track() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        timeline.observe(Track::Video, 10_000_000, 1_000_000);
        let generation = timeline.generation();
        timeline.reset_track(Track::Audio);
        assert!(
            timeline.present_us(Track::Audio, 1_000_000).is_none(),
            "音声の基準が消える"
        );
        assert!(
            timeline.present_us(Track::Video, 1_000_000).is_some(),
            "映像の基準は残る"
        );
        assert_eq!(timeline.generation(), generation, "世代は進めない");
    }

    /// すべてリセットしても世代は残ること
    #[test]
    fn reset_clears_the_bases_and_keeps_the_generation() {
        let mut timeline = PlayoutTimeline::new();
        observe_audio(&mut timeline, 10_000_000, 1_000_000);
        timeline.observe(Track::Video, 10_000_000, 1_000_000);
        let generation = timeline.generation();
        timeline.reset();
        assert!(timeline.present_us(Track::Audio, 1_000_000).is_none());
        assert!(timeline.present_us(Track::Video, 1_000_000).is_none());
        assert_eq!(timeline.generation(), generation + 1);
    }

    /// 同期の制御が観測のたびに動くこと (1 秒間隔を待たない)
    #[test]
    fn sync_runs_on_every_observation() {
        let mut timeline = PlayoutTimeline::new();
        // 映像が 300 ms 遅れている状態を保ちながら、100 ms ごとに観測を足す
        for step in 0..3i64 {
            let wall_us = 10_000_000 + step * 100_000;
            let timestamp_us = 1_000_000 + step * 100_000;
            observe_audio(&mut timeline, wall_us, timestamp_us);
            timeline.observe(Track::Video, wall_us + 300_000, timestamp_us);
            assert!(
                timeline.presentation_delay_us(Track::Audio).unwrap_or(0) > 80_000,
                "{step} 回目の観測で遅れを上げる"
            );
        }
    }
}
