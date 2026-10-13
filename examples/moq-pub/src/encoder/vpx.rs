//! shiguredo_libvpx (libvpx) による VP8 / VP9 エンコーダ実装
//!
//! NV12 入力をリアルタイム CBR で VP8 / VP9 にエンコードする。VP8 と VP9 は
//! libvpx の API が共通であるため 1 つのモジュールにまとめ、[`VpxCodec`] で切り替える。

use std::num::NonZeroUsize;

use shiguredo_libvpx::{
    CodecConfig, EncodeOptions, Encoder, EncoderConfig, EncodingDeadline, ImageData, ImageFormat,
    RateControlMode, Vp8Config, Vp9Config,
};
use shiguredo_video_device::VideoFrameOwned;

use super::EncodedFrame;
use crate::error::{Error, Result};

/// 扱う VP8 / VP9 の種別
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VpxCodec {
    /// VP8
    Vp8,
    /// VP9
    Vp9,
}

impl VpxCodec {
    /// MSF catalog に載せる RFC 6381 形式の codec 文字列
    ///
    /// VP8 は登録名のみ (パラメーター無し)、VP9 は `vp09.<profile>.<level>.<bit depth>` の
    /// 4 フィールドを要求する (RFC 6381 §3.3)。VP9 は Profile 0 (8-bit 4:2:0) 固定とし、
    /// level は 1920x1080 30fps を覆う 4.0 にする。AV1 の `av01.0.08M.08` と同じく
    /// 解像度・フレームレートに依らない固定値である。
    fn catalog_codec_string(self) -> &'static str {
        match self {
            Self::Vp8 => "vp8",
            Self::Vp9 => "vp09.00.40.08",
        }
    }

    /// libvpx に渡すコーデック固有設定
    ///
    /// 既定値のまま使う (VP9 は Profile 0、VP8 は既定のフィルター設定)。
    fn codec_config(self) -> CodecConfig {
        match self {
            Self::Vp8 => CodecConfig::Vp8(Vp8Config::default()),
            Self::Vp9 => CodecConfig::Vp9(Vp9Config::default()),
        }
    }
}

/// VP8 / VP9 エンコーダ
pub(crate) struct VpxEncoder {
    encoder: Encoder,
    codec: VpxCodec,
    /// 1 フレームの表示間隔 (マイクロ秒)
    ///
    /// 1 つの入力から複数の出力フレームが出たときに Timestamp を進める量。
    frame_interval_us: u64,
}

impl VpxEncoder {
    /// VP8 / VP9 エンコーダを作成する
    ///
    /// `bitrate` は kbps 単位である (libvpx には bps で渡す)。
    pub(crate) fn new(
        codec: VpxCodec,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
        keyframe_interval: u32,
    ) -> Result<Self> {
        if fps == 0 {
            return Err(Error::Other(
                "fps must be non-zero to compute the frame interval".to_string(),
            ));
        }
        let mut config = EncoderConfig::new(
            width as usize,
            height as usize,
            ImageFormat::Nv12,
            codec.codec_config(),
        );
        config.fps_numerator = fps as usize;
        config.fps_denominator = 1;
        config.target_bitrate = bitrate as usize * 1_000;
        config.deadline = EncodingDeadline::Realtime;
        config.rate_control = RateControlMode::Cbr;
        config.cpu_used = Some(8);
        config.threads = NonZeroUsize::new(4);
        // 先読みを 1 フレームに固定する。libvpx の既定は VP9 で 25 フレームであり、
        // そのままだと最初の数十フレームが出力されず live 配信の遅延になる。
        // 1 を指定すると各入力に対してその場で出力される
        config.lag_in_frames = NonZeroUsize::new(1);
        config.keyframe_interval = NonZeroUsize::new(keyframe_interval as usize);

        let encoder = Encoder::new(config)?;
        let frame_interval_us = 1_000_000 / u64::from(fps);

        tracing::info!(
            "VP8/VP9 encoder created (codec={codec:?}, {}x{}, {} fps, {} kbps, \
             keyframe_interval={}, frame_interval_us={})",
            width,
            height,
            fps,
            bitrate,
            keyframe_interval,
            frame_interval_us
        );

        Ok(Self {
            encoder,
            codec,
            frame_interval_us,
        })
    }

    /// 1 フレームをエンコードして 0 個以上のエンコード済みフレームを返す
    ///
    /// `timestamp_us` は入力フレームのメディア時刻 (マイクロ秒)。先頭の出力フレームへ
    /// そのまま載せ、複数の出力フレームがある場合は 1 フレームずつ進める。
    pub(crate) fn encode(
        &mut self,
        frame: &VideoFrameOwned,
        timestamp_us: u64,
    ) -> Result<Vec<EncodedFrame>> {
        let image = ImageData::Nv12 {
            y: &frame.data,
            uv: frame.uv_data.as_deref().unwrap_or(&[]),
        };

        let options = EncodeOptions {
            force_keyframe: false,
        };

        self.encoder.encode(&image, &options)?;

        let mut frames = Vec::new();
        let mut output_index: u64 = 0;
        while let Some(encoded) = self.encoder.next_frame() {
            let timestamp =
                super::output_timestamp_us(timestamp_us, output_index, self.frame_interval_us);
            let data = encoded.data().to_vec();
            let is_keyframe = encoded.is_keyframe();
            frames.push(EncodedFrame {
                data,
                is_keyframe,
                timestamp,
                // VP8 / VP9 には avcC / hvcC / AV1 Sequence Header に相当する codec 固有の
                // 設定データが無いため PROP_VIDEO_CONFIG を載せない
                video_config: None,
            });
            output_index += 1;
        }

        Ok(frames)
    }

    /// MSF catalog に載せる RFC 6381 形式の codec 文字列
    pub(crate) fn catalog_codec_string(&self) -> &'static str {
        self.codec.catalog_codec_string()
    }
}

#[cfg(test)]
mod tests {
    use shiguredo_video_device::{PixelFormat as VdPixelFormat, VideoFrameOwned};

    use super::*;

    /// テスト用の NV12 フレームを作る (単色ではなく市松模様にする)
    ///
    /// 単色だとエンコーダが極端に小さい出力を返すため、隣接画素に差を付ける。
    fn test_frame(width: u32, height: u32) -> VideoFrameOwned {
        let mut y = vec![0u8; (width * height) as usize];
        for row in 0..height as usize {
            for col in 0..width as usize {
                y[row * width as usize + col] = if (row + col) % 2 == 0 { 32 } else { 200 };
            }
        }
        let uv = vec![128u8; (width * height / 2) as usize];
        VideoFrameOwned {
            data: y,
            uv_data: Some(uv),
            width: width as i32,
            height: height as i32,
            stride: width as i32,
            stride_uv: width as i32,
            pixel_format: VdPixelFormat::Nv12,
            timestamp_us: 0,
            pixel_buffer: None,
        }
    }

    /// VP8 エンコードがキーフレームを返すこと
    #[test]
    fn vp8_encoder_emits_a_keyframe() {
        let mut encoder = VpxEncoder::new(VpxCodec::Vp8, 64, 48, 30, 500, 1)
            .expect("VP8 エンコーダを作成できること");
        let frames = encoder
            .encode(&test_frame(64, 48), 1_000_000)
            .expect("VP8 エンコードに成功すること");
        assert!(!frames.is_empty(), "エンコード結果が 1 件以上あること");
        assert!(
            frames[0].is_keyframe,
            "先頭フレームがキーフレームであること"
        );
        assert!(!frames[0].data.is_empty(), "圧縮データが空でないこと");
        assert_eq!(
            frames[0].timestamp, 1_000_000,
            "先頭フレームの Timestamp は入力の時刻をそのまま使うこと"
        );
        assert!(
            frames.iter().all(|f| f.video_config.is_none()),
            "VP8 では PROP_VIDEO_CONFIG を載せないこと"
        );
    }

    /// VP9 エンコードがキーフレームを返すこと
    #[test]
    fn vp9_encoder_emits_a_keyframe() {
        let mut encoder = VpxEncoder::new(VpxCodec::Vp9, 64, 48, 30, 500, 1)
            .expect("VP9 エンコーダを作成できること");
        let frames = encoder
            .encode(&test_frame(64, 48), 2_000_000)
            .expect("VP9 エンコードに成功すること");
        assert!(!frames.is_empty(), "エンコード結果が 1 件以上あること");
        assert!(
            frames[0].is_keyframe,
            "先頭フレームがキーフレームであること"
        );
        assert!(!frames[0].data.is_empty(), "圧縮データが空でないこと");
    }

    /// catalog の codec 文字列が RFC 6381 の形で固定されていること
    #[test]
    fn catalog_codec_strings_are_fixed() {
        let vp8 = VpxEncoder::new(VpxCodec::Vp8, 64, 48, 30, 500, 1).expect("VP8 を作成できること");
        assert_eq!(
            vp8.catalog_codec_string(),
            "vp8",
            "VP8 は登録名のみであること"
        );

        let vp9 = VpxEncoder::new(VpxCodec::Vp9, 64, 48, 30, 500, 1).expect("VP9 を作成できること");
        assert_eq!(
            vp9.catalog_codec_string(),
            "vp09.00.40.08",
            "VP9 は profile / level / bit depth を載せること"
        );
    }

    /// fps が 0 の場合はエラーになること
    #[test]
    fn zero_fps_is_rejected() {
        assert!(
            VpxEncoder::new(VpxCodec::Vp8, 64, 48, 0, 500, 1).is_err(),
            "fps が 0 の場合はエラーになること"
        );
    }
}
