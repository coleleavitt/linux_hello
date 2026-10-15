//! Microsoft Windows Hello model integration.
//!
//! These models are converted from Windows Hello binaries and are for personal use only.
//! They should never be committed to the repository.

mod prep;

pub mod ir_liveness;

pub mod occlusion;

pub mod retina;

pub mod pose;

pub use ir_liveness::{MsIrLiveness, MsLivenessResult, MsModelScores};

pub use occlusion::{OcclusionDetector, OcclusionType, OcclusionResult};

pub use retina::{MsRetinaDetector, MsRetinaFace, RetinaModelType};

pub use pose::{HeadPose, PoseChecker, PoseConfig};
