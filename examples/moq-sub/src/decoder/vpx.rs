//! shiguredo_libvpx (libvpx) による VP8 / VP9 デコーダ実装
//!
//! VP8 と VP9 は libvpx の API が共通であるため 1 つのモジュールにまとめ、
//! [`VpxCodec`] で切り替える。デコード結果は既存のコーデックと同じく stride を詰めた
//! I420 (`DecodedVideoFrame`) にする。

use shiguredo_libvpx::{Decoder, DecoderCodec, DecoderConfig};

use crate::decoder::{DecodedVideoFrame, copy_plane};
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
    /// libvpx に渡すコーデック種別
    fn decoder_codec(self) -> DecoderCodec {
        match self {
            Self::Vp8 => DecoderCodec::Vp8,
            Self::Vp9 => DecoderCodec::Vp9,
        }
    }
}

/// VP8 / VP9 デコーダ
pub(crate) struct VpxDecoder {
    decoder: Decoder,
}

impl VpxDecoder {
    /// VP8 / VP9 デコーダを作成する
    pub(crate) fn new(codec: VpxCodec) -> Result<Self> {
        let decoder = Decoder::new(DecoderConfig::new(codec.decoder_codec()))?;
        Ok(Self { decoder })
    }

    /// 1 オブジェクト分のエンコード済みペイロードをデコードする
    ///
    /// `video_config` は使わない。VP8 / VP9 には avcC / hvcC に相当する codec 固有の
    /// 設定データが無く、必要な情報はペイロード内のフレームヘッダに含まれる。
    pub(crate) fn decode(
        &mut self,
        payload: &[u8],
        _video_config: Option<&[u8]>,
    ) -> Result<Vec<DecodedVideoFrame>> {
        self.decoder.decode(payload)?;
        let mut frames = Vec::new();
        while let Some(frame) = self.decoder.next_frame()? {
            // Profile 0 (8-bit 4:2:0) のみを扱う。Profile 2 (10-bit / 12-bit) の
            // プレーンは 16-bit であり、I420 前提の DecodedVideoFrame に載せられない
            if frame.is_high_depth() {
                return Err(Error::Other(
                    "10-bit VP8/VP9 frames are not supported (only Profile 0)".to_string(),
                ));
            }
            let w = frame.width();
            let h = frame.height();
            let y = copy_plane(frame.y_plane(), frame.y_stride(), w, h);
            let u = copy_plane(frame.u_plane(), frame.u_stride(), w / 2, h / 2);
            let v = copy_plane(frame.v_plane(), frame.v_stride(), w / 2, h / 2);
            frames.push(DecodedVideoFrame {
                y,
                u,
                v,
                width: w as i32,
                height: h as i32,
                // Timestamp は Object の Properties から pipeline が載せる
                timestamp_us: None,
            });
        }
        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use shiguredo_libvpx::{
        CodecConfig, EncodeOptions, Encoder, EncoderConfig, ImageData, ImageFormat, Vp8Config,
        Vp9Config,
    };

    use super::*;

    /// テスト用の I420 フレームを作る
    ///
    /// デコーダの出力が入力の寸法と一致することを確かめられるよう、単色ではなく
    /// 位置に応じて値を変える。
    fn test_i420(width: usize, height: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let mut y = vec![0u8; width * height];
        for row in 0..height {
            for col in 0..width {
                y[row * width + col] = ((row + col) % 200) as u8;
            }
        }
        let u = vec![96u8; width / 2 * (height / 2)];
        let v = vec![160u8; width / 2 * (height / 2)];
        (y, u, v)
    }

    /// テスト用に 1 フレームをエンコードして圧縮データを返す
    fn encode_keyframe(codec: VpxCodec, width: usize, height: usize) -> Vec<u8> {
        let codec_config = match codec {
            VpxCodec::Vp8 => CodecConfig::Vp8(Vp8Config::default()),
            VpxCodec::Vp9 => CodecConfig::Vp9(Vp9Config::default()),
        };
        let mut config = EncoderConfig::new(width, height, ImageFormat::I420, codec_config);
        config.keyframe_interval = NonZeroUsize::new(1);
        // 先読みを 1 にしないと VP9 は既定の 25 フレームをバッファし、1 回の encode では
        // 出力が得られない
        config.lag_in_frames = NonZeroUsize::new(1);
        let mut encoder = Encoder::new(config).expect("テスト用エンコーダを作成できること");

        let (y, u, v) = test_i420(width, height);
        let image = ImageData::I420 {
            y: &y,
            u: &u,
            v: &v,
        };
        encoder
            .encode(
                &image,
                &EncodeOptions {
                    force_keyframe: true,
                },
            )
            .expect("テスト用フレームをエンコードできること");
        let frame = encoder.next_frame().expect("エンコード結果が得られること");
        assert!(
            frame.is_keyframe(),
            "テスト用フレームがキーフレームであること"
        );
        frame.data().to_vec()
    }

    /// VP9 のキーフレームをデコードして I420 が得られること
    #[test]
    fn vp9_keyframe_decodes_to_i420() {
        let payload = encode_keyframe(VpxCodec::Vp9, 64, 48);
        let mut decoder = VpxDecoder::new(VpxCodec::Vp9).expect("VP9 デコーダを作成できること");
        let frames = decoder
            .decode(&payload, None)
            .expect("VP9 のキーフレームをデコードできること");
        assert_eq!(frames.len(), 1, "1 フレームがデコードされること");
        let frame = &frames[0];
        assert_eq!(frame.width, 64, "幅が入力と一致すること");
        assert_eq!(frame.height, 48, "高さが入力と一致すること");
        assert_eq!(frame.y.len(), 64 * 48, "Y プレーンのサイズが一致すること");
        assert_eq!(frame.u.len(), 32 * 24, "U プレーンのサイズが一致すること");
        assert_eq!(frame.v.len(), 32 * 24, "V プレーンのサイズが一致すること");
        assert!(
            frame.timestamp_us.is_none(),
            "Timestamp は pipeline が載せるため None であること"
        );
    }

    /// VP8 のキーフレームをデコードして I420 が得られること
    #[test]
    fn vp8_keyframe_decodes_to_i420() {
        let payload = encode_keyframe(VpxCodec::Vp8, 64, 48);
        let mut decoder = VpxDecoder::new(VpxCodec::Vp8).expect("VP8 デコーダを作成できること");
        let frames = decoder
            .decode(&payload, None)
            .expect("VP8 のキーフレームをデコードできること");
        assert_eq!(frames.len(), 1, "1 フレームがデコードされること");
        assert_eq!(frames[0].width, 64, "幅が入力と一致すること");
        assert_eq!(frames[0].height, 48, "高さが入力と一致すること");
        assert_eq!(
            frames[0].y.len(),
            64 * 48,
            "Y プレーンのサイズが一致すること"
        );
    }

    /// 不正なペイロードはエラーになること
    #[test]
    fn invalid_payload_is_rejected() {
        let mut decoder = VpxDecoder::new(VpxCodec::Vp9).expect("VP9 デコーダを作成できること");
        assert!(
            decoder.decode(&[0xFF, 0xFF, 0xFF, 0xFF], None).is_err(),
            "不正なペイロードはエラーになること"
        );
    }
}
