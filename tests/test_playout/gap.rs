//! 再生の隙間を材料化するモジュールのテスト
//!
//! 公開 API (`gap::materialize`) の契約を確認する。隙間が要求した長さちょうどで作られる
//! こと、補間できた分だけが補間され、残りが無音で埋まること、境界のフェードで隙間の最初と
//! 最後、および今回の音の先頭が連続することを確かめる。

use shiguredo_moqt::playout::gap::{AUDIO_GAP_FADE_US, materialize};

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

/// 48 kHz の 2 ms のフェードのサンプル数
fn fade_samples(sample_rate: u32) -> usize {
    // 0.5 サンプルを足してから切り捨てる (0 以上の値では四捨五入と同じ)
    ((AUDIO_GAP_FADE_US.saturating_mul(i64::from(sample_rate)) + 500_000) / 1_000_000) as usize
}

/// 要求した長さちょうどの隙間が作られること
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
    assert_eq!(gap.channels[0][959], 0.0, "隙間の最後は 0 になること");
    assert_eq!(current[0][0], 0.0, "今回の音の先頭は 0 から始まること");
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
        assert!(
            gap.channels.is_empty(),
            "隙間の長さが 0 以下、またはサンプルレートが 0 なら空を返すこと: gap_us={gap_us} sample_rate={sample_rate}"
        );
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
    // 隙間の先頭は直前の音の末尾の続き (1 周期前のサンプル) から始まる
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
    assert_eq!(gap.channels.len(), 1);
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

/// チャンネル数が今回の音と同じときは全チャンネルを補間すること
#[test]
fn materialize_conceals_all_channels() {
    let sample_rate = 48_000;
    let previous = vec![periodic_audio(120, 4_800), periodic_audio(120, 4_800)];
    let mut current = [vec![0.25f32; 960], vec![0.5f32; 960]];
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
    assert_eq!(gap.channels.len(), 2, "2 チャンネルぶん作ること");
    assert_eq!(gap.concealed_us, 20_000);
    for channel in &gap.channels {
        assert_eq!(channel.len(), 960);
        assert!(
            channel.iter().any(|sample| *sample != 0.0),
            "全チャンネルが補間で埋まること"
        );
        assert_eq!(channel[959], 0.0, "全チャンネルの最後は 0 になること");
    }
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
