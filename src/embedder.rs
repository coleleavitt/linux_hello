//! Face embedding using ArcFace (InsightFace).

use anyhow::{Context, Result};
use image::RgbImage;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::TensorRef;
use std::path::Path;
use tracing::debug;

use crate::{get_ort_env, Embedding, Face};

const INPUT_SIZE: u32 = 112;

/// Where ArcFace expects the eyes, nose tip and mouth corners in its 112x112 input.
const ARCFACE_TEMPLATE: [[f32; 2]; 5] =
    [[38.2946, 51.6963], [73.5318, 51.5014], [56.0252, 71.7366], [41.5493, 92.3655], [70.7299, 92.2041]];

/// ArcFace face embedder
pub struct FaceEmbedder {
    session: Session,
}

impl FaceEmbedder {
    pub fn load(model_path: impl AsRef<Path>) -> Result<Self> {
        let model_path = model_path.as_ref();
        debug!(?model_path, "loading ArcFace model");

        let session = Session::builder(get_ort_env()?)
            .map_err(|e| anyhow::anyhow!("session builder: {}", e))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow::anyhow!("optimization level: {}", e))?
            .commit_from_file(model_path)
            .with_context(|| format!("failed to load {}", model_path.display()))?;

        Ok(Self { session })
    }

    /// Align the face with its landmarks, then embed it.
    pub fn embed_face(&mut self, image: &RgbImage, face: &Face) -> Result<Embedding> {
        self.embed(&align(image, &face.landmarks))
    }

    /// Embed an already aligned 112x112 face.
    pub fn embed(&mut self, aligned: &RgbImage) -> Result<Embedding> {
        let s = INPUT_SIZE as usize;
        let plane = s * s;
        let mut data = vec![0.0f32; 3 * plane];
        for (x, y, px) in aligned.enumerate_pixels() {
            let i = y as usize * s + x as usize;
            for c in 0..3 {
                data[c * plane + i] = (px.0[c] as f32 - 127.5) / 127.5;
            }
        }

        let input = TensorRef::from_array_view(([1i64, 3, s as i64, s as i64], data.as_slice()))?;
        let outputs = self.session.run(ort::inputs![input])?;
        let (_, raw) = outputs[0].try_extract_tensor::<f32>()?;

        let norm = raw.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
        let features: Vec<f32> = raw.iter().map(|x| x / norm).collect();
        debug!(dim = features.len(), "extracted embedding");
        Ok(Embedding { features, windows: None })
    }
}

/// Warp the face so its landmarks land on the ArcFace template
/// (least-squares similarity transform, bilinear sampling).
pub fn align(image: &RgbImage, landmarks: &[[f32; 2]; 5]) -> RgbImage {
    let mean = |pts: &[[f32; 2]; 5]| {
        let (sx, sy) = pts.iter().fold((0.0, 0.0), |(x, y), p| (x + p[0], y + p[1]));
        (sx / 5.0, sy / 5.0)
    };
    let (smx, smy) = mean(landmarks);
    let (dmx, dmy) = mean(&ARCFACE_TEMPLATE);

    let (mut num_a, mut num_b, mut den) = (0.0f32, 0.0f32, 0.0f32);
    for (s, d) in landmarks.iter().zip(ARCFACE_TEMPLATE.iter()) {
        let (sx, sy) = (s[0] - smx, s[1] - smy);
        let (dx, dy) = (d[0] - dmx, d[1] - dmy);
        num_a += sx * dx + sy * dy;
        num_b += sx * dy - sy * dx;
        den += sx * sx + sy * sy;
    }
    // Forward map: dst = [[a, -b], [b, a]] * (src - src_mean) + dst_mean
    let (a, b) = (num_a / den.max(1e-12), num_b / den.max(1e-12));
    let inv = 1.0 / (a * a + b * b).max(1e-12);

    RgbImage::from_fn(INPUT_SIZE, INPUT_SIZE, |x, y| {
        let (dx, dy) = (x as f32 - dmx, y as f32 - dmy);
        let sx = inv * (a * dx + b * dy) + smx;
        let sy = inv * (-b * dx + a * dy) + smy;
        sample_bilinear(image, sx, sy)
    })
}

fn sample_bilinear(image: &RgbImage, x: f32, y: f32) -> image::Rgb<u8> {
    let (w, h) = (image.width() as i64, image.height() as i64);
    let (x0, y0) = (x.floor() as i64, y.floor() as i64);
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let px = |xi: i64, yi: i64| -> [f32; 3] {
        if xi < 0 || yi < 0 || xi >= w || yi >= h {
            return [0.0; 3];
        }
        let p = image.get_pixel(xi as u32, yi as u32).0;
        [p[0] as f32, p[1] as f32, p[2] as f32]
    };
    let (p00, p10, p01, p11) = (px(x0, y0), px(x0 + 1, y0), px(x0, y0 + 1), px(x0 + 1, y0 + 1));
    let mut out = [0u8; 3];
    for c in 0..3 {
        let top = p00[c] * (1.0 - fx) + p10[c] * fx;
        let bottom = p01[c] * (1.0 - fx) + p11[c] * fx;
        out[c] = (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
    }
    image::Rgb(out)
}
