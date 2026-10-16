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

/// The converted Windows Hello models that linux_hello runs: the anti-spoofing nets and the
/// occlusion classifier. They come from `scripts/extract_windows_hello_models.py` and the
/// converters in `tools/winhello_models`.
pub struct WindowsHelloModels {
    pub liveness: MsIrLiveness,
    pub occlusion: OcclusionDetector,
}

impl WindowsHelloModels {
    /// Load from `dir` (`models/windows_hello/converted`).
    pub fn load(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        Ok(Self {
            liveness: MsIrLiveness::load(dir.join("convnet"))?,
            occlusion: OcclusionDetector::load(dir.join("occlusion/MODEL_FILES_120.onnx"))?,
        })
    }
}
