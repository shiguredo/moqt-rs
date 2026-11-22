//! ビデオデコーダの Enum 抽象化
//!
//! MP4 再エンコード配信の入力デコードに使う。codec ごとの実装を [`VideoDecoder`] の
//! variant に持ち、`match` で分岐する。具体実装は共有 crate (`moq-codec`) にある。

use moq_codec::DecodedVideoFrame;
use moq_codec::av1::Av1Decoder;
#[cfg(target_os = "macos")]
use moq_codec::h264::H264Decoder;
#[cfg(target_os = "macos")]
use moq_codec::h265::H265Decoder;

use crate::error::Result;

/// 再エンコード配信で扱うビデオデコーダ
///
/// codec ごとの実装を variant に持つ Enum とし、呼び出し側は本型のメソッド経由で
/// 利用する。macOS 限定の codec は variant 単位で `cfg` を付ける。
pub(crate) enum VideoDecoder {
    /// AV1 デコーダ (dav1d、全プラットフォーム)
    Av1(Av1Decoder),
    /// H.264 デコーダ (Apple Video Toolbox、macOS 限定)
    #[cfg(target_os = "macos")]
    H264(H264Decoder),
    /// H.265 デコーダ (Apple Video Toolbox、macOS 限定)
    #[cfg(target_os = "macos")]
    H265(H265Decoder),
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
            VideoDecoder::Av1(d) => Ok(d.decode(payload, video_config)?),
            #[cfg(target_os = "macos")]
            VideoDecoder::H264(d) => Ok(d.decode(payload, video_config)?),
            #[cfg(target_os = "macos")]
            VideoDecoder::H265(d) => Ok(d.decode(payload, video_config)?),
        }
    }

    /// デコーダが内部に保持している遅延フレームを吐き切る (周回の末尾で使う)
    ///
    /// AV1 (dav1d) だけが遅延フレームを持つ。Video Toolbox は各フレームを即座に出力するため
    /// 常に空を返す。`reset` より前に呼ぶこと (`reset` は遅延フレームを破棄する)。
    pub fn drain_delayed(&mut self) -> Result<Vec<DecodedVideoFrame>> {
        match self {
            VideoDecoder::Av1(d) => Ok(d.drain_delayed()?),
            #[cfg(target_os = "macos")]
            VideoDecoder::H264(_) | VideoDecoder::H265(_) => Ok(Vec::new()),
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

#[cfg(test)]
mod tests {
    /// H.264 / H.265 の encode→decode テスト
    ///
    /// デコーダは共有 crate に移したが、エンコーダは publisher 側にあるため、実コーデックでの
    /// encode→decode の検証はここで行う。
    #[cfg(target_os = "macos")]
    mod h264 {
        use crate::encoder::h264::H264Encoder;
        use crate::encoder::test_frame;
        use moq_codec::h264::H264Decoder;

        /// エンコードした H.264 のフレームをデコードできること
        ///
        /// デコーダの入力は publisher が組み立てた avcC と length-prefixed NAL のサンプルである。
        /// 実際の Video Toolbox でエンコードしたフレームをそのまま入力に使い、入力 1 件につき
        /// 1 フレームがデコードされることと、プレーンが stride 除去済みの I420 で返ることを
        /// 確認する。Video Toolbox は出力を遅延させるため、出力が届くまで 30 フレーム投入する。
        #[test]
        fn encoded_frames_are_decoded() {
            let mut encoder =
                H264Encoder::new(64, 48, 30, 500, 60).expect("H.264 エンコーダを作成できること");
            let frame = test_frame(64, 48);
            let mut encoded_frames = Vec::new();
            let mut video_config = None;
            for index in 0..30u64 {
                for encoded in encoder
                    .encode(&frame, index * 33_333, index == 0)
                    .expect("H.264 のエンコードに成功すること")
                {
                    if let Some(config) = &encoded.video_config {
                        video_config = Some(config.clone());
                    }
                    encoded_frames.push(encoded);
                }
            }
            let video_config = video_config.expect("キーフレームの avcC が得られること");
            assert!(
                encoded_frames.len() >= 2,
                "エンコード結果が 2 件以上あること"
            );

            let mut decoder = H264Decoder::new().expect("H.264 デコーダを作成できること");
            let mut decoded_frames = Vec::new();
            for encoded in &encoded_frames {
                decoded_frames.extend(
                    decoder
                        .decode(&encoded.data, Some(&video_config))
                        .expect("H.264 のデコードに成功すること"),
                );
            }
            assert_eq!(
                decoded_frames.len(),
                encoded_frames.len(),
                "入力 1 件につき 1 フレームがデコードされること"
            );
            for decoded in &decoded_frames {
                assert_eq!(
                    (decoded.width, decoded.height),
                    (64, 48),
                    "映像サイズが入力と同じであること"
                );
                assert_eq!(
                    decoded.y.len(),
                    64 * 48,
                    "Y プレーンが stride 除去済みであること"
                );
                assert_eq!(
                    decoded.u.len(),
                    32 * 24,
                    "U プレーンが stride 除去済みであること"
                );
                assert_eq!(
                    decoded.v.len(),
                    32 * 24,
                    "V プレーンが stride 除去済みであること"
                );
            }
        }
    }

    /// H.265 の encode→decode テスト
    ///
    /// デコーダは共有 crate に移したが、エンコーダは publisher 側にあるため、実コーデックでの
    /// encode→decode の検証はここで行う。
    #[cfg(target_os = "macos")]
    mod h265 {
        use crate::encoder::h265::H265Encoder;
        use crate::encoder::test_frame;
        use moq_codec::h265::H265Decoder;

        /// エンコードした H.265 のフレームをデコードできること
        ///
        /// デコーダの入力は publisher が組み立てた hvcC と length-prefixed NAL のサンプルである。
        /// 実際の Video Toolbox でエンコードしたフレームをそのまま入力に使い、入力 1 件につき
        /// 1 フレームがデコードされることと、プレーンが stride 除去済みの I420 で返ることを
        /// 確認する。Video Toolbox は出力を遅延させるため、出力が届くまで 30 フレーム投入する。
        #[test]
        fn encoded_frames_are_decoded() {
            let mut encoder =
                H265Encoder::new(64, 48, 30, 500, 60).expect("H.265 エンコーダを作成できること");
            let frame = test_frame(64, 48);
            let mut encoded_frames = Vec::new();
            let mut video_config = None;
            for index in 0..30u64 {
                for encoded in encoder
                    .encode(&frame, index * 33_333, index == 0)
                    .expect("H.265 のエンコードに成功すること")
                {
                    if let Some(config) = &encoded.video_config {
                        video_config = Some(config.clone());
                    }
                    encoded_frames.push(encoded);
                }
            }
            let video_config = video_config.expect("キーフレームの hvcC が得られること");
            assert!(
                encoded_frames.len() >= 2,
                "エンコード結果が 2 件以上あること"
            );

            let mut decoder = H265Decoder::new().expect("H.265 デコーダを作成できること");
            let mut decoded_frames = Vec::new();
            for encoded in &encoded_frames {
                decoded_frames.extend(
                    decoder
                        .decode(&encoded.data, Some(&video_config))
                        .expect("H.265 のデコードに成功すること"),
                );
            }
            assert_eq!(
                decoded_frames.len(),
                encoded_frames.len(),
                "入力 1 件につき 1 フレームがデコードされること"
            );
            for decoded in &decoded_frames {
                assert_eq!(
                    (decoded.width, decoded.height),
                    (64, 48),
                    "映像サイズが入力と同じであること"
                );
                assert_eq!(
                    decoded.y.len(),
                    64 * 48,
                    "Y プレーンが stride 除去済みであること"
                );
                assert_eq!(
                    decoded.u.len(),
                    32 * 24,
                    "U プレーンが stride 除去済みであること"
                );
                assert_eq!(
                    decoded.v.len(),
                    32 * 24,
                    "V プレーンが stride 除去済みであること"
                );
            }
        }
    }
}
