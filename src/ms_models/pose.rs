//! Head pose estimation using converted Windows Hello IrFaceOrientation models.
//!
//! Pipeline: 64×64 similarity-warped crop → DoG filter → uniform LBP → 7139-d histogram → LDA+KNN.
//!
//! The ONNX model wraps the LDA projection and nearest-centroid lookup.

use anyhow::{Context, Result, bail};
use image::GrayImage;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::TensorRef;
use std::path::Path;
use tracing::debug;

use crate::{get_ort_env, Face};

/// Image size for the similarity-warped face crop.
const SIZE: usize = 64;
/// LBP cell size.
const CELL: usize = 10;
/// Number of LBP cells per axis.
const CELLS: usize = 11;
/// LBP margin from image edge.
const MARGIN: usize = 1;
/// LBP neighbor radius.
const RADIUS: usize = 1;
/// Step between cell origins (SIZE / CELLS = 5, but only 10x10 cells used).
const STEP: usize = SIZE / CELLS;

/// 1/255 as a specific float32 bit pattern from the Windows code.
const INV255: f32 = 0.003921569;

/// Uniform LBP lookup table (58 for non-uniform patterns, 0-57 for uniform).
/// Computed by counting transitions in the 8-bit circular pattern.
#[rustfmt::skip]
const LBP_LUT: [u8; 256] = [
    0, 1, 2, 3, 4, 58, 5, 6, 7, 58, 58, 58, 8, 58, 9, 10,
    11, 58, 58, 58, 58, 58, 58, 58, 12, 58, 58, 58, 13, 58, 14, 15,
    16, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58,
    17, 58, 58, 58, 58, 58, 58, 58, 18, 58, 58, 58, 19, 58, 20, 21,
    22, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58,
    58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58,
    23, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58,
    24, 58, 58, 58, 58, 58, 58, 58, 25, 58, 58, 58, 26, 58, 27, 28,
    29, 30, 58, 31, 58, 58, 58, 32, 58, 58, 58, 58, 58, 58, 58, 33,
    58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 34,
    58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58,
    58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 35,
    36, 37, 58, 38, 58, 58, 58, 39, 58, 58, 58, 58, 58, 58, 58, 40,
    58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 58, 41,
    42, 43, 58, 44, 58, 58, 58, 45, 58, 58, 58, 58, 58, 58, 58, 46,
    47, 48, 58, 49, 58, 58, 58, 50, 51, 52, 58, 53, 54, 55, 56, 57,
];

/// Template coordinates for 5-point alignment (left eye, right eye, nose, left mouth, right mouth).
/// Subset of Windows Hello's 27-point template for 64×64.
const TEMPLATE_5: [[f32; 2]; 5] = [
    [21.661, 20.715],
    [42.995, 20.614],
    [32.072, 32.190],
    [23.588, 42.566],
    [42.102, 42.218],
];

/// Gaussian kernel σ=2 (13 taps).
#[rustfmt::skip]
const K_SIGMA2: [f32; 13] = [
    0.0022182, 0.0087731, 0.0270232, 0.0648252, 0.1211094, 0.1762131, 0.1996756,
    0.1762131, 0.1211094, 0.0648252, 0.0270232, 0.0087731, 0.0022182,
];

/// Gaussian kernel σ=4 (25 taps).
#[rustfmt::skip]
const K_SIGMA4: [f32; 25] = [
    0.0011099, 0.0022773, 0.0043897, 0.0079487, 0.0135211, 0.0216067, 0.0324355,
    0.0457414, 0.0605975, 0.0754148, 0.0881688, 0.0968345, 0.0999084, 0.0968345,
    0.0881688, 0.0754148, 0.0605975, 0.0457414, 0.0324355, 0.0216067, 0.0135211,
    0.0079487, 0.0043897, 0.0022773, 0.0011099,
];

/// Estimated head pose angles.
#[derive(Debug, Clone, Copy, Default)]
pub struct HeadPose {
    /// Pitch angle in degrees (positive = looking up).
    pub pitch: f32,
    /// Yaw angle in degrees (positive = looking right from camera's view).
    pub yaw: f32,
}

impl HeadPose {
    /// Check if the pose is within frontal limits.
    pub fn is_frontal(&self, yaw_limit: f32, pitch_limit: f32) -> bool {
        self.yaw.abs() <= yaw_limit && self.pitch.abs() <= pitch_limit
    }
}

/// Configuration for pose checking.
#[derive(Debug, Clone)]
pub struct PoseConfig {
    /// Maximum allowed yaw magnitude (default 20°).
    pub yaw_limit: f32,
    /// Maximum allowed pitch magnitude (default 25°).
    pub pitch_limit: f32,
    /// Yaw calibration offset to correct for camera/domain shift (default 0).
    pub yaw_offset: f32,
    /// Pitch calibration offset (default 0).
    pub pitch_offset: f32,
}

impl Default for PoseConfig {
    fn default() -> Self {
        Self {
            yaw_limit: 20.0,
            pitch_limit: 25.0,
            yaw_offset: 0.0,
            pitch_offset: 0.0,
        }
    }
}

/// Head pose checker using the converted Windows Hello IrFaceOrientation model.
pub struct PoseChecker {
    session: Session,
    config: PoseConfig,
}

impl PoseChecker {
    /// Load the pose model from an ONNX file.
    pub fn load(model_path: impl AsRef<Path>) -> Result<Self> {
        Self::load_with_config(model_path, PoseConfig::default())
    }

    /// Load the pose model with custom configuration.
    pub fn load_with_config(model_path: impl AsRef<Path>, config: PoseConfig) -> Result<Self> {
        let model_path = model_path.as_ref();
        debug!(?model_path, "loading head pose model");

        let session = Session::builder(get_ort_env()?)
            .map_err(|e| anyhow::anyhow!("session builder: {}", e))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow::anyhow!("optimization level: {}", e))?
            .commit_from_file(model_path)
            .with_context(|| format!("failed to load {}", model_path.display()))?;

        Ok(Self { session, config })
    }

    /// Estimate head pose from an IR image and detected face.
    pub fn estimate(&mut self, image: &GrayImage, face: &Face) -> Result<HeadPose> {
        // Extract 5 landmarks from the face
        let points = &face.landmarks;

        // Compute similarity transform to 64×64 template
        let (a, b, tx, ty) = similarity_to_template(points)?;

        // Warp the face to 64×64
        let warped = warp_face(image, a, b, tx, ty);

        // Apply DoG filter
        let dog = dog_filter(&warped);

        // Compute uniform LBP codes
        let codes = lbp_codes(&dog);

        // Extract histogram feature (7139-d)
        let feature = lbp_feature(&codes);

        // Run ONNX inference
        let input = TensorRef::from_array_view(([1i64, 7139i64], feature.as_slice()))?;
        let outputs = self.session.run(ort::inputs![input])?;

        let (_, pitch_out) = outputs[0].try_extract_tensor::<f32>()?;
        let (_, yaw_out) = outputs[1].try_extract_tensor::<f32>()?;

        let pitch = pitch_out[0] + self.config.pitch_offset;
        let yaw = yaw_out[0] + self.config.yaw_offset;

        debug!(pitch, yaw, "head pose estimated");
        Ok(HeadPose { pitch, yaw })
    }

    /// Check if a face passes the pose gate.
    pub fn check(&mut self, image: &GrayImage, face: &Face) -> Result<(HeadPose, bool)> {
        let pose = self.estimate(image, face)?;
        let frontal = pose.is_frontal(self.config.yaw_limit, self.config.pitch_limit);
        debug!(
            pitch = pose.pitch,
            yaw = pose.yaw,
            frontal,
            "pose check"
        );
        Ok((pose, frontal))
    }

    /// Update configuration.
    pub fn set_config(&mut self, config: PoseConfig) {
        self.config = config;
    }

    /// Get current configuration.
    pub fn config(&self) -> &PoseConfig {
        &self.config
    }
}

/// Compute similarity transform coefficients to map landmarks to the template.
/// Returns (a, b, tx, ty) for the transform [a, -b; b, a] * p + [tx; ty].
fn similarity_to_template(points: &[[f32; 2]; 5]) -> Result<(f32, f32, f32, f32)> {
    let n = 5;
    let mut sx = 0.0f32;
    let mut sy = 0.0f32;
    let mut tx_ = 0.0f32;
    let mut ty_ = 0.0f32;
    let mut sxx = 0.0f32;
    let mut syy = 0.0f32;
    let mut xs = 0.0f32;
    let mut sxty = 0.0f32;
    let mut txsy = 0.0f32;
    let mut syty = 0.0f32;

    for i in 0..n {
        let x = points[i][0];
        let y = points[i][1];
        let u = TEMPLATE_5[i][0];
        let v = TEMPLATE_5[i][1];

        sx += x;
        sy += y;
        tx_ += u;
        ty_ += v;
        sxx += x * x;
        syy += y * y;
        xs += u * x;
        sxty += x * v;
        txsy += u * y;
        syty += y * v;
    }

    let s2 = syy + sxx;
    let nf = n as f32;
    let den = sx * sx + sy * sy - nf * s2;

    if den.abs() < 1e-6 {
        bail!("degenerate landmarks");
    }

    let dot = syty + xs;
    let cross = sxty - txsy;

    let a = (tx_ * sx + ty_ * sy - dot * nf) / den;
    let b = (tx_ * sy - ty_ * sx - cross * nf) / den;
    let tx = (cross * sy + dot * sx - s2 * tx_) / den;
    let ty = (dot * sy - cross * sx - s2 * ty_) / den;

    Ok((a, b, tx, ty))
}

/// Warp the image to 64×64 using the similarity transform with bilinear interpolation.
fn warp_face(image: &GrayImage, a: f32, b: f32, tx: f32, ty: f32) -> [[f32; SIZE]; SIZE] {
    let (w, h) = (image.width() as i32, image.height() as i32);
    let n2 = a * a + b * b;
    let c13 = a / n2;
    let c14 = -b / n2;
    let c15 = (-tx * b - ty * a) / n2;
    let c16 = (ty * b - tx * a) / n2;

    let mut out = [[0.0f32; SIZE]; SIZE];

    for row in 0..SIZE {
        for col in 0..SIZE {
            let x = (col as f32 * c13) + (row as f32 * c14) + c16;
            let y = (row as f32 * c13) - (col as f32 * c14) + c15;

            let x0 = x.floor() as i32;
            let y0 = y.floor() as i32;
            let fx = x - x0 as f32;
            let fy = y - y0 as f32;

            let px = |cx: i32, cy: i32| -> f32 {
                if cx >= 0 && cx < w && cy >= 0 && cy < h {
                    image.get_pixel(cx as u32, cy as u32).0[0] as f32
                } else {
                    0.0
                }
            };

            let p00 = px(x0, y0);
            let p10 = px(x0 + 1, y0);
            let p01 = px(x0, y0 + 1);
            let p11 = px(x0 + 1, y0 + 1);

            let top = (p10 - p00) * fx + p00;
            let bot = (p11 - p01) * fx + p01;
            out[row][col] = ((bot - top) * fy + top) * INV255;
        }
    }

    out
}

/// Apply Gaussian filter with symmetric padding (mirrors at edges).
fn gaussian_filter(input: &[[f32; SIZE]; SIZE], kernel: &[f32]) -> [[f32; SIZE]; SIZE] {
    let r = kernel.len() / 2;
    let mut horiz = [[0.0f32; SIZE]; SIZE];

    // Horizontal pass
    for row in 0..SIZE {
        for col in 0..SIZE {
            let mut sum = 0.0f32;
            for (ki, &kv) in kernel.iter().enumerate() {
                let c = col as i32 + ki as i32 - r as i32;
                // Mirror padding
                let c = if c < 0 {
                    -c
                } else if c >= SIZE as i32 {
                    2 * (SIZE as i32 - 1) - c
                } else {
                    c
                } as usize;
                sum += input[row][c] * kv;
            }
            horiz[row][col] = sum;
        }
    }

    // The Windows code only produces columns 4g and 4g+1, copying to 4g+2 and 4g+3
    // Replicate this quirk for bit-exact matching
    for row in 0..SIZE {
        for g in 0..(SIZE / 4) {
            let v0 = horiz[row][4 * g];
            let v1 = horiz[row][4 * g + 1];
            horiz[row][4 * g + 2] = v0;
            horiz[row][4 * g + 3] = v1;
        }
    }

    // Vertical pass
    let mut out = [[0.0f32; SIZE]; SIZE];
    for col in 0..SIZE {
        for row in 0..SIZE {
            let mut sum = 0.0f32;
            for (ki, &kv) in kernel.iter().enumerate() {
                let r_idx = row as i32 + ki as i32 - r as i32;
                let r_idx = if r_idx < 0 {
                    -r_idx
                } else if r_idx >= SIZE as i32 {
                    2 * (SIZE as i32 - 1) - r_idx
                } else {
                    r_idx
                } as usize;
                sum += horiz[r_idx][col] * kv;
            }
            out[row][col] = sum;
        }
    }

    out
}

/// Difference of Gaussians: G(σ=2) - G(σ=4).
fn dog_filter(input: &[[f32; SIZE]; SIZE]) -> [[f32; SIZE]; SIZE] {
    let g2 = gaussian_filter(input, &K_SIGMA2);
    let g4 = gaussian_filter(input, &K_SIGMA4);

    let mut out = [[0.0f32; SIZE]; SIZE];
    for row in 0..SIZE {
        for col in 0..SIZE {
            out[row][col] = g2[row][col] - g4[row][col];
        }
    }
    out
}

/// Compute uniform LBP codes. Bit set when neighbor > center.
/// Bit order: TL, T, TR, R, BR, B, BL, L.
fn lbp_codes(dog: &[[f32; SIZE]; SIZE]) -> [[u8; SIZE]; SIZE] {
    let mut out = [[0u8; SIZE]; SIZE];
    let lo = MARGIN + RADIUS;
    let hi = SIZE - MARGIN - RADIUS;

    for row in lo..hi {
        for col in lo..hi {
            let center = dog[row][col];
            let mut code = 0u32;

            // Neighbors in order: TL, T, TR, R, BR, B, BL, L
            let neighbors = [
                (-1i32, -1i32), (-1, 0), (-1, 1), (0, 1),
                (1, 1), (1, 0), (1, -1), (0, -1),
            ];

            for (bit, (dr, dc)) in neighbors.iter().enumerate() {
                let nr = (row as i32 + dr) as usize;
                let nc = (col as i32 + dc) as usize;
                if center < dog[nr][nc] {
                    code |= 1 << bit;
                }
            }

            out[row][col] = LBP_LUT[code as usize];
        }
    }

    out
}

/// Extract 7139-d histogram feature from LBP codes.
/// 11×11 cells of 10×10 pixels, 59 bins each.
fn lbp_feature(codes: &[[u8; SIZE]; SIZE]) -> Vec<f32> {
    let mut feature = vec![0i32; CELLS * CELLS * 59];
    let o = MARGIN + RADIUS;

    for cy in 0..CELLS {
        for cx in 0..CELLS {
            let cell_idx = cy * CELLS + cx;
            let base = cell_idx * 59;

            for dy in 0..CELL {
                for dx in 0..CELL {
                    let row = o + STEP * cy + dy;
                    let col = o + STEP * cx + dx;
                    let bin = codes[row][col] as usize;
                    feature[base + bin] += 1;
                }
            }
        }
    }

    // Scale by 0.01 as in the Windows code
    feature.iter().map(|&v| v as f32 * 0.01).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lbp_lut_uniform() {
        // Check that uniform patterns get sequential indices 0-57
        // and non-uniform patterns get 58
        let mut uniform_count = 0;
        for i in 0..256 {
            if LBP_LUT[i] < 58 {
                uniform_count += 1;
            }
        }
        assert_eq!(uniform_count, 58);
    }

    #[test]
    fn test_similarity_transform() {
        // Identity-ish case: landmarks at template positions
        let points = TEMPLATE_5;
        let (a, b, tx, ty) = similarity_to_template(&points).unwrap();
        // Should be close to identity (a≈1, b≈0, tx≈0, ty≈0)
        assert!((a - 1.0).abs() < 0.1);
        assert!(b.abs() < 0.1);
        assert!(tx.abs() < 1.0);
        assert!(ty.abs() < 1.0);
    }
}
