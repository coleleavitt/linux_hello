//! V4L2 capture: the RGB camera for liveness, and the IR camera as lit/ambient frame pairs.
//!
//! The IR emitter lights every other frame by itself (L1). Which frames are lit comes from the
//! uvcvideo metadata node of the IR camera (`MetadataId_FrameIllumination`, L2/L3); without that
//! node the brighter frame of two consecutive frames is taken as the lit one (F15).

use anyhow::{bail, Context, Result};
use image::{GrayImage, RgbImage};
use std::path::Path;
use std::time::Duration;
use tracing::{debug, info};
use v4l::buffer::Type;
use v4l::io::traits::CaptureStream;
use v4l::prelude::*;
use v4l::video::Capture;
use v4l::FourCC;

const WARMUP_FRAMES: usize = 16;
/// A lit frame is only paired with an ambient frame closer in time than this (F13).
const MAX_PAIR_GAP: Duration = Duration::from_millis(300);
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
    /// Open a camera by device path
    pub fn open(path: impl AsRef<Path>, camera_type: CameraType) -> Result<Self> {
        Self::open_with(path, camera_type, WARMUP_FRAMES, 4)
    }

    /// Open with explicit warm-up frame count and buffer count. Fewer buffers keep the
    /// returned frames closer to "now" when the stream is read slower than it runs.
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
        // Let auto-exposure settle; the first frames are dark.
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
    /// Time between the two frames
    pub gap: Duration,
}

/// The IR camera, read as lit/ambient pairs.
pub struct IrCamera {
    video: Camera,
    meta: Option<MetaStream>,
}

impl IrCamera {
    /// Open the IR camera and, if given, its metadata node (e.g. `/dev/video3`).
    pub fn open(device: &str, metadata: Option<&str>) -> Result<Self> {
        // The metadata format can only be set while the metadata queue is idle, so set it before
        // the video stream starts. Its stream starts on the first read, after the warm-up.
        let meta = metadata.map(MetaStream::open).transpose()?;
        let video = Camera::open(device, CameraType::Ir)?;
        Ok(Self { video, meta })
    }

    /// Two consecutive frames, one lit and one ambient, less than [`MAX_PAIR_GAP`] apart.
    pub fn capture_pair(&mut self) -> Result<IrPair> {
        let mut prev = self.next_frame()?;
        for _ in 0..MAX_PAIR_FRAMES {
            let next = self.next_frame()?;
            let gap = next.0.time.saturating_sub(prev.0.time);
            if gap < MAX_PAIR_GAP {
                if let Some(prev_lit) = self.is_first_lit(&prev, &next) {
                    let (lit, ambient) = if prev_lit { (prev.0, next.0) } else { (next.0, prev.0) };
                    return Ok(IrPair { lit: lit.image, ambient: ambient.image, gap });
                }
            }
            debug!(?gap, prev = ?prev.1, next = ?next.1, "frames do not form a lit/ambient pair");
            prev = next;
        }
        bail!("no lit/ambient IR frame pair in {MAX_PAIR_FRAMES} frames (does the emitter alternate?)")
    }

    /// Whether the first of two frames is the lit one, or `None` if they are not a lit/ambient pair.
    fn is_first_lit(&self, a: &(GrayFrame, Option<bool>), b: &(GrayFrame, Option<bool>)) -> Option<bool> {
        if self.meta.is_none() {
            return Some(brightness_sum(&a.0.image) >= brightness_sum(&b.0.image));
        }
        match (a.1, b.1) {
            (Some(x), Some(y)) if x != y => Some(x),
            _ => None,
        }
    }

    /// The next frame and its illumination flag (`None` without metadata, or when the frame's
    /// metadata was dropped).
    fn next_frame(&mut self) -> Result<(GrayFrame, Option<bool>)> {
        let frame = self.video.capture_gray()?;
        let lit = match self.meta.as_mut() {
            Some(meta) => meta.illumination(frame.sequence)?,
            None => None,
        };
        Ok((frame, lit))
    }
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
