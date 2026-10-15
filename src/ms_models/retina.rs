//! Microsoft IR face detector (RetinaEngine ORT models).
//!
//! These models are converted from Windows Hello binaries and are for personal use only.
//! They should never be committed to the repository. The ORT format files load directly
//! into onnxruntime.
//!
//! See `docs/winhello-models/ldmf.md` for the full reverse-engineering documentation.
//! Reference implementation: `tools/winhello_models/ldmf/retina.py`.

use anyhow::{Context, Result};
use image::GrayImage;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::TensorRef;
use std::path::Path;
use tracing::debug;

use crate::{get_ort_env, Face};

/// Model configuration by type.
/// Type 0 (MODEL_FILES_110/112): main IR detector, strides 16/32, smaller anchors.
/// Type 1 (MODEL_FILES_111/113): large-face variant, strides 32/64, doubled anchors.
#[derive(Debug, Clone, Copy)]
pub enum RetinaModelType {
    /// Type 0: strides 16/32, anchors (32,64)/(128,256), includes 6-DoF pose output.
    Standard,
    /// Type 1: strides 32/64, anchors (64,128)/(256,512), no pose output.
    LargeFace,
}

impl RetinaModelType {
    fn strides(&self) -> [usize; 2] {
        match self {
            RetinaModelType::Standard => [16, 32],
            RetinaModelType::LargeFace => [32, 64],
        }
    }

    fn anchors(&self) -> [[f32; 2]; 2] {
        match self {
            RetinaModelType::Standard => [[32.0, 64.0], [128.0, 256.0]],
            RetinaModelType::LargeFace => [[64.0, 128.0], [256.0, 512.0]],
        }
    }

    fn has_pose(&self) -> bool {
        matches!(self, RetinaModelType::Standard)
    }
}

/// Detected face with optional head pose.
#[derive(Debug, Clone)]
pub struct MsRetinaFace {
    /// Standard Face struct for compatibility.
    pub face: Face,
    /// 6-DoF pose (rotation vector + translation), only for Standard model.
    /// De-normalized from the model output.
    pub dofs: Option<[f32; 6]>,
    /// Decoded rotation angles (pitch, yaw, roll) in degrees.
    /// Extracted from dofs using the img2pose-style decode.
    pub rotation: Option<[f32; 3]>,
}

// Configuration constants from RetinaEngine::Initialize
const CONF_THRESHOLD: f32 = 0.925;
const VARIANCE_LOC: f32 = 0.1;
const VARIANCE_SIZE: f32 = 0.2;

// 6-DoF pose normalization constants (img2pose-style)
const POSE_MEAN: [f32; 6] = [-0.09145801, 0.01599428, -0.02233053, 0.02951323, 0.23168705, 3.5088696];
const POSE_STD: [f32; 6] = [0.29370177, 0.5890974, 0.26571402, 0.62606615, 0.18116798, 0.40033603];

/// Microsoft IR RetinaFace detector.
///
/// Loads the decrypted `.ort` model files directly. Input is raw grayscale
/// pixels as floats (0-255, no normalization). Outputs include bounding boxes,
/// landmarks, and optionally 6-DoF head pose.
pub struct MsRetinaDetector {
    session: Session,
    model_type: RetinaModelType,
    conf_threshold: f32,
    max_faces: usize,
}

impl MsRetinaDetector {
    /// Load a decrypted MS Retina ORT model.
    ///
    /// `model_type` should match the model file:
    /// - `Standard`: MODEL_FILES_110.ort or MODEL_FILES_112.ort (identical)
    /// - `LargeFace`: MODEL_FILES_111.ort or MODEL_FILES_113.ort (identical)
    pub fn load(model_path: impl AsRef<Path>, model_type: RetinaModelType) -> Result<Self> {
        let model_path = model_path.as_ref();
        debug!(?model_path, ?model_type, "loading MS Retina model");

        let session = if model_path.ends_with(".onnx") && model_path.join("../weights").exists() {
            let npz_path = model_path.parent().unwrap().join("../weights").join(
                model_path.file_name().unwrap().to_string_lossy().replace(".onnx", ".npz"),
            );
            if npz_path.exists() {
                debug!(?npz_path, "loading retina from npz");
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

        Ok(Self {
            session,
            model_type,
            conf_threshold: CONF_THRESHOLD,
            max_faces: 10,
        })
    }

    /// Set detection threshold (default: 0.925).
    pub fn set_threshold(&mut self, threshold: f32) {
        self.conf_threshold = threshold;
    }

    /// Set maximum faces to return (default: 10).
    pub fn set_max_faces(&mut self, max: usize) {
        self.max_faces = max;
    }

    /// Detect faces in a grayscale IR image.
    ///
    /// Returns faces with landmarks and optional head pose (Standard model only).
    pub fn detect(&mut self, image: &GrayImage) -> Result<Vec<MsRetinaFace>> {
        let (h, w) = (image.height() as usize, image.width() as usize);

        // Input: 1x1xHxW, raw 0-255 floats (no normalization!)
        let data: Vec<f32> = image.as_raw().iter().map(|&p| p as f32).collect();

        let input = TensorRef::from_array_view(([1i64, 1, h as i64, w as i64], data.as_slice()))?;
        let outputs = self.session.run(ort::inputs![input])?;

        // Outputs: bbox [1,N,4], class [1,N,2], landmarks [1,N,10], confidence [1,N,2], dofs [1,N,6]
        // Order from retina.py: bbox=0, class=1, landmarks=2, confidence=3, dofs=4 (if present)
        let (_, bbox) = outputs[0].try_extract_tensor::<f32>()?;
        // outputs[1] is class (raw logits), outputs[3] is confidence (softmax)
        let (_, conf) = outputs[3].try_extract_tensor::<f32>()?;
        let (_, landmarks) = outputs[2].try_extract_tensor::<f32>()?;
        let dofs_data = if self.model_type.has_pose() && outputs.len() > 4 {
            Some(outputs[4].try_extract_tensor::<f32>()?.1)
        } else {
            None
        };

        let strides = self.model_type.strides();
        let anchors = self.model_type.anchors();

        let mut candidates = Vec::new();
        let mut offset = 0;

        for (level, &stride) in strides.iter().enumerate() {
            let gh = h.div_ceil(stride);
            let gw = w.div_ceil(stride);

            for j in 0..gh {
                for k in 0..gw {
                    // Anchor centre
                    let ax = (k as f32 + 0.5) * stride as f32;
                    let ay = (j as f32 + 0.5) * stride as f32;

                    for m in 0..2 {
                        let p = offset + m + 2 * (k + gw * j);

                        // Confidence is softmax output, index 1 is face probability
                        let score = conf[p * 2 + 1];
                        if score <= self.conf_threshold {
                            continue;
                        }

                        let anchor_size = anchors[level][m]; // anchor sizes per level

                        // Decode bbox: cx, cy, w, h
                        let cx = VARIANCE_LOC * bbox[p * 4] * anchor_size + ax;
                        let cy = VARIANCE_LOC * bbox[p * 4 + 1] * anchor_size + ay;
                        let bw = (VARIANCE_SIZE * bbox[p * 4 + 2]).exp() * anchor_size;
                        let bh = (VARIANCE_SIZE * bbox[p * 4 + 3]).exp() * anchor_size;

                        // Skip tiny boxes
                        if bw < 2.0 || bh < 2.0 {
                            continue;
                        }

                        // Convert to x1,y1,x2,y2
                        let x1 = cx - bw * 0.5;
                        let y1 = cy - bh * 0.5;
                        let x2 = cx + bw * 0.5;
                        let y2 = cy + bh * 0.5;

                        // Decode landmarks (5 points, 10 values)
                        let mut lms = [[0.0f32; 2]; 5];
                        for i in 0..5 {
                            lms[i][0] = VARIANCE_LOC * landmarks[p * 10 + i * 2] * anchor_size + ax;
                            lms[i][1] = VARIANCE_LOC * landmarks[p * 10 + i * 2 + 1] * anchor_size + ay;
                        }

                        // Decode 6-DoF pose if available
                        let (dofs, rotation) = if let Some(dofs_raw) = &dofs_data {
                            let mut d = [0.0f32; 6];
                            for i in 0..6 {
                                d[i] = dofs_raw[p * 6 + i] * POSE_STD[i] + POSE_MEAN[i];
                            }
                            let rot = decode_rotation(&d[..3], ax, ay, w as f32, h as f32);
                            (Some(d), Some(rot))
                        } else {
                            (None, None)
                        };

                        candidates.push(MsRetinaFace {
                            face: Face {
                                bbox: [x1, y1, x2, y2],
                                score,
                                landmarks: lms,
                            },
                            dofs,
                            rotation,
                        });
                    }
                }
            }
            offset += 2 * gh * gw;
        }

        // Sort by score descending
        candidates.sort_by(|a, b| b.face.score.total_cmp(&a.face.score));

        // Greedy non-intersecting NMS (MS uses ANY intersection, not IoU threshold)
        let mut kept = Vec::new();
        for c in candidates {
            if kept.len() >= self.max_faces {
                break;
            }
            // Reject if intersects any kept box at all
            if kept.iter().any(|k: &MsRetinaFace| boxes_intersect(&k.face.bbox, &c.face.bbox)) {
                continue;
            }
            kept.push(c);
        }

        // Reject candidates with landmarks outside the frame
        let kept: Vec<MsRetinaFace> = kept
            .into_iter()
            .filter(|f| {
                f.face.landmarks.iter().all(|lm| {
                    lm[0] >= 0.0 && lm[0] < w as f32 && lm[1] >= 0.0 && lm[1] < h as f32
                })
            })
            .collect();

        debug!(count = kept.len(), "detected faces (MS Retina)");
        Ok(kept)
    }

    /// Convert to standard Face structs (discarding pose info).
    pub fn detect_faces(&mut self, image: &GrayImage) -> Result<Vec<Face>> {
        Ok(self.detect(image)?.into_iter().map(|f| f.face).collect())
    }

    #[allow(dead_code)]
    fn num_priors(&self, h: usize, w: usize) -> usize {
        self.model_type
            .strides()
            .iter()
            .map(|&s| 2 * h.div_ceil(s) * w.div_ceil(s))
            .sum()
    }
}

/// Check if two boxes intersect at all (MS NMS uses any intersection, not IoU).
fn boxes_intersect(a: &[f32; 4], b: &[f32; 4]) -> bool {
    a[0] < b[2] && a[2] > b[0] && a[1] < b[3] && a[3] > b[1]
}

/// Decode rotation vector to pitch/yaw/roll angles.
///
/// Uses img2pose-style decode with camera matrices K_box and K_img.
/// Returns (pitch, yaw, roll) in degrees.
fn decode_rotation(rv: &[f32], ax: f32, ay: f32, w: f32, h: f32) -> [f32; 3] {
    use std::f32::consts::PI;

    let fsum = w + h;

    // K_box = [[fsum, 0, ax], [0, fsum, ay], [0, 0, 1]]
    // K_img = [[fsum, 0, W/2], [0, fsum, H/2], [0, 0, 1]]

    // Build rotation matrix from rotation vector (Rodrigues)
    let th = (rv[0] * rv[0] + rv[1] * rv[1] + rv[2] * rv[2]).sqrt();
    let k = if th > 0.001 {
        (th * 0.5).sin() / th
    } else {
        0.5 - th * th / 48.0 + th.powi(4) / 3840.0
    };

    let (x, y, z) = (k * rv[0], k * rv[1], k * rv[2]);
    let w_quat = (th * 0.5).cos();

    // Quaternion to rotation matrix
    let r = [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - z * w_quat),
            2.0 * (x * z + y * w_quat),
        ],
        [
            2.0 * (x * y + z * w_quat),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - x * w_quat),
        ],
        [
            2.0 * (x * z - y * w_quat),
            2.0 * (y * z + x * w_quat),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ];

    // M = K_img^-1 * K_box * R
    // K_img^-1 = [[1/f, 0, -W/(2f)], [0, 1/f, -H/(2f)], [0, 0, 1]]
    // K_box = [[f, 0, ax], [0, f, ay], [0, 0, 1]]
    //
    // K_box * R = [[f*r00 + ax*r20, f*r01 + ax*r21, f*r02 + ax*r22],
    //              [f*r10 + ay*r20, f*r11 + ay*r21, f*r12 + ay*r22],
    //              [r20, r21, r22]]
    //
    // K_img^-1 * (K_box * R) simplifies significantly because the [2,*] row of K_img^-1 is [0,0,1]

    let f = fsum;
    let half_w = w * 0.5;
    let half_h = h * 0.5;

    // Compute M = K_img^-1 * K_box * R
    // First: K_box * R
    let kb_r = [
        [
            f * r[0][0] + ax * r[2][0],
            f * r[0][1] + ax * r[2][1],
            f * r[0][2] + ax * r[2][2],
        ],
        [
            f * r[1][0] + ay * r[2][0],
            f * r[1][1] + ay * r[2][1],
            f * r[1][2] + ay * r[2][2],
        ],
        [r[2][0], r[2][1], r[2][2]],
    ];

    // K_img^-1 * (K_box * R)
    let m = [
        [
            kb_r[0][0] / f - half_w * kb_r[2][0] / f,
            kb_r[0][1] / f - half_w * kb_r[2][1] / f,
            kb_r[0][2] / f - half_w * kb_r[2][2] / f,
        ],
        [
            kb_r[1][0] / f - half_h * kb_r[2][0] / f,
            kb_r[1][1] / f - half_h * kb_r[2][1] / f,
            kb_r[1][2] / f - half_h * kb_r[2][2] / f,
        ],
        [kb_r[2][0], kb_r[2][1], kb_r[2][2]],
    ];

    // Extract angles: (atan2(M12, M22), asin(M02), -atan2(M01, M00))
    let pitch = m[1][2].atan2(m[2][2]) * 180.0 / PI;
    let yaw = m[0][2].clamp(-1.0, 1.0).asin() * 180.0 / PI;
    let roll = -m[0][1].atan2(m[0][0]) * 180.0 / PI;

    [pitch, yaw, roll]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_boxes_intersect() {
        let a = [0.0, 0.0, 10.0, 10.0];
        let b = [5.0, 5.0, 15.0, 15.0];
        assert!(boxes_intersect(&a, &b));

        let c = [20.0, 20.0, 30.0, 30.0];
        assert!(!boxes_intersect(&a, &c));

        // Edge touching is not intersection
        let d = [10.0, 0.0, 20.0, 10.0];
        assert!(!boxes_intersect(&a, &d));
    }

    #[test]
    fn test_decode_rotation_zero() {
        let rv = [0.0, 0.0, 0.0];
        let rot = decode_rotation(&rv, 320.0, 180.0, 640.0, 360.0);
        // Zero rotation vector should give ~zero angles
        assert!(rot[0].abs() < 1.0);
        assert!(rot[1].abs() < 1.0);
        assert!(rot[2].abs() < 1.0);
    }
}
