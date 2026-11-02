//! AAC-LC デコーダラッパ (Apple Audio Toolbox、macOS 限定)

use shiguredo_audio_toolbox::{Decoder, DecoderCodec, DecoderConfig};

use crate::error::{Error, Result};

/// AudioToolbox のデコーダ出力のチャンネル数
///
/// `shiguredo_audio_toolbox::Decoder` の出力はステレオ (2ch) 16bit PCM 固定であり、
/// 入力がモノラルでも AudioConverter が自動でアップミックスする。
const OUTPUT_CHANNELS: u8 = 2;

/// デコーダスレッドへの要求
struct DecodeRequest {
    /// 1 パケット分の圧縮データ (AAC-LC の raw アクセスユニット)
    payload: Vec<u8>,
    /// デコード結果 (S16 interleaved PCM) の返送先
    reply: tokio::sync::oneshot::Sender<Result<Vec<i16>>>,
}

/// AAC-LC デコーダのハンドル
///
/// `shiguredo_audio_toolbox::Decoder` は `Send` を実装しない (Apple が AudioConverter の
/// スレッド間移動を規範的に保証していないため)。一方、本型は
/// `Arc<tokio::sync::Mutex<AudioDecoder>>` として複数の stream task から共有され、
/// stream task は `tokio::spawn` で起動されるため `Send` でなければならない。
/// そこでデコーダの実体を専用スレッドが所有し、本型はチャネル越しにデコードを依頼する
/// `Send` なハンドルとする。
pub(crate) struct AacDecoder {
    /// デコード要求の送信側
    ///
    /// `Drop` では送信側を閉じてからスレッドを join する必要があるため `Option` で持つ
    /// (フィールドのままだと `Drop::drop` の実行中は閉じない)。
    request_tx: Option<tokio::sync::mpsc::UnboundedSender<DecodeRequest>>,
    /// デコーダを所有するスレッドのハンドル (Drop で join する)
    thread: Option<std::thread::JoinHandle<()>>,
    /// 出力のサンプルレート (Hz)
    ///
    /// AudioToolbox のデコーダは入力のサンプルレートと同じレートで出力する
    /// (リサンプリングは行わない)。
    sample_rate: u32,
    /// catalog が宣言した入力のチャンネル数
    ///
    /// 作り直すときに使う (出力はステレオ固定である)。
    input_channels: u8,
}

impl AacDecoder {
    /// AAC-LC デコーダを生成する
    ///
    /// `sample_rate` と `channels` は catalog が宣言した入力の値である。出力はサンプル
    /// レートが入力と同じ、チャンネル数がステレオ固定の S16 interleaved PCM になる。
    /// デコーダの実体は専用スレッドが所有するため、生成もスレッド内で行い、その結果を
    /// 待ってから返す。
    pub(crate) fn new(sample_rate: u32, channels: u8) -> Result<Self> {
        let config = DecoderConfig {
            codec: DecoderCodec::AacLc,
            input_sample_rate: sample_rate,
            input_channels: channels,
        };
        let (request_tx, request_rx) = tokio::sync::mpsc::unbounded_channel();
        // デコーダの生成結果を受け取る。`Decoder` は `Send` ではないため生成もスレッド内で
        // 行い、成否をこのチャネルで受け取る
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("moq-sub-aac-decoder".to_string())
            .spawn(move || decoder_thread(config, request_rx, ready_tx))
            .map_err(Error::Io)?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                request_tx: Some(request_tx),
                thread: Some(thread),
                sample_rate,
                input_channels: channels,
            }),
            Ok(Err(e)) => {
                // 生成に失敗した場合もスレッドを join してから返す
                drop(request_tx);
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                // 生成結果を送る前にスレッドが終了した (スレッド内の panic など)
                drop(request_tx);
                let _ = thread.join();
                Err(Error::Other(
                    "AAC decoder thread exited before initialization".to_string(),
                ))
            }
        }
    }

    /// 1 パケットをデコードして PCM (S16 interleaved、ステレオ固定) を返す
    ///
    /// AudioToolbox のデコーダは 1 パケット入力するごとに PCM を返すが、先頭はデコーダの
    /// 遅延 (priming) により空になることがあるため、空の `Vec` はエラーにせずそのまま返す。
    /// 得られた PCM は順に連結する。
    ///
    /// なお `shiguredo_audio_toolbox` 2026.1.0 は 1 回の `AudioConverterFillComplexBuffer` の中で
    /// 同じパケットを複数回入力するため、1 パケットに対して出力バッファ 1 杯分の PCM を
    /// 返していた (同じ音声が複数回現れる)。本 example はこの不具合が修正された
    /// 2026.2.0-canary.2 を使う (Cargo.toml のコメント参照)。
    pub(crate) async fn decode(&mut self, payload: &[u8]) -> Result<Vec<i16>> {
        let request_tx = self
            .request_tx
            .as_ref()
            .ok_or_else(|| Error::Other("AAC decoder is already shut down".to_string()))?;
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        request_tx
            .send(DecodeRequest {
                payload: payload.to_vec(),
                reply: reply_tx,
            })
            .map_err(|_| Error::Other("AAC decoder thread has stopped".to_string()))?;
        reply_rx
            .await
            .map_err(|_| Error::Other("AAC decoder thread dropped the reply".to_string()))?
    }

    /// 出力チャンネル数を返す
    ///
    /// AudioToolbox のデコーダ出力はステレオ固定であるため、入力のチャンネル数によらず
    /// 常に [`OUTPUT_CHANNELS`] を返す。
    pub(crate) fn channels(&self) -> u8 {
        OUTPUT_CHANNELS
    }

    /// デコーダを作り直す
    ///
    /// 専用スレッドごと作り直し、中に溜まった要求を捨てて live へ戻す。受信側の追いつきで、
    /// 遅れが固定されたときに使う。
    pub(crate) fn reset(&mut self) -> Result<()> {
        let sample_rate = self.sample_rate;
        let input_channels = self.input_channels;
        // 代入は右辺を先に評価するため、新しいデコーダを作ってから古いほうを drop する
        // (drop では送信側を閉じてスレッドを join する)
        *self = Self::new(sample_rate, input_channels)?;
        Ok(())
    }

    /// 出力のサンプルレート (Hz) を返す
    ///
    /// AudioToolbox のデコーダは入力のサンプルレートと同じレートで出力する。
    pub(crate) fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

impl Drop for AacDecoder {
    fn drop(&mut self) {
        // 送信側を閉じてスレッドの受信ループを終わらせ、終了を待つ。
        // テストでスレッドが残らないようにするため join する。
        self.request_tx = None;
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::warn!("AAC decoder thread panicked");
        }
    }
}

/// デコーダスレッドの本体
///
/// デコーダを生成してその成否を `ready_tx` で通知したあと、要求チャネルが閉じる
/// (ハンドルが drop される) まで 1 パケットずつデコードする。要求チャネルが閉じたら
/// `finish()` で入力の終端を通知し、内部に残った PCM を取り出してから終了する。
fn decoder_thread(
    config: DecoderConfig,
    mut request_rx: tokio::sync::mpsc::UnboundedReceiver<DecodeRequest>,
    ready_tx: std::sync::mpsc::Sender<Result<()>>,
) {
    let mut decoder = match Decoder::new(config) {
        Ok(decoder) => decoder,
        Err(e) => {
            // 生成に失敗したことをハンドルへ伝える
            let _ = ready_tx.send(Err(Error::AudioToolbox(e)));
            return;
        }
    };
    if ready_tx.send(Ok(())).is_err() {
        // ハンドルが既に消えている場合は、解放のための終端処理だけを行う
        flush(&mut decoder);
        return;
    }
    while let Some(request) = request_rx.blocking_recv() {
        let result = decode_packet(&mut decoder, &request.payload);
        // 呼び出し側が応答を待たずに消えた場合 (stream task の終了など) も、デコーダは
        // subscription で共有されるため処理を続ける
        if request.reply.send(result).is_err() {
            tracing::debug!("AAC decoder reply was dropped by the caller");
        }
    }
    flush(&mut decoder);
}

/// 1 パケットをデコードし、得られた PCM を順に連結して返す
///
/// `Decoder::decode` は前のパケットを `next_frame` で消費するまで再度呼べないため、
/// 1 パケットごとに `Ok(None)` になるまで `next_frame` を呼び切る。
fn decode_packet(decoder: &mut Decoder, payload: &[u8]) -> Result<Vec<i16>> {
    decoder.decode(payload)?;
    let mut pcm = Vec::new();
    while let Some(frame) = decoder.next_frame()? {
        pcm.extend_from_slice(&frame);
    }
    Ok(pcm)
}

/// 入力の終端を通知し、内部に残った PCM を取り出す
///
/// 取り出した PCM は受け取り手が既にいないため破棄する。
fn flush(decoder: &mut Decoder) {
    if let Err(e) = decoder.finish() {
        tracing::debug!("Failed to finish the AAC decoder: {e}");
        return;
    }
    loop {
        match decoder.next_frame() {
            Ok(Some(_)) => {}
            Ok(None) => return,
            Err(e) => {
                tracing::debug!("Failed to flush the AAC decoder: {e}");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shiguredo_audio_toolbox::{Encoder, EncoderCodec, EncoderConfig};

    /// テスト用のサンプルレート (Hz)
    const TEST_SAMPLE_RATE: u32 = 48_000;
    /// テスト用のチャンネル数
    const TEST_CHANNELS: u8 = 2;
    /// テストでエンコードするフレーム数 (1 フレーム = 1024 サンプル)
    const TEST_FRAMES: usize = 4;

    /// テスト用: 440 Hz の正弦波 PCM (S16 interleaved) を 1024 サンプル分作る
    fn sine_pcm(offset_samples: usize) -> Vec<i16> {
        let mut pcm = Vec::new();
        for i in 0..1024 {
            let t = (offset_samples + i) as f32 / TEST_SAMPLE_RATE as f32;
            let value = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5;
            let sample = (value * f32::from(i16::MAX)) as i16;
            for _ in 0..TEST_CHANNELS {
                pcm.push(sample);
            }
        }
        pcm
    }

    /// テスト用: 実際のエンコーダで AAC-LC パケットを数フレーム分作る
    ///
    /// モックを使わず、同じ Audio Toolbox の `Encoder` で生成したパケットをデコーダへ
    /// 入力する。
    fn encode_aac_lc_packets() -> Vec<Vec<u8>> {
        let mut encoder = Encoder::new(EncoderConfig {
            codec: EncoderCodec::AacLc,
            sample_rate: TEST_SAMPLE_RATE,
            channels: TEST_CHANNELS,
            bitrate: Some(128_000),
            bitrate_control_mode: None,
            codec_quality: None,
            vbr_quality: None,
        })
        .expect("AAC-LC エンコーダを生成できること");
        let mut packets = Vec::new();
        for frame_index in 0..TEST_FRAMES {
            let pcm = sine_pcm(frame_index * 1024);
            encoder.encode(&pcm).expect("PCM を入力できること");
            while let Some(frame) = encoder.next_frame() {
                packets.push(frame.data);
            }
        }
        // 内部バッファに残った PCM をすべてエンコードして回収する
        encoder.finish().expect("エンコーダを終端できること");
        while let Some(frame) = encoder.next_frame() {
            packets.push(frame.data);
        }
        packets
    }

    /// エンコードした AAC-LC パケットをデコードできること
    ///
    /// 出力は S16 interleaved のステレオ固定であるため、PCM が空でなく、長さが偶数で
    /// あることを検証する (1 サンプル 2 バイトの左右が交互に並ぶ)。
    #[tokio::test(flavor = "multi_thread")]
    async fn aac_decoder_decodes_encoded_packets() {
        let packets = encode_aac_lc_packets();
        assert!(
            !packets.is_empty(),
            "テストの前提として AAC-LC パケットが生成されること"
        );
        let mut decoder =
            AacDecoder::new(TEST_SAMPLE_RATE, TEST_CHANNELS).expect("AAC デコーダを生成できること");
        assert_eq!(
            decoder.channels(),
            2,
            "AudioToolbox の出力はステレオ固定であること"
        );
        let mut decoded_packets = 0;
        // チャンネルあたりのデコード済みサンプル数
        let mut decoded_samples = 0usize;
        for (index, packet) in packets.iter().enumerate() {
            let pcm = decoder
                .decode(packet)
                .await
                .expect("AAC-LC パケットをデコードできること");
            // 先頭のパケットはデコーダの遅延 (priming) で PCM が空になることがあるため、
            // 空は失敗とせず、得られた PCM の性質だけを検証する
            if pcm.is_empty() {
                continue;
            }
            assert!(
                pcm.len().is_multiple_of(2),
                "ステレオ interleaved であるため PCM の長さが偶数であること: index={index}, len={}",
                pcm.len(),
            );
            decoded_packets += 1;
            decoded_samples += pcm.len() / 2 / TEST_CHANNELS as usize;
        }
        assert!(
            decoded_packets > 0,
            "少なくとも 1 パケットの PCM が得られること: packets={}",
            packets.len()
        );
        // 同じパケットが複数回デコードされると、入力のサンプル数を超える PCM が得られる。
        // 1 パケットは 1024 サンプルであるため、デコード結果がそれを超えないことを確かめる
        // (shiguredo_audio_toolbox 2026.1.0 ではこの重複が起きていた)
        assert!(
            decoded_samples <= packets.len() * 1024,
            "デコード結果が入力のサンプル数を超えないこと: decoded={decoded_samples}, packets={}",
            packets.len(),
        );
    }

    /// デコーダを drop するとスレッドが終了すること
    ///
    /// `Drop` で join しているため、生成と破棄を繰り返してもスレッドが残らない。
    #[tokio::test(flavor = "multi_thread")]
    async fn aac_decoder_drop_joins_the_thread() {
        for _ in 0..3 {
            let decoder = AacDecoder::new(TEST_SAMPLE_RATE, TEST_CHANNELS)
                .expect("AAC デコーダを生成できること");
            drop(decoder);
        }
    }
}
