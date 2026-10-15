//! Linux Hello - Windows Hello-compatible face authentication for Linux.
//!
//! A Rust implementation of face-based authentication using open-source ONNX models:
//! - Face detection: SCRFD (InsightFace)
//! - Face embedding: ArcFace (InsightFace) 
//! - Anti-spoofing: FaceNox / Silent-Face (RGB veto) plus the IR lighting gate
//!
//! The capture, liveness and matching pipeline is described in `docs/capture-and-liveness.md`.

pub mod antispoof;
pub mod authenticator;
pub mod camera;
pub mod daemon;
pub mod detector;
pub mod embedder;
pub mod lighting;
pub mod liveness;
pub mod matcher;
pub mod pipeline;
pub mod store;
pub mod lid;
pub mod tracker;
#[cfg(feature = "ms-models")]
pub mod ms_models;
#[cfg(feature = "tpm")]
pub mod tpm;

pub use antispoof::{AntiSpoof, LivenessModel};
pub use authenticator::{Attempt, Authenticator, CameraSetup, Capture, OpenCameras};
pub use camera::{Camera, CameraType, IrCamera, IrPair};
pub use detector::FaceDetector;
pub use embedder::FaceEmbedder;
pub use lighting::{Lighting, LightingStatus};
pub use liveness::{LivenessSession, LivenessState, Vote};
pub use matcher::{Print, PrintMatch, PrintSource, Template, MATCH_THRESHOLD};
pub use pipeline::{FacePipeline, FrameCheck};
pub use store::FaceStore;
pub use lid::{get_lid_state, should_suppress_auth, LidState};
pub use tracker::{FaceTracker, Track, TrackState};

use ort::environment::Environment;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// Global ONNX Runtime environment (lazily initialized)
static ORT_ENV: OnceLock<Environment> = OnceLock::new();

/// Get or create the global ORT environment
pub fn get_ort_env() -> anyhow::Result<&'static Environment> {
    // Check if already initialized
    if let Some(env) = ORT_ENV.get() {
        return Ok(env);
    }
    
    // Initialize
    let env = ort::init()
        .with_name("linux_hello")
        .build()
        .map_err(|e| anyhow::anyhow!("failed to init ORT: {}", e))?;
    
    // Store (ignore if another thread beat us)
    let _ = ORT_ENV.set(env);
    
    Ok(ORT_ENV.get().expect("just set"))
}

/// Replicate a grayscale (IR) image into three channels for RGB-trained models.
pub fn gray_to_rgb(image: &image::GrayImage) -> image::RgbImage {
    image::RgbImage::from_fn(image.width(), image.height(), |x, y| {
        let g = image.get_pixel(x, y).0[0];
        image::Rgb([g, g, g])
    })
}

/// Detected face with bounding box and landmarks
#[derive(Debug, Clone)]
pub struct Face {
    /// Bounding box [x1, y1, x2, y2]
    pub bbox: [f32; 4],
    /// Detection confidence
    pub score: f32,
    /// Facial landmarks (5 points)
    pub landmarks: [[f32; 2]; 5],
}

/// Face embedding (feature vector)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Embedding {
    /// Normalized feature vector (typically 512-dim)
    pub features: Vec<f32>,
}

impl Embedding {
    /// Compute cosine similarity with another embedding
    pub fn similarity(&self, other: &Embedding) -> f32 {
        cosine(&self.features, &other.features)
    }
}

/// Cosine similarity of two L2-normalized vectors (their dot product); 0 if the lengths differ.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Liveness detection result
#[derive(Debug, Clone)]
pub struct LivenessResult {
    /// Whether the face is real (not a spoof)
    pub is_real: bool,
    /// Liveness score [0, 1]
    pub score: f32,
}

/// Why a frame pair gives no print (F45, F46). Sent to the PAM module after `NOFACE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaceProblem {
    /// No face detected on the lit frame
    NoFace,
    /// Lit face darker than 35 (F44 status 3)
    TooDark,
    /// Lit face brighter than 205 (F44 status 2)
    TooBright,
    /// Face narrower than 72 px or 0.12 × the shorter frame side (F46)
    TooFar,
    /// Face closer to a frame edge than a fifth of the eye distance (F46)
    Edge,
}

impl FaceProblem {
    pub fn as_str(self) -> &'static str {
        match self {
            FaceProblem::NoFace => "no_face",
            FaceProblem::TooDark => "too_dark",
            FaceProblem::TooBright => "too_bright",
            FaceProblem::TooFar => "too_far",
            FaceProblem::Edge => "edge",
        }
    }
}

/// Outcome of checking one user's face. The daemon sends it to the PAM module as one line whose
/// first word is the verdict: `MATCH 0.850`, `NOMATCH 0.310`, `NOTLIVE`, `NOFACE too_dark`,
/// `NOTENROLLED`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    Match { similarity: f32 },
    NoMatch { similarity: f32 },
    NotLive,
    /// No usable face; the most frequent reason over the session
    NoFace { problem: FaceProblem },
    NotEnrolled,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Verdict::Match { similarity } => write!(f, "MATCH {similarity:.3}"),
            Verdict::NoMatch { similarity } => write!(f, "NOMATCH {similarity:.3}"),
            Verdict::NotLive => f.write_str("NOTLIVE"),
            Verdict::NoFace { problem } => write!(f, "NOFACE {}", problem.as_str()),
            Verdict::NotEnrolled => f.write_str("NOTENROLLED"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedding_similarity() {
        let a = Embedding { features: vec![1.0, 0.0, 0.0] };
        let b = Embedding { features: vec![1.0, 0.0, 0.0] };
        assert!((a.similarity(&b) - 1.0).abs() < 1e-6);
        
        let c = Embedding { features: vec![0.0, 1.0, 0.0] };
        assert!(a.similarity(&c).abs() < 1e-6);
    }
}
