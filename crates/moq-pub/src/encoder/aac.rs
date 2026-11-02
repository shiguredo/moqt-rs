//! shiguredo_audio_toolbox (Apple Audio Toolbox) による AAC-LC エンコーダ実装
//!
//! 48 kHz の PCM (S16 interleaved) を AAC-LC にエンコードする。macOS 限定である
//! (Apple の Audio Toolbox フレームワークを使う)。
//!
//! `shiguredo_audio_toolbox::Encoder` は `Send` を実装していない。publisher の future は
//! `e2e-tests` が `tokio::spawn` するため `Send` でなければならないので、実体を専用スレッドが
//! 所有し、本型はチャネル越しにエンコードを依頼する `Send` なハンドルにする。

use shiguredo_audio_toolbox::{Encoder, EncoderCodec, EncoderConfig};

use crate::error::{Error, Result};

/// MSF catalog の AAC-LC codec 文字列 (RFC 6381)
///
/// `mp4a` は MPEG-4 Audio、`40` は MPEG-4 Audio の object type の 16 進表記、`2` は
/// AAC-LC (Audio Object Type 2) を表す (RFC 6381 §3.3)。
const AAC_CATALOG_CODEC_STRING: &str = "mp4a.40.2";

/// AAC-LC の 1 フレームあたりのサンプル数 (チャンネルあたり)
///
/// AAC-LC のフレーム長は 1024 サンプル固定である (ISO/IEC 14496-3 §4.5.2.1)。
/// 48 kHz では 21.33 ms に相当する。
const AAC_SAMPLES_PER_FRAME: usize = 1024;

/// AAC-LC の Audio Object Type (ISO/IEC 14496-3 Table 1.17)
const AAC_OBJECT_TYPE_LC: u16 = 2;

/// エンコーダスレッドへの要求
struct EncodeRequest {
    /// 1 フレーム分の PCM (S16 interleaved)
    pcm: Vec<i16>,
    /// エンコード結果 (1 パケット) の返送先
    reply: tokio::sync::oneshot::Sender<Result<Vec<u8>>>,
}

/// AAC-LC エンコーダのハンドル
pub struct AacEncoder {
    /// エンコード要求の送信側
    ///
    /// `Drop` では送信側を閉じてからスレッドを join する必要があるため `Option` で持つ。
    request_tx: Option<tokio::sync::mpsc::Sender<EncodeRequest>>,
    /// エンコーダを所有するスレッドのハンドル (Drop で join する)
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AacEncoder {
    /// AAC-LC エンコーダを作成する
    ///
    /// `bitrate` は kbps 単位である (Audio Toolbox には bps で渡す)。エンコーダの実体は
    /// 専用スレッドが所有するため、生成もスレッド内で行い、その結果を待ってから返す。
    pub fn new(sample_rate: u32, channels: u8, bitrate: u32) -> Result<Self> {
        // Audio Config (AudioSpecificConfig) を構築できるサンプルレートか確認する
        sampling_frequency_index(sample_rate)?;
        let config = EncoderConfig {
            codec: EncoderCodec::AacLc,
            sample_rate,
            channels,
            bitrate: Some(bitrate.saturating_mul(1_000)),
            bitrate_control_mode: None,
            codec_quality: None,
            vbr_quality: None,
        };

        let (request_tx, request_rx) = tokio::sync::mpsc::channel(4);
        // エンコーダの生成結果を受け取る。`Encoder` は `Send` ではないため生成もスレッド内で
        // 行い、成否をこのチャネルで受け取る
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("moq-pub-aac-encoder".to_string())
            .spawn(move || encoder_thread(config, request_rx, ready_tx))
            .map_err(Error::Io)?;

        match ready_rx.recv() {
            Ok(Ok(())) => {
                tracing::info!(
                    "AAC-LC encoder created ({} Hz, {} ch, {} kbps, samples_per_frame={})",
                    sample_rate,
                    channels,
                    bitrate,
                    AAC_SAMPLES_PER_FRAME
                );
                Ok(Self {
                    request_tx: Some(request_tx),
                    thread: Some(thread),
                })
            }
            // 生成に失敗した場合もスレッドを join してから返す
            Ok(Err(e)) => {
                drop(request_tx);
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                drop(request_tx);
                let _ = thread.join();
                Err(Error::Other(
                    "AAC encoder thread exited before initialization".to_string(),
                ))
            }
        }
    }

    /// 1 フレーム分の PCM をエンコードして 1 パケットを返す
    ///
    /// 呼び出し側は `samples_per_frame()` サンプルずつ渡す。AAC-LC のフレーム長は
    /// 1024 サンプル固定であるため、1 回の呼び出しに対して 1 パケットが得られる。
    pub async fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>> {
        let request_tx = self
            .request_tx
            .as_ref()
            .ok_or_else(|| Error::Other("AAC encoder is already shut down".to_string()))?;
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        request_tx
            .send(EncodeRequest {
                pcm: pcm.to_vec(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| Error::Other("AAC encoder thread has stopped".to_string()))?;
        reply_rx
            .await
            .map_err(|_| Error::Other("AAC encoder thread dropped the reply".to_string()))?
    }

    /// 1 フレームあたりのサンプル数 (チャンネル単位)
    pub fn samples_per_frame(&self) -> usize {
        AAC_SAMPLES_PER_FRAME
    }

    /// MSF catalog に載せる RFC 6381 形式の codec 文字列
    pub fn catalog_codec_string(&self) -> &'static str {
        AAC_CATALOG_CODEC_STRING
    }
}

impl Drop for AacEncoder {
    fn drop(&mut self) {
        // 送信側を閉じてスレッドの受信ループを終わらせ、終了を待つ。
        // テストでスレッドが残らないようにするため join する。
        self.request_tx = None;
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::warn!("AAC encoder thread panicked");
        }
    }
}

/// エンコーダスレッドの本体
///
/// エンコーダを生成してその成否を `ready_tx` で通知したあと、要求チャネルが閉じる
/// (ハンドルが drop される) まで 1 フレームずつエンコードする。要求チャネルが閉じたら
/// `finish()` で内部に残ったパケットを取り出す (受け取り手がいないため破棄する)。
fn encoder_thread(
    config: EncoderConfig,
    mut request_rx: tokio::sync::mpsc::Receiver<EncodeRequest>,
    ready_tx: std::sync::mpsc::Sender<Result<()>>,
) {
    let mut encoder = match Encoder::new(config) {
        Ok(encoder) => encoder,
        Err(e) => {
            let _ = ready_tx.send(Err(Error::AudioToolbox(e)));
            return;
        }
    };
    if ready_tx.send(Ok(())).is_err() {
        // ハンドルが既に消えている場合は、解放のための終端処理だけを行う
        flush(&mut encoder);
        return;
    }
    while let Some(request) = request_rx.blocking_recv() {
        let result = encode_frame(&mut encoder, &request.pcm);
        if request.reply.send(result).is_err() {
            tracing::debug!("AAC encoder reply was dropped by the caller");
        }
    }
    flush(&mut encoder);
}

/// 1 フレーム分の PCM をエンコードして 1 パケットを返す
fn encode_frame(encoder: &mut Encoder, pcm: &[i16]) -> Result<Vec<u8>> {
    encoder.encode(pcm)?;
    let mut packets = Vec::new();
    while let Some(frame) = encoder.next_frame() {
        packets.push(frame.data);
    }
    match packets.len() {
        // 入力が 1 フレーム分に満たない場合はパケットが得られない。Timestamp とサンプル数の
        // 対応が崩れるため、呼び出し側の前提 (1 チャンク = 1 Object) を壊さずエラーにする
        0 => Err(Error::Other(format!(
            "AAC encoder produced no packet for {} samples",
            pcm.len()
        ))),
        1 => Ok(packets.pop().expect("1 パケットが得られている")),
        n => Err(Error::Other(format!(
            "AAC encoder produced {n} packets for one frame"
        ))),
    }
}

/// 入力の終端を通知し、内部に残ったパケットを取り出す
fn flush(encoder: &mut Encoder) {
    if let Err(e) = encoder.finish() {
        tracing::debug!("Failed to finish the AAC encoder: {e}");
        return;
    }
    while encoder.next_frame().is_some() {}
}

/// Audio Config プロパティ (LOC draft-ietf-moq-loc-04 §2.3.3.1) に載せる
/// AudioSpecificConfig を構築する
///
/// 構成は ISO/IEC 14496-3 §1.6.2.1 (AudioSpecificConfig) の
/// audioObjectType (5 bit) / samplingFrequencyIndex (4 bit) / channelConfiguration (4 bit) と
/// GASpecificConfig の frameLengthFlag / dependsOnCoreCoder / extensionFlag (各 1 bit) であり、
/// 全体で 2 バイトになる。
pub(crate) fn audio_specific_config(sample_rate: u32, channels: u8) -> Result<Vec<u8>> {
    let frequency_index = sampling_frequency_index(sample_rate)?;
    let bits: u16 =
        (AAC_OBJECT_TYPE_LC << 11) | ((frequency_index as u16) << 7) | ((channels as u16) << 3);
    Ok(bits.to_be_bytes().to_vec())
}

/// AAC の samplingFrequencyIndex (ISO/IEC 14496-3 Table 1.18) を返す
///
/// 表に無いサンプルレートはエラーにする (index 15 の明示的な周波数表現は使わない)。
fn sampling_frequency_index(sample_rate: u32) -> Result<u8> {
    match sample_rate {
        96_000 => Ok(0),
        88_200 => Ok(1),
        64_000 => Ok(2),
        48_000 => Ok(3),
        44_100 => Ok(4),
        32_000 => Ok(5),
        24_000 => Ok(6),
        22_050 => Ok(7),
        16_000 => Ok(8),
        12_000 => Ok(9),
        11_025 => Ok(10),
        8_000 => Ok(11),
        7_350 => Ok(12),
        other => Err(Error::Other(format!(
            "unsupported AAC sample rate: {other} Hz"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用の PCM を作る (矩形波)
    fn test_pcm(samples: usize) -> Vec<i16> {
        (0..samples)
            .map(|i| if (i / 24) % 2 == 0 { 8_000 } else { -8_000 })
            .collect()
    }

    /// 1 フレームのエンコードで 1 パケットが得られること
    #[tokio::test(flavor = "multi_thread")]
    async fn encodes_one_packet_per_frame() {
        let mut encoder = AacEncoder::new(48_000, 1, 64).expect("AAC エンコーダを作成できること");
        for frame in 0..3 {
            let pcm = test_pcm(encoder.samples_per_frame());
            let packet = encoder
                .encode(&pcm)
                .await
                .unwrap_or_else(|e| panic!("{frame} フレーム目のエンコードに成功すること: {e}"));
            assert!(!packet.is_empty(), "パケットが空でないこと");
        }
    }

    /// エンコーダを drop するとスレッドが終了すること
    #[tokio::test(flavor = "multi_thread")]
    async fn drop_joins_the_thread() {
        for _ in 0..3 {
            let encoder = AacEncoder::new(48_000, 1, 64).expect("AAC エンコーダを作成できること");
            drop(encoder);
        }
    }

    /// catalog の codec 文字列が AAC-LC のものであること
    #[test]
    fn catalog_codec_string_is_aac_lc() {
        assert_eq!(
            AAC_CATALOG_CODEC_STRING, "mp4a.40.2",
            "AAC-LC の codec 文字列であること"
        );
    }

    /// Audio Config が 48 kHz モノラルの AAC-LC の AudioSpecificConfig になること
    #[test]
    fn audio_config_is_audio_specific_config() {
        assert_eq!(
            audio_specific_config(48_000, 1).expect("Audio Config を構築できること"),
            vec![0x11, 0x88],
            "audioObjectType=2 / samplingFrequencyIndex=3 / channelConfiguration=1 であること"
        );
        assert_eq!(
            audio_specific_config(48_000, 2).expect("Audio Config を構築できること"),
            vec![0x11, 0x90],
            "ステレオでは channelConfiguration が 2 になること"
        );
    }

    /// 表に無いサンプルレートを拒否すること
    #[test]
    fn unsupported_sample_rate_is_rejected() {
        assert!(
            AacEncoder::new(1_000, 1, 64).is_err(),
            "samplingFrequencyIndex を持たないサンプルレートはエラーになること"
        );
    }
}
