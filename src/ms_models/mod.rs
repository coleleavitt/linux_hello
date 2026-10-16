//! Microsoft Windows Hello model integration.
//!
//! These models are converted from Windows Hello binaries and are for personal use only.

// Template and threshold floats are copied bit for bit from the Windows DLLs.
#![allow(clippy::excessive_precision)]

mod prep;

pub mod ir_liveness;

pub mod lbf;

pub mod occlusion;

pub mod retina;

pub mod pose;

pub mod recognizer;

pub use ir_liveness::{MsFrame, MsIrLiveness, MsModelScores};

pub use lbf::{Alignment, LbfAligner};

pub use occlusion::{OcclusionDetector, OcclusionResult, OcclusionType};

pub use retina::{MsRetinaDetector, MsRetinaFace, RetinaModelType};

pub use pose::IrFaceOrientation;

pub use recognizer::MsRecognizer;

use std::path::Path;

use anyhow::{bail, Context, Result};
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

/// Little-endian reader for the original resource files.
struct Reader {
    data: Vec<u8>,
    pos: usize,
    path: std::path::PathBuf,
}

impl Reader {
    fn open(path: &Path) -> Result<Self> {
        let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Ok(Self { data, pos: 0, path: path.to_path_buf() })
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        let Some(bytes) = self.data.get(self.pos..self.pos + N) else {
            bail!("{}: truncated at offset {}", self.path.display(), self.pos);
        };
        self.pos += N;
        Ok(bytes.try_into().expect("N bytes"))
    }

    fn skip(&mut self, n: usize) -> Result<()> {
        if self.pos + n > self.data.len() {
            bail!("{}: truncated at offset {}", self.path.display(), self.pos);
        }
        self.pos += n;
        Ok(())
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take()?))
    }

    fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_le_bytes(self.take()?))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take()?))
    }

    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take()?))
    }

    fn f32s(&mut self, n: usize) -> Result<Vec<f32>> {
        (0..n).map(|_| self.f32()).collect()
    }

    /// Fail unless the whole file was read.
    fn finish(self) -> Result<()> {
        if self.pos != self.data.len() {
            bail!("{}: {} trailing bytes", self.path.display(), self.data.len() - self.pos);
        }
        Ok(())
    }
}

/// The converted Windows Hello models that linux_hello runs: the anti-spoofing nets, the
/// occlusion classifier, the IR Retina detector for the frontal check and the face recogniser
/// for the second opinion in matching. They come from `scripts/extract_windows_hello_models.py`
/// and the converters in `tools/winhello_models`.
pub struct WindowsHelloModels {
    pub liveness: MsIrLiveness,
    pub occlusion: OcclusionDetector,
    pub retina: MsRetinaDetector,
    pub recognizer: MsRecognizer,
}

impl WindowsHelloModels {
    /// Load from `dir` (`models/windows_hello/converted`).
    pub fn load(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        Ok(Self {
            liveness: MsIrLiveness::load(dir.join("convnet"))?,
            occlusion: OcclusionDetector::load(dir.join("occlusion/MODEL_FILES_120.onnx"))?,
            retina: MsRetinaDetector::load(dir.join("ldmf/ort/MODEL_FILES_110.ort"), RetinaModelType::Standard)?,
            recognizer: MsRecognizer::load(dir.join("cefa"))?,
        })
    }
}
