//! Face occlusion detection using converted Windows Hello models.
//!
//! Detects masks, sunglasses, and hands covering the face.
//! Model: MODEL_FILES_120.onnx
//! - Input: 1x1x128x128 (aligned face crop, x/127.5 - 1 normalized)
//! - Output: 1x4 softmax [none, mask, sunglasses, hand]

use anyhow::Result;
use image::GrayImage;
use ort::session::Session;
use ort::value::TensorRef;
use std::path::Path;
use tracing::debug;

use crate::Face;

const INPUT_SIZE: usize = 128;

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

/// 1 − p(none) above which a face is occluded in an unlock (F46); 0.5 while enrolling.
pub const OCCLUSION_THRESHOLD: f32 = 0.9;
/// The enrollment threshold.
pub const ENROLLMENT_OCCLUSION_THRESHOLD: f32 = 0.5;

impl OcclusionDetector {
    /// Load the occlusion model from the given path.
    /// Uses auth threshold (0.9) by default; call `set_enrollment_mode(true)` for enrollment (0.5).
    pub fn load(model_path: impl AsRef<Path>) -> Result<Self> {
        let model_path = model_path.as_ref();
        debug!(?model_path, "loading occlusion model");

        let session = super::load_session(model_path)?;

        Ok(Self { session, threshold: OCCLUSION_THRESHOLD })
    }

    /// Set enrollment mode (threshold 0.5) or auth mode (threshold 0.9).
    pub fn set_enrollment_mode(&mut self, enrollment: bool) {
        self.threshold = if enrollment { ENROLLMENT_OCCLUSION_THRESHOLD } else { OCCLUSION_THRESHOLD };
    }

    /// Check for occlusion using a grayscale image.
    pub fn check(&mut self, lit: &GrayImage, face: &Face) -> Result<OcclusionResult> {
        let data = box_input(lit, face.bbox);

        // Run inference: input shape is 1x1x128x128
        let input = TensorRef::from_array_view(([1i64, 1, INPUT_SIZE as i64, INPUT_SIZE as i64], data.as_slice()))?;
        let outputs = self.session.run(ort::inputs![input])?;
        let (_, raw) = outputs[0].try_extract_tensor::<f32>()?;

        // Softmax the output
        let probs = softmax_4(raw);
        let clear_prob = probs[0];
        let occlusion_score = 1.0 - clear_prob;

        // Find argmax
        let (max_idx, _) = probs.iter().enumerate().max_by(|(_, a), (_, b)| a.total_cmp(b)).unwrap();
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

        Ok(OcclusionResult { clear_prob, occlusion_type, is_occluded, probs })
    }
}

/// Softmax over 4 elements.
fn softmax_4(logits: &[f32]) -> [f32; 4] {
    let max = logits.iter().copied().fold(f32::MIN, f32::max);
    let exp: Vec<f32> = logits.iter().map(|v| (v - max).exp()).collect();
    let sum: f32 = exp.iter().sum();
    [exp[0] / sum, exp[1] / sum, exp[2] / sum, exp[3] / sum]
}

// ============================================================================
// Windows Hello alignment (ported from tools/winhello_models/convnet/msprep.py)
// ============================================================================

/// The face box of the lit luminance image resampled to 128×128, as `x/127.5 − 1`
/// (`sub_18004B5B8` / `sub_18004B6FC`; reference `tools/winhello_models/cefa/occlusion.py`).
fn box_input(image: &GrayImage, bbox: [f32; 4]) -> Vec<f32> {
    let [x0, y0, x1, y1] = bbox.map(|v| v.round());
    let (w, h) = (x1 - x0, y1 - y0);
    let (max_x, max_y) = (image.width() as f32 - 1.0, image.height() as f32 - 1.0);
    let at = |x: f32, y: f32| image.get_pixel(x as u32, y as u32)[0] as f32;
    let mut out = Vec::with_capacity(INPUT_SIZE * INPUT_SIZE);
    for ky in 0..INPUT_SIZE {
        let y = (y0 + (ky as f32 - 63.5) * (h / 128.0) + (h - 1.0) * 0.5).clamp(0.0, max_y);
        let (ya, fy) = (y.floor(), y - y.floor());
        let yb = (ya + 1.0).min(max_y);
        for kx in 0..INPUT_SIZE {
            let x = (x0 + (kx as f32 - 63.5) * (w / 128.0) + (w - 1.0) * 0.5).clamp(0.0, max_x);
            let (xa, fx) = (x.floor(), x - x.floor());
            let xb = (xa + 1.0).min(max_x);
            let v = at(xa, ya) * (1.0 - fx) * (1.0 - fy)
                + at(xb, ya) * fx * (1.0 - fy)
                + at(xa, yb) * (1.0 - fx) * fy
                + at(xb, yb) * fx * fy;
            out.push((v as u8) as f32 / 127.5 - 1.0);
        }
    }
    out
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
