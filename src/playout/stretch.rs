//! 波形の周期による音声の時間圧縮と、欠落した区間の補間
//!
//! 到着の間隔が揺らいだ音声を目標の時刻に合わせるため、波形の周期 (ピッチ) 1 つ分を
//! 削って音の長さをわずかに詰める。音声が欠落した区間は、直前の音の末尾の周期を
//! 繰り返して埋める。周期は第 1 チャンネルを 4 kHz へ間引いた自己相関から求める。
//! 欠落した区間の補間では、その 4 kHz の粗い格子の周辺を元のサンプルレートの相関で評価し、
//! ピークを放物線補間して端数まで周期を求める (4 kHz の格子では 48 kHz で 12 サンプル単位に
//! 量子化され、格子に乗らない音では穴が長いほど位相がずれるため)。時刻や出力デバイスには
//! 触れない。
//!
//! 時間伸長は持たない。この再生は音を伸ばす場面を持たないためである (目標より早く届いた音は
//! 前の音の終わりより後ろに置けばよく、足りない区間は補間 ([`conceal`]) が埋める)。

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
pub const TIME_STRETCH_MAX_SEAM_STEP_RATIO: f32 = 2.0;

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
pub const TIME_STRETCH_CONCEAL_FADE_MS: u32 = 2;

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

/// サンプルレートに対応するフィルタを返す (対応外は None)
fn downsample_filter(sample_rate: u32) -> Option<DownsampleFilter> {
    let filter = match sample_rate {
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

/// 時間圧縮と補間を使えるサンプルレートか
///
/// ピッチの周期は 4 kHz へ間引いた信号の自己相関から求めるため、間引きのフィルタの表
/// (8 kHz / 16 kHz / 32 kHz / 48 kHz) に無いサンプルレートでは周期を求められない。44.1 kHz
/// (AAC など) がこれにあたり、**時間圧縮 ([`compress`]) も補間 ([`conceal`]) も働かない**。
/// 落ちずに「操作しない」を返すため呼び出し側からは区別できないので、先にこの関数で確かめて
/// 計器へ出す
/// ([`record_stretch_unavailable`](crate::playout::timing::AudioPlayoutTimingStats::record_stretch_unavailable)
/// を参照)。
///
/// 判定はフィルタの表の完全一致にする。moqt-js は `Math.round(sampleRate / 4_000) * 4_000` で
/// 4 kHz の倍数へ丸めてから表を引くが、その丸めは写さない。moqt-js の時間圧縮は丸めた値を
/// キーに使うため、表のレートの近傍 (47.999 kHz など) でも 48 kHz のフィルタで動く。この
/// モジュールの [`compress`] はサンプルレートそのもので間引きフィルタを引くため、丸めを
/// 写すと 47.999 kHz を「使える」と答えながら実際には動かない、という食い違いが生じる。
/// 44.1 kHz は丸めても 44 kHz になり表に無いため、どちらの判定でも false である。
///
/// 無音の入力では [`conceal`] がフィルタを引く前に無音と判定して要求どおり 0 を書くため、
/// この判定が false でも補間そのものは起きる (無音を埋めるだけなので実害はない)。
pub fn is_time_stretch_available(sample_rate: u32) -> bool {
    downsample_filter(sample_rate).is_some()
}

/// フィルタ係数の和 (正規化に使う)
fn coefficient_sum(filter: &DownsampleFilter) -> f32 {
    filter
        .coefficients
        .iter()
        .map(|coefficient| *coefficient as f32)
        .sum()
}

/// 間引きの開始位置 (入力のサンプル数)
fn downsample_start(filter: &DownsampleFilter) -> usize {
    filter.coefficients.len() - 1 + filter.delay
}

/// 間引いた信号のサンプル数を返す
fn downsampled_len(signal_len: usize, filter: &DownsampleFilter) -> usize {
    let start = downsample_start(filter);
    if signal_len <= start {
        0
    } else {
        (signal_len - start) / filter.factor
    }
}

/// 間引いた信号の `index` 番目のサンプルを返す
///
/// `index` は `downsampled_len(signal.len(), filter)` 未満であること。
fn downsampled_at(
    signal: &[f32],
    filter: &DownsampleFilter,
    coefficient_sum: f32,
    index: usize,
) -> f32 {
    let start = downsample_start(filter);
    let mut sum = 0.0f32;
    for (tap, coefficient) in filter.coefficients.iter().enumerate() {
        let position = start + index * filter.factor - tap;
        sum += *coefficient as f32 * signal[position];
    }
    sum / coefficient_sum
}

/// 自己相関が最も強いずらし幅 (4 kHz のサンプル数) を返す
///
/// `scratch` の先頭 `TIME_STRETCH_CORRELATION_LEN + max_lag` サンプルを使う。
fn find_lag(scratch: &[f32], max_lag: usize) -> usize {
    let mut best_lag = TIME_STRETCH_MIN_LAG;
    let mut best_value = f64::NEG_INFINITY;
    for lag in TIME_STRETCH_MIN_LAG..=max_lag {
        let mut correlation = 0.0f64;
        for (left, right) in scratch[..TIME_STRETCH_CORRELATION_LEN]
            .iter()
            .zip(&scratch[lag..lag + TIME_STRETCH_CORRELATION_LEN])
        {
            correlation += f64::from(*left) * f64::from(*right);
        }
        if correlation > best_value {
            best_value = correlation;
            best_lag = lag;
        }
    }
    best_lag
}

/// 末尾で繰り返している周期の探索の結果
struct TailPeriod {
    /// 末尾で繰り返している周期 (元のサンプルレートのサンプル数)。端数を含む
    samples: f64,
    /// 末尾で繰り返している度合い (0 から 1)。正規化相関の二乗
    ///
    /// [`analyze`] と同じく、閾値との比較を平方根を取らずに済ませるため、二乗のまま持つ
    /// ([`TIME_STRETCH_CORRELATION_THRESHOLD`] の二乗と比べる)。
    correlation_squared: f64,
}

/// 正規化相関の二乗 (0 から 1) を求める (周期とみなせないときは None)
///
/// 正規化相関 `dot / sqrt(energy_before * energy_after)` の二乗を、平方根を取らずに求める。
/// 逆向きの波形 (相関が負) と、振幅の無い区間は周期とみなさない。
fn correlation_squared(dot: f64, energy_before: f64, energy_after: f64) -> Option<f64> {
    let energy = energy_before * energy_after;
    if dot <= 0.0 || energy <= 0.0 {
        return None;
    }
    Some(dot * dot / energy)
}

/// 4 kHz へ間引いた信号の、2 つの区間の相関の二乗を求める
///
/// `end` を末尾とする長さ `window` の区間と、そこから `lag` だけ前の同じ長さの区間を比べる。
/// 窓の長さは呼び出し側が決める (候補のずらし幅に比例させない)。`lag + window <= end` で
/// あること。
fn downsampled_correlation_squared(
    reference: &[f32],
    filter: &DownsampleFilter,
    coefficient_sum: f32,
    end: usize,
    window: usize,
    lag: usize,
) -> Option<f64> {
    let mut dot = 0.0f64;
    let mut energy_before = 0.0f64;
    let mut energy_after = 0.0f64;
    for index in 0..window {
        let before = f64::from(downsampled_at(
            reference,
            filter,
            coefficient_sum,
            end - lag - window + index,
        ));
        let after = f64::from(downsampled_at(
            reference,
            filter,
            coefficient_sum,
            end - window + index,
        ));
        dot += before * after;
        energy_before += before * before;
        energy_after += after * after;
    }
    correlation_squared(dot, energy_before, energy_after)
}

/// 元のサンプルレートの信号の、2 つの区間の相関の二乗を求める
///
/// `end` を末尾とする長さ `window` の区間と、そこから `lag` だけ前の同じ長さの区間を比べる。
/// `lag + window <= end <= signal.len()` であること。
fn full_rate_correlation_squared(
    signal: &[f32],
    end: usize,
    window: usize,
    lag: usize,
) -> Option<f64> {
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
    correlation_squared(dot, energy_before, energy_after)
}

/// 末尾で繰り返している周期を求める (求められないときは None)
///
/// 1. 4 kHz へ間引いた信号で、固定長の窓どうしの相関が最も高いずらし幅を粗く探す
/// 2. その周辺 (±1 格子) を元のサンプルレートの相関で評価し、ピークを放物線補間して端数まで
///    求める
///
/// 2 段にするのは、4 kHz の格子に周期が乗らない音で位相がずれるためである。4 kHz だけでは
/// 440 Hz の周期 109.09 サンプルを 108 としか求められず、100 ms の穴で 0.44 周期ずれて次の
/// 音との段差が振幅の 195 % になる (端数まで求めると 1.6 % になる)。
///
/// 放物線の頂点は細探索した 3 点から求めるため、最大が細探索の端にあるときは求められない。
/// 端では格子のままにする (端で外挿すると探索の下限の外側のずらし幅を返し、周期が 0.5
/// サンプルずれる)。400 Hz の周期 120 サンプルは細探索の下限そのものであり、この経路を通る。
///
/// 窓の長さは [`TIME_STRETCH_TAIL_CORRELATION_LEN`] に固定し、窓の長さ + ずらし幅が音の長さに
/// 収まる範囲でずらし幅の上限を最大化する。音が短すぎる (窓の長さが取れない)、正の相関が
/// 取れない (波形が逆向き、振幅が無い) ときは None。
fn find_tail_period(
    reference: &[f32],
    filter: &DownsampleFilter,
    coefficient_sum: f32,
) -> Option<TailPeriod> {
    let count = downsampled_len(reference.len(), filter);
    // 窓長は固定にする。窓長 + ずらし幅が間引いた音の長さに収まる範囲でずらし幅を最大化する
    let window = TIME_STRETCH_TAIL_CORRELATION_LEN.min(count.saturating_sub(TIME_STRETCH_MIN_LAG));
    if window == 0 {
        return None;
    }
    let max_lag = TIME_STRETCH_MAX_LAG.min(count - window);
    if max_lag < TIME_STRETCH_MIN_LAG {
        return None;
    }
    // 粗い格子 (4 kHz) で最も相関が高いずらし幅を探す
    let mut coarse: Option<(usize, f64)> = None;
    for lag in TIME_STRETCH_MIN_LAG..=max_lag {
        let Some(score) =
            downsampled_correlation_squared(reference, filter, coefficient_sum, count, window, lag)
        else {
            // 周期とみなせない (逆向きの波形、振幅が無い) 候補は飛ばす
            continue;
        };
        if coarse.is_none_or(|(_, best)| score > best) {
            coarse = Some((lag, score));
        }
    }
    let Some((coarse_lag, coarse_correlation_squared)) = coarse else {
        // 正の相関が 1 つも無い (波形が逆向き、振幅が無い)
        return None;
    };
    // 粗いずらし幅の周辺 (±1 格子) を元のサンプルレートで評価する
    let length = reference.len();
    let full_window =
        (window * filter.factor).min(length.saturating_sub(TIME_STRETCH_MIN_LAG * filter.factor));
    let center = coarse_lag * filter.factor;
    if full_window == 0 {
        // 元のサンプルレートで評価できない (音が短すぎる) ときは粗い格子のまま使う
        return Some(TailPeriod {
            samples: center as f64,
            correlation_squared: coarse_correlation_squared,
        });
    }
    let first_lag = (TIME_STRETCH_MIN_LAG * filter.factor).max(center - filter.factor);
    let last_lag = (length - full_window).min(center + filter.factor);
    if last_lag < first_lag {
        // 元のサンプルレートで評価できる幅が無いときは粗い格子のまま使う
        return Some(TailPeriod {
            samples: center as f64,
            correlation_squared: coarse_correlation_squared,
        });
    }
    let mut best: Option<(usize, f64)> = None;
    for lag in first_lag..=last_lag {
        let Some(score) = full_rate_correlation_squared(reference, length, full_window, lag) else {
            // 周期とみなせない (逆向きの波形、振幅が無い) 候補は飛ばす
            continue;
        };
        if best.is_none_or(|(_, best_score)| score > best_score) {
            best = Some((lag, score));
        }
    }
    let Some((best_lag, best_correlation_squared)) = best else {
        // 元のサンプルレートでは正の相関が取れない (粗い格子でだけ相関があった)
        return None;
    };
    // ピークの前後と最大の 3 点から放物線の頂点を求めて端数まで詰める。頂点は 3 点が揃わないと
    // 求められないため、最大が細探索の端にあるときは格子のままにする。端で外挿すると、評価して
    // いないずらし幅 (探索の下限の外側) を返して周期が 0.5 サンプルずれ、穴の終わりで位相が
    // 合わなくなる (実測: 48 kHz、20 ms のフレーム、400 Hz = 周期 120 サンプルは細探索の下限
    // そのもので、穴 100 ms の段差が振幅の 85.2 % になった)。上に凸でない (曲率が 0 以上) ときも
    // 頂点を求められないため、同じく格子のままにする
    let has_both_neighbors = best_lag > first_lag && best_lag < last_lag;
    let before = if has_both_neighbors {
        full_rate_correlation_squared(reference, length, full_window, best_lag - 1).unwrap_or(0.0)
    } else {
        best_correlation_squared
    };
    let after = if has_both_neighbors {
        full_rate_correlation_squared(reference, length, full_window, best_lag + 1).unwrap_or(0.0)
    } else {
        best_correlation_squared
    };
    let curvature = before - 2.0 * best_correlation_squared + after;
    let offset = if has_both_neighbors && curvature < 0.0 {
        0.5 * (before - after) / curvature
    } else {
        0.0
    };
    Some(TailPeriod {
        samples: best_lag as f64 + offset.clamp(-0.5, 0.5),
        correlation_squared: best_correlation_squared,
    })
}

/// ピッチの周期の探索結果
struct PitchPeriod {
    /// ピッチの周期 (元のサンプルレートのサンプル数)
    samples: usize,
    /// 切れ目の前後の正規化相関が閾値以上か
    sufficiently_correlated: bool,
}

/// 第 1 チャンネルの波形からピッチの周期を求める (求められないときは None)
///
/// 切れ目の位置 `splice` の前後で波形が繰り返しているかを調べ、周期が切れ目に
/// 収まらないときは None を返す。
fn analyze(reference: &[f32], sample_rate: u32, splice: usize) -> Option<PitchPeriod> {
    let filter = downsample_filter(sample_rate)?;
    let count = downsampled_len(reference.len(), &filter);
    // ずらし幅の上限 (min(60, count - 50)) が下限を下回るときは周期を求められない
    if count < TIME_STRETCH_CORRELATION_LEN + TIME_STRETCH_MIN_LAG {
        return None;
    }
    let max_lag = TIME_STRETCH_MAX_LAG.min(count - TIME_STRETCH_CORRELATION_LEN);
    let sum = coefficient_sum(&filter);
    let mut scratch = [0.0f32; TIME_STRETCH_CORRELATION_LEN + TIME_STRETCH_MAX_LAG];
    for (index, sample) in scratch
        .iter_mut()
        .take(TIME_STRETCH_CORRELATION_LEN + max_lag)
        .enumerate()
    {
        *sample = downsampled_at(reference, &filter, sum, index);
    }
    let lag = find_lag(&scratch, max_lag);
    let samples = lag * filter.factor;
    if samples > splice {
        // ピッチ周期が音の中央に収まらない (音が短すぎる)
        return None;
    }
    let mut dot = 0.0f64;
    let mut energy_before = 0.0f64;
    let mut energy_after = 0.0f64;
    for (before, after) in reference[splice - samples..splice]
        .iter()
        .zip(&reference[splice..splice + samples])
    {
        let before = f64::from(*before);
        let after = f64::from(*after);
        dot += before * after;
        energy_before += before * before;
        energy_after += after * after;
    }
    // 正規化相関 dot / sqrt(energy_before * energy_after) >= 閾値 を、sqrt を使わず
    // 二乗の比較で判定する (dot > 0 のとき同値)
    let sufficiently_correlated = dot > 0.0
        && dot * dot
            >= TIME_STRETCH_CORRELATION_THRESHOLD
                * TIME_STRETCH_CORRELATION_THRESHOLD
                * energy_before
                * energy_after;
    Some(PitchPeriod {
        samples,
        sufficiently_correlated,
    })
}

/// 全チャンネルの振幅のピークが無音とみなす値より小さいか
fn is_silent<'a>(channels: impl Iterator<Item = &'a [f32]>) -> bool {
    let mut peak = 0.0f32;
    for channel in channels {
        for sample in channel {
            peak = peak.max(sample.abs());
        }
    }
    peak < TIME_STRETCH_SILENCE_PEAK
}

/// 0 以上の値の小数点以下を切り上げて整数にする
///
/// `no_std` には `f64::ceil` が無いため、整数へ切り捨てた値と比べて求める。NaN と 0 以下は
/// 0 にする (NaN は比較がすべて false になるため先に弾く)。
fn ceil_to_usize(value: f64) -> usize {
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

/// 端数の位置のサンプルを線形補間で読む
///
/// 周期は 4 kHz の格子 (48 kHz で 12 サンプル) に乗らない端数になるため、繰り返しの生成や
/// 継ぎ目の評価では 2 つのサンプルの間を読む。`position` は 0 以上であること。範囲の外は
/// 最後のサンプル (先頭より前は 0) を返す。
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
    let mut natural_step = 0.0f32;
    let start = ceil_to_usize(length as f64 - period).max(1);
    for index in start..length {
        natural_step = natural_step.max((channel[index] - channel[index - 1]).abs());
    }
    let seam_step = (sample_at(channel, length as f64 - period) - channel[length - 1]).abs();
    seam_step <= natural_step * TIME_STRETCH_MAX_SEAM_STEP_RATIO
}

/// 操作するピッチの周期を求める (操作しないときは None)
///
/// 波形が繰り返していない場所で切ると耳につくため、相関が足りないときは操作しない。
/// 無音は詰めても聞こえないため、そのまま操作する。無音かどうかは、相関が
/// 足りないときだけ `channels_are_silent` で調べる。
fn pitch_period_of(
    reference: &[f32],
    sample_rate: u32,
    splice: usize,
    channels_are_silent: impl FnOnce() -> bool,
) -> Option<PitchPeriod> {
    let pitch = analyze(reference, sample_rate, splice)?;
    if !pitch.sufficiently_correlated && !channels_are_silent() {
        return None;
    }
    Some(pitch)
}

/// 切れ目の前後をクロスフェードで繋ぐ
///
/// `target` の先頭 `fade` サンプルを、`source` の先頭 `fade` サンプルへ少しずつ寄せる。
/// 重みは 1 サンプルごとに `1 / (fade + 1)` ずつ動かす。
fn cross_fade(target: &mut [f32], source: &[f32], fade: usize) {
    let step = 1.0f32 / (fade as f32 + 1.0);
    for (index, (target_sample, source_sample)) in
        target.iter_mut().zip(source.iter()).take(fade).enumerate()
    {
        let weight = (index as f32 + 1.0) * step;
        *target_sample = (1.0 - weight) * *target_sample + weight * *source_sample;
    }
}

/// 音声を時間圧縮する (ピッチ周期 1 つ分を削る)
///
/// `channels` はチャンネルごとの音声で、すべて同じ長さであること。サンプルはおおむね
/// `[-1.0, 1.0]` に正規化された有限値であること。圧縮した結果は同じバッファへ書き、
/// 使用する範囲は元の長さに戻り値を足した長さまでになる。
///
/// 削る長さはピッチの周期で決まり、要求では選べない。周期が `max_length_change_samples` を
/// 超えるときは操作しない。遅れより長い周期を削ると、削りすぎた分は「早すぎる」側へ振れ、
/// 次の音でまた遅れて詰める往復になる。往復のたびに周期が 1 つ消えるため、声では
/// 粗さ・ざらつきとして聞こえる。
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
    if channels.is_empty() {
        return 0;
    }
    let length = channels[0].len();
    if length == 0 || channels.iter().any(|channel| channel.len() != length) {
        return 0;
    }
    let reference: &[f32] = channels[0];
    let splice = length / 2;
    let pitch = match pitch_period_of(reference, sample_rate, splice, || {
        is_silent(channels.iter().map(|channel| &**channel))
    }) {
        Some(pitch) => pitch,
        None => return 0,
    };
    let removed = pitch.samples;
    if removed > max_length_change_samples {
        // 遅れより長い周期しか無い。削ると行き過ぎるため、この音では詰めない
        return 0;
    }
    for channel in channels.iter_mut() {
        {
            // 切れ目の前後は同じバッファの隣り合う領域であるため、分割して借用する
            let (head, tail) = channel.split_at_mut(splice);
            cross_fade(&mut head[splice - removed..], &tail[..removed], removed);
        }
        // 削った分だけ後ろを詰める
        channel.copy_within(splice + removed.., splice);
    }
    -(removed as isize)
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

/// 直前の音の末尾を周期で繰り返して、欠落した区間を埋める
///
/// `channels` はチャンネルごとの直前の音声で、すべて同じ長さであること。サンプルは
/// おおむね `[-1.0, 1.0]` に正規化された有限値であること。埋めた音は `output` へ書く
/// (`output` の長さと順序は `channels` に対応させる)。`output` の各要素には、`conceal_us`
/// に相当するサンプル数以上の長さを用意すること。サンプルレートは [compress] と同じ
/// 8 kHz / 16 kHz / 32 kHz / 48 kHz に対応する。
///
/// 周期は末尾の相関から求める ([compress] が操作する位置の前の相関窓で求める
/// のとは別。繰り返しの継ぎ目は末尾にあるため、末尾の周期を直接評価する)。4 kHz へ間引いた
/// 信号で粗く探したあと、その周辺を元のサンプルレートの相関で評価して端数まで求める。
/// 周期どおりに繰り返せているときは、繰り返しの先頭が末尾の続きになるため継ぎ目は波形が
/// 連続する。埋めた音の末尾の振幅は [concealment_end_gain] まで徐々に下げる (繰り返しの
/// 音を目立たなくする)。
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
/// [`TIME_STRETCH_CORRELATION_THRESHOLD`] 未満のとき、継ぎ目の段差が
/// [`TIME_STRETCH_MAX_SEAM_STEP_RATIO`] を超えるとき (どのチャンネルでも)。無音に近い入力
/// では周期が求まらないため、要求ぶんの 0 を書く (埋めた長さは要求どおりになる)。
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
    // 無音に近い入力では相関が常に 0 になり周期が求まらない。周期の代わりに 0 を書く。
    // 埋める長さは要求どおりに揃え、隙間を埋めた事実は残す
    if is_silent(channels.iter().copied()) {
        if output.iter().any(|channel| channel.len() < target) {
            return 0;
        }
        for destination in output.iter_mut() {
            destination[..target].fill(0.0);
        }
        return target as isize;
    }
    let Some(filter) = downsample_filter(sample_rate) else {
        return 0;
    };
    let reference: &[f32] = channels[0];
    let sum = coefficient_sum(&filter);
    let Some(tail_period) = find_tail_period(reference, &filter, sum) else {
        // 末尾に周期が無い (音が短すぎる、波形が繰り返していない)
        return 0;
    };
    if tail_period.samples <= 0.0 || tail_period.samples > length as f64 {
        // 求めた周期が音に収まらない
        return 0;
    }
    if tail_period.correlation_squared
        < TIME_STRETCH_CORRELATION_THRESHOLD * TIME_STRETCH_CORRELATION_THRESHOLD
    {
        // 末尾で波形が繰り返していない
        return 0;
    }
    // 継ぎ目の段差の検査は全チャンネルで行う (周期は第 1 チャンネルだけで決める)
    if !is_seam_smooth(channels, tail_period.samples) {
        return 0;
    }
    if output.iter().any(|channel| channel.len() < target) {
        return 0;
    }
    let end_gain = concealment_end_gain(conceal_us);
    // 周期の推定が確かでないときは、穴の終わりで次の音と位相が合わない。補間をやめるのでは
    // なく、生成した音の末尾を短くフェードして段差を消す (穴の残りは無音でよい)
    let fade = if tail_period.correlation_squared
        < TIME_STRETCH_CONCEAL_FADE_CORRELATION * TIME_STRETCH_CONCEAL_FADE_CORRELATION
    {
        // フェードの長さはサンプルレートから求める。no_std には f64::round が無いため、
        // 0.5 を足してから切り捨てる (0 以上の値では四捨五入と同じ)
        let fade_samples = ((f64::from(TIME_STRETCH_CONCEAL_FADE_MS) / 1_000.0
            * f64::from(sample_rate))
            + 0.5) as usize;
        target.min(fade_samples.max(1))
    } else {
        0
    };
    // 末尾の周期 1 つ分を、端数の周期の位相を保ったまま必要な長さまで繰り返す
    let start = length as f64 - tail_period.samples;
    for (channel, destination) in channels.iter().zip(output.iter_mut()) {
        for (index, sample) in destination.iter_mut().take(target).enumerate() {
            // 0 以上の値では整数への切り捨てが小数点以下の切り捨て (floor) と同じになる
            let periods = (index as f64 / tail_period.samples) as usize;
            let phase = index as f64 - periods as f64 * tail_period.samples;
            // 長い補間ほど末尾の振幅を下げる (繰り返しの音を目立たなくする)
            let gain = 1.0 - (1.0 - end_gain) * ((index as f32 + 1.0) / target as f32);
            *sample = read_repeated(channel, start, phase) * gain;
        }
        if fade > 0 {
            // 末尾を 0 へ落とす (最後のサンプルが 0 になる)
            for index in 0..fade {
                let gain = 1.0 - (index as f32 + 1.0) / fade as f32;
                destination[target - fade + index] *= gain;
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
}
