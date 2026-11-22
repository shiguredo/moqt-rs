//! moq-pub / moq-sub の example が共有する codec デコーダ
//!
//! AV1 / H.264 / H.265 / Opus のデコード処理と、設定レコード (avcC / hvcC) のパースで
//! 使う共通ヘルパを提供する。codec 選択とアプリ固有のフレーム表現は各 example 側に置く。
//!
//! H.264 / H.265 は Apple Video Toolbox を使うため macOS 限定である
//! (`shiguredo_video_toolbox` も macOS 限定の依存にしている)。

pub mod av1;
pub mod error;
#[cfg(target_os = "macos")]
pub mod h264;
#[cfg(target_os = "macos")]
pub mod h265;
pub mod opus;

use crate::error::{Error, Result};

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

/// stride 付きプレーンを連続バッファにコピーする
///
/// 入力バイナリのデコードで使うため、サイズの事前確保はしない。
/// `src` から `height` 行をすべて読み出せる長さでない場合はパニックする。
pub fn copy_plane(src: &[u8], stride: usize, width: usize, height: usize) -> Vec<u8> {
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
/// H.264 / H.265 の設定レコード (avcC / hvcC) のパースで使う。
/// `codec_label` はエラーメッセージ用 (例: "AVCDecoderConfigurationRecord")。
pub fn read_ps_nal(buf: &[u8], pos: &mut usize, codec_label: &str) -> Result<Vec<u8>> {
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

/// `u16 length + NAL` を読み飛ばす
///
/// H.264 の設定レコード (avcC) のパースで、先頭以外の SPS / PPS を飛ばすために使う。
/// `codec_label` はエラーメッセージ用 (例: "AVCDecoderConfigurationRecord")。
pub fn skip_ps_nal(buf: &[u8], pos: &mut usize, codec_label: &str) -> Result<()> {
    if buf.len() < *pos + 2 {
        return Err(Error::Other(format!("{codec_label}: truncated NAL length")));
    }
    let len = u16::from_be_bytes([buf[*pos], buf[*pos + 1]]) as usize;
    *pos += 2 + len;
    if buf.len() < *pos {
        return Err(Error::Other(format!(
            "{codec_label}: truncated NAL payload"
        )));
    }
    Ok(())
}
