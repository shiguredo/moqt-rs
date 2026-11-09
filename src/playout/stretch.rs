//! 波形の周期による音声の時間圧縮・時間伸長と、欠落した区間の補間
//!
//! 到着の間隔が揺らいだ音声を目標の時刻に合わせるため、波形の周期 (ピッチ) 1 つ分を
//! 削って音の長さをわずかに詰める。再同期で浅くなった到着基準の深さを戻すときは、直前の
//! 実音と今回の実音を重ねて周期 1 つ分を足す (時間伸長)。音声が欠落した区間は、直前の
//! 音の末尾の周期を繰り返して埋める。
//!
//! 周期は第 1 チャンネルを 4 kHz へ間引いた自己相関から求める。ピークは 4 kHz の格子に
//! 乗るため、そのままだと周期が 48 kHz で 12 サンプル単位に量子化される。ピークの前後
//! 1 格子の相関を放物線で補間し、端数まで周期を求める。44.1 kHz は解析用の信号だけを
//! 48 kHz の格子へ換算し、求めた周期を元のサンプルレートへ戻して操作する (元の音声は
//! リサンプルしない)。欠落した区間の補間は、末尾の周期を求めて繰り返す。末尾の周期は
//! 4 kHz の粗い格子の周辺を元のサンプルレートの相関で評価し、ピークを放物線補間して
//! 端数まで求める (4 kHz の格子では 48 kHz で 12 サンプル単位に量子化され、格子に乗らない
//! 音では穴が長いほど位相がずれるため)。
//!
//! 時刻や出力デバイスには触れない。

use alloc::vec;
use alloc::vec::Vec;

/// 相関を測る長さ (4 kHz のサンプル数、12.5 ms)
pub const TIME_STRETCH_CORRELATION_LEN: usize = 50;

/// ピッチの周期の下限 (4 kHz のサンプル数、2.5 ms)
pub const TIME_STRETCH_MIN_LAG: usize = 10;

/// ピッチの周期の上限 (4 kHz のサンプル数、15 ms)
pub const TIME_STRETCH_MAX_LAG: usize = 60;

/// 操作を行う正規化相関の下限
pub const TIME_STRETCH_CORRELATION_THRESHOLD: f64 = 0.9;

/// 無音とみなす振幅のピーク (-60 dBFS 相当)
pub const TIME_STRETCH_SILENCE_PEAK: f32 = 0.001;

/// 継ぎ目の段差を許す、末尾の周期の自然な段差に対する倍率
///
/// 周期どおりに繰り返せていれば、繰り返しの先頭は末尾の続きになるため段差は自然な段差と
/// 同程度になる。周期がずれている音では段差が大きくなる (クリックとして聞こえる) ため、
/// この倍率を超える音では埋めない。
pub const TIME_STRETCH_MAX_SEAM_STEP_RATIO: f64 = 2.0;

/// 欠落した区間の補間で使う相関の窓の長さ (4 kHz のサンプル数、9.75 ms)
///
/// 候補のずらし幅に比例させない固定長であることが要点である。窓長をずらし幅に比例させると、
/// 窓が短い (= ずらし幅が短い) 候補ほど相関が高く出て、有色雑音で最短のずらし幅を選び、
/// 400 Hz のブザーになる。
///
/// 窓長は「窓長 + 最大のずらし幅が音の長さに収まる」必要がある。20 ms のフレームは 48 kHz
/// では 4 kHz へ間引くと 79 サンプルであり、100 Hz (40 サンプル) まで探すには窓長 39 が
/// 上限である (39 + 40 = 79)。これより低いピッチは 20 ms のフレームでは探せない (窓を
/// 短くすれば探せるが、相関の推定が不安定になるため伸ばさない)。
pub const TIME_STRETCH_TAIL_CORRELATION_LEN: usize = 39;

/// 補間した音の末尾を無音へ落とす長さ (ミリ秒)
///
/// 穴の終わりで次に鳴る音の位相はこのモジュールからは分からない。末尾の周期の相関が高ければ
/// 繰り返しの位相が次の音と揃うため段差は波形の自然な段差に収まるが、相関が低い音では位相が
/// ずれて段差が大きくなる。そのときは末尾をこの長さで 0 へ落とし、段差を「無音からの
/// 立ち上がり」に変える (穴の残りは無音でよい)。短くても段差を消せて、音を削りすぎない
/// 長さにする。
pub const TIME_STRETCH_CONCEAL_FADE_MS: f64 = 2.0;

/// 補間した音の末尾を無音へ落とし始める相関の下限
///
/// 末尾の周期の相関がこれ未満のときは、繰り返す周期が本当の周期とずれている可能性があり、
/// 穴の終わりで次の音と位相が合わない (段差が出る)。補間をやめる下限
/// ([`TIME_STRETCH_CORRELATION_THRESHOLD`] の 0.9) は超えていても、位相のずれは残る。
pub const TIME_STRETCH_CONCEAL_FADE_CORRELATION: f64 = 0.95;

/// 補間した音の末尾の振幅 (補間の長さが [`TIME_STRETCH_MAX_CONCEAL_US`] 以上のとき)
pub const TIME_STRETCH_CONCEAL_END_GAIN: f32 = 0.5;

/// 補間する長さの上限 (マイクロ秒)
///
/// 長い補間ほど繰り返しが目立つため、埋めた音の末尾へ向けて振幅を
/// [`TIME_STRETCH_CONCEAL_END_GAIN`] まで下げる。その下げきる長さがこの値である。
pub const TIME_STRETCH_MAX_CONCEAL_US: i64 = 100_000;

/// 4 kHz へ間引くフィルタ
struct DownsampleFilter {
    /// フィルタ係数 (整数。和で正規化して使う)
    coefficients: &'static [i32],
    /// 間引きの間隔 (入力のサンプル数)
    factor: usize,
    /// フィルタと間引きで生じる遅れの補正 (入力のサンプル数)
    delay: usize,
}

/// 4 kHz の格子に対応する間引きフィルタを返す (対応外は None)
///
/// `grid` は 4 kHz の 1 サンプルに対応する入力のサンプル数である (8 kHz なら 2)。
fn downsample_filter_for_grid(grid: usize) -> Option<DownsampleFilter> {
    // 32 bit ターゲットでも grid の掛け算でオーバーフローしないように saturating にする
    let filter = match grid.saturating_mul(4_000) {
        8_000 => DownsampleFilter {
            coefficients: &[1229, 1638, 1229],
            factor: 2,
            delay: 2,
        },
        16_000 => DownsampleFilter {
            coefficients: &[614, 819, 1229, 819, 614],
            factor: 4,
            delay: 3,
        },
        32_000 => DownsampleFilter {
            coefficients: &[584, 512, 625, 667, 625, 512, 584],
            factor: 8,
            delay: 4,
        },
        48_000 => DownsampleFilter {
            coefficients: &[1019, 390, 427, 440, 427, 390, 1019],
            factor: 12,
            delay: 4,
        },
        _ => return None,
    };
    Some(filter)
}

/// 解析用の 4 kHz 格子に換算する倍率を返す (対応外は None)
///
/// 44.1 kHz は非整数の倍率 (11.025) を返し、[`AnalysisSignal`] が解析用の信号だけを
/// 48 kHz の格子へ移す。それ以外のレートは 4 kHz の倍数へ丸めた格子を使う (moqt-js の
/// `Math.round(sampleRate / 4_000)` と同じ)。
fn analysis_grid(sample_rate: u32) -> Option<f64> {
    if sample_rate == 44_100 {
        return Some(11.025);
    }
    let rounded = f64::from(sample_rate) / 4_000.0;
    // 0 以上の値では 0.5 を足して切り捨てるのが四捨五入と同じになる (no_std に round が無い)
    let grid = (rounded + 0.5) as usize;
    if grid == 0 {
        return None;
    }
    Some(grid as f64)
}

/// 時間圧縮と時間伸長、補間を使えるサンプルレートか
///
/// ピッチの周期は 4 kHz へ間引いた信号の自己相関から求めるため、間引きのフィルタの表
/// (8 kHz / 16 kHz / 32 kHz / 48 kHz) に 4 kHz の倍数へ丸めたレートが載っている必要がある。
/// 44.1 kHz は解析用の信号だけを 48 kHz へ換算するため使える。それ以外のレートでは
/// 操作しないため、先にこの関数で確かめて計器へ出す。
///
/// 判定は moqt-js と同じく、4 kHz の倍数へ丸めてから表を引く。そのため 47.999 kHz のような
/// 表に無いレートも 48 kHz として扱う (解析は 48 kHz のフィルタで行い、求めた周期は元の
/// サンプルレートのサンプル数へ戻す)。
pub fn is_time_stretch_available(sample_rate: u32) -> bool {
    if sample_rate == 0 {
        return false;
    }
    if sample_rate == 44_100 {
        return true;
    }
    let Some(grid) = analysis_grid(sample_rate) else {
        return false;
    };
    downsample_filter_for_grid(grid as usize).is_some()
}

/// 解析用に 4 kHz へ間引いた信号 (必要になった位置だけを計算する)
struct AnalysisSignal<'a> {
    /// 元のサンプルレートの音声 (第 1 チャンネル)
    reference: &'a [f32],
    /// 4 kHz の格子に対応する間引きフィルタ
    filter: DownsampleFilter,
    /// 4 kHz の 1 サンプルに対応する入力のサンプル数
    samples_per_4khz_sample: f64,
    /// 44.1 kHz の入力を解析用に 48 kHz の格子へ移すか
    resample_44100: bool,
}

impl<'a> AnalysisSignal<'a> {
    /// 解析用の信号を作る (対応外のサンプルレートでは None)
    fn new(reference: &'a [f32], sample_rate: u32) -> Option<Self> {
        let samples_per_4khz_sample = analysis_grid(sample_rate)?;
        // 44.1 kHz は解析用の信号だけを 48 kHz の格子へ移してから、48 kHz のフィルタを通す
        let filter = if sample_rate == 44_100 {
            downsample_filter_for_grid(12)?
        } else {
            downsample_filter_for_grid(samples_per_4khz_sample as usize)?
        };
        Some(Self {
            reference,
            filter,
            samples_per_4khz_sample,
            resample_44100: sample_rate == 44_100,
        })
    }

    /// 間引きの開始位置 (入力のサンプル数)
    fn start(&self) -> usize {
        self.filter.coefficients.len() - 1 + self.filter.delay
    }

    /// フィルタへ入れる信号の長さ
    ///
    /// 44.1 kHz は解析用に 48 kHz の格子へ移すため、移した後の長さになる。
    fn input_len(&self) -> usize {
        if self.resample_44100 {
            (self.reference.len() * 48_000) / 44_100
        } else {
            self.reference.len()
        }
    }

    /// フィルタへ入れる信号の `index` 番目のサンプル
    fn input_at(&self, index: usize) -> f32 {
        if self.resample_44100 {
            // 解析用の信号だけを 48 kHz の格子へ移す。再生する音のサンプルは変更しない
            sample_at(self.reference, index as f64 * 44_100.0 / 48_000.0)
        } else {
            self.reference.get(index).copied().unwrap_or(0.0)
        }
    }

    /// フィルタ係数の和 (正規化に使う)
    fn coefficient_sum(&self) -> f64 {
        self.filter
            .coefficients
            .iter()
            .map(|coefficient| f64::from(*coefficient))
            .sum()
    }

    /// 間引いた信号のサンプル数
    fn len(&self) -> usize {
        let input_len = self.input_len();
        let start = self.start();
        if input_len <= start {
            0
        } else {
            (input_len - start) / self.filter.factor
        }
    }

    /// 間引いた信号の `index` 番目のサンプル
    ///
    /// `index` は [`AnalysisSignal::len`] 未満であること。
    fn at(&self, index: usize) -> f32 {
        // NetEq と同じく、フィルタ長 - 1 + 遅延の分だけ後ろから始める
        let start = self.start();
        let mut sum = 0.0f64;
        for (tap, coefficient) in self.filter.coefficients.iter().enumerate() {
            let position = start + index * self.filter.factor - tap;
            sum += f64::from(*coefficient) * f64::from(self.input_at(position));
        }
        (sum / self.coefficient_sum()) as f32
    }
}

/// 0 以上の平方根 (no_std には `f64::sqrt` が無いため Newton 法で求める)
///
/// 正規化相関と線形予測の残差の大きさに使う。NaN と負の値は 0 にする。初期値を指数部の
/// 半分で作ったあと、値が変わらなくなるまで収束させる (通常は 5 回前後で収束する)。
pub(crate) fn sqrt(value: f64) -> f64 {
    if value.is_nan() || value <= 0.0 {
        // NaN と 0、負の値は 0 を返す (平方根は実数に無い)
        return 0.0;
    }
    // 極小の値はそのままでは初期値が作れないため、大きな値へ移してから戻す
    let (scaled, scale) = if value < 1e-300 {
        (value * 1e300, 1e-150)
    } else {
        (value, 1.0)
    };
    // 指数部を半分にした近似 (相対誤差は 3.5 % 程度) を初期値にする
    let mut guess = f64::from_bits((scaled.to_bits() >> 1) + 0x1ff8_0000_0000_0000);
    for _ in 0..64 {
        let next = 0.5 * (guess + scaled / guess);
        if next == guess {
            break;
        }
        guess = next;
    }
    guess * scale
}

/// 0 以上の値の小数点以下を四捨五入する (moqt-js の `Math.round` と同じ)
///
/// 0.5 を足してから切り捨てる。0 以上の値では切り捨てが `Math.floor` と同じになるため、
/// 0.5 ちょうどは切り上げられる。
pub(crate) fn round_positive(value: f64) -> usize {
    if value.is_nan() || value < 0.0 {
        return 0;
    }
    (value + 0.5) as usize
}

/// 0 以上の値の小数点以下を切り上げて整数にする
fn ceil_positive(value: f64) -> usize {
    if value.is_nan() || value <= 0.0 {
        return 0;
    }
    let truncated = value as usize;
    if value > truncated as f64 {
        truncated + 1
    } else {
        truncated
    }
}

/// 小数点以下を切り捨てる (負の値でも小さい方へ丸める)
fn floor_to_i64(value: f64) -> i64 {
    if value.is_nan() {
        return 0;
    }
    let truncated = value as i64;
    if (truncated as f64) > value {
        truncated - 1
    } else {
        truncated
    }
}

/// 全チャンネルの振幅のピークが無音とみなす値より小さいか
fn is_silent<'a>(channels: impl Iterator<Item = &'a [f32]>) -> bool {
    let mut peak = 0.0f32;
    for channel in channels {
        for sample in channel {
            peak = peak.max(sample.abs());
        }
    }
    f64::from(peak) < f64::from(TIME_STRETCH_SILENCE_PEAK)
}

/// 端数の位置のサンプルを線形補間で読む
///
/// 周期は 4 kHz の格子 (48 kHz で 12 サンプル) に乗らない端数になるため、繰り返しの生成や
/// 継ぎ目の評価では 2 つのサンプルの間を読む。範囲の外は最後のサンプル (先頭より前は 0) を
/// 返す。
fn sample_at(signal: &[f32], position: f64) -> f32 {
    // 0 以上の値では整数への切り捨てが小数点以下の切り捨て (floor) と同じになる
    let index = position as usize;
    let fraction = position - index as f64;
    let before = signal.get(index).copied().unwrap_or(0.0);
    let after = index
        .checked_add(1)
        .and_then(|next| signal.get(next).copied())
        .unwrap_or(before);
    (f64::from(before) + (f64::from(after) - f64::from(before)) * fraction) as f32
}

/// 末尾の周期 1 つ分を繰り返して読む
///
/// 端数の位置は線形補間で読む。参照の末尾をまたぐときは、1 周期前 (周期の先頭) を続きとして
/// 読む。周期が繰り返しているため、参照の末尾の次のサンプルは周期の先頭になる。
fn read_repeated(signal: &[f32], start: f64, phase: f64) -> f32 {
    let position = start + phase;
    // 0 以上の値では整数への切り捨てが小数点以下の切り捨て (floor) と同じになる
    let index = position as usize;
    let fraction = position - index as f64;
    let before = signal.get(index).copied().unwrap_or(0.0);
    let after = match index
        .checked_add(1)
        .and_then(|next| signal.get(next).copied())
    {
        Some(sample) => sample,
        // 末尾をまたぐときは周期の先頭を続きとして読む
        None => sample_at(signal, start),
    };
    (f64::from(before) + (f64::from(after) - f64::from(before)) * fraction) as f32
}

/// 2 つの区間の正規化した相関 (-1 から 1) を、元のサンプルレートの信号から求める
///
/// `end` を末尾とする長さ `window` の区間と、そこから `lag` だけ前の同じ長さの区間を比べる。
/// `lag + window <= end <= signal.len()` であること。
fn full_rate_normalized_correlation(signal: &[f32], end: usize, window: usize, lag: usize) -> f64 {
    let mut dot = 0.0f64;
    let mut energy_before = 0.0f64;
    let mut energy_after = 0.0f64;
    for (before, after) in signal[end - lag - window..end - lag]
        .iter()
        .zip(&signal[end - window..end])
    {
        let before = f64::from(*before);
        let after = f64::from(*after);
        dot += before * after;
        energy_before += before * before;
        energy_after += after * after;
    }
    let denominator = sqrt(energy_before * energy_after);
    if denominator == 0.0 {
        0.0
    } else {
        dot / denominator
    }
}

/// 4 kHz へ間引いた信号の、2 つの区間の正規化した相関を求める
///
/// `end` を末尾とする長さ `window` の区間と、そこから `lag` だけ前の同じ長さの区間を比べる。
fn downsampled_normalized_correlation(
    analysis: &AnalysisSignal,
    end: usize,
    window: usize,
    lag: usize,
) -> f64 {
    let mut dot = 0.0f64;
    let mut energy_before = 0.0f64;
    let mut energy_after = 0.0f64;
    for index in 0..window {
        let before = f64::from(analysis.at(end - lag - window + index));
        let after = f64::from(analysis.at(end - window + index));
        dot += before * after;
        energy_before += before * before;
        energy_after += after * after;
    }
    let denominator = sqrt(energy_before * energy_after);
    if denominator == 0.0 {
        0.0
    } else {
        dot / denominator
    }
}

/// 4 kHz へ間引いた信号の、ずらし幅 `lag` の生の相関 (正規化しない内積)
///
/// 窓長は [`TIME_STRETCH_CORRELATION_LEN`] で固定する。窓長をずらし幅に比例させると、
/// 窓が短い (= ずらし幅が短い) 候補ほど相関が高く出て、有色雑音で最短のずらし幅を選ぶ。
/// 粗い探索も放物線補間も同じ固定長の窓を使うため、3 点の相関を同じ前提で比べられる。
fn lag_correlation(downsampled: &[f32], lag: usize) -> f64 {
    let mut correlation = 0.0f64;
    for index in 0..TIME_STRETCH_CORRELATION_LEN {
        correlation += f64::from(downsampled[index]) * f64::from(downsampled[index + lag]);
    }
    correlation
}

/// 窓のエネルギー (2 乗和)
///
/// 生の相関は正規化していないため、値だけでは完全相関かどうか分からない。ピークの相関を
/// 前後の窓のエネルギーと比べて正規化し、完全相関の判定に使う。
fn window_energy(downsampled: &[f32], offset: usize) -> f64 {
    let mut energy = 0.0f64;
    for index in 0..TIME_STRETCH_CORRELATION_LEN {
        let value = f64::from(downsampled[offset + index]);
        energy += value * value;
    }
    energy
}

/// 4 kHz の粗い探索の結果
struct CoarseLag {
    /// 相関が最も強いずらし幅 (4 kHz のサンプル数)
    lag: usize,
    /// そのずらし幅の生の相関 (正規化しない内積)
    correlation: f64,
}

/// ピッチの周期 (4 kHz のサンプル数) を粗く探す
///
/// 自分自身との相関が最も強いずらし幅を [`TIME_STRETCH_MIN_LAG`] から `max_lag` までで探す。
/// ずらし幅 0 は必ず最大になるため探索に入れない (NetEq も下限 `kMinLag` から探す)。
fn find_lag(downsampled: &[f32], max_lag: usize) -> CoarseLag {
    let mut coarse = CoarseLag {
        lag: TIME_STRETCH_MIN_LAG,
        correlation: f64::NEG_INFINITY,
    };
    for lag in TIME_STRETCH_MIN_LAG..=max_lag {
        let correlation = lag_correlation(downsampled, lag);
        if correlation > coarse.correlation {
            coarse.correlation = correlation;
            coarse.lag = lag;
        }
    }
    coarse
}

/// 4 kHz の粗いずらし幅を、ピークの前後 1 格子の相関による放物線補間で端数まで求める
///
/// 4 kHz の格子では 48 kHz の周期が 12 サンプル単位に量子化されるため、格子に乗らない
/// ピッチの音では位相のずれた 2 区間をクロスフェードすることになり、非調和成分と 50 Hz の
/// 側波帯 (声では粗さ) が増える。補間側の [`find_tail_period`] が元のサンプルレートで端数まで
/// 周期を求めているのに対し、圧縮側は整数ずらし幅のままで非対称だった。
///
/// 放物線には、粗い探索と同じ固定長の窓の生の相関を使う。窓長が固定なので 3 点は同じ前提で
/// 比べられ、上に凸なピークなら頂点が端数のずらし幅になる。次のときは端数にせず整数の
/// ずらし幅を返す (補間しない)。
///
/// - 前後 1 格子を評価できない (探索の上限に高峰が張り付き、窓が信号の外へ出る) とき。
///   外挿すると評価していないずらし幅へ頂点が飛ぶため、[`find_tail_period`] と同じく格子の
///   ままにする
/// - 上に凸でない (曲率が 0 以上) とき。頂点を求められない
/// - 丸め誤差の範囲で完全相関のとき。格子に乗る周期まで放物線の頂点へずらすと、有限窓の
///   前後の相関の非対称で正しい整数周期の位相を壊す ([`find_tail_period`] と同じガード)
fn refine_lag(downsampled: &[f32], coarse: &CoarseLag) -> f64 {
    let lag = coarse.lag;
    // 前後 1 格子のうち、後ろ側が窓ごと信号に収まるか。前側は lag >= TIME_STRETCH_MIN_LAG
    // なので必ず評価できる (ずらし幅 0 は相関 1 になるため評価しない)
    if lag + 1 + TIME_STRETCH_CORRELATION_LEN > downsampled.len() {
        return lag as f64;
    }
    // 完全相関なら整数のずらし幅を保つ。生の相関は正規化していないため、窓のエネルギーで
    // 割って正規化した値で判定する (厳密な整数周期では窓のエネルギーが等しく、値は 1 になる)
    let base_energy = window_energy(downsampled, 0);
    let peak_energy = window_energy(downsampled, lag);
    let denominator = sqrt(base_energy * peak_energy);
    let exact_period = denominator > 0.0 && coarse.correlation >= denominator * (1.0 - 1e-12);
    if exact_period {
        return lag as f64;
    }
    let before = lag_correlation(downsampled, lag - 1);
    let after = lag_correlation(downsampled, lag + 1);
    // f(-1), f(0), f(+1) を通る放物線の頂点の位置 (ずらし幅の端数)
    let curvature = before - 2.0 * coarse.correlation + after;
    if curvature.is_nan() || curvature >= 0.0 {
        // 上に凸でない (曲率が 0 以上、または NaN) ため頂点を求められない
        return lag as f64;
    }
    let offset = (0.5 * (before - after)) / curvature;
    // 隣接点より外側に頂点を作らない (外挿しない)
    lag as f64 + offset.clamp(-0.5, 0.5)
}

/// ピッチの周期の探索結果
struct PitchPeriod {
    /// ピッチの周期 (元のサンプルレートのサンプル数)
    samples: usize,
    /// 切れ目の前後の正規化相関 (-1 から 1)
    correlation: f64,
}

/// 第 1 チャンネルの波形からピッチの周期と、切れ目の前後での一致の度合いを求める
///
/// 周期は 4 kHz の粗い探索のあと、ピークの前後 1 格子の相関による放物線補間で端数まで
/// 求める ([`refine_lag`])。端数の周期は元のサンプルレートへ戻し、整数サンプルへ丸めた値を
/// 削る (伸ばす) 長さにする。さらに、粗い探索の上端や振幅が変わる音でも 4 kHz の格子を
/// そのまま切断位置にしないよう、元のサンプルレートで ±1 格子を評価し、整数周期で重なる
/// 全区間の正規化相関が最も高い候補を選ぶ。
///
/// 切れ目の位置 `splice` の前後で波形が繰り返しているかを調べ、周期が切れ目に収まらない
/// ときは None を返す。
fn analyze(reference: &[f32], sample_rate: u32, splice: usize) -> Option<PitchPeriod> {
    let analysis = AnalysisSignal::new(reference, sample_rate)?;
    let count = analysis.len();
    if count < TIME_STRETCH_CORRELATION_LEN + TIME_STRETCH_MIN_LAG + 1 {
        return None;
    }
    // 自己相関の窓とピッチの周期が 4 kHz の信号に収まる範囲で探す
    let max_lag = TIME_STRETCH_MAX_LAG.min(count - TIME_STRETCH_CORRELATION_LEN);
    if max_lag < TIME_STRETCH_MIN_LAG {
        return None;
    }
    // 粗い探索と放物線補間に使う間引き信号を先頭から作る。前後 1 格子の相関まで評価できる
    // よう、最大ずらし幅の 1 サンプル先まで用意する
    let mut downsampled = [0.0f32; TIME_STRETCH_CORRELATION_LEN + TIME_STRETCH_MAX_LAG + 1];
    let scratch_len = (TIME_STRETCH_CORRELATION_LEN + max_lag + 1).min(count);
    for (index, sample) in downsampled.iter_mut().take(scratch_len).enumerate() {
        *sample = analysis.at(index);
    }
    let coarse = find_lag(&downsampled[..scratch_len], max_lag);
    // 4 kHz の格子に乗らない周期を端数まで求める。44.1 kHz はこのずらし幅に 11.025 を
    // 掛けると元のサンプルレートのサブ格子の周期になる
    let lag = refine_lag(&downsampled[..scratch_len], &coarse);
    // ピッチの周期を元のサンプルレートへ戻す
    let grid = analysis.samples_per_4khz_sample;
    let mut samples = round_positive(lag * grid);
    // 粗い探索の上端や振幅が変わる音でも、4 kHz の格子をそのまま切断位置にしない。
    // 元のレートで ±1 格子を評価する。候補どうしは同じ長さの窓で正規化して比較する。
    // 圧縮は整数サンプルを削るため、整数周期で重なる全区間の相関を直接最大化する
    if !is_silent(core::iter::once(reference)) {
        let first = round_positive(TIME_STRETCH_MIN_LAG as f64 * grid)
            .max(floor_to_i64(samples as f64 - grid).max(0) as usize);
        let last = splice.min(ceil_positive(samples as f64 + grid));
        let window = reference.len() - last;
        let mut best_correlation = f64::NEG_INFINITY;
        for candidate in first..=last {
            let correlation =
                full_rate_normalized_correlation(reference, reference.len(), window, candidate);
            if correlation > best_correlation {
                best_correlation = correlation;
                samples = candidate;
            }
        }
    }
    if samples == 0 || samples > splice || splice + samples > reference.len() {
        // 切れ目の前後が音に収まらない (音が短すぎる)
        return None;
    }
    // 切れ目の前後で波形がどれだけ一致しているか
    let correlation =
        full_rate_normalized_correlation(reference, splice + samples, samples, samples);
    Some(PitchPeriod {
        samples,
        correlation,
    })
}

/// 操作するピッチの周期を求める (操作しないときは None)
///
/// 波形が繰り返していない場所で切ると耳につくため、相関が足りないときは操作しない。
/// 無音は詰めても伸ばしても聞こえないため、そのまま操作する。
///
/// 切れ目の短い 1 周期だけでは、低い声の倍音を基本周期と誤認することがある。残りの波形全体
/// でも一致することを確かめる。周期を選ぶ第 1 チャンネルだけでなく、全チャンネルへ同じ
/// 切断位置を適用できる場合だけ操作する。
fn pitch_period_of<T: AsRef<[f32]>>(
    channels: &[T],
    sample_rate: u32,
    splice: usize,
) -> Option<PitchPeriod> {
    let reference = channels.first()?.as_ref();
    let pitch = analyze(reference, sample_rate, splice)?;
    if pitch.correlation < TIME_STRETCH_CORRELATION_THRESHOLD
        && !is_silent(channels.iter().map(|channel| channel.as_ref()))
    {
        return None;
    }
    for channel in channels {
        let channel = channel.as_ref();
        if is_silent(core::iter::once(channel)) {
            continue;
        }
        let correlation = full_rate_normalized_correlation(
            channel,
            channel.len(),
            channel.len() - pitch.samples,
            pitch.samples,
        );
        if correlation < TIME_STRETCH_CORRELATION_THRESHOLD {
            return None;
        }
    }
    Some(pitch)
}

/// 切れ目の前後をクロスフェードする (NetEq の `AudioVector::CrossFade` と同じ重み)
///
/// `target` の先頭 `fade` サンプルを、`source` の先頭 `fade` サンプルへ少しずつ寄せる。
/// 重みは NetEq と同じで 1 サンプルずつ `1 / (fade + 1)` ずつ動かす (最初のサンプルで既に
/// `1 / (fade + 1)` だけ `source` へ寄り、最後はほぼ `source` になる)。
fn cross_fade(target: &mut [f32], source: &[f32], fade: usize) {
    cross_fade_with_offset(target, source, 0, fade);
}

/// [`cross_fade`] の読み出し位置を `source_offset` からにする
///
/// 読み出し位置が負のときは 0 として扱う (moqt-js が範囲外の読み出しを undefined として
/// 0 にするのと同じ)。
fn cross_fade_with_offset(target: &mut [f32], source: &[f32], source_offset: isize, fade: usize) {
    let step = 1.0 / (fade as f64 + 1.0);
    for (index, target_sample) in target.iter_mut().take(fade).enumerate() {
        let weight = (index as f64 + 1.0) * step;
        let source_index = source_offset + index as isize;
        let source_sample = if source_index < 0 {
            0.0
        } else {
            f64::from(source.get(source_index as usize).copied().unwrap_or(0.0))
        };
        let mixed = (1.0 - weight) * f64::from(*target_sample) + weight * source_sample;
        *target_sample = mixed as f32;
    }
}

/// クロスフェードする 2 区間の一致度と、混ぜることで失われるエネルギー
///
/// `start` は負でもよい (範囲外は 0 として読む)。moqt-js は負の添字を undefined として
/// 0 にするため、同じ扱いにする。
fn compression_splice_score(
    channel: &[f32],
    start: isize,
    period: usize,
) -> CompressionSpliceScore {
    let mut loss = 0.0f64;
    let mut dot = 0.0f64;
    let mut before_energy = 0.0f64;
    let mut after_energy = 0.0f64;
    let mut peak = 0.0f32;
    for index in 0..period {
        let before_index = start + index as isize;
        let after_index = before_index + period as isize;
        let before = if before_index < 0 {
            0.0f32
        } else {
            channel.get(before_index as usize).copied().unwrap_or(0.0)
        };
        let after = if after_index < 0 {
            0.0f32
        } else {
            channel.get(after_index as usize).copied().unwrap_or(0.0)
        };
        let weight = (index as f64 + 1.0) / (period as f64 + 1.0);
        let difference = f64::from(before) - f64::from(after);
        // 線形混合のエネルギーは、元の重み付きエネルギーより w(1-w)(a-b)^2 だけ小さい。
        // 絶対量を比べることで、声の強まりを潰す位置より一致した弱い音の位置を優先する
        loss += weight * (1.0 - weight) * difference * difference;
        dot += f64::from(before) * f64::from(after);
        before_energy += f64::from(before) * f64::from(before);
        after_energy += f64::from(after) * f64::from(after);
        peak = peak.max(before.abs()).max(after.abs());
    }
    // dot >= 閾値 * sqrt(before_energy * after_energy) を平方根を取らずに判定する
    let correlated = dot > 0.0
        && dot * dot
            >= TIME_STRETCH_CORRELATION_THRESHOLD
                * TIME_STRETCH_CORRELATION_THRESHOLD
                * before_energy
                * after_energy;
    CompressionSpliceScore {
        loss,
        correlated,
        silent: f64::from(peak) < f64::from(TIME_STRETCH_SILENCE_PEAK),
    }
}

/// クロスフェードする 2 区間の評価結果
struct CompressionSpliceScore {
    /// 混ぜることで失われるエネルギー
    loss: f64,
    /// 2 区間の正規化相関が閾値以上か
    correlated: bool,
    /// 2 区間が無音に近いか
    silent: bool,
}

/// 削る時間を保ち、中央より波形を打ち消さない切断位置を選ぶ
///
/// 中央で検証した周期をそのまま使うと、声の強まりを前半の弱い音と混ぜて潰すことがある。
/// 周期 1 つ分を削る位置を、クロスフェードで波形を打ち消す量が中央より小さい位置へ移す。
/// 追加の入力は待たない。全チャンネルで悪化せず、その位置でも相関が十分な場合だけ移す。
/// 中央と左右 4 点ずつに制限し、最大 9 周期分の比較で済ませる (長いフレームで処理量が
/// 増えないようにする)。同点や丸め誤差程度の差では中央を保つ。
fn compression_splice<T: AsRef<[f32]>>(channels: &[T], period: usize, center: usize) -> usize {
    let Some(first_channel) = channels.first() else {
        return center;
    };
    let length = first_channel.as_ref().len();
    // 先頭と末尾は隣接フレームへ繋がるため、移動先ではどちらも原音を 1 サンプル以上残す
    let first = 1isize;
    let last = length as isize - 2 * period as isize - 1;
    let original_start = center as isize - period as isize;
    if first > original_start || last < original_start {
        return center;
    }
    let baseline: Vec<CompressionSpliceScore> = channels
        .iter()
        .map(|channel| compression_splice_score(channel.as_ref(), original_start, period))
        .collect();
    let mut best_loss = baseline.iter().map(|score| score.loss).sum::<f64>();
    let mut best_start = original_start;
    for step in 1..=4isize {
        // moqt-js と同じく、移動先は中央から左右へ 4 等分した位置を四捨五入して求める
        let left = original_start as f64 - ((original_start - first) as f64 * step as f64) / 4.0;
        let right = original_start as f64 + ((last - original_start) as f64 * step as f64) / 4.0;
        for candidate in [left, right] {
            let start = round_positive(candidate) as isize;
            if start == original_start {
                continue;
            }
            let mut loss = 0.0f64;
            let mut eligible = true;
            for (index, channel) in channels.iter().enumerate() {
                let score = compression_splice_score(channel.as_ref(), start, period);
                // 大きいチャンネルの改善で、静かなチャンネルの劣化を隠さない。
                // 全体の周期が正しくても、移動先の短い区間が逆相なら混ぜない
                if score.loss > baseline[index].loss || (!score.silent && !score.correlated) {
                    eligible = false;
                    break;
                }
                loss += score.loss;
            }
            if eligible && best_loss - loss > (1e-18f64).max(best_loss * 1e-6) {
                best_loss = loss;
                best_start = start;
            }
        }
    }
    (best_start + period as isize) as usize
}

/// 音声を時間圧縮する (ピッチ周期 1 つ分を削る)
///
/// `channels` はチャンネルごとの音声で、すべて同じ長さであること。サンプルはおおむね
/// `[-1.0, 1.0]` に正規化された有限値であること。圧縮した結果は同じバッファへ書き、
/// 使用する範囲は元の長さに戻り値を足した長さまでになる。
///
/// 削る長さはピッチの周期で決まり、要求では選べない (周期は 2.5 ms から 15 ms)。周期が
/// `max_length_change_samples` を超えるときは操作しない。遅れより長い周期を削ると、削り
/// すぎた分は「早すぎる」側へ振れ、次の音でまた遅れて詰める往復になる。往復のたびに周期が
/// 1 つ消えるため、声では粗さ・ざらつきとして聞こえる。
///
/// 削る位置は音の中央で検証したあと、クロスフェードで波形を打ち消さない位置を探す
/// (`compression_splice`)。削る長さは変えない。
///
/// [`compress`] と違い、直前の実音を解析の履歴として渡せる。5 / 10 ms の音は 4 kHz へ
/// 間引くと相関窓 (61 サンプル、15.25 ms) に足りず、周期を求められない。履歴と連結した
/// 30 ms の窓で周期を探し、現在の音の切断位置でも周期が続いているときだけ使う。
///
/// # 戻り値
///
/// 長さの変化 (サンプル数)。負が圧縮、0 が操作なし。0 のときバッファは変更しない。
/// 0 になるのは、対応外のサンプルレート、空または長さの揃わないチャンネル、音が
/// 短すぎるとき、相関が閾値未満で無音でもないとき、周期が `max_length_change_samples` を
/// 超えるとき。
pub fn compress(
    channels: &mut [&mut [f32]],
    sample_rate: u32,
    max_length_change_samples: usize,
) -> isize {
    compress_with_history(channels, sample_rate, max_length_change_samples, &[])
}

/// 直前の実音を解析の履歴に使って、音声を時間圧縮する
///
/// `history` は `channels` と同じチャンネル数で、各チャンネルの長さが揃っていること。
/// 各チャンネルは短くてもよく、末尾の最大 30 ms だけを使う。音が 20 ms 未満のときに
/// 周期を求められなかった場合だけ連結して探す。20 ms 以上では従来どおり現在の音だけで
/// 求める (履歴との連結で、直前の音の周期を現在の音へ押し付けないため)。
///
/// それ以外の契約と戻り値は [`compress`] と同じである。
pub fn compress_with_history(
    channels: &mut [&mut [f32]],
    sample_rate: u32,
    max_length_change_samples: usize,
    history: &[Vec<f32>],
) -> isize {
    if channels.is_empty() {
        return 0;
    }
    let length = channels[0].len();
    if length == 0 || channels.iter().any(|channel| channel.len() != length) {
        return 0;
    }
    // 圧縮の可否と削る周期は中央で決める。位置の選択で遅延制御を変えない
    let center = length >> 1;
    let mut pitch = pitch_period_of(channels, sample_rate, center);
    // 5 / 10 ms の音は現在の音だけで相関窓を満たさない。履歴と連結して周期を探し、
    // 現在の切断位置でもその周期が続いているときだけ使う
    if pitch.is_none() && length < short_frame_samples(sample_rate) {
        pitch = short_frame_pitch(channels, history, sample_rate, center);
    }
    let Some(pitch) = pitch else {
        return 0;
    };
    if pitch.samples > max_length_change_samples {
        // 遅れより長い周期しか無い。削ると行き過ぎるため、この音では詰めない
        return 0;
    }
    let removed = pitch.samples;
    let splice = compression_splice(channels, removed, center);
    for channel in channels.iter_mut() {
        let channel: &mut [f32] = channel;
        // 切れ目の前後は同じバッファの隣り合う領域であるため、分割して借用する
        let (head, tail) = channel.split_at_mut(splice);
        cross_fade(&mut head[splice - removed..], &tail[..removed], removed);
        // 削った分だけ後ろを詰める
        channel.copy_within(splice + removed.., splice);
    }
    -(removed as isize)
}

/// 短いフレームで、履歴と連結した窓から圧縮の周期を求める
///
/// 連結した窓で求めた周期が、現在の音の切断位置でも続いていることを確かめる。履歴が
/// 強い周期を持っていても、現在の音がその周期で繰り返していなければ削ると継ぎ目が
/// 崩れるため、現在の音の相関と切断位置の一致の両方を見る。
fn short_frame_pitch(
    channels: &[&mut [f32]],
    history: &[Vec<f32>],
    sample_rate: u32,
    center: usize,
) -> Option<PitchPeriod> {
    let combined = with_history(
        channels,
        history,
        round_positive(f64::from(sample_rate) * 0.03),
    )?;
    let pitch = pitch_period_of(&combined, sample_rate, combined.first()?.len() >> 1)?;
    let period = pitch.samples;
    if period == 0 || period > center {
        return None;
    }
    let length = channels.first()?.len();
    for channel in channels {
        let channel: &[f32] = channel;
        let score = compression_splice_score(channel, center as isize - period as isize, period);
        if score.silent {
            // 無音は削っても聞こえないため、そのまま操作してよい
            continue;
        }
        if !score.correlated
            || full_rate_normalized_correlation(channel, length, length - period, period)
                < TIME_STRETCH_CORRELATION_THRESHOLD
        {
            return None;
        }
    }
    Some(pitch)
}

/// 短期間の履歴と今回の音を、解析用の連続したサンプル列にする
///
/// `limit` は履歴として使う最大のサンプル数である。履歴のチャンネル数か長さが今回の音と
/// 合わないときは None を返す (別の形式の音を連結しない)。
fn with_history<T: AsRef<[f32]>>(
    channels: &[T],
    history: &[Vec<f32>],
    limit: usize,
) -> Option<Vec<Vec<f32>>> {
    if channels.is_empty() || history.len() != channels.len() {
        return None;
    }
    let history_len = history.first()?.len();
    if history.iter().any(|channel| channel.len() != history_len) {
        return None;
    }
    let old_length = history_len.min(limit);
    if old_length == 0 {
        return None;
    }
    Some(
        channels
            .iter()
            .zip(history)
            .map(|(channel, previous)| {
                let mut combined = Vec::new();
                combined.extend_from_slice(&previous[previous.len() - old_length..]);
                combined.extend_from_slice(channel.as_ref());
                combined
            })
            .collect(),
    )
}

/// 短いフレームとみなす長さ (サンプル数)
///
/// 20 ms 未満では 4 kHz の相関窓 (61 サンプル、15.25 ms) を現在の音だけで満たせない。
/// この長さ未満のときだけ、履歴を連結した窓で周期を探す。
fn short_frame_samples(sample_rate: u32) -> usize {
    round_positive(f64::from(sample_rate) * 0.02)
}

/// 予備的な時間伸長の結果
///
/// 再同期で浅くなった到着基準の深さを、音を止めずに 1 周期ずつ戻した結果である。操作しな
/// かったときは [`channels`](Self::channels) が空で [`added_samples`](Self::added_samples) が
/// 0 になる。
#[derive(Debug, Clone, PartialEq)]
pub struct PreemptiveExpansion {
    /// 伸長した音声 (チャンネルごと)。長さは今回の音の長さ + 伸ばした長さ
    pub channels: Vec<Vec<f32>>,
    /// 伸ばした長さ (サンプル数)。0 のときは操作なし
    pub added_samples: usize,
}

/// 再同期で浅くなった到着基準の深さを、音を止めずに 1 周期ずつ戻す
///
/// NetEq `PreemptiveExpand::CheckCriteriaAndStretch` の overlap-add を採用する。根拠は
/// WebRTC の `modules/audio_coding/neteq/preemptive_expand.cc` である。上流全体の移植では
/// なく、浮動小数点 PCM と既存の周期判定を使う (上流の方式は将来変わり得る)。20 ms 以上は
/// 直前の実音 10 ms と今回の実音を重ね、短いフレームは最大 30 ms の履歴で周期を推定して
/// 未再生の今回の先頭で伸ばす。追加の入力やタイマーは待たない。
///
/// 手順は次のとおりである。
///
/// - 無音なら、今回の音の最後のサンプルで要求された長さ (今回の音の長さまで) を埋める
/// - 今回の音の前に直前の実音を置く。重ね合わせの位置は、20 ms 以上の音では 15 ms、
///   20 ms 未満の音では履歴の先頭である
/// - その位置の前後 1 周期をクロスフェードして 1 周期を足す
/// - 増やす長さは `max_length_change_samples` を超えない。周期がこれを超えるときは操作しない
/// - 全チャンネルで、その周期の重なりが十分に相関しているときだけ操作する
///
/// `channels` と `history` はチャンネル数が同じで、各チャンネルの長さが揃っていること。
/// `history` の各チャンネルは短くてもよい (直前の実音の末尾だけを使う)。
/// `max_length_change_samples` は有限の値を渡すこと。moqt-js は無限大を渡すと操作しないため、
/// 呼び出し側は伸ばしたい上限 (サンプル数) を必ず決めて渡す。
///
/// # 戻り値
///
/// 操作したときは伸長した音声と、伸ばした長さ (1 以上)。操作しなかったときは
/// [`channels`](PreemptiveExpansion::channels) が空で [`added_samples`](
/// PreemptiveExpansion::added_samples) が 0 の結果を返す。伸ばした長さは常に
/// `max_length_change_samples` 以下になる。
pub fn preemptively_expand(
    channels: &[Vec<f32>],
    history: &[Vec<f32>],
    sample_rate: u32,
    max_length_change_samples: usize,
) -> PreemptiveExpansion {
    let unchanged = PreemptiveExpansion {
        channels: Vec::new(),
        added_samples: 0,
    };
    let Some(first_channel) = channels.first() else {
        return unchanged;
    };
    let length = first_channel.len();
    if length == 0
        || max_length_change_samples == 0
        || !is_time_stretch_available(sample_rate)
        || channels.iter().any(|channel| channel.len() != length)
    {
        return unchanged;
    }
    if is_silent(channels.iter().map(|channel| channel.as_slice())) {
        let added = length.min(max_length_change_samples);
        return PreemptiveExpansion {
            channels: channels
                .iter()
                .map(|channel| {
                    let last = channel.last().copied().unwrap_or(0.0);
                    let mut expanded = channel.clone();
                    expanded.resize(length + added, last);
                    expanded
                })
                .collect(),
            added_samples: added,
        };
    }
    if history.len() != channels.len() {
        return unchanged;
    }
    let history_len = history[0].len();
    if history.iter().any(|channel| channel.len() != history_len) {
        return unchanged;
    }
    // 20 ms 未満の短いフレームは、重ね合わせの位置を履歴の先頭に置き、履歴を最大 30 ms
    // 使って周期を探す。20 ms 以上は直前の実音を最大 10 ms、重ね合わせの位置を 15 ms に
    // する (NetEq の PreemptiveExpand と同じ)
    let short = length < short_frame_samples(sample_rate);
    let old_length = history_len.min(round_positive(
        f64::from(sample_rate) * if short { 0.03 } else { 0.01 },
    ));
    let splice = if short {
        old_length
    } else {
        round_positive(f64::from(sample_rate) * 0.015)
    };
    if old_length + length < round_positive(f64::from(sample_rate) * 0.03) || splice == 0 {
        return unchanged;
    }
    // 直前の実音の末尾と今回の実音を並べたものから周期を探す
    let Some(combined) = with_history(channels, history, old_length) else {
        return unchanged;
    };
    let Some(pitch) = pitch_period_of(&combined, sample_rate, splice) else {
        return unchanged;
    };
    if pitch.samples > max_length_change_samples || splice + pitch.samples > old_length + length {
        return unchanged;
    }
    let period = pitch.samples;
    // 重ね合わせる 2 区間が逆相のチャンネルがあると、混ぜたときに音量が落ちる
    for channel in &combined {
        let score = compression_splice_score(channel, splice as isize - period as isize, period);
        if !score.silent && !score.correlated {
            return unchanged;
        }
    }
    let expanded: Vec<Vec<f32>> = combined
        .iter()
        .map(|channel| {
            let mut output = vec![0.0f32; channel.len() + period];
            output[..splice + period].copy_from_slice(&channel[..splice + period]);
            cross_fade_with_offset(
                &mut output[splice..splice + period],
                channel,
                splice as isize - period as isize,
                period,
            );
            output[splice + period..].copy_from_slice(&channel[splice..]);
            output[old_length..].to_vec()
        })
        .collect();
    PreemptiveExpansion {
        channels: expanded,
        added_samples: period,
    }
}

/// 補間した音の末尾の振幅を、補間する長さ (マイクロ秒) から求める
///
/// 長い補間ほど末尾の振幅を [`TIME_STRETCH_CONCEAL_END_GAIN`] まで下げる。補間の長さが
/// [`TIME_STRETCH_MAX_CONCEAL_US`] 以上ではその値になり、0 以下では下げない。
pub fn concealment_end_gain(conceal_us: i64) -> f32 {
    let ratio = (conceal_us as f64 / TIME_STRETCH_MAX_CONCEAL_US as f64).clamp(0.0, 1.0);
    (1.0 - (1.0 - f64::from(TIME_STRETCH_CONCEAL_END_GAIN)) * ratio) as f32
}

/// 埋める長さ (サンプル数) を求める (埋められないときは None)
///
/// マイクロ秒をサンプル数へ直すときは四捨五入する。埋める長さが 0 になる (0 以下の
/// マイクロ秒、サンプルレートが 0、1 サンプルに満たない) ときと、サンプル数に直せない
/// ほど大きいときは None を返す。
fn conceal_samples(conceal_us: i64, sample_rate: u32) -> Option<usize> {
    if conceal_us <= 0 {
        return None;
    }
    // マイクロ秒をサンプル数へ直す (0.5 サンプルを足してから切り捨てる = 四捨五入)
    let scaled = conceal_us.saturating_mul(i64::from(sample_rate));
    let samples = usize::try_from(scaled.saturating_add(500_000) / 1_000_000).ok()?;
    (samples > 0).then_some(samples)
}

/// 末尾で繰り返している周期の探索の結果
struct TailPeriod {
    /// 末尾で繰り返している周期 (元のサンプルレートのサンプル数)。端数を含む
    samples: f64,
    /// 末尾で繰り返している度合い (-1 から 1)
    correlation: f64,
    /// 候補を評価した、元のレートでの相関窓のサンプル数 (評価できなかったときは 0)
    window_samples: usize,
}

/// 末尾で繰り返しているピッチ周期を求める (求められないときは None)
///
/// 1. 4 kHz へ間引いた信号で、固定長の窓どうしの相関が最も高いずらし幅を粗く探す
/// 2. その周辺 (±1 格子) を元のサンプルレートの正規化相関で評価し、ピークを放物線補間して
///    端数まで求める
///
/// 2 段にするのは、4 kHz の格子に周期が乗らない音で位相がずれるためである。4 kHz だけでは
/// 440 Hz の周期 109.09 サンプルを 108 としか求められず、100 ms の穴で 0.44 周期ずれて次の
/// 音との段差が振幅の 195 % になる (端数まで求めると 3 % になる)。
///
/// 放物線の頂点には実際の隣接 3 点の相関を使う。探索範囲の外側も 1 サンプルずつ評価し、
/// 44.1 kHz のように下限が整数でないレートでも、周期を範囲の内側へ丸めない。入力の長さの
/// 制約で隣接点を評価できないときは、外挿せず格子のままにする。
fn find_tail_period(analysis: &AnalysisSignal) -> Option<TailPeriod> {
    let end = analysis.len();
    // 窓長は固定にする。窓長 + ずらし幅がフレーム長に収まる範囲でずらし幅の上限を最大化する
    let window =
        (TIME_STRETCH_TAIL_CORRELATION_LEN as i64).min(end as i64 - TIME_STRETCH_MIN_LAG as i64);
    if window <= 0 {
        return None;
    }
    let window = window as usize;
    let max_lag = TIME_STRETCH_MAX_LAG.min(end - window);
    if max_lag < TIME_STRETCH_MIN_LAG {
        return None;
    }
    let mut coarse_lag = TIME_STRETCH_MIN_LAG;
    let mut coarse_correlation = f64::NEG_INFINITY;
    for lag in TIME_STRETCH_MIN_LAG..=max_lag {
        let correlation = downsampled_normalized_correlation(analysis, end, window, lag);
        if correlation > coarse_correlation {
            coarse_correlation = correlation;
            coarse_lag = lag;
        }
    }
    // 粗いずらし幅の周辺 (±1 格子) を元のサンプルレートで評価する
    let length = analysis.reference.len();
    let grid = analysis.samples_per_4khz_sample;
    let full_window = floor_to_i64(
        (window as f64 * grid).min(length as f64 - TIME_STRETCH_MIN_LAG as f64 * grid),
    );
    let center = coarse_lag as f64 * grid;
    // 端数の下限を切り上げると 44.1 kHz / 399 Hz の真の周期 110.526 サンプルが探索の外に
    // なり、111 サンプルへ丸めた位相誤差が長い補間で蓄積する。外側へ丸め、放物線補間に
    // 必要な隣接 1 サンプルも実際に評価する
    let first_lag =
        (floor_to_i64((TIME_STRETCH_MIN_LAG as f64 * grid).max(center - grid)) - 1).max(1);
    let last_lag = (ceil_positive(center + grid) as i64 + 1).min(length as i64 - full_window);
    if full_window <= 0 || last_lag < first_lag {
        // 元のサンプルレートで評価できない (音が短すぎる) ときは粗い格子のまま使う
        return Some(TailPeriod {
            samples: center,
            correlation: coarse_correlation,
            window_samples: usize::try_from(full_window).unwrap_or(0),
        });
    }
    let full_window = full_window as usize;
    let first_lag = first_lag as usize;
    let last_lag = last_lag as usize;
    let mut best_lag = first_lag;
    let mut best_correlation = f64::NEG_INFINITY;
    for lag in first_lag..=last_lag {
        let correlation =
            full_rate_normalized_correlation(analysis.reference, length, full_window, lag);
        if correlation > best_correlation {
            best_correlation = correlation;
            best_lag = lag;
        }
    }
    // ピークを放物線で補間して端数まで求める。最大が探索の端にあるときは前後の点を
    // 評価していないため、最大値で代用して架空の頂点を作らず、格子のままにする。上に凸で
    // ない (曲率が 0 以上) ときも同じく格子のままにする
    let has_both_neighbors = best_lag > first_lag && best_lag < last_lag;
    let before = if has_both_neighbors {
        full_rate_normalized_correlation(analysis.reference, length, full_window, best_lag - 1)
    } else {
        best_correlation
    };
    let after = if has_both_neighbors {
        full_rate_normalized_correlation(analysis.reference, length, full_window, best_lag + 1)
    } else {
        best_correlation
    };
    let curvature = before - 2.0 * best_correlation + after;
    // 整数のずらし幅が丸め誤差の範囲で完全相関なら、その周期を維持する。有限窓の前後の
    // 相関は非対称になり得るため、正しい整数周期まで放物線の頂点へずらすと位相を壊す
    let exact_period = best_correlation >= 1.0 - 1e-12;
    let offset = if !exact_period && has_both_neighbors && curvature < 0.0 {
        (0.5 * (before - after)) / curvature
    } else {
        0.0
    };
    Some(TailPeriod {
        samples: best_lag as f64 + offset.clamp(-0.5, 0.5),
        correlation: best_correlation,
        window_samples: full_window,
    })
}

/// 繰り返しの継ぎ目の段差が、末尾の周期の自然な段差と比べて大きすぎないかを確かめる
///
/// 段差は、末尾の最後のサンプルと、繰り返しの先頭になる 1 周期前のサンプルの差である。
/// 周期がずれている音ではここが大きくなり、クリックとして聞こえる。周期は端数を含むため、
/// 1 周期前のサンプルは線形補間で読む。周期は第 1 チャンネルだけで決めるが、段差はどの
/// チャンネルでもクリックになるため、全チャンネルで確かめる。
fn is_seam_smooth(channels: &[&[f32]], period: f64) -> bool {
    channels
        .iter()
        .all(|channel| is_channel_seam_smooth(channel, period))
}

/// 1 つのチャンネルの継ぎ目の段差を確かめる
///
/// `period` は 1 以上 `channel` の長さ以下であること (端数を含む)。
fn is_channel_seam_smooth(channel: &[f32], period: f64) -> bool {
    let length = channel.len();
    // 末尾の周期 1 つ分の中の自然な段差を求める。端数の周期では 1 周期前の位置を切り上げる
    let mut natural_step = 0.0f64;
    let start = ceil_positive(length as f64 - period).max(1);
    for index in start..length {
        natural_step =
            natural_step.max((f64::from(channel[index]) - f64::from(channel[index - 1])).abs());
    }
    let seam_step = (f64::from(sample_at(channel, length as f64 - period))
        - f64::from(channel[length - 1]))
    .abs();
    seam_step <= natural_step * TIME_STRETCH_MAX_SEAM_STEP_RATIO
}

/// 直前の音の末尾を周期で繰り返して、欠落した区間を埋める
///
/// `channels` はチャンネルごとの直前の音声で、すべて同じ長さであること。サンプルは
/// おおむね `[-1.0, 1.0]` に正規化された有限値であること。埋めた音は `output` へ書く
/// (`output` の長さと順序は `channels` に対応させる)。`output` の各要素には、`conceal_us`
/// に相当するサンプル数以上の長さを用意すること。サンプルレートは [`compress`] と同じ
/// 8 kHz / 16 kHz / 32 kHz / 48 kHz と 44.1 kHz に対応する。
///
/// 周期は末尾の相関から求める ([`compress`] が操作する位置の前の相関窓で求めるのとは別。
/// 繰り返しの継ぎ目は末尾にあるため、末尾の周期を直接評価する)。4 kHz へ間引いた信号で
/// 粗く探したあと、その周辺を元のサンプルレートの相関で評価して端数まで求める。周期どおりに
/// 繰り返せているときは、繰り返しの先頭が末尾の続きになるため継ぎ目は波形が連続する。
/// 埋めた音の末尾の振幅は [`concealment_end_gain`] まで徐々に下げる (繰り返しの音を
/// 目立たなくする)。moqt-js の `concealSamples` は末尾ゲインを引数 (`endGain`) で受けるが、
/// この関数は `conceal_us` から導出する (API は変えない)。moqt-js の本番経路である
/// [`expand_audio`](crate::playout::expansion::expand_audio) は常に 100 ms を要求し、その値は
/// 0.5 で一致する。100 ms 以外の長さでは、moqt-js の `expandAudio` が渡す 0.5 とは末尾ゲインが
/// 異なる点に注意する。
///
/// 周期の推定が確かでない ([`TIME_STRETCH_CONCEAL_FADE_CORRELATION`] 未満) ときは、穴の
/// 終わりで次に鳴る音の位相が分からず段差がクリックになるため、生成した音の末尾を
/// [`TIME_STRETCH_CONCEAL_FADE_MS`] で 0 へ落とす。
///
/// # 戻り値
///
/// 埋めた長さ (サンプル数)。0 のとき `output` には何も書かない。0 になるのは、対応外の
/// サンプルレート、空または長さの揃わないチャンネル、`output` の長さが合わないか足りない
/// とき、埋める長さが 0 以下のとき、末尾に周期が無いとき、末尾の相関が
/// [`TIME_STRETCH_CORRELATION_THRESHOLD`] 未満のとき (無音を除く)、第 2 チャンネル以降で
/// 同じ周期が使えないとき、継ぎ目の段差が [`TIME_STRETCH_MAX_SEAM_STEP_RATIO`] を超える
/// とき (どのチャンネルでも)。無音の入力では相関が常に 0 になるが、周期の代わりに 0 を
/// 書いて要求どおりの長さを埋める。
pub fn conceal(
    channels: &[&[f32]],
    output: &mut [&mut [f32]],
    sample_rate: u32,
    conceal_us: i64,
) -> isize {
    if channels.is_empty() || output.len() != channels.len() {
        return 0;
    }
    let length = channels[0].len();
    if length == 0 || channels.iter().any(|channel| channel.len() != length) {
        return 0;
    }
    let Some(target) = conceal_samples(conceal_us, sample_rate) else {
        return 0;
    };
    let Some(analysis) = AnalysisSignal::new(channels[0], sample_rate) else {
        return 0;
    };
    let Some(tail) = find_tail_period(&analysis) else {
        // 末尾に周期が無い (音が短すぎる、対応していないサンプルレート)
        return 0;
    };
    if tail.samples <= 0.0 || tail.samples > length as f64 {
        return 0;
    }
    // 波形が繰り返していない場所で切ると耳につくため、相関が足りないときは操作しない。
    // 無音は詰めても伸ばしても聞こえないため、そのまま操作する (NetEq と同じ扱い)
    if tail.correlation < TIME_STRETCH_CORRELATION_THRESHOLD && !is_silent(channels.iter().copied())
    {
        return 0;
    }
    // 左の周期で右を繰り返せるとは限らない。先頭の段差が偶然小さい位相でも、補間の内部で
    // 別の周期音を作らないよう、他のチャンネルの末尾の相関も確かめる
    let common_lag = round_positive(tail.samples);
    for channel in &channels[1..] {
        if is_silent(core::iter::once(*channel)) {
            continue;
        }
        let available = channel.len() as i64 - common_lag as i64;
        let window = (tail.window_samples as i64).min(available).max(0) as usize;
        if full_rate_normalized_correlation(channel, channel.len(), window, common_lag)
            < TIME_STRETCH_CORRELATION_THRESHOLD
        {
            return 0;
        }
    }
    // 継ぎ目の段差の検査は全チャンネルで行う (周期は第 1 チャンネルだけで決める)
    if !is_seam_smooth(channels, tail.samples) {
        return 0;
    }
    if output.iter().any(|channel| channel.len() < target) {
        return 0;
    }
    let end_gain = concealment_end_gain(conceal_us);
    // 周期の推定が確かでないときは、穴の終わりで次の音と位相が合わない。補間をやめるのでは
    // なく、生成した音の末尾を短くフェードして段差を消す (穴の残りは無音でよい)
    let fade = if tail.correlation < TIME_STRETCH_CONCEAL_FADE_CORRELATION {
        // フェードの長さはサンプルレートから求める (0.5 サンプルを足して切り捨てる)
        let fade_samples =
            round_positive(TIME_STRETCH_CONCEAL_FADE_MS / 1_000.0 * f64::from(sample_rate));
        target.min(fade_samples.max(1))
    } else {
        0
    };
    // 末尾の周期 1 つ分を、端数の周期の位相を保ったまま必要な長さまで繰り返す
    let start = length as f64 - tail.samples;
    for (channel, destination) in channels.iter().zip(output.iter_mut()) {
        for (index, sample) in destination.iter_mut().take(target).enumerate() {
            // 0 以上の値では整数への切り捨てが小数点以下の切り捨て (floor) と同じになる
            let periods = (index as f64 / tail.samples) as usize;
            let phase = index as f64 - periods as f64 * tail.samples;
            // 長い補間ほど末尾の振幅を下げる (繰り返しの音を目立たなくする)
            let gain = 1.0 - (1.0 - f64::from(end_gain)) * ((index as f64 + 1.0) / target as f64);
            *sample = (f64::from(read_repeated(channel, start, phase)) * gain) as f32;
        }
        if fade > 0 {
            // 末尾を 0 へ落とす (最後のサンプルが 0 になる)
            for index in 0..fade {
                let gain = 1.0 - (index as f64 + 1.0) / fade as f64;
                let position = target - fade + index;
                destination[position] = (f64::from(destination[position]) * gain) as f32;
            }
        }
    }
    target as isize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cross_fade_moves_weight_one_step_at_a_time() {
        // 重みは 1 / (fade + 1) ずつ動く (2 サンプルなら 1/3 と 2/3)
        let mut target = [0.0f32, 0.0];
        let source = [1.0f32, 1.0];
        cross_fade(&mut target, &source, 2);
        assert!((target[0] - 1.0 / 3.0).abs() < 1e-6);
        assert!((target[1] - 2.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn cross_fade_reads_out_of_range_source_as_zero() {
        // 読み出し位置が負のときは 0 として扱う (moqt-js が undefined を 0 にするのと同じ)
        let mut target = [1.0f32, 1.0];
        cross_fade_with_offset(&mut target, &[7.0f32], -2, 2);
        // 読み出し位置が両方とも負なので、元の値へ 0 を混ぜる (weight は 1/3 と 2/3)
        assert!((target[0] - 2.0 / 3.0).abs() < 1e-6);
        assert!((target[1] - 1.0 / 3.0).abs() < 1e-6);
        // 1 サンプルだけ外へ出るときは、範囲内のサンプルを読む
        let mut target = [1.0f32, 1.0];
        cross_fade_with_offset(&mut target, &[7.0f32], -1, 2);
        assert!((target[0] - 2.0 / 3.0).abs() < 1e-6);
        assert!((target[1] - (1.0 / 3.0 + 2.0 / 3.0 * 7.0)).abs() < 1e-6);
    }

    #[test]
    fn conceal_samples_rounds_to_the_nearest_sample() {
        // 0.5 サンプルちょうど (1 kHz で 500 µs) は 1 サンプルへ切り上げる
        assert_eq!(conceal_samples(500, 1_000), Some(1));
        // 1 サンプルに満たない長さは埋めない
        assert_eq!(conceal_samples(10, 48_000), None);
        assert_eq!(conceal_samples(20, 48_000), Some(1));
        // 0 以下の長さと、サンプルレートが 0 のときは埋めない
        assert_eq!(conceal_samples(0, 48_000), None);
        assert_eq!(conceal_samples(-1, 48_000), None);
        assert_eq!(conceal_samples(1_000, 0), None);
    }

    #[test]
    fn sqrt_matches_exact_squares() {
        // 完全な平方数の平方根は丸め誤差の範囲で一致する
        for value in [0.0f64, 1.0, 4.0, 9.0, 100.0, 1e-30, 12345.6789] {
            let root = sqrt(value);
            assert!(
                (root * root - value).abs() <= 1e-9 * value.max(1.0),
                "value={value} root={root}"
            );
        }
        // 0 と負の値は 0 になる
        assert_eq!(sqrt(0.0), 0.0);
        assert_eq!(sqrt(-1.0), 0.0);
        assert_eq!(sqrt(f64::NAN), 0.0);
    }

    #[test]
    fn refine_lag_keeps_the_integer_lag_at_the_search_upper_bound() {
        // ずらし幅の上限では前後 1 格子の後ろ側を評価できないため、放物線の頂点を作らず
        // 整数のずらし幅を返す
        let downsampled: Vec<f32> = (0..=TIME_STRETCH_MIN_LAG)
            .map(|index| ((index * 7) % 11) as f32 - 5.0)
            .collect();
        let coarse = CoarseLag {
            lag: TIME_STRETCH_MIN_LAG,
            correlation: 1.0,
        };
        assert_eq!(
            refine_lag(&downsampled, &coarse),
            TIME_STRETCH_MIN_LAG as f64,
            "探索の上限では端数にしないこと"
        );
    }

    #[test]
    fn compression_splice_keeps_the_center_when_no_position_improves() {
        // 周期ごとに値が同じ矩形波では、どこで切っても失うエネルギーが同じなので中央を保つ
        let channel = {
            let period = 240;
            (0..960)
                .map(|index| {
                    if index % period < period / 2 {
                        0.5f32
                    } else {
                        -0.5
                    }
                })
                .collect::<Vec<f32>>()
        };
        assert_eq!(
            compression_splice(&[channel.as_slice()], 240, 480),
            480,
            "改善しないときは中央を保つこと"
        );
    }

    #[test]
    fn compression_splice_score_reads_out_of_range_as_zero() {
        // moqt-js は範囲外の添字を undefined として 0 にする
        let channel = vec![1.0f32; 8];
        let score = compression_splice_score(&channel, -4, 2);
        // 始点 -4 の 2 サンプルは範囲外なので before は 0、after は channel[-2..0] も範囲外で 0
        assert_eq!(score.loss, 0.0);
        assert!(score.silent, "範囲外だけなら無音であること");
        assert!(!score.correlated, "相関は取れないこと");
    }

    #[test]
    fn pitch_period_of_rejects_a_channel_with_a_different_period() {
        // 第 1 チャンネルが 200 Hz、第 2 チャンネルが 333 Hz では、同じ周期を使えないため
        // 周期を返さない
        let sample = |index: f64, frequency: f64| {
            (0.5 * (2.0 * core::f64::consts::PI * frequency * index / 48_000.0).sin()) as f32
        };
        let mut left = vec![0.0f32; 960];
        let mut right = vec![0.0f32; 960];
        for index in 0..960 {
            left[index] = sample(index as f64, 200.0);
            right[index] = sample(index as f64, 333.0);
        }
        assert!(pitch_period_of(&[left.as_slice()], 48_000, 480).is_some());
        assert!(
            pitch_period_of(&[left.as_slice(), right.as_slice()], 48_000, 480).is_none(),
            "全チャンネルで同じ周期を使えないときは操作しないこと"
        );
    }

    #[test]
    fn analysis_grid_rounds_to_the_nearest_4khz_multiple() {
        // moqt-js と同じく 4 kHz の倍数へ丸める (44.1 kHz だけは非整数の倍率)
        assert_eq!(analysis_grid(8_000), Some(2.0));
        assert_eq!(analysis_grid(44_100), Some(11.025));
        assert_eq!(analysis_grid(47_999), Some(12.0));
        assert_eq!(analysis_grid(48_000), Some(12.0));
        // 丸めると 0 になるレートは解析できない
        assert_eq!(analysis_grid(1), None);
    }
}
