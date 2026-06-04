//! raden を用いた疑似キャプチャ
//!
//! 実カメラデバイスを掴まずに、raden でアニメーションを描画し、BT.601
//! limited range で NV12 に変換してキャプチャフレームとして送信する。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use raden::{
    Circle, CompOp, Context, Image, PipelineRuntime, PixelFormat as RadenPixelFormat, Rgba32,
};
use shiguredo_video_device::{PixelFormat as VdPixelFormat, VideoFrameOwned};
use tokio::sync::mpsc;

use crate::cli::Config;
use crate::error::{Error, Result};

/// ウェーブパターンの本数
const NUM_WAVES: usize = 5;
/// バウンドする円の数
const NUM_BALLS: usize = 8;
/// 周回する円の数
const NUM_SHAPES: usize = 6;

/// 疑似キャプチャのハンドル。
///
/// Drop 時に描画スレッドへ停止を要求する。
pub struct FakeCapture {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for FakeCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// 停止要求を見落とさないよう短めに分割して sleep する
///
/// 停止要求があれば true を返す。呼び出し側は true の場合に処理を打ち切ること。
pub(crate) fn sleep_interruptibly(stop: &AtomicBool, duration: Duration) -> bool {
    let chunk = Duration::from_millis(50);
    let mut remaining = duration;
    while remaining > Duration::ZERO {
        if stop.load(Ordering::Acquire) {
            return true;
        }
        let step = remaining.min(chunk);
        std::thread::sleep(step);
        remaining = remaining.saturating_sub(step);
    }
    false
}

/// 疑似キャプチャを開始する
pub fn start_capture(
    config: &Config,
    sender: mpsc::Sender<VideoFrameOwned>,
) -> Result<FakeCapture> {
    if config.width == 0 || config.height == 0 {
        return Err(Error::Other("width/height must be non-zero".to_string()));
    }
    if !config.width.is_multiple_of(2) || !config.height.is_multiple_of(2) {
        return Err(Error::Other(
            "fake capture requires even width and height (NV12 4:2:0)".to_string(),
        ));
    }
    if config.fps == 0 {
        return Err(Error::Other("fps must be non-zero".to_string()));
    }

    let width = config.width;
    let height = config.height;
    let fps = config.fps;
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();

    let handle = std::thread::Builder::new()
        .name("fake-capture".to_string())
        .spawn(move || run_capture_loop(width, height, fps, stop_thread, sender))
        .map_err(|e| Error::Other(format!("failed to spawn fake capture thread: {e}")))?;

    tracing::info!(
        "Fake capture started using raden ({}x{} @ {} fps)",
        width,
        height,
        fps
    );

    Ok(FakeCapture {
        stop,
        handle: Some(handle),
    })
}

/// 描画ループ本体
fn run_capture_loop(
    width: u32,
    height: u32,
    fps: u32,
    stop: Arc<AtomicBool>,
    sender: mpsc::Sender<VideoFrameOwned>,
) {
    let mut image = Image::new(width, height, RadenPixelFormat::Prgb32);
    let mut runtime = PipelineRuntime::new();
    let frame_interval = Duration::from_secs_f64(1.0 / fps as f64);
    let w = width as f64;
    let h = height as f64;

    let y_len = width as usize * height as usize;
    let uv_len = width as usize * (height as usize / 2);
    let mut y_plane = vec![0u8; y_len];
    let mut uv_plane = vec![0u8; uv_len];

    let start = Instant::now();
    let mut next_deadline = start;

    while !stop.load(Ordering::Acquire) {
        let frame_start = Instant::now();
        let t = frame_start.duration_since(start).as_secs_f64();

        {
            let mut ctx = Context::new(&mut image, &mut runtime);
            render_frame(&mut ctx, t, w, h);
            ctx.end();
        }

        bgra_to_nv12(
            image.data(),
            image.stride(),
            width as usize,
            height as usize,
            &mut y_plane,
            &mut uv_plane,
        );

        let owned = VideoFrameOwned {
            data: y_plane.clone(),
            uv_data: Some(uv_plane.clone()),
            width: width as i32,
            height: height as i32,
            stride: width as i32,
            stride_uv: width as i32,
            pixel_format: VdPixelFormat::Nv12,
            timestamp_us: (t * 1_000_000.0) as i64,
            pixel_buffer: None,
        };

        // チャネルが満杯の場合はフレームを破棄する (実キャプチャと同じ挙動)
        match sender.try_send(owned) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {}
            Err(mpsc::error::TrySendError::Closed(_)) => break,
        }

        next_deadline += frame_interval;
        let now = Instant::now();
        if next_deadline > now {
            if sleep_interruptibly(&stop, next_deadline - now) {
                return;
            }
        } else {
            // フレーム生成が追いつかない場合は基準を現在時刻に戻す
            next_deadline = now;
        }
    }
}

/// アニメーションフレームを描画する (raden examples/animation.rs 相当)
fn render_frame(ctx: &mut Context, t: f64, w: f64, h: f64) {
    ctx.set_comp_op(CompOp::SrcCopy);
    let bg_hue = (t * 20.0) % 360.0;
    let (br, bg, bb) = hsv_to_rgb(bg_hue, 0.3, 0.15);
    ctx.set_fill_style(Rgba32::rgb(br, bg, bb));
    ctx.fill_all();

    ctx.set_comp_op(CompOp::SrcOver);

    for wave_idx in 0..NUM_WAVES {
        let wi = wave_idx as f64;
        let wave_offset = wi * 0.5;
        let wave_amplitude = 50.0 + wi * 20.0;
        let wave_freq = 0.008 + wi * 0.002;
        let wave_speed = 2.0 + wi * 0.3;
        let wave_y_base = h * 0.3 + wi * 80.0;

        let wave_hue = (t * 60.0 + wi * 50.0) % 360.0;
        let (wr, wg, wb) = hsv_to_rgb(wave_hue, 0.8, 0.9);
        ctx.set_fill_style(Rgba32::new(wr, wg, wb, 150));

        let mut x = 0.0;
        while x < w {
            let y =
                wave_y_base + wave_amplitude * (wave_freq * x + t * wave_speed + wave_offset).sin();
            let radius = 8.0 + 4.0 * (t * 3.0 + x * 0.01).sin();
            ctx.fill_circle(&Circle::new(x, y, radius));
            x += 20.0;
        }
    }

    for ball_idx in 0..NUM_BALLS {
        let bi = ball_idx as f64;
        let freq_x = 0.5 + bi * 0.15;
        let freq_y = 0.7 + bi * 0.12;
        let phase_x = bi * std::f64::consts::PI / 4.0;
        let phase_y = bi * std::f64::consts::PI / 3.0;

        let bx = w * 0.5 + (w * 0.35) * (t * freq_x + phase_x).sin();
        let by = h * 0.5 + (h * 0.3) * (t * freq_y + phase_y).sin();
        let ball_radius = 30.0 + 15.0 * (t * 4.0 + bi).sin();

        let ball_hue = (bi * 45.0 + t * 100.0) % 360.0;
        let (cr, cg, cb) = hsv_to_rgb(ball_hue, 1.0, 1.0);
        ctx.set_fill_style(Rgba32::new(cr, cg, cb, 200));
        ctx.fill_circle(&Circle::new(bx, by, ball_radius));

        let hl_radius = ball_radius * 0.3;
        ctx.set_fill_style(Rgba32::new(255, 255, 255, 100));
        ctx.fill_circle(&Circle::new(
            bx - ball_radius * 0.3,
            by - ball_radius * 0.3,
            hl_radius,
        ));
    }

    let center_x = w * 0.5;
    let center_y = h * 0.5;
    for shape_idx in 0..NUM_SHAPES {
        let si = shape_idx as f64;
        let angle = t * (1.0 + si * 0.2) + si * std::f64::consts::PI / 3.0;
        let dist = 150.0 + 50.0 * (t * 2.0 + si).sin();
        let sx = center_x + dist * angle.cos();
        let sy = center_y + dist * angle.sin();

        let shape_hue = (si * 60.0 + t * 80.0) % 360.0;
        let (sr, sg, sb) = hsv_to_rgb(shape_hue, 0.9, 0.95);

        let shape_radius = 25.0 + 15.0 * (t * 3.0 + si).sin();
        ctx.set_fill_style(Rgba32::new(sr, sg, sb, 180));
        ctx.fill_circle(&Circle::new(sx, sy, shape_radius));
    }
}

/// HSV を RGB に変換する (h: 0-360, s: 0-1, v: 0-1)
fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (u8, u8, u8) {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;

    let (r, g, b) = if h < 60.0 {
        (c, x, 0.0)
    } else if h < 120.0 {
        (x, c, 0.0)
    } else if h < 180.0 {
        (0.0, c, x)
    } else if h < 240.0 {
        (0.0, x, c)
    } else if h < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };

    (
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

/// raden の Prgb32 (BGRA byte order) を NV12 (BT.601 limited range) に変換する
///
/// 背景を常に不透明で塗りつぶしているため、premultiplied の除算は省略できる。
/// 出力 NV12 のストライドは幅と同一。
fn bgra_to_nv12(
    bgra: &[u8],
    bgra_stride: usize,
    width: usize,
    height: usize,
    y_plane: &mut [u8],
    uv_plane: &mut [u8],
) {
    debug_assert!(width.is_multiple_of(2) && height.is_multiple_of(2));
    debug_assert!(y_plane.len() >= width * height);
    debug_assert!(uv_plane.len() >= width * height / 2);

    for y in 0..height {
        let src_row_start = y * bgra_stride;
        let dst_row_start = y * width;
        for x in 0..width {
            let src = src_row_start + x * 4;
            let b = bgra[src] as i32;
            let g = bgra[src + 1] as i32;
            let r = bgra[src + 2] as i32;
            // BT.601 limited range: Y = 16 + (66R + 129G + 25B + 128) >> 8
            let y_val = 16 + ((66 * r + 129 * g + 25 * b + 128) >> 8);
            y_plane[dst_row_start + x] = y_val.clamp(0, 255) as u8;
        }
    }

    let chroma_height = height / 2;
    for cy in 0..chroma_height {
        let y0 = cy * 2;
        let y1 = y0 + 1;
        let row0_start = y0 * bgra_stride;
        let row1_start = y1 * bgra_stride;
        let dst_row_start = cy * width;
        for cx in 0..(width / 2) {
            let x0 = cx * 2;
            let x1 = x0 + 1;
            let mut b_sum = 0i32;
            let mut g_sum = 0i32;
            let mut r_sum = 0i32;
            for (row_start, col) in [
                (row0_start, x0),
                (row0_start, x1),
                (row1_start, x0),
                (row1_start, x1),
            ] {
                let src = row_start + col * 4;
                b_sum += bgra[src] as i32;
                g_sum += bgra[src + 1] as i32;
                r_sum += bgra[src + 2] as i32;
            }
            let r = r_sum / 4;
            let g = g_sum / 4;
            let b = b_sum / 4;
            // BT.601 limited range
            // U = 128 + (-38R - 74G + 112B + 128) >> 8
            // V = 128 + (112R - 94G - 18B + 128) >> 8
            let u_val = 128 + ((-38 * r - 74 * g + 112 * b + 128) >> 8);
            let v_val = 128 + ((112 * r - 94 * g - 18 * b + 128) >> 8);
            uv_plane[dst_row_start + x0] = u_val.clamp(0, 255) as u8;
            uv_plane[dst_row_start + x1] = v_val.clamp(0, 255) as u8;
        }
    }
}
