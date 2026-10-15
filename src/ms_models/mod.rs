//! Microsoft Windows Hello model integration.
//!
//! These models are converted from Windows Hello binaries and are for personal use only.
//! They should never be committed to the repository.

#[cfg(feature = "ms-models")]
mod prep;

#[cfg(feature = "ms-models")]
pub mod ir_liveness;

#[cfg(feature = "ms-models")]
pub mod occlusion;

#[cfg(feature = "ms-models")]
pub mod retina;

#[cfg(feature = "ms-models")]
pub mod pose;

#[cfg(feature = "ms-models")]
pub use ir_liveness::{MsIrLiveness, MsLivenessResult, MsModelScores};

#[cfg(feature = "ms-models")]
pub use occlusion::{OcclusionDetector, OcclusionType, OcclusionResult};

#[cfg(feature = "ms-models")]
pub use retina::{MsRetinaDetector, MsRetinaFace, RetinaModelType};

#[cfg(feature = "ms-models")]
pub use pose::{HeadPose, PoseChecker, PoseConfig};
