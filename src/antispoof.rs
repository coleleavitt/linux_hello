//! Face anti-spoofing (liveness) models.
//!
//! Both models were trained on visible-light RGB images and reject IR frames,
//! so the pipeline runs them on the RGB camera.

use anyhow::{Context, Result};
use image::RgbImage;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::TensorRef;
use std::path::Path;
use tracing::debug;

use crate::{get_ort_env, Face, LivenessResult};

#[derive(Debug, Clone, Copy)]
pub enum LivenessModel {
    /// facenox/face-antispoof-onnx: 128x128 RGB in [0, 1], square crop 1.5x the box with
    /// reflect padding, logits [real, spoof].
    FaceNox,
    /// minivision-ai Silent-Face 2.7_80x80 MiniFASNetV2: 80x80 BGR in raw [0, 255]
    /// (upstream removed the /255), crop 2.7x the box shifted inside the image,
    /// 3 classes where index 1 is real (upstream `test.py`: `label == 1` is "Real Face").
    MiniFasNetV2,
}

pub struct AntiSpoof {
    session: Session,
    model: LivenessModel,
    /// Minimum probability of "real" to accept a face
    threshold: f32,
}

impl AntiSpoof {
    pub fn load(model_path: impl AsRef<Path>, model: LivenessModel) -> Result<Self> {
        let model_path = model_path.as_ref();
        debug!(?model_path, ?model, "loading anti-spoofing model");

        let session = Session::builder(get_ort_env()?)
            .map_err(|e| anyhow::anyhow!("session builder: {}", e))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow::anyhow!("optimization level: {}", e))?
            .commit_from_file(model_path)
            .with_context(|| format!("failed to load {}", model_path.display()))?;

        Ok(Self { session, model, threshold: 0.5 })
    }

    pub fn check(&mut self, image: &RgbImage, face: &Face) -> Result<LivenessResult> {
        let (crop, size, scale, bgr) = match self.model {
            LivenessModel::FaceNox => (square_crop(image, face, 1.5), 128, 1.0 / 255.0, false),
            LivenessModel::MiniFasNetV2 => (shifted_crop(image, face, 2.7), 80, 1.0, true),
        };
        let resized = image::imageops::resize(&crop, size, size, image::imageops::FilterType::Triangle);

        let s = size as usize;
        let plane = s * s;
        let mut data = vec![0.0f32; 3 * plane];
        for (x, y, px) in resized.enumerate_pixels() {
            let i = y as usize * s + x as usize;
            for c in 0..3 {
                let src = if bgr { 2 - c } else { c };
                data[c * plane + i] = px.0[src] as f32 * scale;
            }
        }

        let input = TensorRef::from_array_view(([1i64, 3, s as i64, s as i64], data.as_slice()))?;
        let outputs = self.session.run(ort::inputs![input])?;
        let (_, out) = outputs[0].try_extract_tensor::<f32>()?;

        let score = match self.model {
            LivenessModel::FaceNox => 1.0 / (1.0 + (out[1] - out[0]).exp()),
            LivenessModel::MiniFasNetV2 => {
                let max = out.iter().copied().fold(f32::MIN, f32::max);
                let exp: Vec<f32> = out.iter().map(|v| (v - max).exp()).collect();
                exp[1] / exp.iter().sum::<f32>()
            }
        };
        let is_real = score >= self.threshold;
        debug!(model = ?self.model, score, is_real, "anti-spoofing result");
        Ok(LivenessResult { is_real, score })
    }

    pub fn set_threshold(&mut self, threshold: f32) {
        self.threshold = threshold;
    }
}

/// Square crop around the face box, `expand` times its larger side, reflect-padded at image edges.
fn square_crop(image: &RgbImage, face: &Face, expand: f32) -> RgbImage {
    let [x1, y1, x2, y2] = face.bbox;
    let side = (x2 - x1).max(y2 - y1) * expand;
    let x0 = ((x1 + x2) / 2.0 - side / 2.0) as i64;
    let y0 = ((y1 + y2) / 2.0 - side / 2.0) as i64;
    let size = (side as u32).max(1);

    let reflect = |i: i64, n: i64| -> u32 {
        let period = 2 * (n - 1).max(1);
        let m = i.rem_euclid(period);
        (if m < n { m } else { period - m }) as u32
    };
    let (w, h) = (image.width() as i64, image.height() as i64);
    RgbImage::from_fn(size, size, |x, y| *image.get_pixel(reflect(x0 + x as i64, w), reflect(y0 + y as i64, h)))
}

/// Silent-Face `CropImage._get_new_box`: scale the box by up to `scale` around its center
/// (capped so it fits the image), then shift it back inside the image instead of padding.
fn shifted_crop(image: &RgbImage, face: &Face, scale: f32) -> RgbImage {
    let (w, h) = (image.width() as f32, image.height() as f32);
    let [x1, y1, x2, y2] = face.bbox;
    let (bw, bh) = ((x2 - x1).max(1.0), (y2 - y1).max(1.0));
    let scale = scale.min((h - 1.0) / bh).min((w - 1.0) / bw);
    let (nw, nh) = (bw * scale, bh * scale);
    let (cx, cy) = (x1 + bw / 2.0, y1 + bh / 2.0);

    let left = (cx - nw / 2.0).max(0.0).min(w - 1.0 - nw).max(0.0);
    let top = (cy - nh / 2.0).max(0.0).min(h - 1.0 - nh).max(0.0);
    image::imageops::crop_imm(image, left as u32, top as u32, (nw as u32 + 1).max(1), (nh as u32 + 1).max(1)).to_image()
}
