//! Microsoft's face recogniser (v5, tech 11): `FaceVerificationCNN<FaceRecognitionCnnImpl2>` with
//! the conv_net_v1 Inception net of `MODEL_FILES_123` (F30, F50, F51; docs/winhello-models/cefa.md).
//! Its feature is a second opinion next to the ArcFace print ([`crate::matcher`]).

use std::path::Path;

use anyhow::{bail, Result};
use image::GrayImage;
use ort::session::Session;
use ort::value::TensorRef;
use tracing::info;

use super::prep::align_crop;
use crate::calibration::{self, CalibrationTable};

/// Floats in a v5 template (`sub_1800AF6E0`: 1024 bytes / 4).
pub const FEATURES: usize = 256;

pub struct MsRecognizer {
    session: Session,
    /// Microsoft's distance → FAR table of this net (F51), in similarity = 1 − distance
    pub calibration: CalibrationTable,
}

impl MsRecognizer {
    /// Load `MODEL_FILES_123.onnx` and the calibration table of the container's header from
    /// `MODEL_FILES_123.json` (both written by `tools/winhello_models/cefa unpack`) in `dir`.
    pub fn load(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        info!(?dir, "loading Microsoft face recogniser");
        let session = super::load_session(&dir.join("MODEL_FILES_123.onnx"))?;
        let calibration = calibration::load_windows(dir.join("MODEL_FILES_123.json"))?;
        Ok(Self { session, calibration })
    }

    /// The recognition crop (`FaceRecognitionCnnImpl2` slot 6, `sub_180015BE0`) of the face whose
    /// five landmarks (left eye, right eye, nose tip, mouth corners; the Windows IR detector's,
    /// F23, F58) are `points` in `image`: the 281-canvas 5-point alignment resampled to 128×128
    /// that the anti-spoofing nets use too (`sub_18001614C`, `sub_180016B20`, `sub_180016440`).
    pub fn crop(image: &GrayImage, points: &[[f32; 2]; 5]) -> GrayImage {
        align_crop(image, points)
    }

    /// The L2-normalised 256-d feature of the face at `points` in `image` ([`Self::crop`]); the
    /// net takes the crop as raw 0..255, its graph holds the `x/127.5 − 1` of the FaceDataLayer.
    pub fn embed(&mut self, image: &GrayImage, points: &[[f32; 2]; 5]) -> Result<[f32; FEATURES]> {
        let crop = Self::crop(image, points);
        let data: Vec<f32> = crop.as_raw().iter().map(|&p| f32::from(p)).collect();
        let input = TensorRef::from_array_view(([1i64, 1, 128, 128], data.as_slice()))?;
        let outputs = self.session.run(ort::inputs![input])?;
        let (_, out) = outputs[0].try_extract_tensor::<f32>()?;
        let Ok(mut features) = <[f32; FEATURES]>::try_from(out) else {
            bail!("recogniser output has {} floats, not {FEATURES}", out.len());
        };
        let norm = features.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 {
            features.iter_mut().for_each(|v| *v /= norm);
        }
        Ok(features)
    }
}

/// Engine distance of two features (`sub_180054190`, F50): 1 − cos, clamped to ≤ 1.
pub fn distance(a: &[f32], b: &[f32]) -> f32 {
    (1.0 - crate::cosine(a, b)).min(1.0)
}
