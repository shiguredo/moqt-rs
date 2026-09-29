//! ビデオデコーダの Enum 抽象化
//!
//! MP4 再エンコード配信の入力デコードに使う。codec ごとの実装を [`VideoDecoder`] の
//! variant に持ち、`match` で分岐する。具体実装は codec 別サブモジュールに分離する。

use crate::error::{Error, Result};

pub(crate) mod av1;
#[cfg(target_os = "macos")]
pub(crate) mod h264;
#[cfg(target_os = "macos")]
pub(crate) mod h265;
pub(crate) mod opus;

/// デコード済みビデオフレーム (I420、stride 除去済み)
pub struct DecodedVideoFrame {
    /// Y プレーン
    pub y: Vec<u8>,
    /// U プレーン
    pub u: Vec<u8>,
    /// V プレーン
    pub v: Vec<u8>,
    /// 映像幅
    pub width: i32,
    /// 映像高さ
    pub height: i32,
}

/// 再エンコード配信で扱うビデオデコーダ
///
/// codec ごとの実装を variant に持つ Enum とし、呼び出し側は本型のメソッド経由で
/// 利用する。macOS 限定の codec は variant 単位で `cfg` を付ける。
pub(crate) enum VideoDecoder {
    /// AV1 デコーダ (dav1d、全プラットフォーム)
    Av1(av1::Av1Decoder),
    /// H.264 デコーダ (Apple Video Toolbox、macOS 限定)
    #[cfg(target_os = "macos")]
    H264(h264::H264Decoder),
    /// H.265 デコーダ (Apple Video Toolbox、macOS 限定)
    #[cfg(target_os = "macos")]
    H265(h265::H265Decoder),
}

impl VideoDecoder {
    /// 1 サンプル分のエンコード済みペイロードをデコードする
    ///
    /// - `payload`: 1 サンプル分の圧縮ビットストリーム (length-prefixed NAL / AV1 OBU)
    /// - `video_config`: avcC / hvcC のレコード本体。AV1 では使わない
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

    /// デコーダの内部状態をリセットする (周回の先頭で使う)
    ///
    /// dav1d は内部に保持している遅延フレームを破棄する。Video Toolbox は各フレームを
    /// 即座に出力するため何もしない (次のキーフレームで状態がリセットされる)。
    pub fn reset(&mut self) {
        match self {
            VideoDecoder::Av1(d) => d.reset(),
            #[cfg(target_os = "macos")]
            VideoDecoder::H264(_) | VideoDecoder::H265(_) => {}
        }
    }
}

/// I420 の U / V プレーンを NV12 のインターリーブ UV プレーンに変換する
///
/// 再エンコードの入力は packed NV12 (stride = width) にするため、UV を交互に並べる。
pub(crate) fn interleave_uv(u: &[u8], v: &[u8]) -> Vec<u8> {
    let mut uv = Vec::new();
    for (u_sample, v_sample) in u.iter().zip(v.iter()) {
        uv.push(*u_sample);
        uv.push(*v_sample);
    }
    uv
}

/// stride 付きプレーンを連続バッファにコピーする
pub(crate) fn copy_plane(src: &[u8], stride: usize, width: usize, height: usize) -> Vec<u8> {
    if stride == width {
        return src[..width * height].to_vec();
    }
    let mut dst = Vec::new();
    for row in 0..height {
        let start = row * stride;
        dst.extend_from_slice(&src[start..start + width]);
    }
    dst
}

/// `u16 length + NAL` を読み出す
///
/// H.264 / H.265 の設定レコード (avcC / hvcC) のパースでのみ使う。
/// `codec_label` はエラーメッセージ用 (例: "AVCDecoderConfigurationRecord")。
#[cfg(target_os = "macos")]
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
