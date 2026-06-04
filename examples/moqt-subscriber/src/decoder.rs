//! ビデオデコーダの Enum 抽象化
//!
//! codec ごとの実装を [`VideoDecoder`] の variant に持ち、`match` で分岐する。
//! 具体実装は codec 別サブモジュールに分離する。

use crate::error::{Error, Result};

pub(crate) mod av1;
#[cfg(target_os = "macos")]
pub(crate) mod h264;
#[cfg(target_os = "macos")]
pub(crate) mod h265;
pub(crate) mod opus;

/// デコード済みビデオフレーム (I420, stride 除去済み)
pub struct DecodedVideoFrame {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub width: i32,
    pub height: i32,
}

/// デコード済み音声フレーム (S16 interleaved PCM)
pub struct DecodedAudioFrame {
    /// S16 interleaved PCM
    pub pcm: Vec<i16>,
    /// サンプルレート (Hz)
    pub sample_rate: u32,
    /// チャンネル数
    pub channels: u8,
    /// Presentation timestamp (マイクロ秒)
    pub pts_us: i64,
}

/// サンプル subscriber が扱うビデオデコーダ
///
/// codec ごとの実装を variant に持つ Enum とし、呼び出し側は本型のメソッド経由で
/// 利用する。macOS 限定の codec は variant 単位で `cfg` を付ける。
pub(crate) enum VideoDecoder {
    /// AV1 デコーダ (dav1d)
    Av1(av1::Av1Decoder),
    /// H.264 デコーダ (Apple Video Toolbox、macOS 限定)
    #[cfg(target_os = "macos")]
    H264(h264::H264Decoder),
    /// H.265 デコーダ (Apple Video Toolbox、macOS 限定)
    #[cfg(target_os = "macos")]
    H265(h265::H265Decoder),
}

impl VideoDecoder {
    /// 1 オブジェクト分のエンコード済みペイロードをデコードする
    ///
    /// - `payload`: 1 オブジェクト分の圧縮ビットストリーム
    /// - `video_config`: PROP_VIDEO_CONFIG 由来の codec 固有設定 (avcC / hvcC / Sequence Header 等)
    ///   - H.264 / H.265 ではキーフレーム到達時に与えられる想定
    ///   - AV1 では payload 内に Sequence Header OBU が含まれるため無視される
    pub fn decode(
        &mut self,
        payload: &[u8],
        video_config: Option<&[u8]>,
    ) -> Result<Vec<DecodedVideoFrame>> {
        match self {
            VideoDecoder::Av1(d) => d.decode(payload, video_config),
            #[cfg(target_os = "macos")]
            VideoDecoder::H264(d) => d.decode(payload, video_config),
            #[cfg(target_os = "macos")]
            VideoDecoder::H265(d) => d.decode(payload, video_config),
        }
    }
}

/// catalog の RFC 6381 codec 文字列からビデオデコーダを生成する
pub fn build_video_decoder(codec: &str) -> Result<VideoDecoder> {
    if codec.starts_with("av01") {
        Ok(VideoDecoder::Av1(av1::Av1Decoder::new()?))
    } else if codec.starts_with("avc1") {
        #[cfg(target_os = "macos")]
        {
            Ok(VideoDecoder::H264(h264::H264Decoder::new()?))
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(Error::Other(format!(
                "H.264 decoder is only supported on macOS (catalog codec: {codec})"
            )))
        }
    } else if codec.starts_with("hvc1") || codec.starts_with("hev1") {
        #[cfg(target_os = "macos")]
        {
            Ok(VideoDecoder::H265(h265::H265Decoder::new()?))
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(Error::Other(format!(
                "H.265 decoder is only supported on macOS (catalog codec: {codec})"
            )))
        }
    } else {
        Err(Error::Other(format!("unsupported video codec: {codec}")))
    }
}

/// stride 付きプレーンを連続バッファにコピーする
pub(crate) fn copy_plane(src: &[u8], stride: usize, width: usize, height: usize) -> Vec<u8> {
    if stride == width {
        return src[..width * height].to_vec();
    }
    let mut dst = Vec::with_capacity(width * height);
    for row in 0..height {
        let start = row * stride;
        dst.extend_from_slice(&src[start..start + width]);
    }
    dst
}

/// `u16 length + NAL` を読み出す
///
/// `codec_label` はエラーメッセージ用 (例: "AVCDecoderConfigurationRecord")。
pub(crate) fn read_ps_nal(buf: &[u8], pos: &mut usize, codec_label: &str) -> Result<Vec<u8>> {
    if buf.len() < *pos + 2 {
        return Err(Error::Other(format!("{codec_label}: truncated NAL length")));
    }
    let len = u16::from_be_bytes([buf[*pos], buf[*pos + 1]]) as usize;
    *pos += 2;
    if buf.len() < *pos + len {
        return Err(Error::Other(format!(
            "{codec_label}: truncated NAL payload"
        )));
    }
    let nal = buf[*pos..*pos + len].to_vec();
    *pos += len;
    Ok(nal)
}
