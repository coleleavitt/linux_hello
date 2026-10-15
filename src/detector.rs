//! Face detection using SCRFD (InsightFace).

use anyhow::{Context, Result, ensure};
use image::{GrayImage, RgbImage};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::TensorRef;
use std::path::Path;
use tracing::debug;

use crate::{get_ort_env, gray_to_rgb, Face};

/// Feature pyramid strides. SCRFD emits outputs in the order
/// [scores x3, bbox deltas x3, keypoint deltas x3], one per stride.
const STRIDES: [usize; 3] = [8, 16, 32];
const ANCHORS_PER_CELL: usize = 2;
const INPUT_SIZE: u32 = 640;

/// SCRFD face detector
pub struct FaceDetector {
    session: Session,
    conf_threshold: f32,
    nms_threshold: f32,
}

impl FaceDetector {
    pub fn load(model_path: impl AsRef<Path>) -> Result<Self> {
        let model_path = model_path.as_ref();
        debug!(?model_path, "loading SCRFD model");

        let session = Session::builder(get_ort_env()?)
            .map_err(|e| anyhow::anyhow!("session builder: {}", e))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow::anyhow!("optimization level: {}", e))?
            .commit_from_file(model_path)
            .with_context(|| format!("failed to load {}", model_path.display()))?;

        Ok(Self { session, conf_threshold: 0.5, nms_threshold: 0.4 })
    }

    pub fn detect_gray(&mut self, image: &GrayImage) -> Result<Vec<Face>> {
        self.detect(&gray_to_rgb(image))
    }

    /// Detect faces. Coordinates are in the input image's pixel space.
    pub fn detect(&mut self, image: &RgbImage) -> Result<Vec<Face>> {
        // Letterbox: scale to fit 640x640 keeping aspect ratio, pad right/bottom with black.
        let scale = INPUT_SIZE as f32 / image.width().max(image.height()) as f32;
        let nw = ((image.width() as f32 * scale) as u32).max(1);
        let nh = ((image.height() as f32 * scale) as u32).max(1);
        let resized = image::imageops::resize(image, nw, nh, image::imageops::FilterType::Triangle);

        let s = INPUT_SIZE as usize;
        let plane = s * s;
        // Padding value 0 normalizes to (0 - 127.5) / 128
        let mut data = vec![-127.5f32 / 128.0; 3 * plane];
        for (x, y, px) in resized.enumerate_pixels() {
            let i = y as usize * s + x as usize;
            for c in 0..3 {
                data[c * plane + i] = (px.0[c] as f32 - 127.5) / 128.0;
            }
        }

        let input = TensorRef::from_array_view(([1i64, 3, s as i64, s as i64], data.as_slice()))?;
        let outputs = self.session.run(ort::inputs![input])?;

        let tensors: Vec<&[f32]> = (0..outputs.len())
            .map(|i| outputs[i].try_extract_tensor::<f32>().map(|(_, d)| d))
            .collect::<Result<_, _>>()?;
        ensure!(tensors.len() == 9, "expected 9 SCRFD outputs, got {}", tensors.len());

        let mut faces = Vec::new();
        for (level, &stride) in STRIDES.iter().enumerate() {
            let (scores, bboxes, kps) = (tensors[level], tensors[level + 3], tensors[level + 6]);
            let fw = s / stride;
            let st = stride as f32;

            for (idx, &score) in scores.iter().enumerate() {
                if score < self.conf_threshold {
                    continue;
                }
                let cell = idx / ANCHORS_PER_CELL;
                let cx = (cell % fw) as f32 * st;
                let cy = (cell / fw) as f32 * st;

                let b = &bboxes[idx * 4..idx * 4 + 4];
                let bbox = [
                    (cx - b[0] * st) / scale,
                    (cy - b[1] * st) / scale,
                    (cx + b[2] * st) / scale,
                    (cy + b[3] * st) / scale,
                ];

                let k = &kps[idx * 10..idx * 10 + 10];
                let mut landmarks = [[0.0f32; 2]; 5];
                for (p, lm) in landmarks.iter_mut().enumerate() {
                    lm[0] = (cx + k[p * 2] * st) / scale;
                    lm[1] = (cy + k[p * 2 + 1] * st) / scale;
                }

                faces.push(Face { bbox, score, landmarks });
            }
        }
        drop(outputs);

        let faces = nms(faces, self.nms_threshold);
        debug!(count = faces.len(), "detected faces");
        Ok(faces)
    }

    pub fn set_threshold(&mut self, conf: f32, nms: f32) {
        self.conf_threshold = conf;
        self.nms_threshold = nms;
    }
}

fn nms(mut faces: Vec<Face>, threshold: f32) -> Vec<Face> {
    faces.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut keep: Vec<Face> = Vec::new();
    for f in faces {
        if keep.iter().all(|k| iou(k, &f) <= threshold) {
            keep.push(f);
        }
    }
    keep
}

fn iou(a: &Face, b: &Face) -> f32 {
    let w = (a.bbox[2].min(b.bbox[2]) - a.bbox[0].max(b.bbox[0])).max(0.0);
    let h = (a.bbox[3].min(b.bbox[3]) - a.bbox[1].max(b.bbox[1])).max(0.0);
    let inter = w * h;
    let area = |f: &Face| (f.bbox[2] - f.bbox[0]) * (f.bbox[3] - f.bbox[1]);
    inter / (area(a) + area(b) - inter + 1e-6)
}
