//! Microsoft Windows Hello model integration.
//!
//! These models are converted from Windows Hello binaries and are for personal use only.

// Template and threshold floats are copied bit for bit from the Windows DLLs.
#![allow(clippy::excessive_precision)]

mod prep;

pub mod ir_liveness;

pub mod occlusion;

pub mod retina;

pub mod pose;

pub use ir_liveness::{MsFrame, MsIrLiveness, MsModelScores};

pub use occlusion::{OcclusionDetector, OcclusionType, OcclusionResult};

pub use retina::{MsRetinaDetector, MsRetinaFace, RetinaModelType};

pub use pose::{HeadPose, PoseChecker, PoseConfig};

use std::path::Path;

use anyhow::{Context, Result};
use ort::session::{builder::GraphOptimizationLevel, Session};

/// Load one converted Windows Hello model (ONNX or ORT format).
fn load_session(path: &Path) -> Result<Session> {
    Session::builder(crate::get_ort_env()?)
        .map_err(|e| anyhow::anyhow!("session builder: {e}"))?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| anyhow::anyhow!("optimization level: {e}"))?
        .commit_from_file(path)
        .with_context(|| format!("loading {} (see docs/winhello-models/)", path.display()))
}

/// Frontal limits of `FaceOrientationResult` (F59).
pub const YAW_LIMIT: f32 = 20.0;
pub const PITCH_LIMIT: f32 = 25.0;

/// Windows' `FaceOrientationResult`: angles in degrees and the four pose flags at +12..15.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FaceOrientation {
    pub pitch: f32,
    pub yaw: f32,
    pub roll: f32,
    /// yaw > 20, yaw < −20, pitch > 25, pitch < −25
    pub flags: [bool; 4],
    /// Nose margin inside the eye/mouth quad (+16); only the legacy estimator sets it
    pub frontal_margin: f32,
}

impl FaceOrientation {
    fn new(pitch: f32, yaw: f32, roll: f32) -> Self {
        let flags = [yaw > YAW_LIMIT, yaw < -YAW_LIMIT, pitch > PITCH_LIMIT, pitch < -PITCH_LIMIT];
        Self { pitch, yaw, roll, flags, frontal_margin: 0.0 }
    }

    /// `IsFrontal` 0x18001a404: no flag set.
    pub fn is_frontal(&self) -> bool {
        !self.flags.contains(&true)
    }
}

/// The converted Windows Hello models that linux_hello runs: the anti-spoofing nets, the
/// occlusion classifier and the IR Retina detector for the frontal check. They come from
/// `scripts/extract_windows_hello_models.py` and the converters in `tools/winhello_models`.
pub struct WindowsHelloModels {
    pub liveness: MsIrLiveness,
    pub occlusion: OcclusionDetector,
    pub retina: MsRetinaDetector,
}

impl WindowsHelloModels {
    /// Load from `dir` (`models/windows_hello/converted`).
    pub fn load(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        Ok(Self {
            liveness: MsIrLiveness::load(dir.join("convnet"))?,
            occlusion: OcclusionDetector::load(dir.join("occlusion/MODEL_FILES_120.onnx"))?,
            retina: MsRetinaDetector::load(dir.join("ldmf/ort/MODEL_FILES_110.ort"), RetinaModelType::Standard)?,
        })
    }
}
