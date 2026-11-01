//! 再生の隙間を材料化するモジュールのテスト
//!
//! 公開 API (`gap::materialize`) の契約を確認する。隙間が要求した長さちょうどで作られる
//! こと、周期補間できた分だけが補間され、周期を推定できない音では線形予測の続き音で埋まる
//! こと、残りが無音で埋まること、境界のフェードで隙間の最初と最後、および今回の音の先頭が
//! 連続することを確かめる。

use shiguredo_moqt::playout::gap::{AUDIO_GAP_FADE_US, materialize};
use shiguredo_moqt::playout::stretch;

/// 1 極フィルタを通した有色雑音を作る (周期を推定できない音)
fn colored_noise(length: usize) -> Vec<f32> {
    let mut state = 731u32;
    let mut previous = 0.0f64;
    (0..length)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            previous = previous * 0.6 + (f64::from(state) / 4_294_967_295.0 - 0.5) * 0.1;
            previous as f32
        })
        .collect()
}

/// 周期を推定できない音では、線形予測で作った続き音で隙間を埋めること
///
/// 周期補間は周期が確かな音でしか働かない。それ以外を無音のまま残すと、出力の段差
/// (クリック) になるため、moqt-js の出力側補間と同じく線形予測の続き音で埋める。
#[test]
fn materialize_fills_an_unvoiced_gap_from_the_previous_sound() {
    let sample_rate = 48_000;
    // 周期が無い有色雑音の直前の音 (30 ms)
    let previous = vec![colored_noise(1_440)];
    // 周期補間では埋められないことを前提として確かめる
    let mut periodic_output = [vec![0.0f32; 960]];
    {
        let sources: [&[f32]; 1] = [previous[0].as_slice()];
        let mut destinations: Vec<&mut [f32]> = periodic_output
            .iter_mut()
            .map(|channel| channel.as_mut_slice())
            .collect();
        assert_eq!(
            stretch::conceal(&sources, &mut destinations, sample_rate, 20_000),
            0,
            "周期補間できない音を使うこと"
        );
    }
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
    assert_eq!(
        gap.concealed_us, 20_000,
        "線形予測の続き音で要求どおり埋めること"
    );
    assert!(
        gap.channels[0].iter().any(|sample| *sample != 0.0),
        "無音のままにせず続き音を作ること"
    );
    assert!(
        gap.channels[0].iter().all(|sample| sample.is_finite()),
        "生成した音が有限であること"
    );
    // 続き音は解析に使った直前の音 (有色雑音) の RMS とピークを超えない
    let previous_peak = previous[0]
        .iter()
        .fold(0.0f32, |maximum, sample| maximum.max(sample.abs()));
    let peak = gap.channels[0]
        .iter()
        .fold(0.0f32, |maximum, sample| maximum.max(sample.abs()));
    assert!(
        peak <= previous_peak + 1e-6,
        "直前の音のピークを超えないこと: {peak} 対 {previous_peak}"
    );
    assert_ne!(
        gap.channels[0][959], 0.0,
        "続き音が今回の音の手前まで続くこと"
    );
    assert_ne!(current[0][0], 0.0, "今回の音の先頭を続き音から繋ぐこと");
}

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
    // 隙間の先頭は続き音の先頭をそのまま使う (moqt-js の出力側補間は先頭にフェードを
    // 掛けない)。直前の音の末尾 20 ms を解析対象にした生の続き音と一致すること
    let current_window = &previous[0][4_800 - 960..];
    let mut raw = vec![0.0f32; 4_800];
    let filled = stretch::conceal(
        &[current_window],
        &mut [raw.as_mut_slice()],
        sample_rate,
        100_000,
    );
    assert_eq!(filled, 4_800, "生の続き音を生成できること");
    let fade = fade_samples(sample_rate);
    assert_eq!(
        &gap.channels[0][..fade],
        &raw[..fade],
        "隙間の先頭 2 ms をフェードで歪めないこと"
    );
    // 周期どおりに繰り返している
    for index in 0..(960 - 120) {
        let difference = (gap.channels[0][index] - gap.channels[0][index + 120]).abs();
        assert!(
            difference < 0.05,
            "{index} 番目が周期ぶん後の音と一致すること: {difference}"
        );
    }
    // 続き音が隙間の後ろまで続くため、隙間の末尾を 0 へ落とさず今回の音へ繋ぐ
    assert_ne!(
        gap.channels[0][959], 0.0,
        "続き音が今回の音の手前まで続くこと: {}",
        gap.channels[0][959]
    );
    assert_ne!(current[0][0], 0.0, "今回の音の先頭を続き音から繋ぐこと");
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
        assert_ne!(
            channel[959], 0.0,
            "続き音が今回の音の手前まで続くこと (全チャンネル)"
        );
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

/// 隙間の先頭フェードが補間した続き音の先頭を歪めないこと
///
/// moqt-js の出力側補間は欠落区間の先頭にフェードを掛けず、生成した続き音をそのまま鳴らす。
/// 旧実装は先頭 2 ms を最初のサンプルへ寄せており、400 Hz 矩形波の続き音の 60 サンプル目が
/// -0.4968 から -0.1334 へ歪んでいた。
#[test]
fn materialize_keeps_the_continuation_head_unfaded() {
    let sample_rate = 48_000;
    // 400 Hz 矩形波 (周期 120) の 30 ms を直前の音にする
    let previous = vec![periodic_audio(120, 1_440)];
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
    assert_eq!(gap.concealed_us, 20_000);
    // 直前の音の末尾 20 ms を解析対象にした生の続き音
    let current_window = &previous[0][1_440 - 960..];
    let mut raw = vec![0.0f32; 4_800];
    let filled = stretch::conceal(
        &[current_window],
        &mut [raw.as_mut_slice()],
        sample_rate,
        100_000,
    );
    assert_eq!(filled, 4_800);
    let fade = fade_samples(sample_rate);
    assert_eq!(
        &gap.channels[0][..fade],
        &raw[..fade],
        "隙間の先頭 2 ms が生の続き音と一致すること (フェードを掛けない)"
    );
    // レビューの実測: 生の続き音の 60 サンプル目は -0.4968 である
    assert!(
        (raw[60] + 0.4968).abs() < 0.01,
        "生の続き音の 60 サンプル目が -0.4968 付近であること: {}",
        raw[60]
    );
    assert_eq!(
        gap.channels[0][60], raw[60],
        "続き音の 60 サンプル目を歪めないこと"
    );
}

/// 続き音が今回の音の手前まで続くときは、隙間の末尾を 0 にせずクロスフェードで今回の音へ
/// 戻ること
///
/// 両方を 0 へ落とす方式だと 2 ms の谷ができる。moqt-js の HEAD は続き音のまま実音の先頭へ
/// クロスフェードする (2.6 の設計意図)。
#[test]
fn materialize_cross_fades_into_the_current_sound() {
    let sample_rate = 48_000;
    // 170 Hz の正弦波 (周期は 960 サンプルの倍数ではない) を直前の音にする
    let previous = vec![sine_audio(170.0, sample_rate, 4_800)];
    let original_current = vec![0.5f32; 960];
    let mut current = [original_current.clone()];
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
    assert_eq!(gap.concealed_us, 20_000);
    let fade = fade_samples(sample_rate);
    assert_ne!(gap.channels[0][959], 0.0, "隙間の末尾を 0 へ落とさないこと");
    // クロスフェードの終わりでは今回の音がそのまま残る
    assert_eq!(
        current[0][fade], original_current[fade],
        "落とす長さの外は今回の音のままであること"
    );
    assert_eq!(
        current[0][959], original_current[959],
        "末尾は今回の音のまま"
    );
    // 先頭は続き音から始まる (正弦波の位相により 0.5 とは一致しない)
    assert!(
        (current[0][0] - original_current[0]).abs() > 1e-3,
        "今回の音の先頭を続き音から繋ぐこと: {}",
        current[0][0]
    );
}

/// 続き音が隙間を埋めきれない (100 ms を超える欠落) ときは、従来どおり両端を 0 へ落とすこと
#[test]
fn materialize_zeroes_the_edges_when_the_continuation_is_too_short() {
    let sample_rate = 48_000;
    let previous = vec![periodic_audio(120, 4_800)];
    let original_current = vec![0.5f32; 960];
    let mut current = [original_current];
    let mut channel_slices: Vec<&mut [f32]> = current
        .iter_mut()
        .map(|channel| channel.as_mut_slice())
        .collect();
    // 120 ms の隙間は 100 ms の続き音では埋めきれない
    let gap = materialize(
        Some(&previous),
        &mut channel_slices,
        sample_rate,
        120_000,
        120_000,
    );
    assert_eq!(gap.channels[0].len(), 5_760);
    assert_eq!(
        gap.channels[0][5_759], 0.0,
        "隙間の最後のサンプルは 0 へ落ちること"
    );
    assert_eq!(current[0][0], 0.0, "今回の音の先頭は 0 から始まること");
}

/// 20 ms の窓では周期を推定できないが、30 ms の履歴なら推定できる音でも補間すること
///
/// 80 Hz の周期 600 サンプルは 4 kHz の 50 サンプルであり、20 ms (960 サンプル) の窓では
/// ずらし幅の上限 (40) を超える。30 ms (1,440 サンプル) なら上限 (60) に収まる。
#[test]
fn materialize_uses_the_long_history_when_the_short_window_fails() {
    let sample_rate = 48_000;
    let previous = vec![sine_audio(80.0, sample_rate, 1_440)];
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
    assert_eq!(
        gap.concealed_us, 20_000,
        "長い履歴で周期を推定して補間すること"
    );
    assert!(
        gap.channels[0].iter().any(|sample| *sample != 0.0),
        "無音で埋めないこと"
    );
    // 周期 600 サンプルで繰り返している (先頭フェードは無い)
    for index in 0..(960 - 600) {
        let difference = (gap.channels[0][index] - gap.channels[0][index + 600]).abs();
        assert!(
            difference < 0.05,
            "{index} 番目が周期ぶん後の音と一致すること: {difference}"
        );
    }
}

/// 周波数 `frequency_hz` の正弦波を作る
fn sine_audio(frequency_hz: f64, sample_rate: u32, length: usize) -> Vec<f32> {
    (0..length)
        .map(|index| {
            (0.5 * (2.0 * std::f64::consts::PI * frequency_hz * index as f64
                / f64::from(sample_rate))
            .sin()) as f32
        })
        .collect()
}
