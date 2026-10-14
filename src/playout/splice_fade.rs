//! 棄却で飛んだ境目をフェードで繋ぐ
//!
//! 配信側は追いつきのために遅れたフレームを捨てる。Opus のパケットは 20 ms であり、
//! パケットの途中で捨てると符号化器へ渡る波形が飛ぶため、1 回ごとにクリックとして聞こえる。
//! 捨てる直前のフレームの末尾と、捨てた直後のフレームの先頭を 0 へ落とし、繋ぎ目を滑らかに
//! する。
//!
//! 呼び出し側は符号化へ渡すフレームを 1 つ保持し、次のフレームの判定が「捨てる」なら保持して
//! いるフレームの末尾を、判定が「捨てる」の直後なら先頭を、落としてから符号化へ渡す。
//! 先読みの分だけ符号化が 1 フレーム遅れる。

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
