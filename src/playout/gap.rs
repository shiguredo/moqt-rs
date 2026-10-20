//! 再生の隙間を埋める音の材料化
//!
//! 音声の出力が FIFO (積んだ順に鳴らす) の場合 (`raw_player` のように PTS を再生時刻に
//! 使わない出力)、隙間を無音としても積まなければ、後続の音がその分だけ早く鳴り、出力が
//! 枯れて段差になる。そこで隙間の全長ぶん (要求した隙間のサンプル数ちょうど) の音を必ず
//! 作り、補間できた分は直前の音の周期の繰り返しで、できなかった分は無音で埋める。
//!
//! 境界はフェードで繋ぐ。隙間の末尾と今回の音の先頭は [`AUDIO_GAP_FADE_US`] で 0 へ落とし、
//! 隙間の先頭は直前の音の最終サンプル (補間したときは補間の先頭の値) から 0 へ向かう重みで
//! 埋める。これで隙間の最初と最後、および今回の音の先頭が段差にならない。
//!
//! 時刻や出力デバイスには触れない。サンプルは呼び出し側が渡す。

use alloc::vec;
use alloc::vec::Vec;

use crate::playout::stretch;

/// 隙間の境界をフェードする長さ (マイクロ秒)
///
/// 隙間の末尾と今回の音の先頭を 0 へ落とし、隙間の先頭を直前の音の最終サンプルから 0 へ
/// 落とす長さである。短すぎると段差が残り、長すぎると隙間の音 (補間した音) を削りすぎる。
pub const AUDIO_GAP_FADE_US: i64 = 2_000;

/// 隙間を埋めた結果
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MaterializedGap {
    /// チャンネルごとの隙間の音
    ///
    /// 本数は `current` の本数と同じであり、各チャンネルの長さは要求した隙間のサンプル数で
    /// ある。補間できなかった分は 0 で埋まっている。
    pub channels: Vec<Vec<f32>>,
    /// 補間で埋めた長さ (マイクロ秒)。補間しなかったときは 0
    ///
    /// 要求した長さ (`conceal_us`) 以下になる。隙間の先頭からこの長さぶんが補間の音である。
    pub concealed_us: i64,
}

/// マイクロ秒をサンプル数へ四捨五入する (0 以下のときは None)
///
/// `no_std` には `f64::round` が無いため、0.5 サンプルを足してから切り捨てる
/// (0 以上の値では四捨五入と同じ)。0 サンプルに丸まる長さは None にする。
fn rounded_samples(us: i64, sample_rate: u32) -> Option<usize> {
    if us <= 0 || sample_rate == 0 {
        return None;
    }
    // マイクロ秒 x サンプルレート を 1_000_000 で割るとサンプル数になる。先に掛けてから
    // 割ることで、整数の整除で丸まる順序を一定にする
    let scaled = us.saturating_mul(i64::from(sample_rate));
    let samples = usize::try_from(scaled.saturating_add(500_000) / 1_000_000).ok()?;
    (samples > 0).then_some(samples)
}

/// 隙間の長さ (サンプル数) を求める (作れないときは None)
///
/// マイクロ秒をサンプル数へ四捨五入する。1 サンプルに満たない長さでも 1 サンプルにする
/// (隙間は必ず材料化し、無音でも積む必要があるため)。長さが 0 以下、またはサンプルレートが
/// 0 のときは None。
fn gap_samples(gap_us: i64, sample_rate: u32) -> Option<usize> {
    if gap_us <= 0 || sample_rate == 0 {
        return None;
    }
    Some(rounded_samples(gap_us, sample_rate).unwrap_or(1))
}

/// フェードの長さ (サンプル数) を求める
///
/// [`AUDIO_GAP_FADE_US`] をサンプル数へ四捨五入する。サンプルレートが極端に低いときでも
/// フェードが 0 にならないよう、1 サンプル以上にする。
fn fade_samples(sample_rate: u32) -> usize {
    rounded_samples(AUDIO_GAP_FADE_US, sample_rate)
        .unwrap_or(1)
        .max(1)
}

/// サンプル数をマイクロ秒へ四捨五入する
///
/// `sample_rate` は 1 以上であること。
fn samples_to_us(samples: usize, sample_rate: u32) -> i64 {
    let samples = i64::try_from(samples).unwrap_or(i64::MAX);
    // 0.5 マイクロ秒を足してから割る (0 以上の値では四捨五入と同じ)
    samples
        .saturating_mul(1_000_000)
        .saturating_add(i64::from(sample_rate) / 2)
        / i64::from(sample_rate)
}

/// 直前の音を補間に使える形式か
///
/// [`stretch::conceal`] は全チャンネルが同じ長さで、空でないことを要求する。チャンネル数が
/// 今回の音と違うときも、補間した音をどのチャンネルへ書くか決められないため使わない。
fn is_concealable(previous: &[Vec<f32>], channels: usize) -> bool {
    if previous.len() != channels || previous.is_empty() {
        return false;
    }
    let length = previous[0].len();
    length > 0 && previous.iter().all(|channel| channel.len() == length)
}

/// 隙間の先頭へ繋ぐ、直前の音の最終サンプル
///
/// 直前の音が無い、または形式が合わないときは 0 (無音から始める)。
fn previous_last_sample(previous: Option<&[Vec<f32>]>, channel: usize, channels: usize) -> f32 {
    let Some(previous) = previous else {
        return 0.0;
    };
    if !is_concealable(previous, channels) {
        return 0.0;
    }
    previous[channel].last().copied().unwrap_or(0.0)
}

/// 隙間を埋める音を作る
///
/// `previous` は直前の音のチャンネルごとの音声、`current` はこれから鳴らす音のチャンネル
/// ごとの音声である。`current` の先頭 `min(2 ms, 長さ)` サンプルには、隙間の最後の無音から
/// 段差なく立ち上がるようフェードインを掛ける (そのため `&mut` で受け取る)。
///
/// 長さは `gap_us` をサンプル数へ四捨五入した長さ (1 サンプル以上) であり、返る
/// [`MaterializedGap::channels`] は `current` と同じ本数のチャンネルを持つ。隙間の音は
/// 先頭から `min(conceal_us, gap_us)` の範囲で [`stretch::conceal`] の補間で埋め、残りは
/// 無音にする。補間は `previous` があり、チャンネル数などの形式が合うときだけ行う。
///
/// # 戻り値
///
/// 隙間の音と、補間できた長さ。`gap_us` が 0 以下、`sample_rate` が 0、`current` が空の
/// ときは、空の [`MaterializedGap`] を返す。
pub fn materialize(
    previous: Option<&[Vec<f32>]>,
    current: &mut [&mut [f32]],
    sample_rate: u32,
    gap_us: i64,
    conceal_us: i64,
) -> MaterializedGap {
    let Some(length) = gap_samples(gap_us, sample_rate) else {
        return MaterializedGap::default();
    };
    if current.is_empty() {
        return MaterializedGap::default();
    }
    let channels = current.len();
    let mut gap = MaterializedGap {
        channels: (0..channels).map(|_| vec![0.0f32; length]).collect(),
        concealed_us: 0,
    };

    // 補間する。隙間より長い補間は書けないため、要求を隙間の全長までに切る
    let conceal_us = conceal_us.min(gap_us);
    if conceal_us > 0
        && let Some(previous) = previous
        && is_concealable(previous, channels)
    {
        let sources: Vec<&[f32]> = previous.iter().map(|channel| channel.as_slice()).collect();
        let mut destinations: Vec<&mut [f32]> = gap
            .channels
            .iter_mut()
            .map(|channel| channel.as_mut_slice())
            .collect();
        let filled = stretch::conceal(&sources, &mut destinations, sample_rate, conceal_us);
        if filled > 0 {
            let filled = usize::try_from(filled).unwrap_or(0).min(length);
            // 生成できた長さを要求以下に切って記録する
            gap.concealed_us = samples_to_us(filled, sample_rate).min(conceal_us);
        }
    }

    // 隙間の先頭: 直前の音の最終サンプル (補間したときは補間の先頭の値) から 0 へ向かう
    // 重みで、隙間の内容へ繋ぐ。この形にすると、直前の音の最終サンプルと隙間の先頭が
    // 段差にならず、補間した音の先頭も削らずに続けられる
    let fade = fade_samples(sample_rate);
    let head_fade = fade.min(length);
    if head_fade > 0 {
        for (channel, samples) in gap.channels.iter_mut().enumerate() {
            let boundary = if gap.concealed_us > 0 {
                // 補間した音の先頭は直前の音の末尾の続きである。その値から始める
                samples[0]
            } else {
                previous_last_sample(previous, channel, channels)
            };
            for (index, sample) in samples.iter_mut().enumerate().take(head_fade) {
                let weight = (index as f32 + 1.0) / head_fade as f32;
                *sample = *sample * weight + boundary * (1.0 - weight);
            }
        }
    }

    // 隙間の末尾: 0 へ線形に落とす。今回の音の先頭は 0 から立ち上がるため、隙間の最後の
    // サンプルを 0 にしておくと段差が無い。長さの半分より長くは落とさない (短い隙間で
    // 先頭のフェードと重なりすぎないようにする)
    let tail_fade = fade.min((length / 2).max(1));
    if tail_fade > 0 {
        for samples in gap.channels.iter_mut() {
            for index in 0..tail_fade {
                let gain = 1.0 - (index as f32 + 1.0) / tail_fade as f32;
                samples[length - tail_fade + index] *= gain;
            }
        }
    }

    // 今回の音の先頭: 0 から立ち上げる。隙間の最後が 0 であるため、ここも段差にならない
    for channel in current.iter_mut() {
        let current_fade = fade.min(channel.len());
        for (index, sample) in channel.iter_mut().enumerate().take(current_fade) {
            let gain = index as f32 / current_fade as f32;
            *sample *= gain;
        }
    }

    gap
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 周期のある音を作る
    ///
    /// `period` はサンプル数である。三角関数を使わず、矩形波で周期を作る (継ぎ目と周期の
    /// 判定には十分である)。
    fn periodic_audio(period: usize, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|index| {
                let phase = (index % period) as f32 / period as f32;
                if phase < 0.5 { 0.5 } else { -0.5 }
            })
            .collect()
    }

    /// 隙間が要求した長さちょうどで作られること
    #[test]
    fn materialize_fills_the_requested_length() {
        let mut current = [vec![1.0f32; 960]];
        let mut channel_slices: Vec<&mut [f32]> = current
            .iter_mut()
            .map(|channel| channel.as_mut_slice())
            .collect();
        let gap = materialize(None, &mut channel_slices, 48_000, 20_000, 20_000);
        assert_eq!(
            gap.channels.len(),
            1,
            "チャンネル数は今回の音と同じになること"
        );
        assert_eq!(
            gap.channels[0].len(),
            960,
            "20 ms は 48 kHz で 960 サンプル"
        );
        assert_eq!(gap.concealed_us, 0, "直前の音が無ければ補間しないこと");
        assert!(
            gap.channels[0].iter().all(|sample| *sample == 0.0),
            "補間しない隙間は無音で埋まること"
        );
    }

    /// 作れない入力では空を返すこと
    #[test]
    fn materialize_returns_nothing_for_unusable_inputs() {
        let mut current = [vec![1.0f32; 10]];
        let mut channel_slices: Vec<&mut [f32]> = current
            .iter_mut()
            .map(|channel| channel.as_mut_slice())
            .collect();
        for (gap_us, sample_rate) in [(0, 48_000), (-1, 48_000), (1_000, 0)] {
            let gap = materialize(None, &mut channel_slices, sample_rate, gap_us, 0);
            assert!(gap.channels.is_empty(), "作れない入力では空を返すこと");
            assert_eq!(gap.concealed_us, 0);
        }
        // 今回の音が空 (チャンネル数 0)
        let mut empty: [&mut [f32]; 0] = [];
        let gap = materialize(None, &mut empty, 48_000, 20_000, 20_000);
        assert!(gap.channels.is_empty(), "チャンネル数 0 なら空を返すこと");
    }

    /// 1 サンプルに満たない隙間も 1 サンプルとして材料化すること
    #[test]
    fn materialize_rounds_the_gap_to_at_least_one_sample() {
        let mut current = [vec![1.0f32; 4]];
        let mut channel_slices: Vec<&mut [f32]> = current
            .iter_mut()
            .map(|channel| channel.as_mut_slice())
            .collect();
        // 48 kHz で 10 µs は 0.48 サンプルであり、四捨五入すると 0 になるが 1 にする
        let gap = materialize(None, &mut channel_slices, 48_000, 10, 0);
        assert_eq!(gap.channels[0].len(), 1, "1 サンプル以上になること");
        assert_eq!(gap.channels[0][0], 0.0, "最後のサンプルは 0 になること");
    }

    /// 直前の音の末尾を周期で繰り返して隙間を埋めること
    #[test]
    fn materialize_conceals_from_the_previous_tail() {
        let sample_rate = 48_000;
        // 400 Hz は 48 kHz で 120 サンプル周期である
        let previous = vec![periodic_audio(120, 4_800)];
        let mut current = [vec![0.5f32; 960]];
        let mut channel_slices: Vec<&mut [f32]> = current
            .iter_mut()
            .map(|channel| channel.as_mut_slice())
            .collect();
        let gap = materialize(
            Some(&previous),
            &mut channel_slices,
            sample_rate,
            20_000,
            20_000,
        );
        assert_eq!(gap.channels[0].len(), 960);
        assert_eq!(gap.concealed_us, 20_000, "補間した長さを返すこと");
        // 隙間の先頭は直前の音の末尾の続き (周期の先頭) から始まる
        let expected = previous[0][4_800 - 120];
        assert!(
            (gap.channels[0][0] - expected).abs() < 0.01,
            "隙間の先頭が直前の音の末尾の続きであること: {} != {expected}",
            gap.channels[0][0]
        );
        // 周期どおりに繰り返している。先頭と末尾はフェードで形を変えているため除く
        let fade = fade_samples(sample_rate);
        for index in fade..(960 - fade - 120) {
            let difference = (gap.channels[0][index] - gap.channels[0][index + 120]).abs();
            assert!(
                difference < 0.05,
                "{index} 番目が周期ぶん後の音と一致すること: {difference}"
            );
        }
        assert_eq!(
            gap.channels[0][959], 0.0,
            "隙間の最後のサンプルは 0 へ落ちること"
        );
    }

    /// 補間しない隙間の先頭が直前の音の最終サンプルから 0 へ落ちること
    #[test]
    fn materialize_fades_the_gap_head_from_the_previous_last_sample() {
        let sample_rate = 48_000;
        // 最終サンプルが 1.0 の音 (補間は要求しない)
        let previous = vec![vec![1.0f32; 480]];
        let mut current = [vec![0.5f32; 480]];
        let mut channel_slices: Vec<&mut [f32]> = current
            .iter_mut()
            .map(|channel| channel.as_mut_slice())
            .collect();
        let gap = materialize(Some(&previous), &mut channel_slices, sample_rate, 10_000, 0);
        assert_eq!(gap.concealed_us, 0);
        let fade = fade_samples(sample_rate);
        assert_eq!(fade, 96, "2 ms は 48 kHz で 96 サンプル");
        // 先頭は直前の最終サンプルから始まり、fade サンプルで 0 へ落ちる
        assert!(
            (gap.channels[0][0] - (1.0 - 1.0 / fade as f32)).abs() < 1e-6,
            "先頭が直前の音の最終サンプルから始まること: {}",
            gap.channels[0][0]
        );
        assert_eq!(gap.channels[0][fade - 1], 0.0, "フェードの終わりは 0");
        assert!(
            gap.channels[0][fade..].iter().all(|sample| *sample == 0.0),
            "フェードの後は無音であること"
        );
    }

    /// 今回の音の先頭が 0 から立ち上がること
    #[test]
    fn materialize_fades_in_the_current_sound() {
        let sample_rate = 48_000;
        let mut current = [vec![1.0f32; 960]];
        let mut channel_slices: Vec<&mut [f32]> = current
            .iter_mut()
            .map(|channel| channel.as_mut_slice())
            .collect();
        let gap = materialize(None, &mut channel_slices, sample_rate, 10_000, 0);
        assert!(gap.channels.len() == 1);
        let fade = fade_samples(sample_rate);
        assert_eq!(current[0][0], 0.0, "今回の音の先頭は 0 から立ち上がること");
        assert!(
            (current[0][fade - 1] - (fade - 1) as f32 / fade as f32).abs() < 1e-6,
            "フェードの終わりの手前まで線形に立ち上がること"
        );
        assert_eq!(current[0][fade], 1.0, "フェードの後は元の音であること");
    }

    /// 隙間より長い補間の要求を隙間の長さに切ること
    #[test]
    fn materialize_clamps_the_concealment_to_the_gap() {
        let sample_rate = 48_000;
        let previous = vec![periodic_audio(120, 4_800)];
        let mut current = [vec![0.5f32; 480]];
        let mut channel_slices: Vec<&mut [f32]> = current
            .iter_mut()
            .map(|channel| channel.as_mut_slice())
            .collect();
        // 隙間は 10 ms だが補間は 20 ms を要求する
        let gap = materialize(
            Some(&previous),
            &mut channel_slices,
            sample_rate,
            10_000,
            20_000,
        );
        assert_eq!(gap.channels[0].len(), 480);
        assert_eq!(gap.concealed_us, 10_000, "隙間の長さに切ること");
    }

    /// チャンネル数が合わない直前の音は補間に使わないこと
    #[test]
    fn materialize_ignores_a_previous_sound_with_a_different_channel_count() {
        let sample_rate = 48_000;
        let previous = vec![periodic_audio(120, 4_800), periodic_audio(120, 4_800)];
        let mut current = [vec![0.5f32; 960]];
        let mut channel_slices: Vec<&mut [f32]> = current
            .iter_mut()
            .map(|channel| channel.as_mut_slice())
            .collect();
        let gap = materialize(
            Some(&previous),
            &mut channel_slices,
            sample_rate,
            20_000,
            20_000,
        );
        assert_eq!(gap.concealed_us, 0, "形式が合わない音は補間に使わないこと");
        assert!(
            gap.channels[0].iter().all(|sample| *sample == 0.0),
            "補間しない隙間は無音で埋まること"
        );
    }

    /// 短い隙間でも先頭と末尾のフェードが段差を作らないこと
    #[test]
    fn materialize_fades_a_short_gap() {
        let sample_rate = 48_000;
        // 1 ms (48 サンプル) の隙間。末尾のフェードは長さの半分 (24 サンプル) までにする
        let mut current = [vec![1.0f32; 480]];
        let mut channel_slices: Vec<&mut [f32]> = current
            .iter_mut()
            .map(|channel| channel.as_mut_slice())
            .collect();
        let gap = materialize(None, &mut channel_slices, sample_rate, 1_000, 0);
        assert_eq!(gap.channels[0].len(), 48);
        assert_eq!(
            gap.channels[0][47], 0.0,
            "短い隙間でも最後のサンプルは 0 へ落ちること"
        );
        assert_eq!(current[0][0], 0.0, "今回の音の先頭は 0 から始まること");
    }
}
