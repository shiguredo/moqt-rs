//! ビデオデコーダの Enum 抽象化
//!
//! codec ごとの実装を `VideoDecoder` の variant に持ち、`match` で分岐する。
//! 具体実装は codec 別サブモジュールに分離する。

use crate::error::{Error, Result};

pub(crate) mod av1;
#[cfg(target_os = "macos")]
pub(crate) mod h264;
#[cfg(target_os = "macos")]
pub(crate) mod h265;
pub(crate) mod opus;
pub(crate) mod vpx;

/// デコード済みビデオフレーム (I420)
///
/// 各プレーンは stride を詰めたバッファを保持する。
pub struct DecodedVideoFrame {
    /// Y プレーン (width x height バイト)
    pub y: Vec<u8>,
    /// U プレーン (width / 2 x height / 2 バイト)
    pub u: Vec<u8>,
    /// V プレーン (width / 2 x height / 2 バイト)
    pub v: Vec<u8>,
    /// 映像の幅 (ピクセル)
    pub width: i32,
    /// 映像の高さ (ピクセル)
    pub height: i32,
    /// LOC の Timestamp から求めた時刻 (マイクロ秒)。無いときは None
    ///
    /// 0103 の完了後は epoch マイクロ秒、それまでは publisher が送った時間軸の値である。
    /// None のときは、表示時刻を時間軸から決められないため到着順に表示する。
    pub timestamp_us: Option<i64>,
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
    /// VP8 デコーダ (libvpx)
    Vp8(vpx::VpxDecoder),
    /// VP9 デコーダ (libvpx)
    Vp9(vpx::VpxDecoder),
}

impl VideoDecoder {
    /// 1 オブジェクト分のエンコード済みペイロードをデコードする
    ///
    /// - `payload`: 1 オブジェクト分の圧縮ビットストリーム
    /// - `video_config`: PROP_VIDEO_CONFIG 由来の codec 固有設定 (avcC / hvcC / Sequence Header 等)
    ///   - H.264 / H.265 ではキーフレーム到達時に与えられる想定
    ///   - AV1 では payload 内に Sequence Header OBU が含まれるため無視される
    ///   - VP8 / VP9 では codec 固有の設定データが無いため無視される
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
            VideoDecoder::Vp8(d) => d.decode(payload, video_config),
            VideoDecoder::Vp9(d) => d.decode(payload, video_config),
        }
    }
}

/// catalog の RFC 6381 codec 文字列からビデオデコーダを生成する
pub(crate) fn build_video_decoder(codec: &str) -> Result<VideoDecoder> {
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
    } else if codec == "vp8" {
        // VP8 は登録名のみ (パラメーター無し) である (RFC 6381 §3.3)
        Ok(VideoDecoder::Vp8(vpx::VpxDecoder::new(vpx::VpxCodec::Vp8)?))
    } else if codec == "vp09" || codec.starts_with("vp09.") {
        // VP9 は `vp09.<profile>.<level>.<bit depth>` を取る
        Ok(VideoDecoder::Vp9(vpx::VpxDecoder::new(vpx::VpxCodec::Vp9)?))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// catalog の codec 文字列から対応するデコーダを生成できること
    ///
    /// `src/msf.rs` の `is_video_codec` が video と判定する codec 文字列は、すべて
    /// デコーダを生成できる必要がある (`vp8` は登録名のみ、`vp09` は登録名単独と
    /// `vp09.` 始まりを受け付ける)。
    #[test]
    fn video_codec_strings_select_a_decoder() {
        assert!(matches!(
            build_video_decoder("av01.0.08M.08").expect("AV1 のデコーダを生成できること"),
            VideoDecoder::Av1(_)
        ));
        assert!(
            matches!(
                build_video_decoder("vp8").expect("VP8 のデコーダを生成できること"),
                VideoDecoder::Vp8(_)
            ),
            "VP8 の codec 文字列で VP8 デコーダが選ばれること"
        );
        for codec in ["vp09", "vp09.00.40.08", "vp09.0.10.08"] {
            assert!(
                matches!(
                    build_video_decoder(codec).expect("VP9 のデコーダを生成できること"),
                    VideoDecoder::Vp9(_)
                ),
                "VP9 の codec 文字列で VP9 デコーダが選ばれること: {codec}"
            );
        }
    }

    /// 登録外の codec 文字列はエラーになること
    #[test]
    fn unknown_video_codec_string_is_rejected() {
        for codec in ["vp8x", "vp09x", "mp4v.20.9", "unknown"] {
            let result = build_video_decoder(codec);
            assert!(
                result.is_err(),
                "登録外の codec 文字列はエラーになること: {codec}"
            );
            assert!(
                result
                    .err()
                    .is_some_and(|e| e.to_string().contains("unsupported video codec")),
                "未対応であることが分かるエラーになること: {codec}"
            );
        }
    }
}
