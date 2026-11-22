#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use shiguredo_moqt::playout::stretch::{
    compress, compress_with_history, conceal, preemptively_expand,
};

/// 時間圧縮 / 伸長 API に与える入力
#[derive(Arbitrary, Debug)]
struct Input {
    /// サンプルレートの選択 (対応外も含む)
    rate_index: u8,
    /// チャンネル数 (0..=2)
    channel_count: u8,
    /// 入力長 (0..=2000)
    length: u16,
    /// 入力サンプルのビット列
    samples: Vec<u32>,
    /// 履歴の長さ (0..=2000)
    history_len: u16,
    /// 履歴サンプルのビット列
    history: Vec<u32>,
    /// 操作で許す最大変化長
    max_change: u16,
    /// conceal する長さ (us)
    conceal_us: i32,
    /// conceal の出力長
    output_len: u16,
    /// チャンネル長を揃えないか
    mismatch: bool,
    /// 履歴のチャンネル長を揃えないか
    history_mismatch: bool,
}

/// サンプル列を作る
fn channel(bits: &[u32], length: usize) -> Vec<f32> {
    (0..length)
        .map(|i| f32::from_bits(bits.get(i).copied().unwrap_or(0)))
        .collect()
}

fuzz_target!(|input: Input| {
    const RATES: [u32; 7] = [8_000, 16_000, 32_000, 44_100, 48_000, 0, 96_000];
    let rate = RATES[usize::from(input.rate_index) % RATES.len()];
    let channels_len = usize::from(input.channel_count % 3);
    let length = usize::from(input.length) % 2_001;
    let max_change = usize::from(input.max_change) % 2_001;
    let history_len = usize::from(input.history_len) % 2_001;
    let conceal_us = i64::from(input.conceal_us.clamp(-200_000, 200_000));
    // conceal が要求するサンプル数 (0.5 サンプルを足して切り捨てる)
    let target = if conceal_us > 0 && rate > 0 {
        usize::try_from(
            conceal_us
                .saturating_mul(i64::from(rate))
                .saturating_add(500_000)
                / 1_000_000,
        )
        .unwrap_or(0)
    } else {
        0
    };
    let output_len = (usize::from(input.output_len) % 20_001).max(target);

    let mut owned: Vec<Vec<f32>> = (0..channels_len)
        .map(|index| {
            let len = if input.mismatch && index > 0 {
                length.saturating_add(1)
            } else {
                length
            };
            channel(&input.samples, len)
        })
        .collect();
    let finite = owned.iter().flatten().all(|sample| sample.is_finite());

    // 時間圧縮: 返す長さは 0 以下で、削る長さは上限と入力長を超えない
    let mut refs: Vec<&mut [f32]> = owned.iter_mut().map(Vec::as_mut_slice).collect();
    let removed = compress(&mut refs, rate, max_change);
    assert!(removed <= 0);
    assert!(removed.unsigned_abs() <= max_change.min(length));

    // 履歴付きの時間圧縮も同じ契約
    let history: Vec<Vec<f32>> = (0..channels_len)
        .map(|index| {
            let len = if input.history_mismatch && index > 0 {
                history_len.saturating_add(1)
            } else {
                history_len
            };
            channel(&input.history, len)
        })
        .collect();
    let mut refs: Vec<&mut [f32]> = owned.iter_mut().map(Vec::as_mut_slice).collect();
    let removed = compress_with_history(&mut refs, rate, max_change, &history);
    assert!(removed <= 0);
    assert!(removed.unsigned_abs() <= max_change.min(length));

    // 先制的な時間伸長: 追加長は上限以下で、追加したときは出力長が入力 + 追加長
    let expansion = preemptively_expand(&owned, &history, rate, max_change);
    assert!(expansion.added_samples <= max_change);
    if expansion.added_samples == 0 {
        assert!(expansion.channels.is_empty());
    } else {
        assert_eq!(expansion.channels.len(), channels_len);
        assert!(expansion
            .channels
            .iter()
            .all(|channel| channel.len() == length + expansion.added_samples));
    }

    // conceal: 返す長さは 0 または要求した出力長で、出力長を超えない
    if finite {
        let source: Vec<&[f32]> = owned.iter().map(Vec::as_slice).collect();
        let mut output: Vec<Vec<f32>> = vec![vec![0.0; output_len]; channels_len];
        let mut output_refs: Vec<&mut [f32]> = output.iter_mut().map(Vec::as_mut_slice).collect();
        let filled = conceal(&source, &mut output_refs, rate, conceal_us);
        assert!(
            filled == 0 || filled as usize == target,
            "conceal は 0 または要求長を返す: filled={filled}, target={target}"
        );
    }
});
