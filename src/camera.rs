//! V4L2 camera capture for IR and RGB cameras.

use anyhow::{Context, Result, bail};
use image::{GrayImage, RgbImage};
use std::path::Path;
use tracing::{debug, info};
use v4l::buffer::Type;
use v4l::io::traits::CaptureStream;
use v4l::prelude::*;
use v4l::video::Capture;
use v4l::FourCC;

const WARMUP_FRAMES: usize = 16;

/// Camera type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraType {
    Rgb,
    Ir,
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
        
        let device = Device::with_path(path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        
        let caps = device.query_caps()?;
        debug!(?caps, "camera capabilities");
        
        // Set format
        let format = match camera_type {
            CameraType::Ir => {
                device.set_format(&v4l::Format::new(640, 480, FourCC::new(b"GREY")))
                    .or_else(|_| device.set_format(&v4l::Format::new(640, 480, FourCC::new(b"YUYV"))))?
            }
            CameraType::Rgb => {
                device.set_format(&v4l::Format::new(640, 480, FourCC::new(b"YUYV")))?
            }
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
    
    /// Capture one frame ready for the face pipeline.
    ///
    /// The IR emitter is on for every other frame, so read two IR frames and keep the brighter.
    pub fn capture(&mut self) -> Result<RgbImage> {
        match self.camera_type {
            CameraType::Ir => {
                let a = self.capture_gray()?;
                let b = self.capture_gray()?;
                let sum = |g: &GrayImage| g.as_raw().iter().map(|&p| p as u64).sum::<u64>();
                Ok(crate::gray_to_rgb(if sum(&a) >= sum(&b) { &a } else { &b }))
            }
            CameraType::Rgb => self.capture_rgb(),
        }
    }

    /// Capture a single frame as grayscale
    pub fn capture_gray(&mut self) -> Result<GrayImage> {
        let (buf, _meta) = self.stream.next()?;
        
        let gray = match &self.fourcc {
            [b'G', b'R', b'E', b'Y'] => {
                GrayImage::from_raw(self.width, self.height, buf.to_vec())
                    .context("invalid GREY frame")?
            }
            [b'Y', b'U', b'Y', b'V'] => {
                let y: Vec<u8> = buf.iter().step_by(2).copied().collect();
                GrayImage::from_raw(self.width, self.height, y)
                    .context("invalid YUYV frame")?
            }
            other => bail!("unsupported format: {:?}", other),
        };
        
        Ok(gray)
    }
    
    /// Capture a single frame as RGB
    pub fn capture_rgb(&mut self) -> Result<RgbImage> {
        let (buf, _meta) = self.stream.next()?;
        
        let mut rgb = Vec::with_capacity((self.width * self.height * 3) as usize);
        for chunk in buf.chunks(4) {
            if chunk.len() < 4 { break; }
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
    
    pub fn width(&self) -> u32 { self.width }
    pub fn height(&self) -> u32 { self.height }
    pub fn camera_type(&self) -> CameraType { self.camera_type }
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
