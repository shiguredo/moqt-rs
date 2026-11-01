//! 音声の境目をフェードで繋ぐ
//!
//! 配信側は追いつきのために遅れたフレームを捨てる。Opus のパケットは 20 ms であり、
//! パケットの途中で捨てると符号化器へ渡る波形が飛ぶため、1 回ごとにクリックとして聞こえる。
//! 捨てる直前のフレームの末尾と、捨てた直後のフレームの先頭を 0 へ落とし、繋ぎ目を滑らかに
//! する。呼び出し側は符号化へ渡すフレームを 1 つ保持し、次のフレームの判定が「捨てる」なら
//! 保持しているフレームの末尾を、判定が「捨てる」の直後なら先頭を、落としてから符号化へ
//! 渡す。先読みの分だけ符号化が 1 フレーム遅れる。
//!
//! 出力側の接続にも使う。短い隙間を直前の末尾から次の先頭への直線で繋ぐ
//! ([`interpolate_splice_gap`])、補間の続きから実音の先頭へ移る ([`cross_fade_head`])、
//! 周期補間の先頭にだけ段差があれば接続を滑らかにする ([`smooth_splice_head`])、
//! チャンネルごとの音声をフェードしてから `f32-planar` の並びへ戻す
//! ([`merge_faded_planes`]) という部品を持つ。

use alloc::vec;
use alloc::vec::Vec;

/// 落とす長さ (マイクロ秒)
///
/// 8 ms のフレームの半分より短くし、音を削りすぎない長さである。20 ms のフレームでは
/// 10 分の 1 であり、聞こえる欠落にならない。
pub const AUDIO_SPLICE_FADE_US: i64 = 2_000;

/// 落とす端
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSpliceEdge {
    /// 先頭を落とす (捨てた直後のフレーム)
    Head,
    /// 末尾を落とす (捨てる直前のフレーム)
    Tail,
}

/// 落とすサンプル数を返す (1 以上、`length` 以内)
///
/// サンプルレートから求めて四捨五入する。バッファが落とす長さより短いときは、バッファの
/// 全体をフェードする。空のバッファでは 0 を返す。
pub fn splice_fade_samples(length: usize, sample_rate: u32) -> usize {
    if length == 0 {
        return 0;
    }
    // サンプル数 = 長さ (マイクロ秒) * サンプルレート / 1_000_000 を四捨五入する
    let samples = AUDIO_SPLICE_FADE_US
        .saturating_mul(i64::from(sample_rate))
        .saturating_add(500_000)
        / 1_000_000;
    let samples = usize::try_from(samples).unwrap_or(usize::MAX).max(1);
    samples.min(length)
}

/// 端を線形に 0 へ落とす (落とす長さの外のサンプルは変えない)
///
/// 端のサンプルは 0 になり、落とす長さだけ離れたサンプルは元の値のままである。チャンネルごとに
/// 同じ処理を行う。
pub fn fade_splice_edge(channels: &mut [&mut [f32]], sample_rate: u32, edge: AudioSpliceEdge) {
    let Some(length) = channels.first().map(|channel| channel.len()) else {
        return;
    };
    let fade_samples = splice_fade_samples(length, sample_rate);
    if fade_samples == 0 {
        return;
    }
    for channel in channels.iter_mut() {
        // チャンネルごとに長さが違っても、それぞれの端から落とす
        let fade_samples = fade_samples.min(channel.len());
        if fade_samples == 0 {
            continue;
        }
        for index in 0..fade_samples {
            // 0 から 1 へ戻る係数。端は 0、落とす長さだけ離れた位置は元の値になる
            let gain = index as f32 / fade_samples as f32;
            let position = match edge {
                AudioSpliceEdge::Head => index,
                AudioSpliceEdge::Tail => channel.len() - 1 - index,
            };
            channel[position] *= gain;
        }
    }
}

/// 端を線形に 0 へ落とす (16 bit PCM)
///
/// [`fade_splice_edge`] と同じ規則を 16 bit PCM に対して行う。符号化器へ渡す音声は
/// 16 bit PCM であり、浮動小数点数へ変換して戻すと量子化の誤差が乗るため、整数のまま
/// 丸める。
pub fn fade_splice_edge_i16(channels: &mut [&mut [i16]], sample_rate: u32, edge: AudioSpliceEdge) {
    let Some(length) = channels.first().map(|channel| channel.len()) else {
        return;
    };
    let fade_samples = splice_fade_samples(length, sample_rate);
    if fade_samples == 0 {
        return;
    }
    for channel in channels.iter_mut() {
        // チャンネルごとに長さが違っても、それぞれの端から落とす
        let fade_samples = fade_samples.min(channel.len());
        if fade_samples == 0 {
            continue;
        }
        for index in 0..fade_samples {
            let position = match edge {
                AudioSpliceEdge::Head => index,
                AudioSpliceEdge::Tail => channel.len() - 1 - index,
            };
            // 0 から 1 へ戻る係数を整数で掛ける。端は 0、落とす長さだけ離れた位置は元の値に
            // なる。`|sample| <= 32767` かつ `index < fade_samples` であるため、掛けた結果は
            // 元の絶対値を超えず 16 bit に収まる
            let sample = i32::from(channel[position]);
            let faded = sample.saturating_mul(index as i32) / fade_samples as i32;
            channel[position] = faded as i16;
        }
    }
}

/// 短い隙間を、直前の末尾から次の先頭への直線で繋ぐ
///
/// 周期性のない子音にも使える。長い欠落の復元には使わず、呼び出し側が長さを制限する。
/// 生成するサンプルは `previous` の各チャンネルの最終サンプル (無いときは 0) から `next` の
/// 先頭 (無いときは 0) へ向かう直線であり、`frames` サンプルぶんを `next` のチャンネル数だけ
/// 返す。
pub fn interpolate_splice_gap(
    previous: &[&[f32]],
    next: &[&[f32]],
    frames: usize,
) -> Vec<Vec<f32>> {
    next.iter()
        .enumerate()
        .map(|(channel_index, channel)| {
            let from = previous
                .get(channel_index)
                .and_then(|samples| samples.last())
                .copied()
                .unwrap_or(0.0);
            let to = channel.first().copied().unwrap_or(0.0);
            let from = f64::from(from);
            let to = f64::from(to);
            (0..frames)
                .map(|index| {
                    let weight = (index as f64 + 1.0) / (frames as f64 + 1.0);
                    (from + (to - from) * weight) as f32
                })
                .collect()
        })
        .collect()
}

/// 補間の続きから実音の先頭へ移る
///
/// 両方を 0 へ落とす方式と違い、同じ波形なら音量を保つ。補間の続きは実音の開始位置に
/// 対応するサンプル列であり、遅延や再生する長さは増やさない。入力の配列は変更しない。
///
/// 混ぜる長さは [`AUDIO_SPLICE_FADE_US`] をサンプル数へ四捨五入した長さと、`channels` の
/// 先頭チャンネルの長さ、`continuation` の先頭チャンネルの長さの最小値である。続きの
/// チャンネルが無いときは、その実音をそのまま複製して返す。チャンネルごとに `continuation`
/// が短いときは、足りない分を 0 として混ぜる (moqt-js の `previous[index] ?? 0` と同じ)。
pub fn cross_fade_head(
    continuation: &[&[f32]],
    channels: &[&[f32]],
    sample_rate: u32,
) -> Vec<Vec<f32>> {
    let fade_samples = channels
        .first()
        .map_or(0, |channel| channel.len())
        .min(continuation.first().map_or(0, |channel| channel.len()))
        .min(splice_fade_samples(usize::MAX, sample_rate));
    channels
        .iter()
        .enumerate()
        .map(|(channel_index, channel)| {
            let mut output = channel.to_vec();
            let Some(previous) = continuation.get(channel_index) else {
                return output;
            };
            for (index, sample) in output.iter_mut().take(fade_samples).enumerate() {
                // 重みは 0 から 1 へ動く。同じ波形なら値が変わらない。続きが足りない分は
                // 0 として扱う (範囲外を 0 にする moqt-js の `?? 0` と同じ)
                let weight = index as f64 / fade_samples as f64;
                let source = f64::from(previous.get(index).copied().unwrap_or(0.0));
                let mixed = source * (1.0 - weight) + f64::from(*sample) * weight;
                *sample = mixed as f32;
            }
            output
        })
        .collect()
}

/// 周期補間の先頭に、直前の音の変化幅を超える段差がある場合だけ接続を滑らかにする
///
/// 通常の波形へ毎回フェードを掛けず、既存の [`AUDIO_SPLICE_FADE_US`] 以内で補間波形へ戻す。
/// `previous` から求めた変化幅 (`natural_step`) を超える段差があるときだけ
/// [`cross_fade_head`] を掛ける。
pub fn smooth_splice_head(
    previous: &[&[f32]],
    channels: &[&[f32]],
    sample_rate: u32,
) -> Vec<Vec<f32>> {
    channels
        .iter()
        .enumerate()
        .map(|(channel_index, channel)| {
            let output = channel.to_vec();
            let Some(history) = previous.get(channel_index) else {
                return output;
            };
            if history.is_empty() || output.is_empty() {
                return output;
            }
            let mut natural_step = 0.0f64;
            for index in 1..history.len() {
                natural_step = natural_step
                    .max((f64::from(history[index]) - f64::from(history[index - 1])).abs());
            }
            let tail = history[history.len() - 1];
            if (f64::from(output[0]) - f64::from(tail)).abs() <= natural_step {
                return output;
            }
            let continuation = vec![tail; output.len()];
            let continuation_slices: [&[f32]; 1] = [continuation.as_slice()];
            let channel_slices: [&[f32]; 1] = [output.as_slice()];
            cross_fade_head(&continuation_slices, &channel_slices, sample_rate)
                .first()
                .cloned()
                .unwrap_or(output)
        })
        .collect()
}

/// フェードする端の指定
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AudioSpliceEdges {
    /// 先頭を 0 へ落とすか
    pub head: bool,
    /// 末尾を 0 へ落とすか
    pub tail: bool,
}

/// チャンネルごとの音声をフェードしてから 1 つのバッファへ戻す
///
/// [`fade_splice_edge`] はチャンネルごとの配列を扱うが、符号化へ渡す `AudioData` は
/// `f32-planar` の並び (チャンネルごとに連結した 1 つのバッファ) を取る。その変換とフェードを
/// まとめて行う。`planes` はすべて同じ長さであること。
///
/// # 戻り値
///
/// `f32-planar` の並びのバッファ。長さは「先頭チャンネルの長さ x チャンネル数」である。
pub fn merge_faded_planes(
    planes: &[&[f32]],
    sample_rate: u32,
    edges: AudioSpliceEdges,
) -> Vec<f32> {
    let Some(length) = planes.first().map(|plane| plane.len()) else {
        return Vec::new();
    };
    if length == 0 {
        // 長さ 0 の平面は空のバッファになる (moqt-js も長さ 0 を返す)
        return Vec::new();
    }
    let mut merged = vec![0.0f32; length * planes.len()];
    for (index, plane) in planes.iter().enumerate() {
        let destination = &mut merged[index * length..(index + 1) * length];
        // チャンネルの長さが先頭と違うときは、入る分だけ写す (moqt-js の set と同じ)
        let copy_length = plane.len().min(length);
        destination[..copy_length].copy_from_slice(&plane[..copy_length]);
    }
    for edge in [AudioSpliceEdge::Head, AudioSpliceEdge::Tail] {
        let enabled = match edge {
            AudioSpliceEdge::Head => edges.head,
            AudioSpliceEdge::Tail => edges.tail,
        };
        if !enabled {
            continue;
        }
        let mut channel_slices: Vec<&mut [f32]> = merged.chunks_mut(length).collect();
        fade_splice_edge(&mut channel_slices, sample_rate, edge);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cross_fade_head_keeps_the_continuation_at_the_start() {
        // 先頭は補間の続きのまま、混ぜる長さだけ離れた位置は実音になる
        let continuation = [0.25f32; 128];
        let channel = [0.75f32; 128];
        let output = cross_fade_head(&[continuation.as_slice()], &[channel.as_slice()], 16_000);
        assert_eq!(output[0][0], 0.25);
        assert_eq!(output[0][32], 0.75);
        assert_eq!(output[0][127], 0.75);
    }

    #[test]
    fn interpolate_splice_gap_moves_toward_the_next_head() {
        let previous = [0.2f32];
        let next = [-0.2f32];
        let output = interpolate_splice_gap(&[previous.as_slice()], &[next.as_slice()], 16);
        assert_eq!(output[0].len(), 16);
        // 直前の最終サンプルから次の先頭へ向かう直線になる
        assert!(output[0][0] < 0.2);
        assert!(output[0][15] > -0.2);
        for (before, after) in output[0].iter().zip(&output[0][1..]) {
            assert!(after < before, "次の先頭へ向かって単調に下がること");
        }
    }
}
