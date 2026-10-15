//! Microsoft IR liveness ensemble.
//!
//! Uses the Windows Hello anti-spoofing models (flag-on variants 121–125):
//! - 121 (slot 11): general IR - main anti-spoofing on aligned crop
//! - 122 (slot 12): diffusion IR - difference image (lit − ambient) crop
//! - 123 (slot 13): color BGR - RGB camera veto only
//! - 124 (slot 14): multi-task border - whole frame 128×128 veto
//! - 125 (slot 15): multi-task HH - whole frame 128×128 veto
//!
//! The models are proprietary and remain local (git-ignored).

use std::path::Path;

use anyhow::{Context, Result, bail};
use image::{GrayImage, RgbImage};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::TensorRef;
use tracing::{debug, info};

use super::prep::{align_crop, difference_image, thumbnail};
use crate::{Face, get_ort_env};

/// Thresholds for the flag-on models (from `docs/winhello-models/convnet.md` section 4).
/// Using mode 0 thresholds (lower, more permissive).
mod thresholds {
    // Slot 11 (general IR): prob[0] > real → live; prob[1] > spoof → spoof
    pub const GENERAL_IR_REAL: f32 = 0.4711;
    pub const GENERAL_IR_SPOOF: f32 = 1.0; // mode 0: never triggers spoof via threshold

    // Slot 12 (diffusion IR): same logic
    pub const DIFFUSION_IR_REAL: f32 = 0.5037;
    pub const DIFFUSION_IR_SPOOF: f32 = 1.0;

    // Slot 13 (color): prob[0] > threshold → spoof veto
    pub const COLOR_SPOOF: f32 = 0.5;

    // Slot 14 (border): prob[1] > threshold → spoof veto
    pub const BORDER_SPOOF: f32 = 0.7;

    // Slot 15 (HH): prob[1] > threshold → spoof veto
    pub const HH_SPOOF: f32 = 0.5;
}

/// Per-model scores for debugging.
#[derive(Debug, Clone, Default)]
pub struct MsModelScores {
    /// General IR (slot 11): [p_real, p_spoof]
    pub general_ir: Option<[f32; 2]>,
    /// Diffusion IR (slot 12): [p_real, p_spoof]
    pub diffusion_ir: Option<[f32; 2]>,
    /// Color BGR (slot 13): [p_spoof, p_real]
    pub color: Option<[f32; 2]>,
    /// Multi-task border (slot 14): [p_live, p_spoof]
    pub border: Option<[f32; 2]>,
    /// Multi-task HH (slot 15): [p_live, p_spoof]
    pub hh: Option<[f32; 2]>,
}

/// Result of the Microsoft IR liveness ensemble.
#[derive(Debug, Clone)]
pub struct MsLivenessResult {
    /// Overall liveness score (0.0 = spoof, 1.0 = live).
    pub score: f32,
    /// Whether the face is considered live.
    pub is_live: bool,
    /// Net votes: positive = live, negative = spoof.
    pub net_votes: i32,
    /// Per-model raw scores for debugging.
    pub model_scores: MsModelScores,
}

/// Microsoft IR liveness ensemble.
///
/// Uses the Windows Hello anti-spoofing models to determine liveness.
/// Voting logic:
/// - General IR and diffusion IR vote: live (+1) or spoof (−1) based on thresholds.
/// - Color, border, and HH only veto (if spoof detected, vote −1).
/// - Net votes > 0 → live, otherwise spoof.
pub struct MsIrLiveness {
    /// Slot 11: general IR CNN
    general_ir: Session,
    /// Slot 12: diffusion IR CNN
    diffusion_ir: Session,
    /// Slot 13: color BGR CNN
    color: Session,
    /// Slot 14: multi-task border
    border: Session,
    /// Slot 15: multi-task HH
    hh: Session,
}

impl MsIrLiveness {
    /// Load all 5 models from the given directory.
    ///
    /// Expects `MODEL_FILES_121.onnx` through `MODEL_FILES_125.onnx`.
    pub fn load(model_dir: impl AsRef<Path>) -> Result<Self> {
        let model_dir = model_dir.as_ref();
        info!(?model_dir, "loading Microsoft IR liveness ensemble");

        let load_model = |id: u32| -> Result<Session> {
            let path = model_dir.join(format!("MODEL_FILES_{}.onnx", id));
            if !path.exists() {
                bail!(
                    "Microsoft model not found: {} (see docs/winhello-models/)",
                    path.display()
                );
            }
            debug!(?path, "loading model");
            Session::builder(get_ort_env()?)
                .map_err(|e| anyhow::anyhow!("session builder: {}", e))?
                .with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(|e| anyhow::anyhow!("optimization level: {}", e))?
                .commit_from_file(&path)
                .with_context(|| format!("failed to load {}", path.display()))
        };

        Ok(Self {
            general_ir: load_model(121)?,
            diffusion_ir: load_model(122)?,
            color: load_model(123)?,
            border: load_model(124)?,
            hh: load_model(125)?,
        })
    }

    /// Check if the models directory contains all required files.
    pub fn models_available(model_dir: impl AsRef<Path>) -> bool {
        let model_dir = model_dir.as_ref();
        [121, 122, 123, 124, 125]
            .iter()
            .all(|id| model_dir.join(format!("MODEL_FILES_{}.onnx", id)).exists())
    }

    /// Run the liveness check.
    ///
    /// # Arguments
    /// - `ir_lit`: The IR frame with illumination (lit frame).
    /// - `ir_ambient`: Optional ambient IR frame (for difference image). If not provided,
    ///   the diffusion IR model is skipped.
    /// - `rgb`: Optional RGB frame (for color model). If not provided, the color model is skipped.
    /// - `face`: Detected face with 5-point landmarks.
    pub fn check(
        &mut self,
        ir_lit: &GrayImage,
        ir_ambient: Option<&GrayImage>,
        rgb: Option<&RgbImage>,
        face: &Face,
    ) -> Result<MsLivenessResult> {
        let mut scores = MsModelScores::default();
        let mut votes = 0i32;

        // Convert Face landmarks to the format expected by preprocessing
        let landmarks: [[f32; 2]; 5] = face.landmarks;

        // 1. General IR (slot 11): aligned crop of lit frame
        let aligned_ir = align_crop(ir_lit, &landmarks);
        let general_probs = run_grayscale(&mut self.general_ir, &aligned_ir)?;
        scores.general_ir = Some(general_probs);
        debug!(
            p_real = general_probs[0],
            p_spoof = general_probs[1],
            "general IR"
        );

        if general_probs[0] > thresholds::GENERAL_IR_REAL {
            votes += 1;
            debug!("general IR: vote LIVE");
        } else if general_probs[1] > thresholds::GENERAL_IR_SPOOF {
            votes -= 1;
            debug!("general IR: vote SPOOF");
        }

        // 2. Diffusion IR (slot 12): difference image crop
        if let Some(ambient) = ir_ambient {
            let diff = difference_image(ir_lit, ambient);
            let aligned_diff = align_crop(&diff, &landmarks);
            let diffusion_probs = run_grayscale(&mut self.diffusion_ir, &aligned_diff)?;
            scores.diffusion_ir = Some(diffusion_probs);
            debug!(
                p_real = diffusion_probs[0],
                p_spoof = diffusion_probs[1],
                "diffusion IR"
            );

            if diffusion_probs[0] > thresholds::DIFFUSION_IR_REAL {
                votes += 1;
                debug!("diffusion IR: vote LIVE");
            } else if diffusion_probs[1] > thresholds::DIFFUSION_IR_SPOOF {
                votes -= 1;
                debug!("diffusion IR: vote SPOOF");
            }
        } else {
            debug!("diffusion IR: skipped (no ambient frame)");
        }

        // 3. Color BGR (slot 13): veto only
        if let Some(rgb_frame) = rgb {
            let color_probs = run_color(&mut self.color, rgb_frame, &landmarks)?;
            scores.color = Some(color_probs);
            debug!(
                p_spoof = color_probs[0],
                p_real = color_probs[1],
                "color BGR"
            );

            if color_probs[0] > thresholds::COLOR_SPOOF {
                votes -= 1;
                debug!("color: VETO (spoof)");
            }
        } else {
            debug!("color: skipped (no RGB frame)");
        }

        // 4. Multi-task border (slot 14): whole frame thumbnail, veto only
        let thumb = thumbnail(ir_lit);
        let border_probs = run_grayscale(&mut self.border, &thumb)?;
        scores.border = Some(border_probs);
        debug!(
            p_live = border_probs[0],
            p_spoof = border_probs[1],
            "border"
        );

        if border_probs[1] > thresholds::BORDER_SPOOF {
            votes -= 1;
            debug!("border: VETO (spoof)");
        }

        // 5. Multi-task HH (slot 15): whole frame thumbnail, veto only
        let hh_probs = run_grayscale(&mut self.hh, &thumb)?;
        scores.hh = Some(hh_probs);
        debug!(p_live = hh_probs[0], p_spoof = hh_probs[1], "HH");

        if hh_probs[1] > thresholds::HH_SPOOF {
            votes -= 1;
            debug!("HH: VETO (spoof)");
        }

        // Final decision
        let is_live = votes > 0;
        // Convert votes to a 0-1 score: map [-5, 5] to [0, 1]
        let score = ((votes as f32 + 5.0) / 10.0).clamp(0.0, 1.0);

        info!(
            votes,
            is_live,
            score,
            general_ir = ?scores.general_ir,
            diffusion_ir = ?scores.diffusion_ir,
            color = ?scores.color,
            border = ?scores.border,
            hh = ?scores.hh,
            "MS IR liveness result"
        );

        Ok(MsLivenessResult {
            score,
            is_live,
            net_votes: votes,
            model_scores: scores,
        })
    }
}

/// Run a grayscale model (1×128×128 input).
fn run_grayscale(session: &mut Session, img: &GrayImage) -> Result<[f32; 2]> {
    assert_eq!(img.dimensions(), (128, 128));

    // NCHW: [1, 1, 128, 128], raw 0-255 values (model normalizes internally)
    let plane = 128 * 128;
    let mut data = vec![0.0f32; plane];
    for (x, y, px) in img.enumerate_pixels() {
        let i = y as usize * 128 + x as usize;
        data[i] = px.0[0] as f32;
    }

    let input = TensorRef::from_array_view(([1i64, 1, 128, 128], data.as_slice()))?;
    let outputs = session.run(ort::inputs![input])?;

    // Output "prob" is the softmax probabilities (last output)
    let prob_idx = outputs.len() - 1;
    let (_, probs) = outputs[prob_idx].try_extract_tensor::<f32>()?;

    Ok([probs[0], probs[1]])
}

/// Run the color model (3×128×128 BGR input).
fn run_color(session: &mut Session, rgb: &RgbImage, landmarks: &[[f32; 2]; 5]) -> Result<[f32; 2]> {
    let aligned = align_crop_rgb(rgb, landmarks);

    // NCHW: [1, 3, 128, 128], BGR order, raw 0-255 values
    let plane = 128 * 128;
    let mut data = vec![0.0f32; 3 * plane];
    for (x, y, px) in aligned.enumerate_pixels() {
        let i = y as usize * 128 + x as usize;
        // BGR order: Blue = channel 0, Green = channel 1, Red = channel 2
        // BGR planar layout (plane 0 = B, plane 1 = G, plane 2 = R)
        data[i] = px.0[2] as f32;             // B from RGB R position
        data[plane + i] = px.0[1] as f32;     // G
        data[2 * plane + i] = px.0[0] as f32; // R from RGB B position
    }

    let input = TensorRef::from_array_view(([1i64, 3, 128, 128], data.as_slice()))?;
    let outputs = session.run(ort::inputs![input])?;

    let prob_idx = outputs.len() - 1;
    let (_, probs) = outputs[prob_idx].try_extract_tensor::<f32>()?;

    Ok([probs[0], probs[1]])
}

/// Align and crop an RGB image using 5-point landmarks.
fn align_crop_rgb(img: &RgbImage, landmarks: &[[f32; 2]; 5]) -> RgbImage {
    const TEMPLATE_281: [[f32; 2]; 5] = [
        [97.82990264892578, 127.7791976928711],
        [181.05320739746094, 126.33809661865234],
        [139.57400512695312, 179.00059509277344],
        [102.69640350341797, 216.70140075683594],
        [179.30679321289062, 215.37330627441406],
    ];

    // Fit similarity transform
    let m = fit_similarity_rgb(landmarks, &TEMPLATE_281);
    let inv = invert_affine_rgb(&m);

    // Warp to 281×281
    let mut warped = RgbImage::new(281, 281);
    for y in 0..281u32 {
        for x in 0..281u32 {
            let xf = x as f32;
            let yf = y as f32;
            let sx = xf * inv[0][0] + yf * inv[0][1] + inv[0][2];
            let sy = xf * inv[1][0] + yf * inv[1][1] + inv[1][2];
            let rgb = bilinear_sample_rgb(img, sx, sy);
            warped.put_pixel(x, y, image::Rgb(rgb));
        }
    }

    // Resample central 230×230 to 128×128
    let mut out = RgbImage::new(128, 128);
    let half = 64.0f32;
    let scale = 230.0 / 127.0;

    for y in 0..128u32 {
        for x in 0..128u32 {
            let sx = (x as f32 - half) * scale + 140.5;
            let sy = (y as f32 - half) * scale + 140.5;
            let rgb = bilinear_sample_rgb(&warped, sx, sy);
            out.put_pixel(x, y, image::Rgb(rgb));
        }
    }

    out
}

fn fit_similarity_rgb(src: &[[f32; 2]; 5], dst: &[[f32; 2]; 5]) -> [[f32; 3]; 2] {
    let n = 5.0f32;

    let (mut mx, mut my, mut mu, mut mv) = (0.0f32, 0.0, 0.0, 0.0);
    for i in 0..5 {
        mx += src[i][0];
        my += src[i][1];
        mu += dst[i][0];
        mv += dst[i][1];
    }
    mx /= n;
    my /= n;
    mu /= n;
    mv /= n;

    let mut sxx = 0.0f32;
    let mut sp = 0.0f32;
    let mut sm = 0.0f32;
    for i in 0..5 {
        let (x, y) = (src[i][0], src[i][1]);
        let (u, v) = (dst[i][0], dst[i][1]);
        sxx += x * x + y * y;
        sp += u * x + v * y;
        sm += u * y - v * x;
    }
    sxx /= n;
    sp /= n;
    sm /= n;

    let var = sxx - mx * mx - my * my;
    if var.abs() < 1e-6 {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    }

    let a = (sp - mx * mu - my * mv) / var;
    let b = (sm - my * mu + mx * mv) / var;
    let tx = (-my * sm - mx * sp + sxx * mu) / var;
    let ty = (-my * sp + mx * sm + sxx * mv) / var;

    [[a, b, tx], [-b, a, ty]]
}

fn invert_affine_rgb(m: &[[f32; 3]; 2]) -> [[f32; 3]; 2] {
    let a = m[0][0] as f64;
    let b = m[0][1] as f64;
    let tx = m[0][2] as f64;
    let c = m[1][0] as f64;
    let d = m[1][1] as f64;
    let ty = m[1][2] as f64;

    let det = a * d - b * c;
    if det.abs() < 1e-12 {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    }

    let inv_det = 1.0 / det;
    [
        [
            (d * inv_det) as f32,
            (-b * inv_det) as f32,
            ((b * ty - d * tx) * inv_det) as f32,
        ],
        [
            (-c * inv_det) as f32,
            (a * inv_det) as f32,
            ((c * tx - a * ty) * inv_det) as f32,
        ],
    ]
}

fn bilinear_sample_rgb(img: &RgbImage, sx: f32, sy: f32) -> [u8; 3] {
    let (w, h) = (img.width() as f32, img.height() as f32);

    if sx < 0.0 || sx > w - 1.0 || sy < 0.0 || sy > h - 1.0 {
        return [0, 0, 0];
    }

    let x0 = sx.floor() as u32;
    let y0 = sy.floor() as u32;
    let x1 = (x0 + 1).min(img.width() - 1);
    let y1 = (y0 + 1).min(img.height() - 1);

    let fx = sx - x0 as f32;
    let fy = sy - y0 as f32;
    let gx = 1.0 - fx;
    let gy = 1.0 - fy;

    let p00 = img.get_pixel(x0, y0).0;
    let p01 = img.get_pixel(x1, y0).0;
    let p10 = img.get_pixel(x0, y1).0;
    let p11 = img.get_pixel(x1, y1).0;

    let mut result = [0u8; 3];
    for c in 0..3 {
        let val = p00[c] as f32 * gy * gx
            + p01[c] as f32 * gy * fx
            + p10[c] as f32 * fy * gx
            + p11[c] as f32 * fy * fx;
        result[c] = val.trunc() as u8;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_models_available_false() {
        // Non-existent directory should return false
        assert!(!MsIrLiveness::models_available("/nonexistent/path"));
    }
}
