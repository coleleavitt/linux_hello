//! V4L2 capture: the RGB camera for liveness, and the IR camera as lit/ambient frame pairs.
//!
//! The IR emitter lights every other frame by itself (L1). Which frames are lit comes from the
//! uvcvideo metadata node of the IR camera (`MetadataId_FrameIllumination`, L2/L3); without that
//! node the brighter frame of two consecutive frames is taken as the lit one (F15).

use anyhow::{bail, Context, Result};
use image::{GrayImage, RgbImage};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};
use v4l::buffer::Type;
use v4l::device::Handle;
use v4l::io::traits::CaptureStream;
use v4l::prelude::*;
use v4l::v4l_sys::v4l2_rect;
use v4l::video::Capture;
use v4l::FourCC;

/// IR frames dropped after the stream starts. The camera resumes near the exposure it had when
/// it last streamed and converges by 1–2 % per frame, so the first lit frames are 15–40 % too
/// bright or too dark; after 4 frames they are within ≈ 30 % of the settled value, which
/// changes a print's cosine similarity by < 0.02 (docs/capture-and-liveness.md §1).
const WARMUP_FRAMES: usize = 4;
/// A lit frame is only paired with an ambient frame closer in time than this (F13).
pub(crate) const MAX_PAIR_GAP: Duration = Duration::from_millis(300);
/// Frames read while looking for one lit/ambient pair before giving up.
const MAX_PAIR_FRAMES: usize = 8;
/// `V4L2_META_FMT_UVC_MSXU_1_5`: uvcvideo metadata with the Microsoft UVC 1.5 items.
const META_FORMAT: &[u8; 4] = b"UVCM";
const META_BUFFERS: u32 = 4;
/// The metadata node only yields buffers while the IR stream runs; a node that stays silent
/// (wrong device) must not hang the daemon.
const META_TIMEOUT: Duration = Duration::from_secs(1);
/// `MetadataId_FrameIllumination` in Microsoft's UVC metadata; payload `u32 Flags` (bit 0 = on).
const METADATA_FRAME_ILLUMINATION: u32 = 6;
/// `V4L2_CID_REGION_OF_INTEREST_RECT` and `_AUTO`: the UVC 1.5 `CT_REGION_OF_INTEREST_CONTROL`
/// as two V4L2 controls (Linux 6.14+), the rectangle and the bitmask of what it steers.
const ROI_RECT: u32 = 0x0098_1ae1;
const ROI_AUTO: u32 = 0x0098_1ae2;
/// `V4L2_UVC_REGION_OF_INTEREST_AUTO_EXPOSURE`: auto-exposure meters the rectangle.
const ROI_AUTO_EXPOSURE: i32 = 1;

/// Camera type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraType {
    Rgb,
    Ir,
}

/// One grayscale frame with its V4L2 buffer sequence number and timestamp.
struct GrayFrame {
    image: GrayImage,
    sequence: u32,
    time: Duration,
}

/// V4L2 camera wrapper
pub struct Camera {
    stream: MmapStream<'static>,
    width: u32,
    height: u32,
    fourcc: [u8; 4],
    camera_type: CameraType,
}

impl Camera {
    /// Open a camera by device path, dropping `warmup` frames after the stream starts, with
    /// `buffers` V4L2 buffers. Fewer buffers keep the returned frames closer to "now" when the
    /// stream is read slower than it runs.
    pub fn open_with(path: impl AsRef<Path>, camera_type: CameraType, warmup: usize, buffers: u32) -> Result<Self> {
        let path = path.as_ref();
        info!(?path, ?camera_type, "opening camera");

        let device = Device::with_path(path).with_context(|| format!("failed to open {}", path.display()))?;

        let caps = device.query_caps()?;
        debug!(?caps, "camera capabilities");

        // Set format
        let format = match camera_type {
            CameraType::Ir => device
                .set_format(&v4l::Format::new(640, 480, FourCC::new(b"GREY")))
                .or_else(|_| device.set_format(&v4l::Format::new(640, 480, FourCC::new(b"YUYV"))))?,
            CameraType::Rgb => device.set_format(&v4l::Format::new(640, 480, FourCC::new(b"YUYV")))?,
        };

        let width = format.width;
        let height = format.height;
        let fourcc = format.fourcc.repr;
        debug!(width, height, fourcc = ?fourcc, "camera format");

        let stream = MmapStream::with_buffers(&device, Type::VideoCapture, buffers)?;
        let mut camera = Self { stream, width, height, fourcc, camera_type };
        camera.skip(warmup)?;
        Ok(camera)
    }

    /// Read and drop `n` frames.
    pub fn skip(&mut self, n: usize) -> Result<()> {
        for _ in 0..n {
            self.stream.next()?;
        }
        Ok(())
    }

    /// Give up waiting for a frame after `timeout`; the capture then fails with an error for
    /// which [`timed_out`] is true.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.stream.set_timeout(timeout);
    }

    /// Capture a single frame as grayscale
    fn capture_gray(&mut self) -> Result<GrayFrame> {
        let (buf, meta) = self.stream.next()?;

        let image = match &self.fourcc {
            [b'G', b'R', b'E', b'Y'] => {
                GrayImage::from_raw(self.width, self.height, buf.to_vec()).context("invalid GREY frame")?
            }
            [b'Y', b'U', b'Y', b'V'] => {
                let y: Vec<u8> = buf.iter().step_by(2).copied().collect();
                GrayImage::from_raw(self.width, self.height, y).context("invalid YUYV frame")?
            }
            other => bail!("unsupported format: {:?}", other),
        };
        let time = Duration::from_secs(meta.timestamp.sec as u64) + Duration::from_micros(meta.timestamp.usec as u64);
        Ok(GrayFrame { image, sequence: meta.sequence, time })
    }

    /// Capture a single frame as RGB
    pub fn capture_rgb(&mut self) -> Result<RgbImage> {
        let (buf, _meta) = self.stream.next()?;

        let mut rgb = Vec::with_capacity((self.width * self.height * 3) as usize);
        for chunk in buf.chunks(4) {
            if chunk.len() < 4 {
                break;
            }
            let (y0, u, y1, v) = (chunk[0] as f32, chunk[1] as f32, chunk[2] as f32, chunk[3] as f32);

            for y in [y0, y1] {
                let r = (y + 1.402 * (v - 128.0)).clamp(0.0, 255.0) as u8;
                let g = (y - 0.344136 * (u - 128.0) - 0.714136 * (v - 128.0)).clamp(0.0, 255.0) as u8;
                let b = (y + 1.772 * (u - 128.0)).clamp(0.0, 255.0) as u8;
                rgb.extend_from_slice(&[r, g, b]);
            }
        }

        RgbImage::from_raw(self.width, self.height, rgb).context("invalid RGB conversion")
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn camera_type(&self) -> CameraType {
        self.camera_type
    }
}

/// An IR frame with the emitter on and the ambient-only frame next to it.
pub struct IrPair {
    pub lit: GrayImage,
    pub ambient: GrayImage,
    /// Buffer timestamps of the two frames (`CLOCK_MONOTONIC`)
    pub lit_at: Duration,
    pub ambient_at: Duration,
}

impl IrPair {
    /// Time between the two frames
    pub fn gap(&self) -> Duration {
        self.lit_at.abs_diff(self.ambient_at)
    }
}

/// One IR frame with its illumination flag.
pub struct IrFrame {
    pub image: GrayImage,
    /// V4L2 buffer timestamp
    pub time: Duration,
    /// Whether the emitter lit the frame; `None` without metadata, or when the frame's metadata
    /// was dropped
    pub lit: Option<bool>,
}

/// The IR camera, read as lit/ambient pairs.
pub struct IrCamera {
    video: Camera,
    meta: Option<MetaStream>,
    /// `None` on cameras without region-of-interest controls
    exposure: Option<ExposureRegion>,
}

impl IrCamera {
    /// Open the IR camera and, if given, its metadata node (e.g. `/dev/video3`).
    pub fn open(device: &str, metadata: Option<&str>) -> Result<Self> {
        Self::open_with(device, metadata, WARMUP_FRAMES)
    }

    /// Open with an explicit warm-up frame count.
    pub fn open_with(device: &str, metadata: Option<&str>, warmup: usize) -> Result<Self> {
        // The metadata format can only be set while the metadata queue is idle, so set it before
        // the video stream starts.
        let meta = metadata.map(MetaStream::open).transpose()?;
        let video = Camera::open_with(device, CameraType::Ir, 0, 4)?;
        let exposure = ExposureRegion::open(video.stream.handle());
        let mut camera = Self { video, meta, exposure };
        // Read the metadata along with the warm-up frames so that it is in step with the video
        // by the first pair.
        for _ in 0..warmup {
            camera.capture_frame()?;
        }
        Ok(camera)
    }

    /// Two consecutive frames, one lit and one ambient, less than [`MAX_PAIR_GAP`] apart.
    pub fn capture_pair(&mut self) -> Result<IrPair> {
        let mut prev = self.capture_frame()?;
        for _ in 0..MAX_PAIR_FRAMES {
            let next = self.capture_frame()?;
            let gap = next.time.saturating_sub(prev.time);
            if gap < MAX_PAIR_GAP {
                if let Some(prev_lit) = self.is_first_lit(&prev, &next) {
                    let (lit, ambient) = if prev_lit { (prev, next) } else { (next, prev) };
                    return Ok(IrPair {
                        lit: lit.image,
                        ambient: ambient.image,
                        lit_at: lit.time,
                        ambient_at: ambient.time,
                    });
                }
            }
            debug!(?gap, prev = ?prev.lit, next = ?next.lit, "frames do not form a lit/ambient pair");
            prev = next;
        }
        bail!("no lit/ambient IR frame pair in {MAX_PAIR_FRAMES} frames (does the emitter alternate?)")
    }

    /// Whether the first of two frames is the lit one, or `None` if they are not a lit/ambient pair.
    fn is_first_lit(&self, a: &IrFrame, b: &IrFrame) -> Option<bool> {
        if self.meta.is_none() {
            return Some(brightness_sum(&a.image) >= brightness_sum(&b.image));
        }
        match (a.lit, b.lit) {
            (Some(x), Some(y)) if x != y => Some(x),
            _ => None,
        }
    }

    /// The next frame.
    pub fn capture_frame(&mut self) -> Result<IrFrame> {
        let frame = self.video.capture_gray()?;
        let lit = match self.meta.as_mut() {
            Some(meta) => meta.illumination(frame.sequence)?,
            None => None,
        };
        Ok(IrFrame { image: frame.image, time: frame.time, lit })
    }

    /// Meter auto-exposure on the face box `[x1, y1, x2, y2]` (lit-frame pixels) instead of the
    /// whole frame, as Windows' `SensorController` does (Q1). No-op on cameras without the
    /// controls; a camera that rejects the rectangle is left as it is.
    pub fn expose_on(&mut self, bbox: [f32; 4]) {
        let Some(exposure) = self.exposure.as_mut() else { return };
        let (w, h) = (self.video.width as f32, self.video.height as f32);
        let [x1, y1, x2, y2] = bbox;
        let (left, top) = (x1.clamp(0.0, w - 2.0).round(), y1.clamp(0.0, h - 2.0).round());
        let rect = v4l2_rect {
            left: left as i32,
            top: top as i32,
            width: (x2.round().clamp(left + 2.0, w) - left) as u32,
            height: (y2.round().clamp(top + 2.0, h) - top) as u32,
        };
        if let Err(e) = exposure.set(rect) {
            warn!(%e, ?rect, "cannot set the exposure region; metering the whole frame");
            self.exposure = None;
        }
    }
}

/// The IR camera's UVC region of interest: the rectangle auto-exposure meters instead of the
/// whole frame, so that a close face in front of a dark room does not overexpose.
struct ExposureRegion {
    handle: Arc<Handle>,
    /// The rectangle set last
    rect: v4l2_rect,
    /// The driver's default (the whole frame), restored when the camera closes
    default: v4l2_rect,
}

impl ExposureRegion {
    /// `None` when the camera has no region-of-interest controls. Starts from the default
    /// rectangle, in case an earlier run died with a face rectangle set.
    fn open(handle: Arc<Handle>) -> Option<Self> {
        let mut default = v4l2_rect { left: 0, top: 0, width: 0, height: 0 };
        let mut auto = 0;
        if let Err(e) = roi_controls(&handle, true, v4l::v4l_sys::V4L2_CTRL_WHICH_DEF_VAL, &mut default, &mut auto) {
            debug!(%e, "no region-of-interest controls; exposure meters the whole frame");
            return None;
        }
        let mut region = Self { handle, rect: default, default };
        if let Err(e) = region.write(default) {
            warn!(%e, "cannot set the exposure region; metering the whole frame");
            return None;
        }
        Some(region)
    }

    /// Meter exposure on `rect`, unless it is the current rectangle.
    fn set(&mut self, rect: v4l2_rect) -> std::io::Result<()> {
        if (rect.left, rect.top, rect.width, rect.height)
            == (self.rect.left, self.rect.top, self.rect.width, self.rect.height)
        {
            return Ok(());
        }
        debug!(?rect, "exposure region");
        self.write(rect)
    }

    fn write(&mut self, mut rect: v4l2_rect) -> std::io::Result<()> {
        let mut auto = ROI_AUTO_EXPOSURE;
        roi_controls(&self.handle, false, 0, &mut rect, &mut auto)?;
        self.rect = rect;
        Ok(())
    }
}

impl Drop for ExposureRegion {
    fn drop(&mut self) {
        if let Err(e) = self.write(self.default) {
            warn!(%e, "cannot restore the camera's exposure region");
        }
    }
}

/// `VIDIOC_G_EXT_CTRLS` (`get`, of the `which` value class) or `VIDIOC_S_EXT_CTRLS` on both
/// region-of-interest controls at once: they are one UVC control, and the device rejects a
/// bitmask set on its own. The v4l crate cannot read rect or bitmask controls, so this uses its
/// raw bindings.
fn roi_controls(handle: &Handle, get: bool, which: u32, rect: &mut v4l2_rect, auto: &mut i32) -> std::io::Result<()> {
    use v4l::v4l_sys::{
        v4l2_ext_control, v4l2_ext_control__bindgen_ty_1, v4l2_ext_controls, v4l2_ext_controls__bindgen_ty_1,
    };
    let mut controls = [
        v4l2_ext_control {
            id: ROI_RECT,
            size: std::mem::size_of::<v4l2_rect>() as u32,
            reserved2: [0],
            __bindgen_anon_1: v4l2_ext_control__bindgen_ty_1 { p_rect: rect },
        },
        v4l2_ext_control {
            id: ROI_AUTO,
            size: 0,
            reserved2: [0],
            __bindgen_anon_1: v4l2_ext_control__bindgen_ty_1 { value: *auto },
        },
    ];
    let mut ext = v4l2_ext_controls {
        __bindgen_anon_1: v4l2_ext_controls__bindgen_ty_1 { which },
        count: controls.len() as u32,
        error_idx: 0,
        request_fd: 0,
        reserved: [0],
        controls: controls.as_mut_ptr(),
    };
    let request = if get { v4l::v4l2::vidioc::VIDIOC_G_EXT_CTRLS } else { v4l::v4l2::vidioc::VIDIOC_S_EXT_CTRLS };
    // SAFETY: `ext`, `controls` and `rect` outlive the call; the ioctl writes 16 bytes to `rect`.
    unsafe {
        v4l::v4l2::ioctl(handle.fd(), request, &mut ext as *mut v4l2_ext_controls as *mut std::os::raw::c_void)?;
        // SAFETY: the driver filled `value` for the bitmask control.
        *auto = controls[1].__bindgen_anon_1.value;
    }
    Ok(())
}

/// Whether a capture error is the stream timeout set by [`Camera::set_timeout`].
pub fn timed_out(error: &anyhow::Error) -> bool {
    error.chain().any(|e| e.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::TimedOut))
}

/// Sum of every 5th pixel of every 5th row, like Windows' `FrameStrobe` (F15).
fn brightness_sum(image: &GrayImage) -> u64 {
    image.rows().step_by(5).flat_map(|row| row.step_by(5)).map(|p| p.0[0] as u64).sum()
}

/// The uvcvideo metadata node of the IR camera. Its buffers carry the sequence number of the
/// video frame they describe.
struct MetaStream {
    stream: MmapStream<'static>,
    /// A metadata buffer read ahead of its video frame
    pending: Option<(u32, Option<bool>)>,
}

impl MetaStream {
    fn open(path: &str) -> Result<Self> {
        info!(path, "opening IR metadata");
        let device = Device::with_path(path).with_context(|| format!("failed to open {path}"))?;
        set_meta_format(&device).with_context(|| format!("setting UVCM metadata format on {path}"))?;
        let mut stream = MmapStream::with_buffers(&device, Type::MetaCapture, META_BUFFERS)?;
        stream.set_timeout(META_TIMEOUT);
        Ok(Self { stream, pending: None })
    }

    /// The illumination flag of video frame `sequence`; `None` if its metadata was dropped or
    /// has no illumination item.
    fn illumination(&mut self, sequence: u32) -> Result<Option<bool>> {
        loop {
            let (seq, lit) = match self.pending.take() {
                Some(p) => p,
                None => {
                    let (buf, meta) = self.stream.next().context("reading IR metadata")?;
                    (meta.sequence, frame_illumination(&buf[..meta.bytesused as usize]))
                }
            };
            match seq.cmp(&sequence) {
                std::cmp::Ordering::Less => continue,
                std::cmp::Ordering::Equal => return Ok(lit),
                std::cmp::Ordering::Greater => {
                    self.pending = Some((seq, lit));
                    return Ok(None);
                }
            }
        }
    }
}

/// `VIDIOC_S_FMT` with `V4L2_BUF_TYPE_META_CAPTURE` and format UVCM. The v4l crate has no API
/// for metadata formats, so this uses its raw ioctl bindings. After boot the node defaults to
/// UVCH (plain payload headers), which lacks the Microsoft items.
fn set_meta_format(device: &Device) -> Result<()> {
    use v4l::v4l_sys::{v4l2_format, v4l2_format__bindgen_ty_1, v4l2_meta_format};
    // SAFETY: v4l2_format is a plain C struct; all-zero is a valid value.
    let mut format: v4l2_format = unsafe { std::mem::zeroed() };
    format.type_ = Type::MetaCapture as u32;
    format.fmt = v4l2_format__bindgen_ty_1 {
        meta: v4l2_meta_format { dataformat: u32::from_le_bytes(*META_FORMAT), ..unsafe { std::mem::zeroed() } },
    };
    // SAFETY: VIDIOC_S_FMT takes a pointer to a v4l2_format that lives for the call.
    unsafe {
        v4l::v4l2::ioctl(
            device.handle().fd(),
            v4l::v4l2::vidioc::VIDIOC_S_FMT,
            &mut format as *mut v4l2_format as *mut std::os::raw::c_void,
        )?;
    }
    // SAFETY: the driver filled the `meta` member for a META_CAPTURE format.
    let set = unsafe { format.fmt.meta.dataformat };
    if set != u32::from_le_bytes(*META_FORMAT) {
        bail!("driver chose metadata format {:?} instead of UVCM", set.to_le_bytes());
    }
    Ok(())
}

/// The `MetadataId_FrameIllumination` flag in one frame's UVCM metadata, if present.
fn frame_illumination(buf: &[u8]) -> Option<bool> {
    metadata_items(buf)
        .into_iter()
        .find(|(id, _)| *id == METADATA_FRAME_ILLUMINATION)
        .and_then(|(_, payload)| payload.get(..4))
        .map(|flags| u32::from_le_bytes(flags.try_into().expect("4 bytes")) & 1 != 0)
}

/// Microsoft metadata items `(id, payload)` in a UVCM buffer.
///
/// The buffer is a sequence of `struct uvc_meta_buf { u64 ns; u16 sof; u8 length; u8 flags;
/// u8 buf[length - 2]; }` blocks, where `length`/`flags` are the UVC payload header length and
/// `bmHeaderInfo`. `buf` starts with the PTS (4 bytes, flag 0x04) and SCR (6 bytes, flag 0x08)
/// when present, followed by items `{ u32 id; u32 size; u8 payload[size - 8]; }`.
fn metadata_items(buf: &[u8]) -> Vec<(u32, &[u8])> {
    let u32_at = |b: &[u8], i: usize| u32::from_le_bytes(b[i..i + 4].try_into().expect("4 bytes"));
    let mut items = Vec::new();
    let mut rest = buf;
    while rest.len() >= 12 {
        let (length, flags) = (rest[10] as usize, rest[11]);
        if length < 2 || rest.len() < 10 + length {
            break;
        }
        let mut payload = &rest[12..10 + length];
        let skip = if flags & 0x04 != 0 { 4 } else { 0 } + if flags & 0x08 != 0 { 6 } else { 0 };
        payload = payload.get(skip..).unwrap_or_default();
        while payload.len() >= 8 {
            let (id, size) = (u32_at(payload, 0), u32_at(payload, 4) as usize);
            if size < 8 || size > payload.len() {
                break;
            }
            items.push((id, &payload[8..size]));
            payload = &payload[size..];
        }
        rest = &rest[10 + length..];
    }
    items
}

/// Find IR camera on the system
pub fn find_ir_camera() -> Result<String> {
    let candidates = ["/dev/video2", "/dev/video3", "/dev/video4"];

    for path in candidates {
        if let Ok(device) = Device::with_path(path) {
            let caps = device.query_caps().ok();
            let name = caps.as_ref().map(|c| c.card.as_str()).unwrap_or("");

            if name.to_lowercase().contains("ir") || name.to_lowercase().contains("infrared") {
                info!(path, name, "found IR camera");
                return Ok(path.to_string());
            }
        }
    }

    bail!("no IR camera found")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 41 `uvc_meta_buf` blocks recorded from /dev/video3 while /dev/video2 streamed: one
    /// header-only block, then 40 frames whose illumination flag alternates.
    #[test]
    fn parses_recorded_uvcm_metadata() {
        let data = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/ir_meta_uvcm.bin")).unwrap();
        let flags: Vec<bool> = metadata_items(&data)
            .into_iter()
            .filter(|(id, _)| *id == METADATA_FRAME_ILLUMINATION)
            .map(|(_, p)| u32::from_le_bytes(p[..4].try_into().unwrap()) & 1 != 0)
            .collect();
        assert_eq!(flags.len(), 40);
        assert!(flags.iter().enumerate().all(|(i, &on)| on == (i % 2 == 1)));
        // The second block alone is one frame's buffer, with the emitter off.
        assert_eq!(frame_illumination(&data[22..]), Some(false));
        assert_eq!(frame_illumination(&data[..22]), None);
    }
}
