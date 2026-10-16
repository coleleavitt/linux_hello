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

use anyhow::Result;
use image::{GrayImage, RgbImage};
use ort::session::Session;
use ort::value::TensorRef;
use tracing::{debug, info};

use super::prep::{align_crop, difference_image, thumbnail};
use crate::Face;

/// Thresholds for the flag-on models (from `docs/winhello-models/convnet.md` section 4).
/// Using mode 0 thresholds (lower, more permissive).
pub mod thresholds {
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

/// One frame's anti-spoofing evidence (`RunCascadedClassification`, F25).
#[derive(Debug, Clone, Default)]
pub struct MsFrame {
    /// +1 for a live vote of the IR net, −1 for each spoof vote or veto. Above 0 the frame is a
    /// live frame, below 0 a spoof frame.
    pub total: i32,
    pub scores: MsModelScores,
}

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

        let load_model = |id: u32| super::load_session(&model_dir.join(format!("MODEL_FILES_{id}.onnx")));

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
        [121, 122, 123, 124, 125].iter().all(|id| model_dir.join(format!("MODEL_FILES_{id}.onnx")).exists())
    }

    /// Score one frame pair (F24, F25, F42).
    ///
    /// With `ambient`, the diffusion net (slot 12) runs on the difference image, as Windows does for
    /// good-lighting frames (F40); without it the general net (slot 11) runs on the lit frame. The
    /// colour net (slot 13) needs the face found in the RGB frame: the RGB camera sees a different
    /// view, so IR landmarks do not fit it. The multi-task nets (slots 14, 15) run on every frame;
    /// Windows enables them after the first live vote, so this can only add vetoes.
    pub fn frame(
        &mut self,
        lit: &GrayImage,
        ambient: Option<&GrayImage>,
        face: &Face,
        rgb: Option<(&RgbImage, &Face)>,
    ) -> Result<MsFrame> {
        let mut scores = MsModelScores::default();
        let mut total = 0;

        let (p, real, spoof) = match ambient {
            Some(ambient) => {
                let crop = align_crop(&difference_image(lit, ambient), &face.landmarks);
                let p = run_grayscale(&mut self.diffusion_ir, &crop)?;
                scores.diffusion_ir = Some(p);
                (p, thresholds::DIFFUSION_IR_REAL, thresholds::DIFFUSION_IR_SPOOF)
            }
            None => {
                let p = run_grayscale(&mut self.general_ir, &align_crop(lit, &face.landmarks))?;
                scores.general_ir = Some(p);
                (p, thresholds::GENERAL_IR_REAL, thresholds::GENERAL_IR_SPOOF)
            }
        };
        if p[0] > real {
            total += 1;
        } else if p[1] > spoof {
            total -= 1;
        }

        if let Some((image, rgb_face)) = rgb {
            let p = run_color(&mut self.color, image, &rgb_face.landmarks)?;
            scores.color = Some(p);
            total -= i32::from(p[0] > thresholds::COLOR_SPOOF);
        }

        let thumb = thumbnail(lit);
        let border = run_grayscale(&mut self.border, &thumb)?;
        let hh = run_grayscale(&mut self.hh, &thumb)?;
        scores.border = Some(border);
        scores.hh = Some(hh);
        total -= i32::from(border[1] > thresholds::BORDER_SPOOF);
        total -= i32::from(hh[1] > thresholds::HH_SPOOF);

        debug!(total, ?scores, "Windows Hello anti-spoofing frame");
        Ok(MsFrame { total, scores })
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
        data[i] = px.0[2] as f32; // B from RGB R position
        data[plane + i] = px.0[1] as f32; // G
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
        [(d * inv_det) as f32, (-b * inv_det) as f32, ((b * ty - d * tx) * inv_det) as f32],
        [(-c * inv_det) as f32, (a * inv_det) as f32, ((c * tx - a * ty) * inv_det) as f32],
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
        let val = p00[c] as f32 * gy * gx + p01[c] as f32 * gy * fx + p10[c] as f32 * fy * gx + p11[c] as f32 * fy * fx;
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
