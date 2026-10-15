//! Face occlusion detection using converted Windows Hello models.
//!
//! Detects masks, sunglasses, and hands covering the face.
//! Model: MODEL_FILES_120.onnx
//! - Input: 1x1x128x128 (aligned face crop, x/127.5 - 1 normalized)
//! - Output: 1x4 softmax [none, mask, sunglasses, hand]

use anyhow::{Context, Result};
use image::{GrayImage, RgbImage};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::TensorRef;
use std::path::Path;
use tracing::debug;

use crate::{get_ort_env, Face};

const INPUT_SIZE: usize = 128;
const ALIGN_SIZE: usize = 281;

/// Detected occlusion type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcclusionType {
    None,
    Mask,
    Sunglasses,
    Hand,
}

impl OcclusionType {
    fn from_index(idx: usize) -> Self {
        match idx {
            0 => Self::None,
            1 => Self::Mask,
            2 => Self::Sunglasses,
            3 => Self::Hand,
            _ => Self::None,
        }
    }

    /// Failure reason string for the daemon reply.
    pub fn failure_reason(&self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Mask => Some("occluded_mask"),
            Self::Sunglasses => Some("occluded_sunglasses"),
            Self::Hand => Some("occluded_hand"),
        }
    }
}

impl std::fmt::Display for OcclusionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "none"),
            Self::Mask => write!(f, "mask"),
            Self::Sunglasses => write!(f, "sunglasses"),
            Self::Hand => write!(f, "hand"),
        }
    }
}

/// Result of occlusion detection.
#[derive(Debug, Clone)]
pub struct OcclusionResult {
    /// Probability of no occlusion (p_none).
    pub clear_prob: f32,
    /// Detected occlusion type (argmax of the 4 classes).
    pub occlusion_type: OcclusionType,
    /// Whether the face is considered occluded at the given threshold.
    pub is_occluded: bool,
    /// Per-class probabilities [none, mask, sunglasses, hand].
    pub probs: [f32; 4],
}

/// Face occlusion detector using Windows Hello models.
pub struct OcclusionDetector {
    session: Session,
    /// Threshold for 1 - p_none to consider occluded (0.9 for auth, 0.5 for enrollment).
    threshold: f32,
}

impl OcclusionDetector {
    /// Load the occlusion model from the given path.
    /// Uses auth threshold (0.9) by default; call `set_enrollment_mode(true)` for enrollment (0.5).
    pub fn load(model_path: impl AsRef<Path>) -> Result<Self> {
        let model_path = model_path.as_ref();
        debug!(?model_path, "loading occlusion model");

        let session = if model_path.ends_with(".onnx") && model_path.join("../weights").exists() {
            let npz_path = model_path.parent().unwrap().join("../weights").join(
                model_path.file_name().unwrap().to_string_lossy().replace(".onnx", ".npz"),
            );
            if npz_path.exists() {
                debug!(?npz_path, "loading occlusion from npz");
                Session::builder(get_ort_env()?)
                    .map_err(|e| anyhow::anyhow!("session builder: {}", e))?
                    .with_optimization_level(GraphOptimizationLevel::Level3)
                    .map_err(|e| anyhow::anyhow!("optimization level: {}", e))?
                    .commit_from_file(&std::path::PathBuf::from("/tmp/.ms_model_tmp.onnx"))
                    .with_context(|| format!("failed to load {}", npz_path.display()))?
            } else {
                Session::builder(get_ort_env()?)
                    .map_err(|e| anyhow::anyhow!("session builder: {}", e))?
                    .with_optimization_level(GraphOptimizationLevel::Level3)
                    .map_err(|e| anyhow::anyhow!("optimization level: {}", e))?
                    .commit_from_file(model_path)
                    .with_context(|| format!("failed to load {}", model_path.display()))?
            }
        } else {
            Session::builder(get_ort_env()?)
                .map_err(|e| anyhow::anyhow!("session builder: {}", e))?
                .with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(|e| anyhow::anyhow!("optimization level: {}", e))?
                .commit_from_file(model_path)
                .with_context(|| format!("failed to load {}", model_path.display()))?
        };

        Ok(Self { session, threshold: 0.9 })
    }

    /// Set enrollment mode (threshold 0.5) or auth mode (threshold 0.9).
    pub fn set_enrollment_mode(&mut self, enrollment: bool) {
        self.threshold = if enrollment { 0.5 } else { 0.9 };
    }

    /// Check for occlusion in the given image with detected face.
    /// The image should be grayscale IR or RGB (will be converted to grayscale).
    pub fn check(&mut self, image: &RgbImage, face: &Face) -> Result<OcclusionResult> {
        let gray = image::imageops::grayscale(image);
        self.check_gray(&gray, face)
    }

    /// Check for occlusion using a grayscale image.
    pub fn check_gray(&mut self, image: &GrayImage, face: &Face) -> Result<OcclusionResult> {
        // Align and crop to 128x128 using Windows Hello preprocessing
        let aligned = align_crop_winhello(image, &face.landmarks);

        // Normalize: x / 127.5 - 1
        let mut data = vec![0.0f32; INPUT_SIZE * INPUT_SIZE];
        for (i, &px) in aligned.iter().enumerate() {
            data[i] = px as f32 / 127.5 - 1.0;
        }

        // Run inference: input shape is 1x1x128x128
        let input = TensorRef::from_array_view((
            [1i64, 1, INPUT_SIZE as i64, INPUT_SIZE as i64],
            data.as_slice(),
        ))?;
        let outputs = self.session.run(ort::inputs![input])?;
        let (_, raw) = outputs[0].try_extract_tensor::<f32>()?;

        // Softmax the output
        let probs = softmax_4(raw);
        let clear_prob = probs[0];
        let occlusion_score = 1.0 - clear_prob;

        // Find argmax
        let (max_idx, _) = probs
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.total_cmp(b))
            .unwrap();
        let occlusion_type = OcclusionType::from_index(max_idx);

        let is_occluded = occlusion_score > self.threshold;
        debug!(
            clear_prob,
            occlusion_score,
            ?occlusion_type,
            is_occluded,
            threshold = self.threshold,
            "occlusion check"
        );

        Ok(OcclusionResult {
            clear_prob,
            occlusion_type,
            is_occluded,
            probs,
        })
    }
}

/// Softmax over 4 elements.
fn softmax_4(logits: &[f32]) -> [f32; 4] {
    let max = logits.iter().copied().fold(f32::MIN, f32::max);
    let exp: Vec<f32> = logits.iter().map(|v| (v - max).exp()).collect();
    let sum: f32 = exp.iter().sum();
    [
        exp[0] / sum,
        exp[1] / sum,
        exp[2] / sum,
        exp[3] / sum,
    ]
}

// ============================================================================
// Windows Hello alignment (ported from tools/winhello_models/convnet/msprep.py)
// ============================================================================

/// 5 landmark template in 281x281 space (from FaceRecognitionEngineAdapter.dll).
#[allow(clippy::excessive_precision)]  // Exact values from Windows DLL memory
const TEMPLATE_281: [[f32; 2]; 5] = [
    [97.82990264892578, 127.7791976928711],
    [181.05320739746094, 126.33809661865234],
    [139.57400512695312, 179.00059509277344],
    [102.69640350341797, 216.70140075683594],
    [179.30679321289062, 215.37330627441406],
];

/// Align and crop face using Windows Hello preprocessing.
/// Matches sub_180015F68 -> sub_18001614C -> sub_180016B20 -> sub_180016440.
fn align_crop_winhello(image: &GrayImage, landmarks: &[[f32; 2]; 5]) -> Vec<u8> {
    // Step 1: Fit similarity transform from landmarks to template
    let m = fit_similarity(landmarks);

    // Step 2: Warp to 281x281
    let warped = warp_281(image, &m);

    // Step 3: Resize central 230x230 to 128x128
    resize_281_to_128(&warped)
}

/// Least-squares similarity transform: dst = [[a, b], [-b, a]] * (src - src_mean) + dst_mean.
/// Returns the 2x3 matrix [[a, b, tx], [-b, a, ty]].
fn fit_similarity(src: &[[f32; 2]; 5]) -> [[f32; 3]; 2] {
    let dst = &TEMPLATE_281;

    // Compute means
    let (mut smx, mut smy) = (0.0f32, 0.0f32);
    let (mut dmx, mut dmy) = (0.0f32, 0.0f32);
    for i in 0..5 {
        smx += src[i][0];
        smy += src[i][1];
        dmx += dst[i][0];
        dmy += dst[i][1];
    }
    smx /= 5.0;
    smy /= 5.0;
    dmx /= 5.0;
    dmy /= 5.0;

    // Compute sums for the transform (matching Python msprep.py)
    let (mut sxx, mut sp, mut sm) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..5 {
        let sx = src[i][0] - smx;
        let sy = src[i][1] - smy;
        let dx = dst[i][0] - dmx;
        let dy = dst[i][1] - dmy;
        sxx += sx * sx + sy * sy;
        sp += sx * dx + sy * dy;
        sm += sx * dy - sy * dx;
    }

    let var = sxx.max(1e-6);
    let a = sp / var;
    let b = sm / var;
    let tx = dmx - a * smx - b * smy;
    let ty = dmy + b * smx - a * smy;

    [[a, b, tx], [-b, a, ty]]
}

/// Warp image to 281x281 using the inverse of the similarity transform.
fn warp_281(image: &GrayImage, m: &[[f32; 3]; 2]) -> Vec<u8> {
    // Compute inverse of the 2x3 affine matrix
    // M = [[a, b, tx], [-b, a, ty]]
    // det = a^2 + b^2
    let a = m[0][0];
    let b = m[0][1];
    let tx = m[0][2];
    let ty = m[1][2];
    let det = a * a + b * b;
    let inv_det = 1.0 / det.max(1e-12);

    // Inverse: [[a, -b, -(a*tx - b*ty)], [b, a, -(b*tx + a*ty)]] / det
    let ia = a * inv_det;
    let ib = -b * inv_det;
    let itx = -(a * tx - b * ty) * inv_det;
    let ity = -(b * tx + a * ty) * inv_det;

    let (w, h) = (image.width() as i64, image.height() as i64);
    let mut out = vec![0u8; ALIGN_SIZE * ALIGN_SIZE];

    for dy in 0..ALIGN_SIZE {
        for dx in 0..ALIGN_SIZE {
            let dxf = dx as f32;
            let dyf = dy as f32;
            let sx = ia * dxf + ib * dyf + itx;
            let sy = -ib * dxf + ia * dyf + ity;

            // Bilinear sample with truncation (Windows Hello behavior)
            out[dy * ALIGN_SIZE + dx] = bilinear_trunc_gray(image, sx, sy, w, h);
        }
    }

    out
}

/// Bilinear sample with truncation (matches Windows DLL behavior).
/// Outside [0, w-1] x [0, h-1] returns 0.
fn bilinear_trunc_gray(image: &GrayImage, sx: f32, sy: f32, w: i64, h: i64) -> u8 {
    if sx < 0.0 || sy < 0.0 || sx > (w - 1) as f32 || sy > (h - 1) as f32 {
        return 0;
    }

    let x0 = sx.floor() as i64;
    let y0 = sy.floor() as i64;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);

    let fx = sx - x0 as f32;
    let fy = sy - y0 as f32;
    let gx = 1.0 - fx;
    let gy = 1.0 - fy;

    let p00 = image.get_pixel(x0 as u32, y0 as u32).0[0] as f32;
    let p10 = image.get_pixel(x1 as u32, y0 as u32).0[0] as f32;
    let p01 = image.get_pixel(x0 as u32, y1 as u32).0[0] as f32;
    let p11 = image.get_pixel(x1 as u32, y1 as u32).0[0] as f32;

    let val = p00 * gy * gx + p10 * gy * fx + p01 * fy * gx + p11 * fy * fx;
    // Truncate (floor), not round, matching the DLL
    val.floor().clamp(0.0, 255.0) as u8
}

/// Resize the central 230x230 of a 281x281 image to 128x128.
/// Matches sub_180016440: src = (i - out/2) * 230/(out-1) + 140.5.
fn resize_281_to_128(warped: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; INPUT_SIZE * INPUT_SIZE];
    let half = (INPUT_SIZE / 2) as f32;
    let scale = 230.0 / (INPUT_SIZE as f32 - 1.0);
    let center = 140.5;

    for j in 0..INPUT_SIZE {
        for i in 0..INPUT_SIZE {
            let sx = (i as f32 - half) * scale + center;
            let sy = (j as f32 - half) * scale + center;
            out[j * INPUT_SIZE + i] = bilinear_trunc_281(warped, sx, sy);
        }
    }

    out
}

/// Bilinear sample from 281x281 buffer with truncation.
fn bilinear_trunc_281(data: &[u8], sx: f32, sy: f32) -> u8 {
    let size = ALIGN_SIZE as i64;
    if sx < 0.0 || sy < 0.0 || sx > (size - 1) as f32 || sy > (size - 1) as f32 {
        return 0;
    }

    let x0 = sx.floor() as usize;
    let y0 = sy.floor() as usize;
    let x1 = (x0 + 1).min(ALIGN_SIZE - 1);
    let y1 = (y0 + 1).min(ALIGN_SIZE - 1);

    let fx = sx - x0 as f32;
    let fy = sy - y0 as f32;
    let gx = 1.0 - fx;
    let gy = 1.0 - fy;

    let p00 = data[y0 * ALIGN_SIZE + x0] as f32;
    let p10 = data[y0 * ALIGN_SIZE + x1] as f32;
    let p01 = data[y1 * ALIGN_SIZE + x0] as f32;
    let p11 = data[y1 * ALIGN_SIZE + x1] as f32;

    let val = p00 * gy * gx + p10 * gy * fx + p01 * fy * gx + p11 * fy * fx;
    val.floor().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_softmax() {
        let logits = [1.0, 2.0, 3.0, 4.0];
        let probs = softmax_4(&logits);
        let sum: f32 = probs.iter().sum();
        assert!((sum - 1.0).abs() < 1e-6);
        // Largest logit should have largest probability
        assert!(probs[3] > probs[2]);
        assert!(probs[2] > probs[1]);
        assert!(probs[1] > probs[0]);
    }

    #[test]
    fn test_occlusion_type_display() {
        assert_eq!(format!("{}", OcclusionType::None), "none");
        assert_eq!(format!("{}", OcclusionType::Mask), "mask");
        assert_eq!(format!("{}", OcclusionType::Sunglasses), "sunglasses");
        assert_eq!(format!("{}", OcclusionType::Hand), "hand");
    }

    #[test]
    fn test_failure_reason() {
        assert_eq!(OcclusionType::None.failure_reason(), None);
        assert_eq!(OcclusionType::Mask.failure_reason(), Some("occluded_mask"));
        assert_eq!(OcclusionType::Sunglasses.failure_reason(), Some("occluded_sunglasses"));
        assert_eq!(OcclusionType::Hand.failure_reason(), Some("occluded_hand"));
    }
}
