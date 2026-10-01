//! 波形の周期による音声の時間圧縮と時間伸長
//!
//! 到着の間隔が揺らいだ音声を目標の時刻に合わせるため、波形の周期 (ピッチ) 1 つ分を
//! 削ったり挿したりして音の長さをわずかに変える。周期は第 1 チャンネルを 4 kHz へ
//! 間引いた自己相関から求める。時刻や出力デバイスには触れない。

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
}
