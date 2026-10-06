//! 波形の周期による音声の時間圧縮・時間伸長と、欠落した区間の補間
//!
//! 到着の間隔が揺らいだ音声を目標の時刻に合わせるため、波形の周期 (ピッチ) 1 つ分を
//! 削ったり挿したりして音の長さをわずかに変える。音声が欠落した区間は、直前の音の末尾の
//! 周期を繰り返して埋める。周期は第 1 チャンネルを 4 kHz へ間引いた自己相関から求める。
//! 時刻や出力デバイスには触れない。

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

/// 末尾で繰り返している長さの探索の結果
struct TailPeriod {
    /// 末尾で繰り返している長さ (4 kHz のサンプル数)。見つからなければ 0
    lag: usize,
    /// 末尾の 2 周期分の正規化相関の二乗
    ///
    /// [`analyze`] と同じく、閾値との比較を平方根を取らずに済ませるため、二乗のまま持つ
    /// ([`TIME_STRETCH_CORRELATION_THRESHOLD`] の二乗と比べる)。
    correlation_squared: f64,
}

/// 末尾で繰り返している長さ (4 kHz のサンプル数) を探す
///
/// 末尾の 2 周期分どうしを比べ、正規化した相関の二乗が最も大きいずらし幅を返す。
/// [`find_lag`] が操作する位置の前の相関窓で探すのに対し、繰り返しの継ぎ目になる末尾を
/// 直接評価する。逆向きの波形 (相関が負) と、振幅の無い区間は周期とみなさない。2 周期分の
/// 末尾が取れない (音が短すぎる) ときは lag 0 を返す。`tail` は末尾を切り出した間引い後の
/// 信号であり、先頭が `tail.len()` サンプル前の位置に対応する。
fn find_tail_lag(tail: &[f32]) -> TailPeriod {
    let end = tail.len();
    let mut best_lag = 0;
    let mut best_score = 0.0f64;
    // 2 周期分が末尾に収まるずらし幅だけを探す
    for lag in TIME_STRETCH_MIN_LAG..=TIME_STRETCH_MAX_LAG.min(end / 2) {
        let mut dot = 0.0f64;
        let mut energy_before = 0.0f64;
        let mut energy_after = 0.0f64;
        for (before, after) in tail[end - 2 * lag..end - lag]
            .iter()
            .zip(&tail[end - lag..end])
        {
            let before = f64::from(*before);
            let after = f64::from(*after);
            dot += before * after;
            energy_before += before * before;
            energy_after += after * after;
        }
        let energy = energy_before * energy_after;
        // 逆向きの波形 (相関が負) と、振幅が無い区間は周期とみなさない
        if dot <= 0.0 || energy <= 0.0 {
            continue;
        }
        let score = dot * dot / energy;
        if score > best_score {
            best_score = score;
            best_lag = lag;
        }
    }
    TailPeriod {
        lag: best_lag,
        correlation_squared: best_score,
    }
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

/// 繰り返しの継ぎ目の段差が、末尾の周期の自然な段差と比べて大きすぎないかを確かめる
///
/// 段差は、末尾の最後のサンプルと、繰り返しの先頭になる 1 周期前のサンプルの差である。
/// 周期がずれている音ではここが大きくなり、クリックとして聞こえる。`period` は 1 以上
/// `reference` の長さ以下であること。
fn is_seam_smooth(reference: &[f32], period: usize) -> bool {
    let length = reference.len();
    let mut natural_step = 0.0f32;
    for (before, after) in reference[length - period + 1..]
        .iter()
        .zip(&reference[length - period..])
    {
        natural_step = natural_step.max((*before - *after).abs());
    }
    let seam_step = (reference[length - period] - reference[length - 1]).abs();
    seam_step <= natural_step * TIME_STRETCH_MAX_SEAM_STEP_RATIO
}

/// 操作するピッチの周期を求める (操作しないときは None)
///
/// 波形が繰り返していない場所で切ると耳につくため、相関が足りないときは操作しない。
/// 無音は詰めても伸ばしても聞こえないため、そのまま操作する。無音かどうかは、相関が
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
/// # 戻り値
///
/// 長さの変化 (サンプル数)。負が圧縮、0 が操作なし。0 のときバッファは変更しない。
/// 0 になるのは、対応外のサンプルレート、空または長さの揃わないチャンネル、音が
/// 短すぎるとき、相関が閾値未満で無音でもないとき。
pub fn compress(channels: &mut [&mut [f32]], sample_rate: u32) -> isize {
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

/// 音声を時間伸長する (ピッチ周期 1 つ分を挿す)
///
/// `channels` はチャンネルごとの音声で、すべて同じ長さであること。サンプルはおおむね
/// `[-1.0, 1.0]` に正規化された有限値であること。結果は `output` へ書く (`output` の
/// 長さと順序は `channels` に対応させる)。`output` の各要素には、元の長さに
/// `TIME_STRETCH_MAX_LAG` (60) × 間引き率を足した長さ以上を用意すること。間引き率は
/// 8 kHz / 16 kHz / 32 kHz / 48 kHz でそれぞれ 2 / 4 / 8 / 12。
///
/// # 戻り値
///
/// 長さの変化 (サンプル数)。正が伸長、0 が操作なし。0 のとき `output` には何も書かない。
/// 0 になるのは、[compress] と同じ条件に加え、`output` の長さが `channels` と合わない
/// とき、`output` が短すぎるとき。
pub fn expand(channels: &[&[f32]], output: &mut [&mut [f32]], sample_rate: u32) -> isize {
    if channels.is_empty() {
        return 0;
    }
    let length = channels[0].len();
    if length == 0
        || channels.iter().any(|channel| channel.len() != length)
        || output.len() != channels.len()
    {
        return 0;
    }
    let reference: &[f32] = channels[0];
    let splice = length / 2;
    let pitch = match pitch_period_of(reference, sample_rate, splice, || {
        is_silent(channels.iter().copied())
    }) {
        Some(pitch) => pitch,
        None => return 0,
    };
    let added = pitch.samples;
    if output.iter().any(|channel| channel.len() < length + added) {
        return 0;
    }
    for (channel, destination) in channels.iter().zip(output.iter_mut()) {
        // 切れ目までと、挿す分 (切れ目の直前の周期) を写す
        destination[..splice + added].copy_from_slice(&channel[..splice + added]);
        // 挿した分の先頭を、切れ目の直前の音とクロスフェードする
        cross_fade(
            &mut destination[splice..splice + added],
            &channel[splice - added..splice],
            added,
        );
        // 切れ目より後を写す
        destination[splice + added..length + added].copy_from_slice(&channel[splice..length]);
    }
    added as isize
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
/// 周期は末尾の 2 周期分の相関から求める ([compress] や [expand] が操作する位置の前の
/// 相関窓で求めるのとは別。繰り返しの継ぎ目は末尾にあるため、末尾の周期を直接評価する)。
/// 周期どおりに繰り返せているときは、繰り返しの先頭が末尾の続きになるため継ぎ目は波形が
/// 連続する。埋めた音の末尾の振幅は [concealment_end_gain] まで徐々に下げる (繰り返しの
/// 音を目立たなくする)。
///
/// # 戻り値
///
/// 埋めた長さ (サンプル数)。0 のとき `output` には何も書かない。0 になるのは、対応外の
/// サンプルレート、空または長さの揃わないチャンネル、`output` の長さが合わないか足りない
/// とき、埋める長さが 0 以下のとき、無音に近いとき、末尾に周期が無いとき、末尾の相関が
/// [`TIME_STRETCH_CORRELATION_THRESHOLD`] 未満のとき、継ぎ目の段差が
/// [`TIME_STRETCH_MAX_SEAM_STEP_RATIO`] を超えるとき。
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
    // 無音に近い入力では相関が常に 0 になり周期が求まらない。埋めても無音になるため埋めない
    if is_silent(channels.iter().copied()) {
        return 0;
    }
    let Some(filter) = downsample_filter(sample_rate) else {
        return 0;
    };
    let reference: &[f32] = channels[0];
    // 末尾の周期は 4 kHz の 2 周期分 (TIME_STRETCH_MAX_LAG の 2 倍) を見れば足りる
    let count = downsampled_len(length, &filter);
    let tail_len = (2 * TIME_STRETCH_MAX_LAG).min(count);
    let sum = coefficient_sum(&filter);
    let mut tail = [0.0f32; 2 * TIME_STRETCH_MAX_LAG];
    for (index, sample) in tail.iter_mut().take(tail_len).enumerate() {
        *sample = downsampled_at(reference, &filter, sum, count - tail_len + index);
    }
    let tail_period = find_tail_lag(&tail[..tail_len]);
    if tail_period.lag == 0 {
        // 末尾に周期が無い (音が短すぎる)
        return 0;
    }
    if tail_period.correlation_squared
        < TIME_STRETCH_CORRELATION_THRESHOLD * TIME_STRETCH_CORRELATION_THRESHOLD
    {
        // 末尾で波形が繰り返していない
        return 0;
    }
    let period = tail_period.lag * filter.factor;
    if !is_seam_smooth(reference, period) {
        return 0;
    }
    if output.iter().any(|channel| channel.len() < target) {
        return 0;
    }
    let end_gain = concealment_end_gain(conceal_us);
    for (channel, destination) in channels.iter().zip(output.iter_mut()) {
        // 末尾の周期 1 つ分を、必要な長さまで位相を保ったまま繰り返す
        let source = &channel[length - period..];
        for (index, sample) in destination.iter_mut().take(target).enumerate() {
            // 長い補間ほど末尾の振幅を下げる (繰り返しの音を目立たなくする)
            let gain = 1.0 - (1.0 - end_gain) * ((index as f32 + 1.0) / target as f32);
            *sample = source[index % period] * gain;
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
